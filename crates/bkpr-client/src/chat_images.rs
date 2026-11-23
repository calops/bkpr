//! Worker-side attachment loading and retained history-preview resources.

use crate::transport::Endpoint;
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{
    Frame, IMAGE_MAX_BYTES, Image, ImageLayer, ImageSource, ImageSourceEncoding, Operation,
    Payload, PixelFormat, Placement, Size,
    channels::{
        AttachmentId, AttachmentMediaType, ChannelId, ChannelRequest, ChannelResponse,
        MAX_ATTACHMENT_BYTES,
    },
    image_source::decode_png_rgba,
};
use bkpr_tui_framework::PreparedTransport;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Rows reserved below the ordinary image label, independent of load or decode status.
pub(super) const PREVIEW_ROWS: u16 = 6;

const MAX_ENTRIES: usize = 64;
const MAX_ENCODED_BYTES: usize = 32 * 1024 * 1024;
// Attachments are limited to this extent by the channel upload contract.
const MAX_DIMENSION: u32 = 4096;
// A stable scene surface can outlive any chat cache or channel selection.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Immutable canonical image and its original Kitty transport, produced off the UI thread.
pub(super) struct Loaded {
    image: Arc<Image>,
    transport: PreparedTransport,
}

/// Fetch one exact PNG through the owning endpoint and decode it on a worker.
pub(super) fn load(
    endpoint: &Endpoint,
    channel_id: ChannelId,
    attachment_id: AttachmentId,
) -> Result<Loaded> {
    let response = endpoint.checked_payload(Operation::Channels {
        request: ChannelRequest::GetAttachment {
            channel_id,
            attachment_id: attachment_id.clone(),
        },
    })?;
    let Payload::Channels(ChannelResponse::Attachment { attachment, bytes }) = response else {
        anyhow::bail!("unexpected channel attachment reply");
    };
    ensure!(
        attachment.id == attachment_id && attachment.media_type == AttachmentMediaType::Png,
        "unexpected channel attachment identity or format"
    );
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Result<Loaded> {
    ensure!(bytes.len() <= MAX_ATTACHMENT_BYTES, "attachment byte limit");
    let dimensions = bytes
        .strip_prefix(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR")
        .and_then(|header| header.get(..8))
        .context("missing PNG dimensions")?;
    let width = u32::from_be_bytes(dimensions[..4].try_into()?);
    let height = u32::from_be_bytes(dimensions[4..].try_into()?);
    ensure!(
        (1..=MAX_DIMENSION).contains(&width) && (1..=MAX_DIMENSION).contains(&height),
        "attachment dimensions exceed limits"
    );
    let format = PixelFormat::Rgba;
    let len = width as usize * height as usize * format.bytes_per_pixel();
    ensure!(len <= IMAGE_MAX_BYTES, "attachment pixel limit");
    let pixels = decode_png_rgba(bytes, width, height, format, len)?;
    let generation = NEXT_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()
        .context("chat image generations exhausted")?;
    let png = Arc::from(bytes);
    let image = Arc::new(Image {
        id: 1,
        generation,
        width,
        height,
        format,
        pixels,
        source: Some(Arc::new(ImageSource {
            encoding: ImageSourceEncoding::Png,
            bytes: Arc::clone(&png),
        })),
    });
    let transport = PreparedTransport::from_verified_png(&image, png);
    Ok(Loaded { image, transport })
}

struct Slot {
    last_visible: u64,
    state: State,
}

enum State {
    Pending,
    Ready(Arc<Image>),
    Fallback,
}

fn png_bytes(image: &Image) -> usize {
    image.source.as_ref().map_or(0, |source| source.bytes.len())
}

/// Bounded attachment cache for one endpoint's live authority. In-flight keys
/// deduplicate requests; failed and over-budget previews retain the text fallback.
#[derive(Default)]
pub(super) struct Cache {
    channels: HashMap<ChannelId, HashMap<AttachmentId, Slot>>,
    frame: u64,
    count: usize,
    decoded: usize,
    encoded: usize,
}

impl Cache {
    /// Start rebuilding the history graphics frame. Appended resources are pinned
    /// until the next rebuild, so simultaneous visible attachments cannot evict each other.
    pub(super) fn start_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Reserve one worker job for a visible, previously uncached attachment.
    pub(super) fn begin(&mut self, channel: &ChannelId, attachment: &AttachmentId) -> bool {
        let frame = self.frame;
        if let Some(slot) = self.slot(channel, attachment) {
            slot.last_visible = frame;
            return false;
        }
        if self.count >= MAX_ENTRIES && !self.evict(false) {
            return false;
        }
        let slot = Slot {
            last_visible: frame,
            state: State::Pending,
        };
        if let Some(entries) = self.channels.get_mut(channel) {
            entries.insert(attachment.clone(), slot);
        } else {
            self.channels
                .insert(channel.clone(), HashMap::from([(attachment.clone(), slot)]));
        }
        self.count += 1;
        true
    }

    /// Release a reservation for which no job was admitted and no result will arrive.
    pub(super) fn cancel(&mut self, channel: &ChannelId, attachment: &AttachmentId) {
        let Some(entries) = self.channels.get_mut(channel) else {
            return;
        };
        if !entries
            .get(attachment)
            .is_some_and(|slot| matches!(slot.state, State::Pending))
        {
            return;
        }
        entries.remove(attachment);
        if entries.is_empty() {
            self.channels.remove(channel);
        }
        self.count -= 1;
    }

    /// Publish the result of an admitted job. Register a returned payload with
    /// the terminal guard before submitting any frame that uses its image.
    pub(super) fn finish(
        &mut self,
        channel: &ChannelId,
        attachment: &AttachmentId,
        result: Result<Loaded>,
    ) -> Option<PreparedTransport> {
        if !self
            .slot(channel, attachment)
            .is_some_and(|slot| matches!(slot.state, State::Pending))
        {
            return None;
        }
        let loaded = result.ok().filter(|loaded| self.reserve(&loaded.image));
        let slot = self.slot(channel, attachment).unwrap();
        if let Some(Loaded { image, transport }) = loaded {
            slot.state = State::Ready(image);
            Some(transport)
        } else {
            slot.state = State::Fallback;
            None
        }
    }

    pub(super) fn unavailable(&self, channel: &ChannelId, attachment: &AttachmentId) -> bool {
        self.channels
            .get(channel)
            .and_then(|entries| entries.get(attachment))
            .is_some_and(|slot| matches!(slot.state, State::Fallback))
    }

    /// Add a ready preview to the history-local graphics frame. A negative row
    /// leaves partially scrolled images to ordinary scene clipping.
    pub(super) fn append(
        &mut self,
        frame: &mut Frame,
        channel: &ChannelId,
        attachment: &AttachmentId,
        col: u16,
        row: i32,
        columns: u16,
    ) {
        let visible_frame = self.frame;
        let Some(slot) = self.slot(channel, attachment) else {
            return;
        };
        if row >= i32::from(frame.size.rows) || row <= -i32::from(PREVIEW_ROWS) {
            return;
        }
        slot.last_visible = visible_frame;
        let State::Ready(image) = &slot.state else {
            return;
        };
        let Some(placement) = placement(image, frame.size, col, row, columns) else {
            return;
        };
        if !frame.images.iter().any(|entry| Arc::ptr_eq(entry, image)) {
            frame.images.push(Arc::clone(image));
        }
        frame.placements.push(placement);
    }

    fn slot(&mut self, channel: &ChannelId, attachment: &AttachmentId) -> Option<&mut Slot> {
        self.channels.get_mut(channel)?.get_mut(attachment)
    }

    fn reserve(&mut self, image: &Image) -> bool {
        let decoded = image.pixels.len();
        let encoded = png_bytes(image);
        // Match the compositor's aggregate canonical pixel limit for this frame.
        while self.decoded + decoded > IMAGE_MAX_BYTES || self.encoded + encoded > MAX_ENCODED_BYTES
        {
            if !self.evict(true) {
                return false;
            }
        }
        self.decoded += decoded;
        self.encoded += encoded;
        true
    }

    fn evict(&mut self, needs_bytes: bool) -> bool {
        let evictable = |slot: &Slot| {
            slot.last_visible != self.frame
                && match slot.state {
                    State::Pending => false,
                    State::Ready(_) => true,
                    State::Fallback => !needs_bytes,
                }
        };
        let Some(oldest) = self
            .channels
            .values()
            .flat_map(HashMap::values)
            .filter(|slot| evictable(slot))
            .map(|slot| slot.last_visible)
            .min()
        else {
            return false;
        };
        let Some((_, slot)) = self.channels.values_mut().find_map(|entries| {
            entries
                .extract_if(|_, slot| slot.last_visible == oldest && evictable(slot))
                .next()
        }) else {
            return false;
        };
        if let State::Ready(image) = slot.state {
            self.decoded -= image.pixels.len();
            self.encoded -= png_bytes(&image);
        }
        self.count -= 1;
        self.channels.retain(|_, entries| !entries.is_empty());
        true
    }
}

fn placement(image: &Image, size: Size, col: u16, row: i32, columns: u16) -> Option<Placement> {
    let max_width =
        u32::from(columns.min(size.cols.saturating_sub(col))).checked_mul(size.cell_width)?;
    let max_height = u32::from(PREVIEW_ROWS).checked_mul(size.cell_height)?;
    if max_width == 0 || max_height == 0 {
        return None;
    }
    let width_at_max_height = u64::from(max_height) * u64::from(image.width);
    let height_at_max_width = u64::from(max_width) * u64::from(image.height);
    let (width, height) = if height_at_max_width <= width_at_max_height {
        (
            max_width,
            (height_at_max_width / u64::from(image.width)).max(1) as u32,
        )
    } else {
        (
            (width_at_max_height / u64::from(image.height)).max(1) as u32,
            max_height,
        )
    };
    // A Kitty scaled draw must end exactly on cell edges to use the original
    // PNG without rasterizing a resized copy. Shift at most one cell per axis.
    let offset_x = (size.cell_width - width % size.cell_width) % size.cell_width;
    let offset_y = (size.cell_height - height % size.cell_height) % size.cell_height;
    let top = i64::from(row) * i64::from(size.cell_height) + i64::from(offset_y);
    let bottom = i64::from(size.rows) * i64::from(size.cell_height);
    if size.rows == 0 || top >= bottom || top + i64::from(height) <= 0 {
        return None;
    }
    Some(Placement {
        image_id: image.id,
        generation: image.generation,
        placement_id: 0,
        col: i32::from(col),
        row,
        offset_x,
        offset_y,
        width,
        height,
        source_x: 0,
        source_y: 0,
        source_width: image.width,
        source_height: image.height,
        layer: ImageLayer::BelowText,
        z: -1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partly_visible_preview_preserves_aspect_and_native_png_geometry() {
        let image = Image {
            id: 1,
            generation: 2,
            width: 160,
            height: 90,
            format: PixelFormat::Rgba,
            pixels: vec![0; 160 * 90 * 4],
            source: None,
        };
        let size = Size {
            cols: 20,
            rows: 10,
            cell_width: 10,
            cell_height: 20,
        };
        let preview = placement(&image, size, 2, -1, 12).unwrap();
        assert_eq!((preview.width, preview.height), (120, 67));
        assert_eq!((preview.col, preview.row), (2, -1));
        assert_eq!((preview.source_width, preview.source_height), (160, 90));
        assert_eq!((preview.width + preview.offset_x) % size.cell_width, 0);
        assert_eq!((preview.height + preview.offset_y) % size.cell_height, 0);
        assert!(placement(&image, size, 2, -4, 12).is_none());
    }

    #[test]
    fn a_failed_preview_does_not_reload_or_block_another_channel() {
        let mut cache = Cache::default();
        let channel = ChannelId("one".into());
        let other = ChannelId("two".into());
        let attachment = AttachmentId("attachment".into());
        assert!(cache.begin(&channel, &attachment));
        assert!(!cache.begin(&channel, &attachment));
        assert!(
            cache
                .finish(
                    &channel,
                    &attachment,
                    Err(anyhow::anyhow!("decode failure"))
                )
                .is_none()
        );
        cache.start_frame();
        assert!(!cache.begin(&channel, &attachment));
        assert!(cache.begin(&other, &attachment));
        cache.cancel(&other, &attachment);
        assert!(cache.begin(&other, &attachment));
    }
}
