pub use crate::prepared_png::{PreparedTransport, TransportEncoding};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bkpr_protocol::*;
#[cfg(test)]
use std::sync::Arc;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    io::{self, Write},
    time::Instant,
};
use termwiz::input::{InputEvent, InputParser, KeyCode, Modifiers};

#[path = "background.rs"]
mod background;
#[path = "render_geometry.rs"]
mod geometry;
#[path = "render_style.rs"]
mod style;
pub use background::TerminalBackground;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ColorState {
    foreground: [u8; 3],
    background: [u8; 3],
    host_revision: u64,
    truecolor: bool,
    physical_background: Option<[u8; 3]>,
}

#[derive(Default)]
struct TextState {
    viewport: Option<Size>,
    rows: Vec<Row>,
    palette: Vec<[u8; 3]>,
    colors: Option<ColorState>,
    chrome: String,
    cursor: Option<Cursor>,
    physical_cursor_color: Option<Option<[u8; 3]>>,
}

const MAX_CACHED_PROJECTIONS: usize = 512;
const MAX_CACHED_PROJECTION_BYTES: usize = 8 * 1024 * 1024;
const MAX_PREPARED_TRANSPORTS: usize = 64;
const MAX_PREPARED_TRANSPORT_BYTES: usize = 16 * 1024 * 1024;

struct ProjectionResource {
    id: u32,
    rgba_bytes: usize,
    placed: bool,
}

struct GraphicsState {
    next_image: u32,
    image_ids: HashMap<ImageKey, u32>,
    derived_ids: hashlink::LinkedHashMap<geometry::DerivedKey, ProjectionResource>,
    derived_bytes: usize,
    prepared_transports: hashlink::LinkedHashMap<usize, PreparedTransport>,
    prepared_transport_bytes: usize,
    text: TextState,
    projections: Vec<geometry::Projection>,
}
impl GraphicsState {
    fn new(start_id: u32) -> Self {
        Self {
            next_image: start_id,
            image_ids: HashMap::new(),
            derived_ids: hashlink::LinkedHashMap::new(),
            derived_bytes: 0,
            prepared_transports: hashlink::LinkedHashMap::new(),
            prepared_transport_bytes: 0,
            text: TextState::default(),
            projections: Vec::new(),
        }
    }
    fn allocate(&mut self) -> Result<u32> {
        self.next_image = self
            .next_image
            .checked_add(1)
            .context("physical image ID capacity exhausted")?;
        Ok(self.next_image)
    }
    fn all_ids(&self) -> impl Iterator<Item = &u32> {
        self.image_ids
            .values()
            .chain(self.derived_ids.values().map(|resource| &resource.id))
    }

    fn purge_expired_pngs(&mut self) {
        let bytes = &mut self.prepared_transport_bytes;
        self.prepared_transports.retain(|_, source| {
            if source.is_live() {
                true
            } else {
                *bytes -= source.byte_len();
                false
            }
        });
    }

    fn register_prepared_transports<'a>(
        &mut self,
        sources: impl IntoIterator<Item = &'a PreparedTransport>,
    ) {
        self.purge_expired_pngs();
        for source in sources {
            let bytes = source.byte_len();
            if !source.is_live() || bytes > MAX_PREPARED_TRANSPORT_BYTES {
                continue;
            }
            let key = source.source_id();
            if self.prepared_transports.to_back(&key).is_some() {
                continue;
            }
            while self.prepared_transports.len() >= MAX_PREPARED_TRANSPORTS
                || self.prepared_transport_bytes + bytes > MAX_PREPARED_TRANSPORT_BYTES
            {
                let (_, evicted) = self.prepared_transports.pop_front().unwrap();
                self.prepared_transport_bytes -= evicted.byte_len();
            }
            self.prepared_transports.insert(key, source.clone());
            self.prepared_transport_bytes += bytes;
        }
    }

    fn prepared_transport(&self, image: &Image) -> Option<&PreparedTransport> {
        self.prepared_transports
            .get(&(std::ptr::from_ref(image) as usize))
            .filter(|source| source.matches(image))
    }

    fn prepare_projection_cache(
        &mut self,
        prepared: &geometry::Prepared<'_>,
        out: &mut impl Write,
    ) -> Result<()> {
        let mut error = None;
        let bytes = &mut self.derived_bytes;
        self.derived_ids.retain(|key, resource| {
            if prepared.images.contains_key(&key.image) || error.is_some() {
                return true;
            }
            match write!(out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", resource.id) {
                Ok(()) => {
                    *bytes -= resource.rgba_bytes;
                    false
                }
                Err(failure) => {
                    error = Some(failure);
                    true
                }
            }
        });
        if let Some(error) = error {
            return Err(error.into());
        }
        let mut visible_bytes = 0;
        let mut visible_count = 0;
        for key in &prepared.derived {
            if let Some(resource) = self.derived_ids.to_back(key) {
                visible_bytes += resource.rgba_bytes;
                visible_count += 1;
            }
        }
        // Touch current projections first, so every inactive entry precedes them.
        // Prune before admitting new visible rasters: cache residency cannot add
        // another former full scene to the next frame's peak GPU byte charge.
        while self.derived_ids.len() - visible_count > MAX_CACHED_PROJECTIONS
            || self.derived_bytes - visible_bytes > MAX_CACHED_PROJECTION_BYTES
        {
            let (key, resource) = self
                .derived_ids
                .front()
                .context("projection cache accounting underflow")?;
            if prepared.derived.contains(key) {
                bail!("visible projection cache accounting mismatch");
            }
            write!(out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", resource.id)?;
            let (_, resource) = self.derived_ids.pop_front().unwrap();
            self.derived_bytes -= resource.rgba_bytes;
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct OwnedNotification {
    id: crate::notification::Id,
    report_activation: bool,
    report_close: bool,
}

/// Owning host-terminal modes, physical observations and presentation resources.
pub struct TerminalGuard {
    graphics: GraphicsState,
    /// Whether the owned Kitty keyboard mode must be popped on exit.
    pub kitty_keyboard: bool,
    clipboard_mode: Option<bool>,
    requested_clipboard_mode: Option<bool>,
    theme_mode: Option<bool>,
    background: background::Background,
    notification_probe: crate::notification::Probe,
    notifications: VecDeque<OwnedNotification>,
    observed_focus: Option<bool>,
}
impl TerminalGuard {
    /// Retain bounded worker-verified PNG transports for exact source allocations.
    /// Expired sources are purged; registration performs no codec or disk work.
    pub fn register_prepared_transports<'a>(
        &mut self,
        sources: impl IntoIterator<Item = &'a PreparedTransport>,
    ) {
        self.graphics.register_prepared_transports(sources);
    }

    /// Positively correlated physical-terminal features, or unknown.
    pub fn notification_capabilities(&self) -> Option<crate::notification::Capabilities> {
        self.notification_probe.capabilities()
    }

    /// Pending discovery deadline for the existing input loop, without polling.
    pub fn notification_deadline(&self) -> Option<Instant> {
        self.notification_probe.deadline()
    }

    /// Expire unanswered discovery without inventing negative capability evidence.
    pub fn expire_notifications(&mut self, now: Instant) {
        self.notification_probe.expire(now);
    }

    /// Consume correlated capabilities and return only owned, requested callbacks.
    /// A tracked close releases ownership; an untracked report is not closure.
    pub fn notification_reply(
        &mut self,
        packet: &[u8],
        now: Instant,
    ) -> Option<crate::notification::Reply> {
        use crate::notification::Reply;
        let reply = crate::notification::parse_reply(packet)?;
        match reply {
            Reply::Capabilities { .. } => {
                self.notification_probe.consume(reply, now);
                None
            }
            Reply::Activated { id } => self
                .notifications
                .iter()
                .any(|owned| owned.id == id && owned.report_activation)
                .then_some(reply),
            Reply::Closed { id, tracked } => {
                let index = self
                    .notifications
                    .iter()
                    .position(|owned| owned.id == id && owned.report_close)?;
                if tracked {
                    self.notifications.remove(index);
                }
                Some(reply)
            }
        }
    }

    /// Physical terminal focus from trusted reports; initially unknown.
    pub fn observed_focus(&self) -> Option<bool> {
        self.observed_focus
    }

    /// Record only the shared demultiplexer's exact physical focus reports.
    pub fn observe_focus_reply(&mut self, packet: &[u8]) -> Option<bool> {
        let focused = match packet {
            b"focus;in" => true,
            b"focus;out" => false,
            _ => return None,
        };
        self.observed_focus = Some(focused);
        Some(focused)
    }

    /// Write a validated notification and own its bounded cleanup/callback lifetime.
    /// Returns the evicted correlation for caller callback removal. Output, including
    /// partial output on error, never establishes provider acceptance or display.
    pub fn show_notification(
        &mut self,
        out: &mut impl Write,
        notice: &crate::notification::Notification<'_>,
    ) -> io::Result<Option<crate::notification::Id>> {
        let capabilities = self.notification_capabilities().unwrap_or_default();
        crate::notification::validate(capabilities, notice)?;
        if let Some(index) = self
            .notifications
            .iter()
            .position(|owned| owned.id == notice.id)
        {
            self.notifications.remove(index);
        }
        let evicted = if self.notifications.len() == crate::notification::MAX_ACTIVE {
            self.notifications.pop_front().map(|owned| owned.id)
        } else {
            None
        };
        self.notifications.push_back(OwnedNotification {
            id: notice.id,
            report_activation: false,
            report_close: false,
        });
        if let Some(id) = evicted
            && capabilities.close
        {
            crate::notification::close(out, capabilities, id)?;
        }
        crate::notification::show_validated(out, capabilities, notice)?;
        let owned = self
            .notifications
            .back_mut()
            .expect("notification was retained");
        owned.report_activation = matches!(
            notice.activation,
            crate::notification::Activation::Report
                | crate::notification::Activation::ReportAndFocus
        );
        owned.report_close = notice.report_close;
        Ok(evicted)
    }

    /// Release an owned correlation and request closure only when supported.
    /// Unknown IDs produce no output; a write is not proof of OS closure.
    pub fn close_notification(
        &mut self,
        out: &mut impl Write,
        id: crate::notification::Id,
    ) -> io::Result<()> {
        let Some(index) = self.notifications.iter().position(|owned| owned.id == id) else {
            return Ok(());
        };
        self.notifications.remove(index);
        if let Some(capabilities) = self.notification_capabilities()
            && capabilities.close
        {
            crate::notification::close(out, capabilities, id)?;
        }
        Ok(())
    }

    /// Snapshot ownership immediately before sending ColorProbe::query().
    pub fn prepare_background_query(&mut self) {
        self.background.prepare_query();
    }
    /// Filter demultiplexed OSC 11 before ColorProbe::feed. All other replies
    /// are borrowed unchanged; no second input parser or reader is involved.
    pub fn observe_background_reply<'a>(
        &mut self,
        reply: &'a [u8],
        out: &mut impl Write,
    ) -> Result<Cow<'a, [u8]>> {
        self.background.observe(reply, out)
    }
    /// Apply only for actually ready pane material; None restores normal output.
    pub fn set_background(
        &mut self,
        out: &mut impl Write,
        background: Option<TerminalBackground>,
    ) -> Result<()> {
        self.background.apply(background, out)
    }
    /// Retain the first observed enhanced-paste mode for exact exit restoration.
    pub fn observe_enhanced_paste(&mut self, enabled: bool) {
        self.clipboard_mode.get_or_insert(enabled);
        self.requested_clipboard_mode.get_or_insert(enabled);
    }
    /// Select the host paste framing without touching clipboard contents.
    /// Restore the originally observed mode on exit and avoid redundant writes.
    pub fn set_enhanced_paste(&mut self, out: &mut impl Write, enabled: bool) -> Result<()> {
        if self.clipboard_mode.is_none() || self.requested_clipboard_mode == Some(enabled) {
            return Ok(());
        }
        write!(out, "\x1b[?5522{}", if enabled { "h" } else { "l" })?;
        out.flush()?;
        self.requested_clipboard_mode = Some(enabled);
        Ok(())
    }
    /// Enable host theme-change notifications only after observing a mutable
    /// supported DEC mode; restore exactly the originally observed mode.
    pub fn theme_reply(&mut self, reply: &[u8], out: &mut impl Write) -> Result<()> {
        let enabled = match reply {
            b"theme-cap;?2031;1$" => true,
            b"theme-cap;?2031;2$" => false,
            _ => return Ok(()),
        };
        if self.theme_mode.is_none() {
            self.theme_mode = Some(enabled);
            if !enabled {
                out.write_all(b"\x1b[?2031h")?;
                out.flush()?;
            }
        }
        Ok(())
    }
}
impl TerminalGuard {
    fn restore_output(&mut self, out: &mut impl Write) {
        // OSC restoration must precede alternate-screen reset, including when
        // unwinding after a partial write or render error.
        let _ = self.background.restore(out);
        while let Some(owned) = self.notifications.pop_front() {
            if let Some(capabilities) = self.notification_capabilities()
                && capabilities.close
            {
                let _ = crate::notification::close(out, capabilities, owned.id);
            }
        }
        for id in self.graphics.all_ids() {
            let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
        }
        if self.kitty_keyboard {
            let _ = write!(out, "\x1b[<u");
        }
        if let Some(enabled) = self.clipboard_mode {
            let _ = write!(out, "\x1b[?5522{}", if enabled { "h" } else { "l" });
        }
        if let Some(enabled) = self.theme_mode {
            let _ = write!(out, "\x1b[?2031{}", if enabled { "h" } else { "l" });
        }
        let _ = write!(
            out,
            "\x1b[?2026l\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l"
        );
        let _ = out.flush();
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore_output(&mut std::io::stdout());
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
fn is_scene_protocol_char(c: char) -> bool {
    // The Kitty placeholder is printable Unicode, but the host interprets it
    // as an image reference. Only typed, remapped placements may reference IDs.
    c.is_control() || c == '\u{10eeee}'
}
/// Remove control characters and host-interpreted Kitty placeholder glyphs from UI text.
pub fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|c| !is_scene_protocol_char(*c))
        .collect()
}
fn validate_text(text: &str) -> Result<()> {
    if text.len() > 8192 || text.chars().any(is_scene_protocol_char) {
        bail!("untrusted scene contains invalid text");
    }
    Ok(())
}
/// Read host grid/pixel geometry, using positive fallback cell dimensions when unavailable.
pub fn dimensions() -> Result<Size> {
    let (cols, rows) = crossterm::terminal::size()?;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe {
        libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws);
    }
    Ok(Size {
        cols: cols.max(1),
        rows: rows.max(1),
        cell_width: if ws.ws_xpixel > 0 {
            (ws.ws_xpixel / cols.max(1)).max(1) as u32
        } else {
            8
        },
        cell_height: if ws.ws_ypixel > 0 {
            (ws.ws_ypixel / rows.max(1)).max(1) as u32
        } else {
            16
        },
    })
}
fn color(
    out: &mut impl Write,
    rgb: [u8; 3],
    source: ColorSource,
    background: bool,
    frame: &Frame,
    host: &ColorProbe,
    physical_background: Option<[u8; 3]>,
) -> Result<()> {
    if source == ColorSource::Default
        && (if background {
            physical_background.or(host.background)
        } else {
            host.foreground
        })
        .is_none_or(|physical| physical == rgb)
    {
        write!(out, "\x1b[{}m", if background { 49 } else { 39 })?;
    } else if let ColorSource::Palette(index @ 0..=15) = source {
        write!(
            out,
            "\x1b[{}m",
            (if background { 40 } else { 30 }) + index % 8 + if index >= 8 { 60 } else { 0 }
        )?;
    } else if host.truecolor {
        write!(
            out,
            "\x1b[{};2;{};{};{}m",
            if background { 48 } else { 38 },
            rgb[0],
            rgb[1],
            rgb[2]
        )?;
    } else {
        let index = (0..16)
            .min_by_key(|index| {
                host.palette
                    .get(index)
                    .or_else(|| frame.palette.get(*index))
                    .unwrap_or(&[0, 0, 0])
                    .iter()
                    .zip(rgb)
                    .map(|(a, b)| {
                        let d = i32::from(*a) - i32::from(b);
                        d * d
                    })
                    .sum::<i32>()
            })
            .unwrap_or(7);
        let code =
            (if background { 40 } else { 30 }) + (index % 8) + if index >= 8 { 60 } else { 0 };
        write!(out, "\x1b[{code}m")?;
    }
    Ok(())
}

fn transmit_image(
    out: &mut impl Write,
    id: u32,
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: &[u8],
) -> Result<()> {
    let (format, pixels) = match format {
        PixelFormat::Rgb => (24, Cow::Borrowed(pixels)),
        PixelFormat::Rgba => (32, Cow::Borrowed(pixels)),
        PixelFormat::Gray | PixelFormat::GrayAlpha => {
            let mut encoded = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut encoded, width, height);
                encoder.set_color(if format == PixelFormat::Gray {
                    png::ColorType::Grayscale
                } else {
                    png::ColorType::GrayscaleAlpha
                });
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(pixels)?;
                writer.finish()?;
            }
            (100, Cow::Owned(encoded))
        }
    };
    transmit_image_payload(out, id, width, height, format, false, &pixels)
}

fn transmit_image_payload(
    out: &mut impl Write,
    id: u32,
    width: u32,
    height: u32,
    format: u8,
    zlib: bool,
    pixels: &[u8],
) -> Result<()> {
    // Encode one protocol chunk at a time rather than allocating a second
    // image-sized base64 string alongside canonical/derived raster buffers.
    let mut encoded = [0u8; 4096];
    let mut chunks = pixels.chunks(3072).peekable();
    let mut first = true;
    let compression = if zlib { ",o=z" } else { "" };
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(
                out,
                "\x1b_Ga=t,f={format}{compression},s={width},v={height},i={id},q=1,m={more};"
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        let count = BASE64
            .encode_slice(chunk, &mut encoded)
            .context("encoding image protocol chunk")?;
        out.write_all(&encoded[..count])?;
        write!(out, "\x1b\\")?;
    }
    Ok(())
}

struct PhysicalTarget<'a> {
    graphics: &'a mut GraphicsState,
    background: Option<[u8; 3]>,
}

#[cfg(test)]
fn draw(
    out: &mut impl Write,
    frame: &Frame,
    viewport: Size,
    chrome: &str,
    host: &ColorProbe,
    graphics: &mut GraphicsState,
) -> Result<()> {
    let prepared = geometry::prepare(frame, viewport)?;
    draw_prepared(
        out,
        frame,
        viewport,
        chrome,
        host,
        PhysicalTarget {
            graphics,
            background: None,
        },
        prepared,
    )
}

// Match the physical writer: continuation cells do not advance the cursor and
// a glyph crossing the viewport edge is not emitted.
fn row_glyphs(row: Option<&Row>, cols: u16) -> impl Iterator<Item = (u16, u16, &Cell)> {
    row.into_iter()
        .flat_map(|row| &row.cells)
        .scan(0u16, move |x, cell| {
            let end = x.saturating_add(u16::from(cell.width));
            if *x >= cols || end > cols {
                return None;
            }
            let start = *x;
            *x = end;
            Some((cell.width != 0).then_some((start, end, cell)))
        })
        .flatten()
}

fn row_damage(row: Option<&Row>, previous: Option<&Row>, cols: u16) -> Option<(u16, u16)> {
    let mut current = row_glyphs(row, cols).peekable();
    let mut old = row_glyphs(previous, cols).peekable();
    let (mut start, mut end) = (cols, 0);
    loop {
        let a = current.peek().copied();
        let b = old.peek().copied();
        if a.is_none() && b.is_none() {
            break;
        }
        if a != b {
            for (left, right, _) in a.into_iter().chain(b) {
                start = start.min(left);
                end = end.max(right);
            }
        }
        // Walk both rows in physical-column order. Comparing complete glyph
        // extents expands damage across either side of a wide/narrow change.
        if a.is_some_and(|(_, right, _)| b.is_none_or(|(_, old_right, _)| right <= old_right)) {
            current.next();
        }
        if b.is_some_and(|(_, right, _)| a.is_none_or(|(_, new_right, _)| right <= new_right)) {
            old.next();
        }
    }
    (start < end).then_some((start, end))
}

fn draw_prepared(
    out: &mut impl Write,
    frame: &Frame,
    viewport: Size,
    chrome: &str,
    host: &ColorProbe,
    target: PhysicalTarget<'_>,
    prepared: geometry::Prepared<'_>,
) -> Result<()> {
    let PhysicalTarget {
        graphics,
        background: physical_background,
    } = target;
    graphics.purge_expired_pngs();
    // Scene strings are never interpreted as terminal programs. Only renderer-owned
    // escapes and validated normalized colors/positions reach the physical terminal.
    if frame.rows.len() > frame.size.rows as usize
        || frame.images.len() > 1024
        || frame.placements.len() > 4096
    {
        bail!("scene dimension/resource limits");
    }
    for row in &frame.rows {
        if row.cells.len() > frame.size.cols as usize {
            bail!("scene row limit");
        }
        for cell in &row.cells {
            validate_text(&cell.text)?;
            if cell.width > 2 {
                bail!("invalid cell width");
            }
            if let Some(link) = &cell.hyperlink {
                validate_text(link)?;
            }
        }
    }
    let colors = ColorState {
        foreground: frame.foreground,
        background: frame.background,
        host_revision: host.revision,
        truecolor: host.truecolor,
        physical_background,
    };
    let full = graphics.text.viewport != Some(viewport)
        || graphics.text.colors != Some(colors)
        || graphics.text.palette != frame.palette;
    let text_changed =
        full || (0..viewport.rows as usize).any(|y| frame.rows.get(y) != graphics.text.rows.get(y));
    let chrome_changed = full || graphics.text.chrome != chrome;
    let placements_changed = graphics.projections != prepared.placements;
    let resources_changed = graphics
        .image_ids
        .keys()
        .any(|key| !prepared.images.contains_key(key))
        || graphics
            .derived_ids
            .keys()
            .any(|key| !prepared.images.contains_key(&key.image))
        || prepared
            .original
            .iter()
            .any(|key| !graphics.image_ids.contains_key(key))
        || prepared
            .derived
            .iter()
            .any(|key| !graphics.derived_ids.contains_key(key));
    if !text_changed
        && !chrome_changed
        && !placements_changed
        && !resources_changed
        && graphics.text.cursor.as_ref() == Some(&frame.cursor)
    {
        out.flush()?;
        return Ok(());
    }
    // Placement/source retirement belongs to the same physical update as its
    // replacement. Deleting before this boundary exposes an empty-image frame
    // when the write spans flushes, even if every raster remains GPU-resident.
    write!(out, "\x1b[?2026h\x1b[?25l\x1b[0m")?;
    for (_, id) in graphics
        .image_ids
        .extract_if(|key, _| !prepared.images.contains_key(key))
    {
        write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
    }
    if placements_changed || resources_changed {
        graphics.prepare_projection_cache(&prepared, out)?;
    }
    if placements_changed {
        for id in graphics.image_ids.values() {
            write!(out, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\")?;
        }
        for resource in graphics
            .derived_ids
            .values_mut()
            .filter(|resource| resource.placed)
        {
            write!(out, "\x1b_Ga=d,d=i,i={},q=2\x1b\\", resource.id)?;
            resource.placed = false;
        }
    }
    let visible_rows = frame.rows.len().min(viewport.rows as usize);
    let mut previous_style: Option<&Style> = None;
    for y in 0..viewport.rows as usize {
        let row = frame.rows.get(y);
        if !full && row == graphics.text.rows.get(y) {
            continue;
        }
        let damage = if full {
            Some((0, viewport.cols))
        } else {
            row_damage(row, graphics.text.rows.get(y), viewport.cols)
        };
        if let Some((start, end)) = damage {
            write!(out, "\x1b[{};{}H", y + 1, start + 1)?;
            let mut x = start;
            for (left, right, cell) in row_glyphs(row, viewport.cols) {
                if left < start {
                    continue;
                }
                if left >= end {
                    break;
                }
                let s = &cell.style;
                if previous_style != Some(s) {
                    style::transition(out, previous_style, s, frame, host, physical_background)?;
                    previous_style = Some(s);
                }
                if let Some(link) = &cell.hyperlink {
                    write!(out, "\x1b]8;;{link}\x1b\\")?;
                }
                write!(
                    out,
                    "{}",
                    if cell.text.is_empty() {
                        " "
                    } else {
                        &cell.text
                    }
                )?;
                if cell.hyperlink.is_some() {
                    write!(out, "\x1b]8;;\x1b\\")?;
                }
                x = right;
            }
            if x < end {
                write!(out, "\x1b[0m")?;
                if end == viewport.cols {
                    write!(out, "\x1b[K")?;
                } else {
                    write!(out, "\x1b[{}X", end - x)?;
                }
                previous_style = None;
            }
        }
        if let Some(row) = row {
            if let Some(previous) = graphics.text.rows.get_mut(y) {
                previous.clone_from(row);
            } else {
                graphics.text.rows.push(row.clone());
            }
        }
    }
    for (key, image) in &prepared.images {
        if !prepared.original.contains(key) || graphics.image_ids.contains_key(key) {
            continue;
        }
        let id = graphics.allocate()?;
        graphics.image_ids.insert(*key, id);
        if let Some(transport) = graphics.prepared_transport(image) {
            let (format, zlib) = match transport.encoding() {
                TransportEncoding::Png => (100, false),
                TransportEncoding::Zlib if image.format == PixelFormat::Rgb => (24, true),
                TransportEncoding::Zlib => (32, true),
            };
            transmit_image_payload(
                out,
                id,
                image.width,
                image.height,
                format,
                zlib,
                transport.bytes(),
            )?;
        } else {
            transmit_image(
                out,
                id,
                image.width,
                image.height,
                image.format,
                &image.pixels,
            )?;
        }
    }
    for p in prepared.placements.iter().filter(|_| placements_changed) {
        let id = if let Some(key) = p.derived {
            if let Some(resource) = graphics.derived_ids.get_mut(&key) {
                resource.placed = true;
                resource.id
            } else {
                let image = prepared.images[&key.image];
                let pixels = geometry::raster(image, key)?;
                let id = graphics.allocate()?;
                let rgba_bytes = usize::try_from(key.visible.width)?
                    .checked_mul(usize::try_from(key.visible.height)?)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .context("projected image cache byte capacity exceeded")?;
                graphics.derived_bytes += rgba_bytes;
                graphics.derived_ids.insert(
                    key,
                    ProjectionResource {
                        id,
                        rgba_bytes,
                        placed: true,
                    },
                );
                transmit_image(
                    out,
                    id,
                    key.visible.width,
                    key.visible.height,
                    image.format,
                    &pixels,
                )?;
                id
            }
        } else {
            graphics.image_ids[&p.image]
        };
        write!(
            out,
            "\x1b[{};{}H\x1b_Ga=p,i={id},p={},q=1,x={},y={},w={},h={},X={},Y={}",
            p.row + 1,
            p.col + 1,
            p.placement_id,
            p.source.x,
            p.source.y,
            p.source.width,
            p.source.height,
            p.offset_x,
            p.offset_y
        )?;
        if let Some((cols, rows)) = p.cells {
            write!(out, ",c={cols},r={rows}")?;
        }
        write!(out, ",z={},C=1\x1b\\", p.z)?;
    }
    if chrome_changed && !chrome.is_empty() {
        write!(
            out,
            "\x1b[{};1H\x1b[0;7m{}\x1b[K\x1b[0m",
            viewport.rows + 1,
            safe_text(chrome)
                .chars()
                .take(viewport.cols as usize)
                .collect::<String>()
        )?;
        graphics.text.chrome.clear();
        graphics.text.chrome.push_str(chrome);
    }
    graphics.text.rows.truncate(visible_rows);
    graphics.text.viewport = Some(viewport);
    graphics.text.colors = Some(colors);
    graphics.text.palette.clone_from(&frame.palette);
    graphics.projections = prepared.placements;
    if frame.cursor.x < viewport.cols && frame.cursor.y < viewport.rows {
        if frame.cursor.visible {
            let shape = match frame.cursor.shape {
                CursorShape::Block | CursorShape::Hollow => 2,
                CursorShape::Underline => 4,
                CursorShape::Bar => 6,
            } - u8::from(frame.cursor.blinking);
            write!(out, "\x1b[{shape} q")?;
        }
        // Text, graphics placements and passive chrome all move the physical
        // cursor. Restore the focused anchor even when DECTCEM remains hidden.
        write!(out, "\x1b[{};{}H", frame.cursor.y + 1, frame.cursor.x + 1)?;
        if frame.cursor.visible {
            write!(out, "\x1b[?25h")?;
            if graphics.text.physical_cursor_color != Some(frame.cursor.color) {
                if let Some([r, g, b]) = frame.cursor.color {
                    write!(out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x1b\\")?;
                } else {
                    write!(out, "\x1b]112\x1b\\")?;
                }
                graphics.text.physical_cursor_color = Some(frame.cursor.color);
            }
        }
    }
    graphics.text.cursor = Some(frame.cursor.clone());
    write!(out, "\x1b[?2026l")?;
    out.flush()?;
    Ok(())
}

/// Demultiplex terminal-owned OSC/DCS/APC responses before the maintained key
/// parser. A clipboard payload can never become a keystroke, including after
/// timeout. This is input framing, not a second application terminal parser.
pub enum InputPart {
    /// Bytes for the maintained keyboard/paste parser.
    Keys(Vec<u8>),
    /// Device-reply payload, never keyboard input.
    Reply(Vec<u8>),
    /// Complete SGR packet with only its otherwise-lossy button identity saved.
    Mouse {
        /// Complete original SGR mouse packet.
        bytes: Vec<u8>,
        /// Physical button identity preserved beyond the maintained parser.
        button: MouseButton,
        /// Pointer transition or wheel action.
        action: MouseAction,
    },
}
/// Decoded user input or separately routed host-device response.
pub enum ClientEvent {
    /// Maintained-parser key, paste, focus or legacy pointer event.
    Legacy(InputEvent),
    /// Host-device response payload, not application input.
    Reply(Vec<u8>),
    /// Maintained-parser coordinates/modifiers plus physical SGR identity.
    Mouse {
        /// Parsed zero-based host cell position and modifiers.
        event: termwiz::input::MouseEvent,
        /// Physical button identity from the original SGR packet.
        button: MouseButton,
        /// Pointer transition from the original SGR packet.
        action: MouseAction,
    },
}
fn emit_reply(parts: &mut Vec<InputPart>, keys: &mut Vec<u8>, reply: Vec<u8>) {
    if !keys.is_empty() {
        parts.push(InputPart::Keys(std::mem::take(keys)));
    }
    parts.push(InputPart::Reply(reply));
}
fn client_input(event: InputEvent) -> Option<ClientEvent> {
    let event = match event {
        InputEvent::Key(mut key) if key.key == KeyCode::Char('\0') => {
            // Legacy terminals encode Ctrl-Space/Ctrl-@ as NUL. Normalize only
            // this legacy event; CSI-u functional-key placeholders stay distinct.
            key.key = KeyCode::Char(' ');
            key.modifiers.insert(Modifiers::CTRL);
            InputEvent::Key(key)
        }
        InputEvent::Mouse(mut mouse) => {
            // Termwiz preserves SGR cell coordinates as one-based values.
            // Scene rectangles and hit tests use zero-based cell coordinates.
            mouse.x = mouse.x.checked_sub(1)?;
            mouse.y = mouse.y.checked_sub(1)?;
            InputEvent::Mouse(mouse)
        }
        event => event,
    };
    Some(ClientEvent::Legacy(event))
}
/// Decode framed input without passing device responses to the user-input parser.
/// `more` indicates whether ordinary input may continue in another chunk.
pub fn decode_input(
    parser: &mut InputParser,
    parts: Vec<InputPart>,
    more: bool,
) -> Vec<ClientEvent> {
    let mut events = Vec::new();
    for part in parts {
        match part {
            InputPart::Keys(bytes) => events.extend(
                parser
                    .parse_as_vec(&bytes, more)
                    .into_iter()
                    .filter_map(client_input),
            ),
            InputPart::Mouse {
                bytes,
                button,
                action,
            } => {
                // A framed packet cannot continue a preceding legacy Escape.
                events.extend(
                    parser
                        .parse_as_vec(&[], false)
                        .into_iter()
                        .filter_map(client_input),
                );
                events.extend(
                    parser
                        .parse_as_vec(&bytes, false)
                        .into_iter()
                        .filter_map(|event| {
                            Some(match client_input(event)? {
                                ClientEvent::Legacy(InputEvent::Mouse(event)) => {
                                    ClientEvent::Mouse {
                                        event,
                                        button,
                                        action,
                                    }
                                }
                                event => event,
                            })
                        }),
                );
            }
            InputPart::Reply(reply) => {
                // A complete framed report cannot continue the preceding key.
                // Resolve a pending legacy Escape before its structured release
                // or another report changes the key's input owner.
                events.extend(
                    parser
                        .parse_as_vec(&[], false)
                        .into_iter()
                        .filter_map(client_input),
                );
                events.push(ClientEvent::Reply(reply));
            }
        }
    }
    if !more {
        events.extend(
            parser
                .parse_as_vec(&[], false)
                .into_iter()
                .filter_map(client_input),
        );
    }
    events
}

// Termwiz remains the mouse decoder. SGR's final byte and first parameter carry
// physical identity it discards (all releases become an empty button set, and
// repeated same-button presses/motion cannot be distinguished afterward).
// The maintained input parser supports SGR mouse, not historical X10 packets;
// this framing metadata must not expand its supported protocol grammars.
fn sgr_mouse_identity(final_byte: u8, params: &[u8]) -> Option<(MouseButton, MouseAction)> {
    if !matches!(final_byte, b'M' | b'm') {
        return None;
    }
    let params = params.strip_prefix(b"<")?;
    let end = params.iter().position(|byte| *byte == b';')?;
    let code: u8 = std::str::from_utf8(&params[..end]).ok()?.parse().ok()?;
    let button = match code & 0xc3 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::None,
        64 => MouseButton::WheelUp,
        65 => MouseButton::WheelDown,
        66 => MouseButton::WheelLeft,
        67 => MouseButton::WheelRight,
        128 => MouseButton::Back,
        129 => MouseButton::Forward,
        _ => return None,
    };
    let action = if final_byte == b'm' {
        MouseAction::Release
    } else if code & 32 != 0 {
        MouseAction::Motion
    } else {
        MouseAction::Press
    };
    Some((button, action))
}
/// Incremental host input/reply framing, preserving paste bytes as literal input.
#[derive(Default)]
pub struct ReplyDemux {
    buffer: Vec<u8>,
    in_paste: bool,
    paste_bytes: usize,
}
impl ReplyDemux {
    /// Frame a new byte chunk; incomplete control packets remain pending.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputPart> {
        self.buffer.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut parts = Vec::new();
        let mut cursor = 0;
        while cursor < self.buffer.len() {
            if self.in_paste {
                const END: &[u8] = b"\x1b[201~";
                let tail = &self.buffer[cursor..];
                if tail.starts_with(END) {
                    keys.extend_from_slice(END);
                    cursor += END.len();
                    self.in_paste = false;
                    continue;
                }
                if END.starts_with(tail) {
                    break;
                }
                self.paste_bytes += 1;
                if self.paste_bytes > MAX_INPUT_BYTES {
                    emit_reply(
                        &mut parts,
                        &mut keys,
                        b"input-error;oversized paste".to_vec(),
                    );
                    self.buffer.clear();
                    return parts;
                }
                keys.push(self.buffer[cursor]);
                cursor += 1;
                continue;
            }
            if self.buffer[cursor] == 0x1b {
                if cursor + 1 == self.buffer.len() {
                    break;
                }
                if self.buffer[cursor + 1] == b'[' {
                    if cursor + 2 == self.buffer.len() {
                        break;
                    }
                    if matches!(self.buffer[cursor + 2], b'I' | b'O') {
                        emit_reply(
                            &mut parts,
                            &mut keys,
                            if self.buffer[cursor + 2] == b'I' {
                                b"focus;in".to_vec()
                            } else {
                                b"focus;out".to_vec()
                            },
                        );
                        cursor += 3;
                        continue;
                    }
                    let tail = &self.buffer[cursor + 2..];
                    if let Some(end) = tail.iter().position(|b| (0x40..=0x7e).contains(b)) {
                        if tail[end] == b'~' && &tail[..end] == b"200" {
                            keys.extend_from_slice(b"\x1b[200~");
                            self.in_paste = true;
                            self.paste_bytes = 0;
                            cursor += end + 3;
                            continue;
                        }
                        if let Some((button, action)) = sgr_mouse_identity(tail[end], &tail[..end])
                        {
                            if !keys.is_empty() {
                                parts.push(InputPart::Keys(std::mem::take(&mut keys)));
                            }
                            parts.push(InputPart::Mouse {
                                bytes: self.buffer[cursor..cursor + end + 3].to_vec(),
                                button,
                                action,
                            });
                            cursor += end + 3;
                            continue;
                        }
                        // Theme notifications are private DSR reports: the '?'
                        // is part of the wire grammar, not an optional marker.
                        if tail[end] == b'n' && tail.starts_with(b"?997;") {
                            if matches!(&tail[..end], b"?997;1" | b"?997;2") {
                                let mut reply = b"theme-change;".to_vec();
                                reply.push(tail[end - 1]);
                                emit_reply(&mut parts, &mut keys, reply);
                            }
                            // Unknown private theme modes remain host reports,
                            // never keyboard input or positive refresh evidence.
                            cursor += end + 3;
                            continue;
                        }
                        if tail[end] == b'y'
                            && matches!(
                                &tail[..end],
                                b"?2031;0$" | b"?2031;1$" | b"?2031;2$" | b"?2031;3$" | b"?2031;4$"
                            )
                        {
                            let mut reply = b"theme-cap;".to_vec();
                            reply.extend_from_slice(&tail[..end]);
                            emit_reply(&mut parts, &mut keys, reply);
                            cursor += end + 3;
                            continue;
                        }
                        if tail[end] == b'y' && tail.starts_with(b"?5522;") {
                            let mut reply = b"clipboard-cap;".to_vec();
                            reply.extend_from_slice(&tail[..end]);
                            emit_reply(&mut parts, &mut keys, reply);
                            cursor += end + 3;
                            continue;
                        }
                        if tail[end] == b'u' {
                            let mut reply = if tail.first() == Some(&b'?') {
                                b"keyboard;".to_vec()
                            } else {
                                b"key;".to_vec()
                            };
                            reply.extend_from_slice(&tail[..end]);
                            emit_reply(&mut parts, &mut keys, reply);
                            cursor += end + 3;
                            continue;
                        }
                        if let Some(payload) = kitty_functional(tail[end], &tail[..end]) {
                            let mut reply = b"key;".to_vec();
                            reply.extend_from_slice(payload.as_bytes());
                            emit_reply(&mut parts, &mut keys, reply);
                            cursor += end + 3;
                            continue;
                        }
                    } else {
                        if tail.len() > 256 {
                            emit_reply(
                                &mut parts,
                                &mut keys,
                                b"input-error;oversized CSI".to_vec(),
                            );
                            self.buffer.clear();
                            return parts;
                        }
                        break;
                    }
                }
                if matches!(self.buffer[cursor + 1], b']' | b'P' | b'_' | b'^') {
                    let start = cursor;
                    let mut end = None;
                    let mut i = cursor + 2;
                    while i < self.buffer.len() {
                        if self.buffer[i] == 7 {
                            end = Some((i, i + 1));
                            break;
                        }
                        if self.buffer[i] == 0x1b && self.buffer.get(i + 1) == Some(&b'\\') {
                            end = Some((i, i + 2));
                            break;
                        }
                        i += 1;
                    }
                    if let Some((payload_end, next)) = end {
                        let payload = &self.buffer[start + 2..payload_end];
                        let prefix: &[u8] = if self.buffer[start + 1] == b'_'
                            && next == payload_end + 2
                            && payload.starts_with(b"G")
                        {
                            b"graphics;"
                        } else if self.buffer[start + 1] == b']' && payload.starts_with(b"99;") {
                            crate::notification::REPLY_PREFIX
                        } else if payload.starts_with(b"graphics;")
                            || payload.starts_with(crate::notification::REPLY_PREFIX)
                            || payload.starts_with(b"focus;")
                        {
                            // Internal route tags cannot be forged by a literal
                            // OSC/DCS payload carrying their spelling.
                            b"unrecognized;"
                        } else {
                            b""
                        };
                        let mut reply = Vec::with_capacity(prefix.len() + payload.len());
                        reply.extend_from_slice(prefix);
                        reply.extend_from_slice(payload);
                        emit_reply(&mut parts, &mut keys, reply);
                        cursor = next;
                        continue;
                    }
                    if self.buffer.len() - start > 2 * 1024 * 1024 {
                        let trailing_escape = self.buffer.last() == Some(&0x1b);
                        self.buffer.truncate(start + 2);
                        if trailing_escape {
                            self.buffer.push(0x1b);
                        }
                    }
                    break;
                }
            }
            keys.push(self.buffer[cursor]);
            cursor += 1;
        }
        self.buffer.drain(..cursor);
        if !keys.is_empty() {
            parts.push(InputPart::Keys(keys));
        }
        parts
    }
    /// Release a lone pending Escape; never flush a partial reply or bracketed paste.
    pub fn flush_escape(&mut self) -> Vec<u8> {
        if !self.in_paste && self.buffer == [0x1b] {
            std::mem::take(&mut self.buffer)
        } else {
            Vec::new()
        }
    }
}
/// One pending host clipboard transfer and its caller-owned consent state.
pub struct Consent {
    /// Exact request correlation placed in enhanced clipboard messages.
    pub id: String,
    /// Requested MIME operation, selection and native grant context.
    pub request: ClipboardRequest,
    /// Transfer start time for caller-enforced expiry.
    pub started: Instant,
    /// Whether the host operation has been submitted.
    pub sent: bool,
    /// Whether presentation policy approved the operation.
    pub allowed: bool,
    /// Accumulated exact MIME response contents.
    pub contents: Vec<ClipboardContent>,
    /// Accumulated advertised MIME names.
    pub available: Vec<String>,
    /// Observed enhanced clipboard support; `None` means the startup response is pending.
    pub enhanced_supported: Option<bool>,
    /// Whether this transfer services a native paste offer.
    pub paste: bool,
    /// Original host-issued paste password, if this is an offer-bound read.
    pub password: Option<String>,
}
fn success(contents: Vec<ClipboardContent>) -> ClipboardResponse {
    ClipboardResponse {
        result: ClipboardResult::Success,
        contents,
        available: Vec::new(),
        remember: false,
    }
}
fn selection(location: ClipboardLocation) -> &'static str {
    match location {
        ClipboardLocation::Standard => "c",
        ClipboardLocation::Selection => "s",
        ClipboardLocation::Primary => "p",
    }
}
/// In-progress host paste offer, correlated by its original password.
pub struct PasteOffer {
    /// Exact encoded host-issued password.
    pub password: String,
    /// Validated MIME targets collected before completion.
    pub mimes: Vec<String>,
    /// Whether the offer refers to the primary selection.
    pub primary: bool,
    /// Offer start time for caller-enforced expiry.
    pub started: Instant,
}

/// Routing identity only; payload validation and transfer state remain in
/// `clipboard_packet` / `paste_offer_packet`. Borrowing avoids copying MIME data.
pub enum ClipboardReplyOrigin<'a> {
    /// Explicit transfer request ID.
    Request(&'a str),
    /// Paste-offer password for a read without an explicit request ID.
    Offer(&'a str),
}

/// Read routing correlation only; does not validate payloads, grant consent or complete transfers.
pub fn clipboard_reply_origin(packet: &[u8]) -> Option<ClipboardReplyOrigin<'_>> {
    let packet = packet.strip_prefix(b"5522;")?;
    let metadata = std::str::from_utf8(packet.split(|byte| *byte == b';').next()?).ok()?;
    let mut id = None;
    let mut password = None;
    let mut read = false;
    for (key, value) in metadata
        .split(':')
        .filter_map(|field| field.split_once('='))
    {
        match key {
            "id" if !value.is_empty() => id = Some(value),
            "pw" => password = Some(value),
            "type" => read = value == "read",
            _ => {}
        }
    }
    if let Some(id) = id {
        Some(ClipboardReplyOrigin::Request(id))
    } else if read {
        password.map(ClipboardReplyOrigin::Offer)
    } else {
        None
    }
}

/// Advance a correlated enhanced paste offer and return a pending consent on completion.
/// Rejects malformed encoding, overlapping offers and excessive MIME data.
pub fn paste_offer_packet(
    offer: &mut Option<PasteOffer>,
    packet: &[u8],
) -> Result<Option<Consent>> {
    let Some(packet) = packet.strip_prefix(b"5522;") else {
        return Ok(None);
    };
    let mut parts = packet.splitn(2, |b| *b == b';');
    let meta = std::str::from_utf8(parts.next().unwrap_or_default())?;
    let fields = meta
        .split(':')
        .filter_map(|field| field.split_once('='))
        .collect::<HashMap<_, _>>();
    if fields.get("type").copied() != Some("read")
        || fields.get("id").is_some_and(|id| !id.is_empty())
    {
        return Ok(None);
    }
    let Some(password) = fields.get("pw").copied() else {
        return Ok(None);
    };
    if BASE64.decode(password)?.len() > 128 {
        bail!("paste password size limit");
    }
    match fields.get("status").copied().unwrap_or("") {
        "OK" => {
            if offer.is_some() {
                bail!("overlapping enhanced paste offer");
            }
            *offer = Some(PasteOffer {
                password: password.into(),
                mimes: Vec::new(),
                primary: fields.get("loc").copied() == Some("primary"),
                started: Instant::now(),
            });
        }
        "DATA" => {
            if let Some(offer) = offer.as_mut().filter(|o| o.password == password)
                && fields.get("mime").copied() == Some("Lg==")
            {
                let data = BASE64.decode(parts.next().unwrap_or_default())?;
                if data.len() > 4096 {
                    bail!("paste targets size limit");
                }
                for mime in std::str::from_utf8(&data)?.split_whitespace() {
                    if mime.len() > 256
                        || offer.mimes.len() >= 16
                        || offer.mimes.iter().map(String::len).sum::<usize>() + mime.len() > 4096
                    {
                        bail!("paste MIME aggregate limit");
                    }
                    offer.mimes.push(mime.into());
                }
            }
        }
        "DONE" if offer.as_ref().is_some_and(|o| o.password == password) => {
            let offer = offer.take().unwrap();
            if offer.mimes.is_empty() {
                return Ok(None);
            }
            return Ok(Some(Consent {
                id: uuid::Uuid::new_v4().to_string(),
                request: ClipboardRequest {
                    operation: ClipboardOperation::Read {
                        mimes: offer.mimes,
                        list: false,
                    },
                    location: if offer.primary {
                        ClipboardLocation::Primary
                    } else {
                        ClipboardLocation::Standard
                    },
                    name: "bkpr".into(),
                    granted: false,
                    can_remember: false,
                    paste_offer_id: None,
                },
                started: Instant::now(),
                sent: false,
                allowed: false,
                contents: Vec::new(),
                available: Vec::new(),
                enhanced_supported: Some(true),
                paste: true,
                password: Some(offer.password),
            }));
        }
        _ => {}
    }
    Ok(None)
}
/// Submit an approved host clipboard operation or wait for capability evidence.
/// Returns an immediate response for completed plain-text writes or unsupported operations.
pub fn begin_clipboard(
    out: &mut impl Write,
    consent: &mut Consent,
) -> Result<Option<ClipboardResponse>> {
    let needs_enhanced = !matches!(
        &consent.request.operation,
        ClipboardOperation::Write { contents }
            if contents.len() == 1
                && contents[0].mime == "text/plain"
                && consent.request.name.is_empty()
    );
    if needs_enhanced && consent.enhanced_supported.is_none() {
        return Ok(None);
    }
    consent.sent = true;
    match &consent.request.operation {
        ClipboardOperation::Write { contents } => {
            if contents.len() == 1
                && contents[0].mime == "text/plain"
                && consent.request.name.is_empty()
            {
                write!(
                    out,
                    "\x1b]52;{};{}\x1b\\",
                    selection(consent.request.location),
                    BASE64.encode(&contents[0].data)
                )?;
                out.flush()?;
                return Ok(Some(success(Vec::new())));
            }
            if consent.enhanced_supported != Some(true) {
                return Ok(Some(ClipboardResponse {
                    result: ClipboardResult::Unsupported,
                    ..ClipboardResponse::denied()
                }));
            }
            write!(
                out,
                "\x1b]5522;type=write:id={}{}:name={}\x1b\\",
                consent.id,
                if consent.request.location == ClipboardLocation::Primary {
                    ":loc=primary"
                } else {
                    ""
                },
                BASE64.encode(&consent.request.name)
            )?;
            for content in contents {
                for chunk in content.data.chunks(4096) {
                    write!(
                        out,
                        "\x1b]5522;type=wdata:mime={};{}\x1b\\",
                        BASE64.encode(&content.mime),
                        BASE64.encode(chunk)
                    )?;
                }
            }
            write!(out, "\x1b]5522;type=wdata\x1b\\")?;
            out.flush()?;
            Ok(None)
        }
        ClipboardOperation::Read { mimes, list } => {
            if consent.enhanced_supported != Some(true) {
                return Ok(Some(ClipboardResponse {
                    result: ClipboardResult::Unsupported,
                    ..ClipboardResponse::denied()
                }));
            }
            let mut requested = mimes.clone();
            if *list {
                requested.push(".".into());
            }
            write!(
                out,
                "\x1b]5522;type=read:id={}{}:name={}{};{}\x1b\\",
                consent.id,
                if consent.request.location == ClipboardLocation::Primary {
                    ":loc=primary"
                } else {
                    ""
                },
                BASE64.encode(&consent.request.name),
                consent
                    .password
                    .as_ref()
                    .map(|pw| format!(":pw={pw}"))
                    .unwrap_or_default(),
                BASE64.encode(requested.join(" "))
            )?;
            out.flush()?;
            Ok(None)
        }
    }
}
/// Accumulate a matching enhanced clipboard response; unrelated packets leave state unchanged.
/// Rejects invalid UTF-8/base64 and excessive response data or MIME counts.
pub fn clipboard_packet(consent: &mut Consent, packet: &[u8]) -> Result<Option<ClipboardResponse>> {
    let Some(packet) = packet.strip_prefix(b"5522;") else {
        return Ok(None);
    };
    let mut parts = packet.splitn(2, |b| *b == b';');
    let metadata = std::str::from_utf8(parts.next().unwrap_or_default())?;
    let fields = metadata
        .split(':')
        .filter_map(|field| field.split_once('='))
        .collect::<HashMap<_, _>>();
    if fields.get("type").copied()
        != Some(
            if matches!(&consent.request.operation, ClipboardOperation::Read { .. }) {
                "read"
            } else {
                "write"
            },
        )
    {
        return Ok(None);
    }
    if fields.get("id").copied() != Some(consent.id.as_str()) {
        return Ok(None);
    }
    match fields.get("status").copied().unwrap_or("") {
        "OK" => Ok(None),
        "DATA" => {
            let mime =
                String::from_utf8(BASE64.decode(fields.get("mime").copied().unwrap_or(""))?)?;
            let data = BASE64.decode(parts.next().unwrap_or_default())?;
            if mime.len() > 256
                || data.len() > 4096
                || consent.contents.iter().map(|c| c.data.len()).sum::<usize>()
                    + consent.available.iter().map(String::len).sum::<usize>()
                    + data.len()
                    > 1024 * 1024
            {
                bail!("clipboard aggregate response limit");
            }
            if mime == "." {
                for mime in std::str::from_utf8(&data)?.split_whitespace() {
                    if mime.len() > 256 || consent.available.len() >= 16 {
                        bail!("clipboard MIME listing count limit");
                    }
                    consent.available.push(mime.into());
                }
            } else if let Some(content) = consent.contents.iter_mut().find(|c| c.mime == mime) {
                content.data.extend(data);
            } else {
                if consent.contents.len() >= 16 {
                    bail!("clipboard MIME count limit");
                }
                consent.contents.push(ClipboardContent { mime, data });
            }
            Ok(None)
        }
        "DONE" => Ok(Some(ClipboardResponse {
            result: ClipboardResult::Success,
            contents: std::mem::take(&mut consent.contents),
            available: std::mem::take(&mut consent.available),
            remember: false,
        })),
        _ => Ok(Some(ClipboardResponse::denied())),
    }
}
/// Map a maintained-parser key event into canonical key/modifier/text fields.
pub fn key_event(key: termwiz::input::KeyEvent) -> KeyEvent {
    let mut modifiers = 0;
    for (flag, bit) in [
        (Modifiers::SHIFT, 1),
        (Modifiers::CTRL, 2),
        (Modifiers::ALT, 4),
        (Modifiers::SUPER, 8),
    ] {
        if key.modifiers.contains(flag) {
            modifiers |= bit;
        }
    }
    // Termwiz normalizes alphabetic C0 keys but leaves these aliases as raw
    // characters. The native key encoder requires printable key semantics;
    // raw control text is deliberately not forwarded as keyboard UTF-8.
    let key = match key.key {
        KeyCode::Char('\0') => {
            modifiers |= 2;
            KeyCode::Char(' ')
        }
        KeyCode::Char(c @ '\u{1c}'..='\u{1f}') => {
            modifiers |= 2;
            KeyCode::Char(char::from(c as u8 + b'@'))
        }
        key => key,
    };
    let (name, text, codepoint) = match key {
        KeyCode::Char(c) => {
            let name = if c.is_ascii_alphabetic() {
                format!("Key{}", c.to_ascii_uppercase())
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                match c {
                    ' ' => "Space",
                    '-' => "Minus",
                    '=' => "Equal",
                    '[' => "BracketLeft",
                    ']' => "BracketRight",
                    '\\' => "Backslash",
                    ';' => "Semicolon",
                    '\'' => "Quote",
                    ',' => "Comma",
                    '.' => "Period",
                    '/' => "Slash",
                    '`' => "Backquote",
                    _ => "Unidentified",
                }
                .into()
            };
            (name, c.to_string(), c.to_ascii_lowercase() as u32)
        }
        KeyCode::Function(n) => (format!("F{n}"), String::new(), 0),
        code => (
            match code {
                KeyCode::Enter => "Enter",
                KeyCode::Tab => "Tab",
                KeyCode::Escape => "Escape",
                KeyCode::Backspace => "Backspace",
                KeyCode::Delete => "Delete",
                KeyCode::Insert => "Insert",
                KeyCode::Home => "Home",
                KeyCode::End => "End",
                KeyCode::PageUp => "PageUp",
                KeyCode::PageDown => "PageDown",
                KeyCode::UpArrow => "ArrowUp",
                KeyCode::DownArrow => "ArrowDown",
                KeyCode::LeftArrow => "ArrowLeft",
                KeyCode::RightArrow => "ArrowRight",
                _ => "Unidentified",
            }
            .into(),
            String::new(),
            0,
        ),
    };
    KeyEvent {
        key: name,
        modifiers,
        text,
        action: KeyAction::Press,
        unshifted_codepoint: codepoint,
    }
}
// Termwiz owns legacy key/mouse/paste decoding. Its KeyEvent lacks release
// state, so this bounded Kitty CSI-u extension retains that native information.
fn kitty_functional(final_byte: u8, body: &[u8]) -> Option<String> {
    let body = std::str::from_utf8(body).ok()?;
    if !body
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b';' | b':'))
    {
        return None;
    }
    let mut fields = body.splitn(2, ';');
    let first = fields.next().unwrap_or("");
    let modifiers = fields.next().unwrap_or("1");
    let code = match final_byte {
        b'A' => 57352,
        b'B' => 57353,
        b'C' => 57351,
        b'D' => 57350,
        b'H' => 57356,
        b'F' => 57357,
        b'P' => 57364,
        b'Q' => 57365,
        b'R' => 57366,
        b'S' => 57367,
        b'~' => match first {
            "2" => 57348,
            "3" => 57349,
            "5" => 57354,
            "6" => 57355,
            "1" | "7" => 57356,
            "4" | "8" => 57357,
            "11" => 57364,
            "12" => 57365,
            "13" => 57366,
            "14" => 57367,
            "15" => 57368,
            "17" => 57369,
            "18" => 57370,
            "19" => 57371,
            "20" => 57372,
            "21" => 57373,
            "23" => 57374,
            "24" => 57375,
            _ => return None,
        },
        _ => return None,
    };
    if final_byte != b'~' && !matches!(first, "" | "1") {
        return None;
    }
    Some(format!("{code};{modifiers}"))
}
/// Decode one Kitty keyboard payload, rejecting malformed or unsupported key/modifier fields.
pub fn kitty_key(payload: &[u8]) -> Result<KeyEvent> {
    if payload.len() > 256 {
        bail!("oversized Kitty key event");
    }
    let value = std::str::from_utf8(payload)?;
    let mut fields = value.split(';');
    let mut codes = fields.next().context("missing Kitty key")?.split(':');
    let code = codes.next().unwrap_or("").parse::<u32>()?;
    let shifted = codes
        .next()
        .filter(|s| !s.is_empty())
        .map(str::parse::<u32>)
        .transpose()?;
    let _base = codes
        .next()
        .filter(|s| !s.is_empty())
        .map(str::parse::<u32>)
        .transpose()?;
    if codes.next().is_some() {
        bail!("malformed Kitty key codes");
    }
    let mut mods = fields
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("1")
        .split(':');
    let raw_mods = mods
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("1")
        .parse::<u16>()?
        .checked_sub(1)
        .context("invalid Kitty modifiers")?;
    if raw_mods & 0x30 != 0 || raw_mods > 255 {
        bail!("unsupported Kitty hyper/meta modifier");
    }
    let mut modifiers = 0;
    for (source, target) in [(1, 1), (2, 4), (4, 2), (8, 8), (64, 16), (128, 32)] {
        if raw_mods & source != 0 {
            modifiers |= target;
        }
    }
    let action = match mods.next().filter(|s| !s.is_empty()).unwrap_or("1") {
        "1" => KeyAction::Press,
        "2" => KeyAction::Repeat,
        "3" => KeyAction::Release,
        _ => bail!("invalid Kitty event type"),
    };
    if mods.next().is_some() {
        bail!("malformed Kitty modifiers");
    }
    let text = fields.next();
    if fields.next().is_some() {
        bail!("malformed Kitty key event");
    }
    let physical = match code {
        57358 => Some("CapsLock"),
        57359 => Some("ScrollLock"),
        57360 => Some("NumLock"),
        57361 => Some("PrintScreen"),
        57362 => Some("Pause"),
        57363 => Some("ContextMenu"),
        57409 => Some("NumpadDecimal"),
        57410 => Some("NumpadDivide"),
        57411 => Some("NumpadMultiply"),
        57412 => Some("NumpadSubtract"),
        57413 => Some("NumpadAdd"),
        57414 => Some("NumpadEnter"),
        57415 => Some("NumpadEqual"),
        57416 => Some("NumpadComma"),
        57441 => Some("ShiftLeft"),
        57442 => Some("ControlLeft"),
        57443 => Some("AltLeft"),
        57444 => Some("MetaLeft"),
        57447 => Some("ShiftRight"),
        57448 => Some("ControlRight"),
        57449 | 57453 => Some("AltRight"),
        57450 => Some("MetaRight"),
        _ => None,
    };
    let key = match code {
        27 | 57344 => KeyCode::Escape,
        13 | 57345 => KeyCode::Enter,
        9 | 57346 => KeyCode::Tab,
        127 | 57347 => KeyCode::Backspace,
        57348 => KeyCode::Insert,
        57349 => KeyCode::Delete,
        57350 => KeyCode::LeftArrow,
        57351 => KeyCode::RightArrow,
        57352 => KeyCode::UpArrow,
        57353 => KeyCode::DownArrow,
        57354 => KeyCode::PageUp,
        57355 => KeyCode::PageDown,
        57356 => KeyCode::Home,
        57357 => KeyCode::End,
        57364..=57398 => KeyCode::Function((code - 57364 + 1) as u8),
        57399..=57408 => KeyCode::Char('\0'),
        57344..=63743 if physical.is_some() => KeyCode::Char('\0'),
        57344..=63743 => bail!("unsupported Kitty functional key {code}"),
        _ => KeyCode::Char(char::from_u32(code).context("invalid Kitty Unicode key")?),
    };
    let mut event = key_event(termwiz::input::KeyEvent {
        key,
        modifiers: Modifiers::NONE,
    });
    if let Some(physical) = physical {
        event.key = physical.into();
    } else if (57399..=57408).contains(&code) {
        event.key = format!("Numpad{}", code - 57399);
    }
    event.modifiers = modifiers;
    event.action = action;
    // The alternate PC-101 key is not the active layout's unshifted key.
    event.unshifted_codepoint = if !(57344..=63743).contains(&code) {
        code
    } else {
        0
    };
    event.text.clear();
    if let Some(text) = text {
        for part in text.split(':').filter(|part| !part.is_empty()) {
            let c = char::from_u32(part.parse()?).context("invalid Kitty text scalar")?;
            if c.is_control() {
                bail!("invalid Kitty text control");
            }
            event.text.push(c);
        }
    } else if action != KeyAction::Release
        && matches!(key, KeyCode::Char(_))
        && code > 0
        && !(57344..=63743).contains(&code)
    {
        event.text.push(
            char::from_u32(if modifiers & 1 != 0 {
                shifted.unwrap_or(code)
            } else {
                code
            })
            .context("invalid Kitty text")?,
        );
    }
    Ok(event)
}
/// Observed per-client colors and independent positive terminal-capability evidence.
#[derive(Default)]
pub struct ColorProbe {
    /// Positive XTGETTCAP truecolor evidence, not inferred from palette replies.
    pub truecolor: bool,
    /// Observed terminal-default foreground RGB.
    pub foreground: Option<[u8; 3]>,
    /// Observed logical terminal-default background RGB.
    pub background: Option<[u8; 3]>,
    /// Observed default cursor RGB.
    pub cursor: Option<[u8; 3]>,
    /// Observed indexed palette entries.
    pub palette: HashMap<usize, [u8; 3]>,
    /// Change token for accepted color/capability observations.
    pub revision: u64,
    /// Independently correlated Kitty graphics query state.
    pub graphics: GraphicsProbe,
}

/// Explicit Kitty query evidence, independent of TERM and color capabilities.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct GraphicsProbe {
    pending: Option<(u32, Instant)>,
    issued_id: Option<u32>,
    supported: bool,
}
// Shared interpretation of the existing ReplyDemux APC route. A query and an
// upload ACK have the same wire shape; ownership, not a guessed action, decides
// dispatch. Keep placement headers distinguishable from plain image replies.
pub(crate) fn parse_graphics_reply(bytes: &[u8]) -> Option<(u32, &str, bool)> {
    let text = std::str::from_utf8(bytes.strip_prefix(b"graphics;")?).ok()?;
    let (header, status) = text.split_once(';').unwrap_or((text, ""));
    let value = header.strip_prefix("Gi=")?;
    let image_id = value.split(',').next()?;
    if image_id.is_empty()
        || image_id.starts_with('0')
        || !image_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((image_id.parse().ok()?, status, value == image_id))
}
impl GraphicsProbe {
    /// Send these bytes through the existing terminal-output owner, then feed
    /// ReplyDemux's APC payloads through ColorProbe::feed.
    pub fn begin(&mut self) -> Vec<u8> {
        static NEXT_ID: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0x7000_0000);
        let Ok(id) = NEXT_ID.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| (id < 0x8000_0000).then(|| id + 1),
        ) else {
            self.pending = None;
            self.issued_id = None;
            self.supported = false;
            return Vec::new();
        };
        self.pending = Some((id, Instant::now()));
        self.issued_id = Some(id);
        self.supported = false;
        format!("\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\").into_bytes()
    }
    /// Whether the current query completed with positive graphics evidence.
    pub fn supported(&self) -> bool {
        self.supported
    }
    /// Whether a query has completed, negatively or positively, including expiry.
    pub fn resolved(&self) -> bool {
        self.issued_id.is_some() && self.pending.is_none()
    }
    /// Deadline for unresolved capability evidence, absent once resolved.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .map(|(_, started)| started + std::time::Duration::from_secs(2))
    }
    /// Expire an unanswered query without allowing a late reply to grant support.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.next_deadline().is_some_and(|deadline| now >= deadline) {
            self.pending = None;
            true
        } else {
            false
        }
    }
    /// Consume only this probe's tagged query response. False means the caller
    /// must preserve its ordinary graphics acknowledgement/error dispatch.
    /// Late, duplicate and invalid responses for our query id are consumed but
    /// cannot turn an expired/completed query into positive evidence.
    pub fn consume_tagged(&mut self, bytes: &[u8]) -> bool {
        let Some((image_id, status, plain)) = parse_graphics_reply(bytes) else {
            return false;
        };
        if Some(image_id) != self.issued_id {
            return false;
        }
        let Some((_, started)) = self.pending else {
            return true;
        };
        if started.elapsed() >= std::time::Duration::from_secs(2) {
            self.pending = None;
            return true;
        }
        // Ownership is not capability evidence. Reject extra query fields,
        // malformed statuses and placement acknowledgements for our id.
        if !plain || status.len() > 256 {
            return true;
        }
        if status == "OK" {
            self.pending = None;
            self.supported = true;
        } else if status.starts_with('E') && status.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            self.pending = None;
        }
        true
    }
}
fn parse_rgb(value: &str) -> Option<[u8; 3]> {
    if let Some(hex) = value.strip_prefix('#') {
        if !matches!(hex.len(), 3 | 6 | 9 | 12) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        let width = hex.len() / 3;
        let maximum = (1u32 << (4 * width)) - 1;
        let mut result = [0; 3];
        for (index, channel) in result.iter_mut().enumerate() {
            let value = u32::from_str_radix(&hex[index * width..(index + 1) * width], 16).ok()?;
            *channel = ((value * 255) / maximum) as u8;
        }
        return Some(result);
    }
    let mut values = value.strip_prefix("rgb:")?.split('/');
    let mut result = [0; 3];
    for component in &mut result {
        let s = values.next()?;
        if s.is_empty() || s.len() > 4 || !s.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(s, 16).ok()?;
        *component = ((value * 255) / ((1u32 << (4 * s.len())) - 1)) as u8;
    }
    if values.next().is_some() {
        return None;
    }
    Some(result)
}
impl ColorProbe {
    /// Shared startup/notification OSC queries. Never query theme notifications
    /// here: receiving a theme notification must not cause another notification.
    pub fn query() -> Vec<u8> {
        let mut query = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\".to_vec();
        for index in 0..256 {
            write!(&mut query, "\x1b]4;{index};?\x1b\\").expect("writing to a Vec");
        }
        query
    }
    /// Terminal notifications invalidate observations, not independent positive
    /// capability evidence. This function never interprets light/dark as colors.
    pub fn theme_changed(&mut self, reply: &[u8]) -> bool {
        if !matches!(reply, b"theme-change;1" | b"theme-change;2") {
            return false;
        }
        let changed = self.foreground.take().is_some()
            | self.background.take().is_some()
            | self.cursor.take().is_some()
            | !self.palette.is_empty();
        self.palette.clear();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        true
    }
    /// Accept demultiplexed device evidence; malformed or unrelated replies are ignored.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.graphics.consume_tagged(bytes) {
            return;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        // Match complete supported XTGETTCAP values, including numeric RGB
        // variants returned by xterm/Kitty. Palette replies imply no capability.
        if text.strip_prefix("1+r").is_some_and(|entries| {
            let supported = |entry: &str| {
                matches!(
                    entry,
                    "5463" | "5463=31" | "524742" | "524742=31" | "524742=38" | "524742=3234"
                )
            };
            entries
                .split(';')
                .all(|entry| supported(entry) || matches!(entry, "5463=30" | "524742=30"))
                && entries.split(';').any(supported)
        }) && !self.truecolor
        {
            self.truecolor = true;
            self.revision = self.revision.wrapping_add(1);
        }
        let mut parts = text.split(';');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(code @ ("10" | "11" | "12")), Some(value), None, None) => {
                if let Some(rgb) = parse_rgb(value) {
                    let target = match code {
                        "10" => &mut self.foreground,
                        "11" => &mut self.background,
                        _ => &mut self.cursor,
                    };
                    if *target != Some(rgb) {
                        *target = Some(rgb);
                        self.revision = self.revision.wrapping_add(1);
                    }
                }
            }
            (Some("4"), Some(index), Some(value), None) => {
                if !index.is_empty()
                    && index.bytes().all(|byte| byte.is_ascii_digit())
                    && let (Ok(index), Some(rgb)) = (index.parse::<usize>(), parse_rgb(value))
                    && index < 256
                    && self.palette.get(&index) != Some(&rgb)
                {
                    self.palette.insert(index, rgb);
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            _ => {}
        }
    }
}
/// Map an in-bounds zero-based host cell into canonical pane pixels.
pub fn mouse_position(col: u16, row: u16, viewport: Size, canonical: Size) -> Option<(f32, f32)> {
    (col < viewport.cols && row < viewport.rows && col < canonical.cols && row < canonical.rows)
        .then_some((
            col as f32 * canonical.cell_width as f32,
            row as f32 * canonical.cell_height as f32,
        ))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn split(parts: Vec<InputPart>) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut keys = Vec::new();
        let mut replies = Vec::new();
        for part in parts {
            match part {
                InputPart::Keys(bytes) => keys.extend(bytes),
                InputPart::Reply(reply) => replies.push(reply),
                InputPart::Mouse { bytes, .. } => keys.extend(bytes),
            }
        }
        (keys, replies)
    }
    #[test]
    fn fragmented_clipboard_replies_never_reach_key_parser() {
        let mut p = ReplyDemux::default();
        assert_eq!(split(p.feed(b"a\x1b]52;c;")).0, b"a");
        let (keys, replies) = split(p.feed(b"c2VjcmV0\x1b\\b"));
        assert_eq!(keys, b"b");
        assert_eq!(replies, vec![b"52;c;c2VjcmV0".to_vec()]);
        assert_eq!(split(p.feed(b"\x1b_Gi=1;OK\x1b\\")).0, b"");
    }
    #[test]
    fn scene_text_cannot_inject_terminal_programs() {
        assert!(validate_text("x\x1b]52;c;YQ==\x07").is_err());
        assert!(validate_text("\u{10eeee}\u{305}\u{305}").is_err());
        assert_eq!(safe_text("left\u{10eeee}\x1b]right"), "left]right");
        assert!(validate_text("ordinary PUA \u{e000} \u{10eeef}").is_ok());
        assert!(validate_text("normal 👋").is_ok());
    }
    #[test]
    fn independent_positive_color_evidence_survives_negative_query() {
        let mut p = ColorProbe::default();
        p.feed(b"1+r5463");
        p.feed(b"0+r524742");
        assert!(p.truecolor);
        assert_eq!(parse_rgb("rgb:ffff/0000/8080"), Some([255, 0, 128]));
    }
    #[test]
    fn duplicate_and_malformed_color_replies_leave_observations_unchanged() {
        let mut probe = ColorProbe::default();
        for invalid in [
            b"1+r5463garbage".as_slice(),
            b"1+r524742=not-hex",
            b"1+r5463;malformed",
            b"10;rgb:+f/0/0",
            b"4;256;rgb:f/f/f",
            b"11;rgb:ffff/ffff/ffff;extra",
        ] {
            probe.feed(invalid);
        }
        assert_eq!(probe.revision, 0);
        assert!(!probe.truecolor);
        for valid in [
            b"10;rgb:aaaa/bbbb/cccc".as_slice(),
            b"11;rgb:1111/2222/3333",
            b"4;2;rgb:1234/5678/9abc",
        ] {
            probe.feed(valid);
            let revision = probe.revision;
            probe.feed(valid);
            assert_eq!(probe.revision, revision);
        }
        assert!(!probe.truecolor);
        probe.feed(b"1+r524742=3234");
        assert!(probe.truecolor);
        let revision = probe.revision;
        probe.feed(b"0+r5463");
        probe.feed(b"1+r524742=3234");
        assert_eq!(probe.revision, revision);
    }
    #[test]
    fn fragmented_theme_notifications_never_become_keys_and_refresh_keeps_capability() {
        for packet in [b"\x1b[?997;1n", b"\x1b[?997;2n"] {
            for boundary in 0..=packet.len() {
                let mut demux = ReplyDemux::default();
                let mut probe = ColorProbe::default();
                probe.feed(b"1+r5463");
                probe.feed(b"10;rgb:f/f/f");
                probe.feed(b"11;rgb:0/0/0");
                probe.feed(b"4;2;rgb:0/f/0");
                let mut refreshes = 0;
                for bytes in [&packet[..boundary], &packet[boundary..]] {
                    let (keys, replies) = split(demux.feed(bytes));
                    assert!(keys.is_empty());
                    for reply in replies {
                        refreshes += usize::from(probe.theme_changed(&reply));
                    }
                }
                assert_eq!(refreshes, 1);
                assert!(probe.truecolor);
                assert!(
                    probe.foreground.is_none()
                        && probe.background.is_none()
                        && probe.palette.is_empty()
                );
            }
        }
        let mut demux = ReplyDemux::default();
        let paste = b"\x1b[200~\x1b[?997;2n\x1b[201~";
        let (keys, replies) = split(demux.feed(paste));
        assert_eq!(keys, paste);
        assert!(replies.is_empty());
    }
    #[test]
    fn undefined_private_theme_modes_are_not_keyboard_input_or_refresh_evidence() {
        let mut demux = ReplyDemux::default();
        let (keys, replies) = split(demux.feed(b"before\x1b[?997;3nafter"));
        assert_eq!(keys, b"beforeafter");
        assert!(replies.is_empty());
        let (_, replies) = split(demux.feed(b"\x1b[997;1n"));
        let mut probe = ColorProbe::default();
        assert!(replies.iter().all(|reply| !probe.theme_changed(reply)));
    }
    #[test]
    fn legacy_c0_aliases_reach_shortcuts_and_preserve_native_control_bytes() {
        let mut native = bkpr_terminal_engine::Terminal::new(Size {
            cols: 20,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        })
        .unwrap();
        for byte in [0, 0x1c, 0x1d, 0x1e, 0x1f] {
            let events = InputParser::new().parse_as_vec(&[byte], false);
            let [InputEvent::Key(key)] = events.as_slice() else {
                panic!("control byte did not produce one key: {byte}");
            };
            let event = key_event(key.clone());
            if byte == 0 {
                assert_eq!((&*event.key, event.modifiers), ("Space", 2));
            } else if byte == 0x1d {
                assert_eq!((&*event.key, event.modifiers), ("BracketRight", 2));
            }
            assert_eq!(native.encode_key(&event).unwrap(), [byte], "{byte:#x}");
        }
        let events = InputParser::new().parse_as_vec(b"\x1b\x1d", false);
        let [InputEvent::Key(key)] = events.as_slice() else {
            panic!("Alt-control alias did not produce one key");
        };
        assert_eq!(
            native.encode_key(&key_event(key.clone())).unwrap(),
            b"\x1b\x1d"
        );
    }

    #[test]
    fn notification_subscription_requires_confirmed_mutable_support() {
        let mut guard = std::mem::ManuallyDrop::new(TerminalGuard {
            graphics: GraphicsState::new(1000),
            kitty_keyboard: false,
            clipboard_mode: None,
            requested_clipboard_mode: None,
            theme_mode: None,
            background: background::Background::default(),
            notification_probe: crate::notification::Probe::default(),
            notifications: VecDeque::new(),
            observed_focus: None,
        });
        let mut out = Vec::new();
        for reply in [
            b"theme-cap;?2031;0$".as_slice(),
            b"theme-cap;?2031;4$",
            b"theme-cap;?2031;2$malformed",
        ] {
            guard.theme_reply(reply, &mut out).unwrap();
        }
        assert!(out.is_empty());
        guard.theme_reply(b"theme-cap;?2031;2$", &mut out).unwrap();
        assert_eq!(out, b"\x1b[?2031h");
        guard.theme_reply(b"theme-cap;?2031;2$", &mut out).unwrap();
        assert_eq!(out, b"\x1b[?2031h");
    }
    #[test]
    fn oversized_reply_tail_is_not_keyboard_input() {
        let mut p = ReplyDemux::default();
        let mut data = b"\x1b]52;c;".to_vec();
        data.extend(vec![b'A'; 2 * 1024 * 1024]);
        assert!(split(p.feed(&data)).0.is_empty());
        assert!(split(p.feed(b"secret-tail")).0.is_empty());
        assert_eq!(split(p.feed(b"\x1b\\z")).0, b"z");
    }
    #[test]
    fn kitty_release_and_text_are_preserved_without_reencoding_legacy() {
        let key = kitty_key(b"97;5:3").unwrap();
        assert_eq!(key.action, KeyAction::Release);
        assert_eq!(key.modifiers, 2);
        assert_eq!(key.key, "KeyA");
        assert!(key.text.is_empty());
        let key = kitty_key(b"97:65;2:1;65").unwrap();
        assert_eq!(key.text, "A");
        assert_eq!(key.modifiers, 1);
        let key = kitty_key(b"128075;1:1;128075").unwrap();
        assert_eq!(key.text, "👋");
        assert_eq!(key.key, "Unidentified");
        assert_eq!(kitty_key(b"57442;5:1").unwrap().key, "ControlLeft");
        assert_eq!(kitty_key(b"0;;229").unwrap().text, "å");
        assert_eq!(
            kitty_key(kitty_functional(b'A', b"1;5:3").unwrap().as_bytes())
                .unwrap()
                .action,
            KeyAction::Release
        );
    }
    #[test]
    fn paste_payload_is_not_misclassified_as_host_response() {
        let mut p = ReplyDemux::default();
        let (keys, replies) = split(p.feed(b"\x1b[200~literal \x1b]52;c;secret\x1b\\\x1b[20"));
        assert!(replies.is_empty());
        assert_eq!(keys, b"\x1b[200~literal \x1b]52;c;secret\x1b\\");
        let (keys, replies) = split(p.feed(b"1~z"));
        assert_eq!(keys, b"\x1b[201~z");
        assert!(replies.is_empty());
    }
    #[test]
    fn sgr_mouse_coordinates_enter_scene_space_once_for_press_and_release() {
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        assert!(decode_input(&mut parser, demux.feed(b"\x1b[<0;6;"), true).is_empty());
        let events = decode_input(&mut parser, demux.feed(b"6M\x1b[<0;6;6m"), false);
        let mice = events
            .into_iter()
            .map(|event| match event {
                ClientEvent::Mouse {
                    event,
                    button,
                    action,
                } => (event, button, action),
                _ => panic!("expected decoded SGR mouse event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(mice.len(), 2);
        assert_eq!((mice[0].0.x, mice[0].0.y), (5, 5));
        assert_eq!(mice[0].1, MouseButton::Left);
        assert_eq!(mice[0].2, MouseAction::Press);
        assert!(
            mice[0]
                .0
                .mouse_buttons
                .contains(termwiz::input::MouseButtons::LEFT)
        );
        assert_eq!((mice[1].0.x, mice[1].0.y), (5, 5));
        assert_eq!(mice[1].1, MouseButton::Left);
        assert_eq!(mice[1].2, MouseAction::Release);
        assert!(mice[1].0.mouse_buttons.is_empty());
        let events = decode_input(&mut parser, demux.feed(b"\x1b[<0;1;1M"), false);
        assert!(
            matches!(events.as_slice(), [ClientEvent::Mouse { event, button: MouseButton::Left, action: MouseAction::Press }]
            if event.x == 0 && event.y == 0)
        );
    }
    #[test]
    fn chorded_sgr_buttons_retain_each_press_and_release_identity_at_every_split() {
        let wire = b"\x1b[<0;6;6M\x1b[<2;6;6M\x1b[<0;6;6m\x1b[<2;6;6m";
        for split in 0..=wire.len() {
            let mut parser = InputParser::new();
            let mut demux = ReplyDemux::default();
            let mut events = decode_input(&mut parser, demux.feed(&wire[..split]), true);
            events.extend(decode_input(&mut parser, demux.feed(&wire[split..]), false));
            let identities = events
                .into_iter()
                .map(|event| match event {
                    ClientEvent::Mouse {
                        event,
                        button,
                        action,
                    } => {
                        assert_eq!((event.x, event.y), (5, 5));
                        (button, action)
                    }
                    _ => panic!("SGR mouse packet lost its physical identity at split {split}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                identities,
                [
                    (MouseButton::Left, MouseAction::Press),
                    (MouseButton::Right, MouseAction::Press),
                    (MouseButton::Left, MouseAction::Release),
                    (MouseButton::Right, MouseAction::Release),
                ],
                "SGR chord changed at split {split}"
            );
        }
    }

    #[test]
    fn sgr_repeated_presses_and_motion_remain_distinct_with_maintained_modifiers() {
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let events = decode_input(
            &mut parser,
            demux.feed(b"\x1b[<4;2;3M\x1b[<4;2;3M\x1b[<36;3;3M\x1b[<36;3;3M\x1b[<39;4;3M"),
            false,
        );
        let identities = events
            .into_iter()
            .map(|event| match event {
                ClientEvent::Mouse {
                    event,
                    button,
                    action,
                } => {
                    assert!(event.modifiers.contains(Modifiers::SHIFT));
                    (event.x, event.y, button, action)
                }
                _ => panic!("expected maintained-parser mouse event with physical metadata"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            identities,
            [
                (1, 2, MouseButton::Left, MouseAction::Press),
                (1, 2, MouseButton::Left, MouseAction::Press),
                (2, 2, MouseButton::Left, MouseAction::Motion),
                (2, 2, MouseButton::Left, MouseAction::Motion),
                (3, 2, MouseButton::None, MouseAction::Motion),
            ]
        );
    }

    #[test]
    fn pending_escape_precedes_structured_key_release_and_mouse_release() {
        let wire = b"\x1b\x1b[27;1:3u\x1b[<0;4;4m";
        for split in 0..=wire.len() {
            let mut parser = InputParser::new();
            let mut demux = ReplyDemux::default();
            let mut events = decode_input(&mut parser, demux.feed(&wire[..split]), true);
            events.extend(decode_input(&mut parser, demux.feed(&wire[split..]), true));
            events.extend(decode_input(&mut parser, Vec::new(), false));
            assert!(
                matches!(events.as_slice(), [
                    ClientEvent::Legacy(InputEvent::Key(press)),
                    ClientEvent::Reply(release),
                    ClientEvent::Mouse { event: mouse, button: MouseButton::Left, action: MouseAction::Release },
                ] if press.key == KeyCode::Escape
                    && release.strip_prefix(b"key;").is_some_and(|payload| {
                        kitty_key(payload).is_ok_and(|key| key.key == "Escape"
                            && key.action == bkpr_protocol::KeyAction::Release)
                    })
                    && mouse.mouse_buttons.is_empty()),
                "input order changed at wire split {split}"
            );
        }
    }
    #[test]
    fn malformed_zero_sgr_coordinates_cannot_become_top_left_clicks() {
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let events = decode_input(
            &mut parser,
            demux.feed(b"\x1b[<0;0;6M\x1b[<0;6;0M\x1b[<0;0;0M\x1b[<0;0;6ma"),
            false,
        );
        assert!(
            matches!(events.as_slice(), [ClientEvent::Legacy(InputEvent::Key(key))]
            if key.key == KeyCode::Char('a'))
        );
    }
}

#[cfg(test)]
#[path = "render_regressions.rs"]
mod regressions;

#[cfg(test)]
#[path = "render_geometry_regressions.rs"]
mod geometry_regressions;

include!("scene.rs");

#[cfg(test)]
mod graphics_probe_tests {
    use super::*;

    fn query_id(probe: &mut ColorProbe) -> u32 {
        probe.graphics.begin();
        probe.graphics.pending.unwrap().0
    }
    fn feed(probe: &mut ColorProbe, bytes: &[u8]) {
        let mut demux = ReplyDemux::default();
        for part in demux.feed(bytes) {
            if let InputPart::Reply(reply) = part {
                probe.feed(&reply);
            }
        }
    }
    #[test]
    fn only_pending_matching_apc_query_confirms_graphics() {
        let mut probe = ColorProbe::default();
        feed(&mut probe, b"\x1b_Gi=1;OK\x1b\\");
        assert!(!probe.graphics.supported());
        let id = query_id(&mut probe);
        for packet in [
            format!("\x1b]Gi={id};OK\x1b\\"),
            format!("\x1b]graphics;Gi={id};OK\x1b\\"),
            format!("\x1bPGi={id};OK\x1b\\"),
            format!("\x1b_Gi={id};OK\x07"),
            format!("\x1b_Gi={id},p=1;OK\x1b\\"),
            format!("\x1b_Gi={};OK\x1b\\", id + 1),
            format!("\x1b_Gi={id};OK;extra\x1b\\"),
        ] {
            feed(&mut probe, packet.as_bytes());
            assert!(!probe.graphics.supported());
        }
        feed(&mut probe, format!("\x1b_Gi={id};OK\x1b\\").as_bytes());
        assert!(probe.graphics.supported());
    }
    #[test]
    fn fragmented_query_reply_never_becomes_keyboard_input() {
        for split in 0..32 {
            let mut probe = ColorProbe::default();
            let id = query_id(&mut probe);
            let packet = format!("\x1b_Gi={id};OK\x1b\\");
            if split > packet.len() {
                continue;
            }
            let mut demux = ReplyDemux::default();
            for bytes in [&packet.as_bytes()[..split], &packet.as_bytes()[split..]] {
                for part in demux.feed(bytes) {
                    match part {
                        InputPart::Keys(keys) => assert!(keys.is_empty()),
                        InputPart::Reply(reply) => probe.feed(&reply),
                        InputPart::Mouse { .. } => panic!("graphics response became mouse input"),
                    }
                }
            }
            assert!(probe.graphics.supported());
        }
    }
    #[test]
    fn negative_and_expired_replies_cannot_enable_graphics() {
        let mut probe = ColorProbe::default();
        let id = query_id(&mut probe);
        feed(
            &mut probe,
            format!("\x1b_Gi={id};ENOTSUP: unsupported\x1b\\").as_bytes(),
        );
        feed(&mut probe, format!("\x1b_Gi={id};OK\x1b\\").as_bytes());
        assert!(!probe.graphics.supported());
        let id = query_id(&mut probe);
        probe.graphics.pending.as_mut().unwrap().1 =
            Instant::now() - std::time::Duration::from_secs(3);
        feed(&mut probe, format!("\x1b_Gi={id};OK\x1b\\").as_bytes());
        assert!(!probe.graphics.supported());
    }
    #[test]
    fn renderer_failures_remain_unconsumed_while_query_failures_are_owned() {
        let mut probe = ColorProbe::default();
        let id = query_id(&mut probe);
        assert!(
            !probe
                .graphics
                .consume_tagged(b"graphics;Gi=1000;EINVAL: invalid projection")
        );
        assert!(
            probe
                .graphics
                .consume_tagged(format!("graphics;Gi={id};ENOTSUP: unsupported").as_bytes())
        );
        assert!(!probe.graphics.supported());
        assert!(
            probe
                .graphics
                .consume_tagged(format!("graphics;Gi={id};OK").as_bytes())
        );
        assert!(!probe.graphics.supported());
        let id = query_id(&mut probe);
        probe.graphics.pending.as_mut().unwrap().1 =
            Instant::now() - std::time::Duration::from_secs(3);
        assert!(
            probe
                .graphics
                .consume_tagged(format!("graphics;Gi={id};OK").as_bytes())
        );
        assert!(!probe.graphics.supported());
        assert!(
            !probe
                .graphics
                .consume_tagged(b"graphics;Gi=1000,p=1;EINVAL: invalid projection")
        );
    }
}

#[cfg(test)]
mod material_cache_tests {
    use super::*;
    use crate::{
        decoration::{DecorationKind, DecorationStyle},
        graphics::{SvgAsset, SvgGraphics},
    };

    fn style() -> DecorationStyle {
        DecorationStyle {
            kind: DecorationKind::Recessed,
            background: [30; 3],
            surface: [20; 3],
            foreground: [200; 3],
            inset_px: [5; 2],
            outset_px: [0; 2],
            radius_px: 10,
            sigma_milli: 3000,
            offset_milli: [3500; 2],
            shadow_opacity_milli: 720,
            lip_width_milli: 1200,
            lip_highlight_milli: 240,
            lip_shade_milli: 240,
        }
    }

    fn material(graphics: &SvgGraphics, size: Size) -> Frame {
        let mut frame = graphics_frame(size);
        for (index, rect) in [
            Rect {
                x: 0,
                y: 0,
                width: size.cols / 2 + 1,
                height: size.rows,
            },
            Rect {
                x: size.cols / 2,
                y: 0,
                width: size.cols - size.cols / 2,
                height: size.rows,
            },
        ]
        .into_iter()
        .enumerate()
        {
            assert!(graphics.decorate_window(&mut frame, rect, style(), index as u32 * 64));
        }
        assert_eq!(frame.images.len(), 1);
        assert_eq!(frame.placements.len(), 16);
        frame
    }

    fn render(
        renderer: &mut Renderer,
        terminal: &mut TerminalGuard,
        frame: &Frame,
        cover: Option<Rect>,
    ) -> Vec<u8> {
        let popup = blank(Size {
            cols: 2,
            rows: 1,
            ..frame.size
        });
        let mut scene = Scene {
            size: frame.size,
            surfaces: vec![Surface {
                id: "material",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: frame.size.cols,
                    height: frame.size.rows,
                },
                frame,
                palette: None,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
                content: SurfaceContent::Graphics,
            }],
        };
        if let Some(rect) = cover {
            scene.surfaces.push(Surface {
                id: "cover",
                rect,
                frame: &popup,
                palette: None,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
                content: SurfaceContent::Cells,
            });
        }
        let mut output = Vec::new();
        renderer
            .render(&mut output, &scene, &ColorProbe::default(), terminal)
            .unwrap();
        output
    }

    fn uploads(output: &[u8]) -> usize {
        output
            .windows(b"\x1b_Ga=t,".len())
            .filter(|chunk| *chunk == b"\x1b_Ga=t,")
            .count()
    }

    fn encoded_bytes(output: &[u8]) -> usize {
        std::str::from_utf8(output)
            .unwrap()
            .split("\x1b_G")
            .skip(1)
            .filter_map(|packet| packet.split_once("\x1b\\"))
            .filter_map(|(packet, _)| packet.split_once(';'))
            .filter(|(header, _)| header.starts_with("a=t,") || header.starts_with("m="))
            .map(|(_, payload)| payload.len())
            .sum()
    }

    #[test]
    fn prepared_edges_share_one_upload_across_warm_resize_and_opaque_clipping() {
        static ASSETS: &[SvgAsset] = &[SvgAsset {
            id: 1,
            svg: r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><path d="M0 0h1v1H0z"/></svg>"#,
        }];
        let mut graphics = SvgGraphics::new(ASSETS, 1, 1).unwrap();
        let mut host = ColorProbe::default();
        let query = host.graphics.begin();
        let query = std::str::from_utf8(&query).unwrap();
        let id = query.split("i=").nth(1).unwrap().split(',').next().unwrap();
        host.feed(format!("graphics;Gi={id};OK").as_bytes());
        let size = Size {
            cols: 120,
            rows: 40,
            cell_width: 10,
            cell_height: 20,
        };
        graphics.prepare_with_decorations(&host, size, &[], &[style()]);
        let mut poll = libc::pollfd {
            fd: graphics.wake_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        while !graphics.drain().unwrap() {
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
        }
        let first = material(&graphics, size);
        let mut renderer = Renderer::default();
        let mut terminal = std::mem::ManuallyDrop::new(TerminalGuard {
            graphics: GraphicsState::new(1000),
            kitty_keyboard: false,
            clipboard_mode: None,
            requested_clipboard_mode: None,
            theme_mode: None,
            background: background::Background::default(),
            notification_probe: crate::notification::Probe::default(),
            notifications: VecDeque::new(),
            observed_focus: None,
        });

        let cold = render(&mut renderer, &mut terminal, &first, None);
        assert_eq!(uploads(&cold), 1);
        assert_eq!(
            encoded_bytes(&cold),
            first.images[0].pixels.len().div_ceil(3) * 4
        );
        assert_eq!(terminal.graphics.image_ids.len(), 1);
        assert!(terminal.graphics.derived_ids.is_empty());
        let physical = terminal.graphics.image_ids.clone();
        assert!(render(&mut renderer, &mut terminal, &first, None).is_empty());

        let resized = Size {
            cols: 160,
            rows: 48,
            ..size
        };
        graphics.prepare_with_decorations(&host, resized, &[], &[style()]);
        let frame = material(&graphics, resized);
        assert!(Arc::ptr_eq(&first.images[0], &frame.images[0]));
        assert_eq!(
            uploads(&render(&mut renderer, &mut terminal, &frame, None)),
            0
        );
        assert_eq!(terminal.graphics.image_ids, physical);
        assert!(terminal.graphics.derived_ids.is_empty());

        // An opaque cell layer cuts a horizontal strip in two. Moving and
        // dismissing it must only change placements, not raster/base64/upload.
        for x in [5, 7, 9, 5] {
            let cover = Some(Rect {
                x,
                y: 0,
                width: 2,
                height: 1,
            });
            assert_eq!(
                uploads(&render(&mut renderer, &mut terminal, &frame, cover)),
                0
            );
            assert_eq!(terminal.graphics.image_ids, physical);
            assert!(terminal.graphics.derived_ids.is_empty());
            assert!(render(&mut renderer, &mut terminal, &frame, cover).is_empty());
        }
        assert_eq!(
            uploads(&render(&mut renderer, &mut terminal, &frame, None)),
            0
        );
        assert_eq!(terminal.graphics.image_ids, physical);
        assert!(terminal.graphics.derived_ids.is_empty());
    }
}

#[cfg(test)]
mod notification_host_tests {
    use super::*;
    use crate::notification::{Activation, Id, MAX_ACTIVE, Notification, Reply, Urgency};

    fn terminal() -> std::mem::ManuallyDrop<TerminalGuard> {
        std::mem::ManuallyDrop::new(TerminalGuard {
            graphics: GraphicsState::new(1000),
            kitty_keyboard: false,
            clipboard_mode: None,
            requested_clipboard_mode: None,
            theme_mode: None,
            background: background::Background::default(),
            notification_probe: crate::notification::Probe::default(),
            notifications: VecDeque::new(),
            observed_focus: None,
        })
    }

    fn confirm(guard: &mut TerminalGuard, fields: &str, now: Instant) {
        let mut query = Vec::new();
        guard.notification_probe.begin(&mut query, now).unwrap();
        assert!(guard.notification_deadline().is_some());
        let query = std::str::from_utf8(&query).unwrap();
        let id = query.split_once("i=").unwrap().1.split(':').next().unwrap();
        let packet = format!("notification;99;i={id}:p=?;{fields}");
        assert_eq!(guard.notification_reply(packet.as_bytes(), now), None);
        assert!(guard.notification_capabilities().is_some());
        assert_eq!(guard.notification_deadline(), None);
    }

    fn notice(id: Id) -> Notification<'static> {
        Notification {
            id,
            app_name: "test",
            title: "attention",
            body: "",
            activation: Activation::Report,
            report_close: true,
            urgency: Urgency::Normal,
        }
    }

    fn activate(guard: &mut TerminalGuard, id: Id, now: Instant) -> Option<Reply> {
        guard.notification_reply(format!("notification;99;i={id};").as_bytes(), now)
    }

    #[test]
    fn only_owned_requested_callbacks_survive_until_tracked_close() {
        let now = Instant::now();
        let mut guard = terminal();
        let mut out = Vec::new();
        let id = Id::new();
        assert_eq!(
            guard
                .show_notification(&mut out, &notice(id))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(out.is_empty());
        assert_eq!(activate(&mut guard, id, now), None);
        confirm(&mut guard, "p=title,close:a=report:c=1", now);
        guard.show_notification(&mut out, &notice(id)).unwrap();
        assert_eq!(activate(&mut guard, Id::new(), now), None);
        assert_eq!(activate(&mut guard, id, now), Some(Reply::Activated { id }));
        let untracked = format!("notification;99;i={id}:p=close;untracked");
        assert_eq!(
            guard.notification_reply(untracked.as_bytes(), now),
            Some(Reply::Closed { id, tracked: false })
        );
        assert_eq!(activate(&mut guard, id, now), Some(Reply::Activated { id }));
        let closed = format!("notification;99;i={id}:p=close;");
        assert_eq!(
            guard.notification_reply(closed.as_bytes(), now),
            Some(Reply::Closed { id, tracked: true })
        );
        assert_eq!(activate(&mut guard, id, now), None);
        assert_eq!(guard.notification_reply(closed.as_bytes(), now), None);
        let mut silent = notice(Id::new());
        silent.activation = Activation::None;
        silent.report_close = false;
        guard.show_notification(&mut out, &silent).unwrap();
        assert_eq!(activate(&mut guard, silent.id, now), None);
        let closed = format!("notification;99;i={}:p=close;", silent.id);
        assert_eq!(guard.notification_reply(closed.as_bytes(), now), None);
    }

    #[test]
    fn eviction_explicit_close_and_exit_release_owned_resources() {
        let now = Instant::now();
        let mut guard = terminal();
        confirm(&mut guard, "p=title,close:a=report:c=1", now);
        let mut out = Vec::new();
        let first = Id::new();
        guard.show_notification(&mut out, &notice(first)).unwrap();
        for _ in 1..MAX_ACTIVE {
            assert_eq!(
                guard
                    .show_notification(&mut out, &notice(Id::new()))
                    .unwrap(),
                None
            );
        }
        let last = Id::new();
        assert_eq!(
            guard.show_notification(&mut out, &notice(last)).unwrap(),
            Some(first)
        );
        assert_eq!(activate(&mut guard, first, now), None);
        assert!(out.windows(6).any(|bytes| bytes == b":p=clo"));
        out.clear();
        guard.close_notification(&mut out, last).unwrap();
        assert_eq!(out, format!("\x1b]99;i={last}:p=close;\x1b\\").as_bytes());
        assert_eq!(activate(&mut guard, last, now), None);
        out.clear();
        guard.close_notification(&mut out, last).unwrap();
        assert!(out.is_empty());
        guard.restore_output(&mut out);
        let output = std::str::from_utf8(&out).unwrap();
        assert_eq!(output.matches(":p=close;").count(), MAX_ACTIVE - 1);
        assert!(output.rfind(":p=close;").unwrap() < output.find("\x1b[?1049l").unwrap());
        out.clear();
        guard.restore_output(&mut out);
        assert!(!std::str::from_utf8(&out).unwrap().contains(":p=close;"));
    }

    #[test]
    fn close_without_confirmed_support_is_local_only() {
        let now = Instant::now();
        let mut guard = terminal();
        confirm(&mut guard, "p=title:a=report", now);
        let mut out = Vec::new();
        let mut notification = notice(Id::new());
        notification.report_close = false;
        guard.show_notification(&mut out, &notification).unwrap();
        out.clear();
        guard.close_notification(&mut out, notification.id).unwrap();
        assert!(out.is_empty());
        assert_eq!(activate(&mut guard, notification.id, now), None);
    }

    #[test]
    fn partial_output_does_not_authorize_callbacks_but_retains_cleanup() {
        struct PartialWriter {
            remaining: usize,
            written: Vec<u8>,
        }
        impl Write for PartialWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Err(io::Error::other("fixture output failure"));
                }
                let count = bytes.len().min(self.remaining);
                self.written.extend_from_slice(&bytes[..count]);
                self.remaining -= count;
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let now = Instant::now();
        let mut guard = terminal();
        confirm(&mut guard, "p=title,close:a=report:c=1", now);
        let id = Id::new();
        let mut out = PartialWriter {
            remaining: 20,
            written: Vec::new(),
        };
        assert!(guard.show_notification(&mut out, &notice(id)).is_err());
        assert!(!out.written.is_empty());
        assert_eq!(activate(&mut guard, id, now), None);
        let mut restored = Vec::new();
        guard.restore_output(&mut restored);
        assert!(restored.starts_with(format!("\x1b]99;i={id}:p=close;\x1b\\").as_bytes()));
    }

    #[test]
    fn physical_focus_requires_exact_trusted_demux_provenance() {
        let mut guard = terminal();
        assert_eq!(guard.observed_focus(), None);
        for wire in [
            b"\x1b]focus;in\x1b\\".as_slice(),
            b"\x1bPfocus;out\x1b\\",
            b"\x1b_focus;in\x1b\\",
            b"\x1b^focus;out\x1b\\",
        ] {
            for split in 0..=wire.len() {
                let mut demux = ReplyDemux::default();
                for chunk in [&wire[..split], &wire[split..]] {
                    for part in demux.feed(chunk) {
                        let InputPart::Reply(packet) = part else {
                            panic!("forged focus leaked as keyboard input");
                        };
                        assert_eq!(guard.observe_focus_reply(&packet), None);
                    }
                }
                assert_eq!(guard.observed_focus(), None);
            }
        }
        for (wire, focused) in [(b"\x1b[I".as_slice(), true), (b"\x1b[O", false)] {
            let mut demux = ReplyDemux::default();
            for part in demux.feed(wire) {
                let InputPart::Reply(packet) = part else {
                    panic!("focus leaked as keyboard input");
                };
                assert_eq!(guard.observe_focus_reply(&packet), Some(focused));
            }
            assert_eq!(guard.observed_focus(), Some(focused));
        }
        assert_eq!(guard.observe_focus_reply(b"focus;in;extra"), None);
        assert_eq!(guard.observed_focus(), Some(false));
    }
}

#[cfg(test)]
mod graphics_query_lifecycle_tests {
    use super::*;

    #[test]
    fn unanswered_graphics_queries_resolve_without_an_idle_deadline() {
        let mut probe = GraphicsProbe::default();
        assert!(!probe.resolved());
        drop(probe.begin());
        let deadline = probe.next_deadline().unwrap();
        assert!(!probe.expire(deadline - std::time::Duration::from_nanos(1)));
        assert!(!probe.resolved());
        assert!(probe.expire(deadline));
        assert!(probe.resolved());
        assert!(!probe.supported());
        assert_eq!(probe.next_deadline(), None);
        assert!(!probe.expire(deadline + std::time::Duration::from_secs(1)));
    }
}

#[cfg(test)]
mod prepared_png_tests {
    use super::*;
    use crate::prepared_png::{decode, encode};

    #[derive(Debug)]
    struct Upload {
        format: u8,
        zlib: bool,
        id: u32,
        width: u32,
        height: u32,
        payload: Vec<u8>,
    }

    fn uploads(mut output: &[u8]) -> Vec<Upload> {
        let mut uploads: Vec<Upload> = Vec::new();
        while let Some(start) = output.windows(3).position(|bytes| bytes == b"\x1b_G") {
            output = &output[start + 3..];
            let end = output
                .windows(2)
                .position(|bytes| bytes == b"\x1b\\")
                .unwrap();
            let packet = std::str::from_utf8(&output[..end]).unwrap();
            output = &output[end + 2..];
            let Some((header, payload)) = packet.split_once(';') else {
                continue;
            };
            let fields: HashMap<_, _> = header
                .split(',')
                .map(|field| field.split_once('=').unwrap())
                .collect();
            if fields.get("a") == Some(&"t") {
                uploads.push(Upload {
                    format: fields["f"].parse().unwrap(),
                    zlib: fields.get("o") == Some(&"z"),
                    id: fields["i"].parse().unwrap(),
                    width: fields["s"].parse().unwrap(),
                    height: fields["v"].parse().unwrap(),
                    payload: Vec::new(),
                });
            } else {
                assert_eq!(fields.len(), 1);
                assert!(fields.contains_key("m"));
            }
            uploads
                .last_mut()
                .unwrap()
                .payload
                .extend(BASE64.decode(payload).unwrap());
        }
        uploads
    }

    fn image(width: u32, height: u32, format: PixelFormat) -> Arc<Image> {
        let mut random = 1u32;
        let pixels = (0..width as usize * height as usize * format.bytes_per_pixel())
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            })
            .collect();
        Arc::new(Image {
            id: 7,
            generation: 3,
            width,
            height,
            format,
            pixels,
            source: None,
        })
    }

    fn terminal() -> std::mem::ManuallyDrop<TerminalGuard> {
        std::mem::ManuallyDrop::new(TerminalGuard {
            graphics: GraphicsState::new(1000),
            kitty_keyboard: false,
            clipboard_mode: None,
            requested_clipboard_mode: None,
            theme_mode: None,
            background: background::Background::default(),
            notification_probe: crate::notification::Probe::default(),
            notifications: VecDeque::new(),
            observed_focus: None,
        })
    }

    fn paint(terminal: &mut TerminalGuard, image: Arc<Image>) -> Vec<Upload> {
        let size = Size {
            cols: image.width as u16,
            rows: image.height as u16,
            cell_width: 1,
            cell_height: 1,
        };
        let mut frame = graphics_frame(size);
        frame.placements.push(Placement {
            image_id: image.id,
            generation: image.generation,
            placement_id: 1,
            col: 0,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            width: image.width,
            height: image.height,
            source_x: 0,
            source_y: 0,
            source_width: image.width,
            source_height: image.height,
            layer: ImageLayer::AboveText,
            z: 0,
        });
        frame.images.push(image);
        let mut output = Vec::new();
        draw(
            &mut output,
            &frame,
            size,
            "",
            &ColorProbe::default(),
            &mut terminal.graphics,
        )
        .unwrap();
        uploads(&output)
    }

    #[test]
    fn exact_registered_source_uploads_original_png_bytes_in_chunks() {
        let image = image(64, 32, PixelFormat::Rgba);
        let encoded = encode(image.width, image.height, image.format, &image.pixels).unwrap();
        assert!(encoded.len() > 3072);
        let prepared = PreparedTransport::from_verified(&image, encoded.clone());
        let mut terminal = terminal();
        terminal.register_prepared_transports([&prepared]);
        let uploads = paint(&mut terminal, image);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].format, 100);
        assert_eq!(uploads[0].id, 1001);
        assert_eq!((uploads[0].width, uploads[0].height), (64, 32));
        assert_eq!(uploads[0].payload, encoded.as_ref());
    }

    #[test]
    fn compressed_raw_transport_uploads_zlib_that_inflates_to_exact_pixels() {
        for (format, wire_format) in [(PixelFormat::Rgb, 24), (PixelFormat::Rgba, 32)] {
            let mut flat = image(64, 32, format).as_ref().clone();
            for (index, byte) in flat.pixels.iter_mut().enumerate() {
                *byte = (index / 97) as u8;
            }
            let flat = Arc::new(flat);
            let prepared = PreparedTransport::compress(&flat).unwrap();
            let mut terminal = terminal();
            terminal.register_prepared_transports([&prepared]);
            let uploads = paint(&mut terminal, flat.clone());
            assert_eq!(uploads.len(), 1);
            assert_eq!((uploads[0].format, uploads[0].zlib), (wire_format, true));
            assert!(uploads[0].payload.len() < flat.pixels.len());
            let mut inflated = Vec::new();
            std::io::Read::read_to_end(
                &mut flate2::read::ZlibDecoder::new(uploads[0].payload.as_slice()),
                &mut inflated,
            )
            .unwrap();
            assert_eq!(inflated, flat.pixels);
        }
        // Incompressible and gray pixels keep their exact existing raw/PNG transport.
        assert!(PreparedTransport::compress(&image(64, 32, PixelFormat::Rgb)).is_none());
        assert!(PreparedTransport::compress(&image(4, 4, PixelFormat::Gray)).is_none());
    }

    #[test]
    fn same_native_ids_and_pixels_do_not_grant_another_allocation_png_transport() {
        let original = image(4, 2, PixelFormat::Rgba);
        let encoded = encode(4, 2, original.format, &original.pixels).unwrap();
        let prepared = PreparedTransport::from_verified(&original, encoded);
        for change_pixels in [false, true] {
            let mut other = original.as_ref().clone();
            if change_pixels {
                other.pixels[0] ^= 255;
            }
            let other = Arc::new(other);
            let mut terminal = terminal();
            terminal.register_prepared_transports([&prepared]);
            let uploads = paint(&mut terminal, other.clone());
            assert_eq!(uploads.len(), 1);
            assert_eq!(uploads[0].format, 32);
            assert_eq!(uploads[0].id, 1001);
            assert_eq!(uploads[0].payload, other.pixels);
        }
    }

    #[test]
    fn unregistered_channel_formats_keep_exact_existing_transport() {
        for (format, wire_format) in [
            (PixelFormat::Rgb, 24),
            (PixelFormat::Rgba, 32),
            (PixelFormat::Gray, 100),
            (PixelFormat::GrayAlpha, 100),
        ] {
            let source = image(4, 2, format);
            let uploads = paint(&mut terminal(), source.clone());
            assert_eq!(uploads.len(), 1);
            assert_eq!(uploads[0].format, wire_format);
            if wire_format == 100 {
                assert_eq!(
                    decode(&uploads[0].payload, 4, 2, format).unwrap(),
                    source.pixels
                );
            } else {
                assert_eq!(uploads[0].payload, source.pixels);
            }
        }
    }

    #[test]
    fn registration_bounds_resources_and_releases_expired_sources() {
        let source = image(1, 1, PixelFormat::Rgba);
        let encoded = encode(1, 1, source.format, &source.pixels).unwrap();
        let images: Vec<_> = (0..MAX_PREPARED_TRANSPORTS + 1)
            .map(|_| Arc::new(source.as_ref().clone()))
            .collect();
        let prepared: Vec<_> = images
            .iter()
            .map(|image| PreparedTransport::from_verified(image, encoded.clone()))
            .collect();
        let mut terminal = terminal();
        terminal.register_prepared_transports(&prepared);
        assert_eq!(
            terminal.graphics.prepared_transports.len(),
            MAX_PREPARED_TRANSPORTS
        );
        assert_eq!(
            terminal.graphics.prepared_transport_bytes,
            MAX_PREPARED_TRANSPORTS * encoded.len()
        );
        assert!(terminal.graphics.prepared_transport(&images[0]).is_none());
        assert!(
            terminal
                .graphics
                .prepared_transport(images.last().unwrap())
                .is_some()
        );
        drop(images);
        terminal.register_prepared_transports(std::iter::empty::<&PreparedTransport>());
        assert!(terminal.graphics.prepared_transports.is_empty());
        assert_eq!(terminal.graphics.prepared_transport_bytes, 0);
    }

    #[test]
    fn registration_charges_encoded_bytes_before_admission() {
        let source = image(512, 256, PixelFormat::Rgba);
        let encoded = encode(source.width, source.height, source.format, &source.pixels).unwrap();
        let capacity = MAX_PREPARED_TRANSPORT_BYTES / encoded.len();
        assert!(capacity < MAX_PREPARED_TRANSPORTS);
        let images: Vec<_> = (0..capacity + 1)
            .map(|_| Arc::new(source.as_ref().clone()))
            .collect();
        let prepared: Vec<_> = images
            .iter()
            .map(|image| PreparedTransport::from_verified(image, encoded.clone()))
            .collect();
        let mut terminal = terminal();
        terminal.register_prepared_transports(&prepared);
        assert_eq!(terminal.graphics.prepared_transports.len(), capacity);
        assert_eq!(
            terminal.graphics.prepared_transport_bytes,
            capacity * encoded.len()
        );
        assert!(terminal.graphics.prepared_transport_bytes <= MAX_PREPARED_TRANSPORT_BYTES);
        assert!(terminal.graphics.prepared_transport(&images[0]).is_none());
    }
}
