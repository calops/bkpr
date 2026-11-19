//! Lossless image delivery independent of scene and input responses.
use crate::{IMAGE_MAX_BYTES, Image, ImageKey, PixelFormat, Placement};
use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::io::Read;

/// Largest binary data slice in one resource response.
pub const IMAGE_CHUNK_BYTES: usize = 64 * 1024;
/// Preallocation bound for a native resource response, including its typed envelope.
pub const IMAGE_RESOURCE_MAX_FRAME: usize = IMAGE_CHUNK_BYTES + 4096;
/// Explicit scene opt-in required for descriptor-only image delivery.
pub const PROGRESSIVE_SCENE_ENCODING: &str = "planus_progressive_v1";

/// Complete pixel shape validated before receiving or allocating its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDescriptor {
    /// Immutable pane-scoped resource identity.
    pub key: ImageKey,
    /// Width in pixels, greater than zero.
    pub width: u32,
    /// Height in pixels, greater than zero.
    pub height: u32,
    /// Packed channel layout.
    pub format: PixelFormat,
    /// Exact packed pixel byte length, at most [`IMAGE_MAX_BYTES`].
    pub decoded_len: u64,
}
impl ImageDescriptor {
    /// Describe canonical pixels, rejecting invalid dimensions or resource bounds.
    pub fn from_image(image: &Image) -> Result<Self> {
        let descriptor = Self {
            key: image.key(),
            width: image.width,
            height: image.height,
            format: image.format,
            decoded_len: image.pixels.len() as u64,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
    /// Check dimensions, checked arithmetic and the decoded byte quota before allocation.
    pub fn validate(&self) -> Result<()> {
        let expected = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .and_then(|pixels| pixels.checked_mul(self.format.bytes_per_pixel() as u64));
        ensure!(
            self.width > 0
                && self.height > 0
                && expected == Some(self.decoded_len)
                && self.decoded_len <= IMAGE_MAX_BYTES as u64,
            "image descriptor dimensions or decoded byte limit"
        );
        Ok(())
    }
    /// Validate the descriptor and require the image's exact identity, dimensions, format and byte length.
    pub fn validate_image(&self, image: &Image) -> Result<()> {
        self.validate()?;
        ensure!(
            image.key() == self.key
                && image.width == self.width
                && image.height == self.height
                && image.format == self.format
                && image.pixels.len() as u64 == self.decoded_len,
            "image differs from its resource descriptor"
        );
        Ok(())
    }
    /// Check exact identity and a nonempty source crop wholly inside the image.
    pub fn validate_placement(&self, placement: &Placement) -> Result<()> {
        ensure!(
            placement.image_id == self.key.id && placement.generation == self.key.generation,
            "image placement generation mismatch"
        );
        ensure!(
            placement.source_width > 0
                && placement.source_height > 0
                && placement
                    .source_x
                    .checked_add(placement.source_width)
                    .is_some_and(|end| end <= self.width)
                && placement
                    .source_y
                    .checked_add(placement.source_height)
                    .is_some_and(|end| end <= self.height),
            "image placement crop outside descriptor"
        );
        Ok(())
    }
}

/// Fixed lossless image encodings; zstd uses one frame without a dictionary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageEncoding {
    /// Exact packed pixels, used when compression would expand them.
    Raw,
    /// A single zstd frame with its exact decoded content size.
    Zstd,
}

/// One response in a request-scoped resource stream; only End completes reception.
#[derive(Clone)]
pub enum ImageResource {
    /// Accepted descriptor while bounded shared preparation is pending; not pixel progress.
    Preparing {
        /// Exact shape expected by the scene manifest.
        descriptor: ImageDescriptor,
    },
    /// Encoded body announcement, preceding all data.
    Start {
        /// Exact shape expected by the scene manifest.
        descriptor: ImageDescriptor,
        /// Lossless body representation.
        encoding: ImageEncoding,
        /// Exact encoded length, nonzero and no larger than the decoded resource.
        encoded_len: u64,
    },
    /// Contiguous bounded body segment.
    Data {
        /// Exact immutable generation announced by Start.
        key: ImageKey,
        /// Byte position in the encoded body.
        offset: u64,
        /// At most [`IMAGE_CHUNK_BYTES`] bytes, shared with native receive storage.
        data: Bytes,
    },
    /// Completion fence; no resource may be retained before this frame validates.
    End {
        /// Exact immutable generation announced by Start.
        key: ImageKey,
        /// Exact total body length.
        encoded_len: u64,
    },
}
impl ImageResource {
    /// Identity for this frame, independently of its stream state.
    pub fn key(&self) -> ImageKey {
        match self {
            Self::Preparing { descriptor } | Self::Start { descriptor, .. } => descriptor.key,
            Self::Data { key, .. } | Self::End { key, .. } => *key,
        }
    }
    /// Body-free diagnostics; never expands pixels into JSON.
    pub fn diagnostic(&self) -> serde_json::Value {
        match self {
            Self::Preparing { descriptor } => {
                serde_json::json!({"state":"preparing","descriptor":descriptor})
            }
            Self::Start {
                descriptor,
                encoding,
                encoded_len,
            } => {
                serde_json::json!({"state":"start","descriptor":descriptor,"encoding":encoding,"encoded_len":encoded_len})
            }
            Self::Data { key, offset, data } => {
                serde_json::json!({"state":"data","key":key,"offset":offset,"length":data.len()})
            }
            Self::End { key, encoded_len } => {
                serde_json::json!({"state":"end","key":key,"encoded_len":encoded_len})
            }
        }
    }
}
impl std::fmt::Debug for ImageResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparing { descriptor } => formatter
                .debug_struct("Preparing")
                .field("descriptor", descriptor)
                .finish(),
            Self::Start {
                descriptor,
                encoding,
                encoded_len,
            } => formatter
                .debug_struct("Start")
                .field("descriptor", descriptor)
                .field("encoding", encoding)
                .field("encoded_len", encoded_len)
                .finish(),
            Self::Data { key, offset, data } => formatter
                .debug_struct("Data")
                .field("key", key)
                .field("offset", offset)
                .field("length", &data.len())
                .finish(),
            Self::End { key, encoded_len } => formatter
                .debug_struct("End")
                .field("key", key)
                .field("encoded_len", encoded_len)
                .finish(),
        }
    }
}

/// Encode canonical pixels once off-path; never returns a body larger than the pixels.
/// Callers bound worker concurrency, input retention and reusable cache bytes.
pub fn encode_image(image: &Image) -> Result<(ImageEncoding, std::borrow::Cow<'_, [u8]>)> {
    ImageDescriptor::from_image(image)?;
    let mut compressor = zstd::bulk::Compressor::new(1)?;
    compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
    let compressed = compressor.compress(&image.pixels)?;
    if compressed.len() < image.pixels.len() {
        Ok((
            ImageEncoding::Zstd,
            std::borrow::Cow::Owned(compressed.into_boxed_slice().into_vec()),
        ))
    } else {
        Ok((
            ImageEncoding::Raw,
            std::borrow::Cow::Borrowed(&image.pixels),
        ))
    }
}

/// Bounded request-local receiver. Errors permanently poison this transfer.
/// Consumers must additionally fence endpoint, pane, view and current generation before installation.
pub struct ImageTransfer {
    descriptor: ImageDescriptor,
    body: Vec<u8>,
    start: Option<(ImageEncoding, u64)>,
    ended: bool,
    failed: bool,
}
impl std::fmt::Debug for ImageTransfer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageTransfer")
            .field("descriptor", &self.descriptor)
            .field("received", &self.body.len())
            .field("start", &self.start)
            .field("ended", &self.ended)
            .field("failed", &self.failed)
            .finish()
    }
}
impl ImageTransfer {
    /// Validate the descriptor without allocating its pixel or encoded body.
    pub fn new(descriptor: ImageDescriptor) -> Result<Self> {
        descriptor.validate()?;
        Ok(Self {
            descriptor,
            body: Vec::new(),
            start: None,
            ended: false,
            failed: false,
        })
    }
    /// Validate ordering, exact lengths and offsets; return an image only at a valid End.
    /// Reject corrupt, concatenated, oversized-window or wrong-sized compressed frames.
    /// Check cancellation before every bounded decoding turn and before publishing pixels.
    pub fn accept(
        &mut self,
        resource: ImageResource,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Image>> {
        ensure!(
            !self.failed && !self.ended,
            "image transfer already ended or failed"
        );
        let result = self.accept_inner(resource, &cancelled);
        if result.is_err() {
            self.failed = true;
            self.body = Vec::new();
        }
        result
    }
    fn accept_inner(
        &mut self,
        resource: ImageResource,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Image>> {
        ensure!(!cancelled(), "image transfer cancelled");
        ensure!(
            resource.key() == self.descriptor.key,
            "image stream generation mismatch"
        );
        match resource {
            ImageResource::Preparing { descriptor } => {
                ensure!(
                    self.start.is_none() && descriptor == self.descriptor,
                    "unexpected image preparation descriptor"
                );
            }
            ImageResource::Start {
                descriptor,
                encoding,
                encoded_len,
            } => {
                ensure!(
                    self.start.is_none() && descriptor == self.descriptor,
                    "unexpected image start descriptor"
                );
                ensure!(
                    encoded_len > 0
                        && encoded_len <= descriptor.decoded_len
                        && (encoding != ImageEncoding::Raw
                            || encoded_len == descriptor.decoded_len),
                    "image encoded length limit"
                );
                self.body.try_reserve_exact(encoded_len as usize)?;
                self.start = Some((encoding, encoded_len));
            }
            ImageResource::Data { offset, data, .. } => {
                let (_, length) = self.start.context("image data before start")?;
                ensure!(
                    !data.is_empty()
                        && data.len() <= IMAGE_CHUNK_BYTES
                        && offset == self.body.len() as u64
                        && offset
                            .checked_add(data.len() as u64)
                            .is_some_and(|end| end <= length),
                    "image data offset or length mismatch"
                );
                self.body.extend_from_slice(&data);
            }
            ImageResource::End { encoded_len, .. } => {
                let (encoding, length) = self.start.context("image end before start")?;
                ensure!(
                    encoded_len == length && self.body.len() as u64 == length,
                    "incomplete image body"
                );
                let pixels = match encoding {
                    ImageEncoding::Raw => std::mem::take(&mut self.body),
                    ImageEncoding::Zstd => {
                        ensure!(
                            zstd::zstd_safe::find_frame_compressed_size(&self.body)
                                .map_err(|_| anyhow::anyhow!("invalid zstd image frame"))?
                                == self.body.len(),
                            "image zstd trailing or concatenated frames"
                        );
                        ensure!(
                            zstd::zstd_safe::get_frame_content_size(&self.body)
                                .map_err(|_| anyhow::anyhow!("invalid zstd image content size"))?
                                == Some(self.descriptor.decoded_len),
                            "image zstd decoded length mismatch"
                        );
                        let mut decoder =
                            zstd::stream::read::Decoder::with_buffer(self.body.as_slice())?
                                .single_frame();
                        decoder.window_log_max(23)?;
                        let mut pixels = Vec::new();
                        pixels.try_reserve_exact(self.descriptor.decoded_len as usize)?;
                        let mut chunk = [0; IMAGE_CHUNK_BYTES];
                        while pixels.len() < self.descriptor.decoded_len as usize {
                            ensure!(!cancelled(), "image transfer cancelled");
                            let remaining = self.descriptor.decoded_len as usize - pixels.len();
                            let count =
                                decoder.read(&mut chunk[..remaining.min(IMAGE_CHUNK_BYTES)])?;
                            ensure!(count > 0, "image decoded body truncated");
                            pixels.extend_from_slice(&chunk[..count]);
                        }
                        ensure!(
                            decoder.read(&mut [0; 1])? == 0,
                            "image decoded length exceeds descriptor"
                        );
                        pixels
                    }
                };
                ensure!(
                    pixels.len() as u64 == self.descriptor.decoded_len,
                    "image decoded length mismatch"
                );
                ensure!(!cancelled(), "image transfer cancelled");
                self.ended = true;
                self.body = Vec::new();
                return Ok(Some(Image {
                    id: self.descriptor.key.id,
                    generation: self.descriptor.key.generation,
                    width: self.descriptor.width,
                    height: self.descriptor.height,
                    format: self.descriptor.format,
                    pixels,
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
