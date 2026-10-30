use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bkpr_protocol::*;
#[cfg(test)]
use std::sync::Arc;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::Write,
    time::Instant,
};
use termwiz::input::{InputEvent, InputParser, KeyCode, Modifiers};

#[path = "render_geometry.rs"]
mod geometry;
#[path = "render_style.rs"]
mod style;

#[derive(Default)]
struct TextState {
    viewport: Option<Size>,
    rows: Vec<Row>,
    palette: Vec<[u8; 3]>,
    colors: Option<([u8; 3], [u8; 3], u64, bool)>,
    chrome: String,
    cursor: Option<Cursor>,
    physical_cursor_color: Option<Option<[u8; 3]>>,
}

struct GraphicsState {
    next_image: u32,
    image_ids: HashMap<ImageKey, u32>,
    derived_ids: HashMap<geometry::DerivedKey, u32>,
    text: TextState,
    projections: Vec<geometry::Projection>,
}
impl GraphicsState {
    fn new(start_id: u32) -> Self {
        Self {
            next_image: start_id,
            image_ids: HashMap::new(),
            derived_ids: HashMap::new(),
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
        self.image_ids.values().chain(self.derived_ids.values())
    }
}
pub struct TerminalGuard {
    graphics: GraphicsState,
    pub kitty_keyboard: bool,
    pub clipboard_mode: Option<bool>,
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        for id in self.graphics.all_ids() {
            let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
        }
        if self.kitty_keyboard {
            let _ = write!(out, "\x1b[<u");
        }
        if let Some(enabled) = self.clipboard_mode {
            let _ = write!(out, "\x1b[?5522{}", if enabled { "h" } else { "l" });
        }
        let _ = write!(
            out,
            "\x1b[?2026l\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l"
        );
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
fn is_scene_protocol_char(c: char) -> bool {
    // The Kitty placeholder is printable Unicode, but the host interprets it
    // as an image reference. Only typed, remapped placements may reference IDs.
    c.is_control() || c == '\u{10eeee}'
}
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
) -> Result<()> {
    if source == ColorSource::Default
        && (if background {
            host.background
        } else {
            host.foreground
        })
        .is_none_or(|physical| physical == rgb)
    {
        write!(out, "\x1b[{}m", if background { 49 } else { 39 })?;
    } else if let ColorSource::Palette(index @ 0..=15) = source
        && host
            .palette
            .get(&(index as usize))
            .is_some_and(|physical| *physical == rgb)
    {
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
    // Encode one protocol chunk at a time rather than allocating a second
    // image-sized base64 string alongside canonical/derived raster buffers.
    let mut encoded = [0u8; 4096];
    let mut chunks = pixels.chunks(3072).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(
                out,
                "\x1b_Ga=t,f={format},s={width},v={height},i={id},q=1,m={more};"
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
    draw_prepared(out, frame, viewport, chrome, host, graphics, prepared)
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
    graphics: &mut GraphicsState,
    prepared: geometry::Prepared<'_>,
) -> Result<()> {
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
    let colors = (
        frame.foreground,
        frame.background,
        host.revision,
        host.truecolor,
    );
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
            .any(|key| !prepared.derived.contains(key))
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
    for (_, id) in graphics
        .image_ids
        .extract_if(|key, _| !prepared.images.contains_key(key))
    {
        write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
    }
    for (_, id) in graphics
        .derived_ids
        .extract_if(|key, _| !prepared.derived.contains(key))
    {
        write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
    }
    if placements_changed {
        for id in graphics.all_ids() {
            write!(out, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\")?;
        }
    }
    write!(out, "\x1b[?2026h\x1b[?25l\x1b[0m")?;
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
                    style::transition(out, previous_style, s, frame, host)?;
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
        transmit_image(
            out,
            id,
            image.width,
            image.height,
            image.format,
            &image.pixels,
        )?;
    }
    for p in prepared.placements.iter().filter(|_| placements_changed) {
        let id = if let Some(key) = p.derived {
            if let Some(id) = graphics.derived_ids.get(&key) {
                *id
            } else {
                let image = prepared.images[&key.image];
                let pixels = geometry::raster(image, key)?;
                let id = graphics.allocate()?;
                graphics.derived_ids.insert(key, id);
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
    if frame.cursor.visible && frame.cursor.x < viewport.cols && frame.cursor.y < viewport.rows {
        let shape = match frame.cursor.shape {
            CursorShape::Block | CursorShape::Hollow => 2,
            CursorShape::Underline => 4,
            CursorShape::Bar => 6,
        } - u8::from(frame.cursor.blinking);
        write!(
            out,
            "\x1b[{shape} q\x1b[{};{}H\x1b[?25h",
            frame.cursor.y + 1,
            frame.cursor.x + 1
        )?;
        if graphics.text.physical_cursor_color != Some(frame.cursor.color) {
            if let Some([r, g, b]) = frame.cursor.color {
                write!(out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x1b\\")?;
            } else {
                write!(out, "\x1b]112\x1b\\")?;
            }
            graphics.text.physical_cursor_color = Some(frame.cursor.color);
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
    Keys(Vec<u8>),
    Reply(Vec<u8>),
}
pub enum ClientEvent {
    Legacy(InputEvent),
    Reply(Vec<u8>),
}
fn emit_reply(parts: &mut Vec<InputPart>, keys: &mut Vec<u8>, reply: Vec<u8>) {
    if !keys.is_empty() {
        parts.push(InputPart::Keys(std::mem::take(keys)));
    }
    parts.push(InputPart::Reply(reply));
}
fn client_input(event: InputEvent) -> Option<ClientEvent> {
    let event = match event {
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
            InputPart::Reply(reply) => events.push(ClientEvent::Reply(reply)),
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
#[derive(Default)]
pub struct ReplyDemux {
    buffer: Vec<u8>,
    in_paste: bool,
    paste_bytes: usize,
}
impl ReplyDemux {
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
                        emit_reply(
                            &mut parts,
                            &mut keys,
                            self.buffer[start + 2..payload_end].to_vec(),
                        );
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
    pub fn flush_escape(&mut self) -> Vec<u8> {
        if !self.in_paste && self.buffer == [0x1b] {
            std::mem::take(&mut self.buffer)
        } else {
            Vec::new()
        }
    }
}
pub struct Consent {
    pub id: String,
    pub request: ClipboardRequest,
    pub started: Instant,
    pub sent: bool,
    pub allowed: bool,
    pub contents: Vec<ClipboardContent>,
    pub available: Vec<String>,
    // None means the startup DECRQM response has not arrived, not unsupported.
    pub enhanced_supported: Option<bool>,
    pub paste: bool,
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
pub struct PasteOffer {
    pub password: String,
    pub mimes: Vec<String>,
    pub primary: bool,
    pub started: Instant,
}
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
    let (name, text, codepoint) = match key.key {
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
    let base = codes
        .next()
        .filter(|s| !s.is_empty())
        .map(str::parse::<u32>)
        .transpose()?
        .unwrap_or(code);
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
        _ => KeyCode::Char(char::from_u32(base).context("invalid Kitty Unicode key")?),
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
    event.unshifted_codepoint = if !(57344..=63743).contains(&base) {
        base
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
#[derive(Default)]
pub struct ColorProbe {
    pub truecolor: bool,
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub cursor: Option<[u8; 3]>,
    pub palette: HashMap<usize, [u8; 3]>,
    pub revision: u64,
}
fn parse_rgb(value: &str) -> Option<[u8; 3]> {
    let mut values = value.strip_prefix("rgb:")?.split('/');
    let mut result = [0; 3];
    for component in &mut result {
        let s = values.next()?;
        if s.is_empty() || s.len() > 4 {
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
    pub fn feed(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        if (text.starts_with("1+r5463") || text.starts_with("1+r524742")) && !self.truecolor {
            self.truecolor = true;
            self.revision += 1;
        }
        let parts = text.split(';').collect::<Vec<_>>();
        match parts.as_slice() {
            ["10", rgb] => {
                if let Some(rgb) = parse_rgb(rgb) {
                    self.foreground = Some(rgb);
                    self.revision += 1;
                }
            }
            ["11", rgb] => {
                if let Some(rgb) = parse_rgb(rgb) {
                    self.background = Some(rgb);
                    self.revision += 1;
                }
            }
            ["12", rgb] => {
                if let Some(rgb) = parse_rgb(rgb) {
                    self.cursor = Some(rgb);
                    self.revision += 1;
                }
            }
            ["4", index, rgb] => {
                if let (Ok(index), Some(rgb)) = (index.parse::<usize>(), parse_rgb(rgb))
                    && index < 256
                {
                    self.palette.insert(index, rgb);
                    self.revision += 1;
                }
            }
            _ => {}
        }
    }
}
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
                ClientEvent::Legacy(InputEvent::Mouse(mouse)) => mouse,
                _ => panic!("expected decoded SGR mouse event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(mice.len(), 2);
        assert_eq!((mice[0].x, mice[0].y), (5, 5));
        assert!(
            mice[0]
                .mouse_buttons
                .contains(termwiz::input::MouseButtons::LEFT)
        );
        assert_eq!((mice[1].x, mice[1].y), (5, 5));
        assert!(mice[1].mouse_buttons.is_empty());
        let events = decode_input(&mut parser, demux.feed(b"\x1b[<0;1;1M"), false);
        assert!(
            matches!(events.as_slice(), [ClientEvent::Legacy(InputEvent::Mouse(mouse))]
            if mouse.x == 0 && mouse.y == 0)
        );
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
