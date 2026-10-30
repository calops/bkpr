use crate::transport;
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bkpr_terminal::*;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use termwiz::input::{InputEvent, InputParser, KeyCode, Modifiers};

#[path = "render_geometry.rs"]
mod geometry;

struct GraphicsState {
    next_image: u32,
    image_ids: HashMap<ImageKey, u32>,
    derived_ids: HashMap<geometry::DerivedKey, u32>,
}
impl GraphicsState {
    fn new(start_id: u32) -> Self {
        Self {
            next_image: start_id,
            image_ids: HashMap::new(),
            derived_ids: HashMap::new(),
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
struct TerminalGuard {
    graphics: GraphicsState,
    kitty_keyboard: bool,
    clipboard_mode: Option<bool>,
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
            "\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l"
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
fn safe_text(text: &str) -> String {
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
fn dimensions() -> Result<Size> {
    let (cols, rows) = crossterm::terminal::size()?;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe {
        libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws);
    }
    Ok(Size {
        cols: cols.max(1),
        rows: rows.saturating_sub(1).max(1),
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

fn draw(
    out: &mut impl Write,
    frame: &Frame,
    viewport: Size,
    chrome: &str,
    host: &ColorProbe,
    graphics: &mut GraphicsState,
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
    let prepared = geometry::prepare(frame, viewport)?;
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
    for id in graphics.all_ids() {
        write!(out, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\")?;
    }
    write!(out, "\x1b[?25l\x1b[0m")?;
    // ED2 also destroys Kitty image resources. EL2 clears only text, so
    // retained generation receipts remain valid across text-only repaints.
    for row in 0..=viewport.rows {
        write!(out, "\x1b[{};1H\x1b[2K", row + 1)?;
    }
    let visible_rows = frame.rows.len().min(viewport.rows as usize);
    for (y, row) in frame.rows.iter().take(visible_rows).enumerate() {
        let mut x = 0u16;
        let mut previous_style: Option<&Style> = None;
        for cell in &row.cells {
            if x >= viewport.cols || x.saturating_add(cell.width as u16) > viewport.cols {
                break;
            }
            if cell.width == 0 {
                continue;
            }
            let s = &cell.style;
            write!(out, "\x1b[{};{}H", y + 1, x + 1)?;
            if previous_style != Some(s) {
                write!(out, "\x1b[0m")?;
                color(out, s.foreground, s.foreground_source, false, frame, host)?;
                color(out, s.background, s.background_source, true, frame, host)?;
                for (set, code) in [
                    (s.bold, 1),
                    (s.faint, 2),
                    (s.italic, 3),
                    (s.blink, 5),
                    (s.inverse, 7),
                    (s.invisible, 8),
                    (s.strikethrough, 9),
                    (s.overline, 53),
                ] {
                    if set {
                        write!(out, "\x1b[{code}m")?;
                    }
                }
                if s.underline > 0 {
                    write!(out, "\x1b[4:{}m", s.underline.min(5))?;
                    match s.underline_source {
                        ColorSource::Default => write!(out, "\x1b[59m")?,
                        _ if host.truecolor => write!(
                            out,
                            "\x1b[58;2;{};{};{}m",
                            s.underline_color[0], s.underline_color[1], s.underline_color[2]
                        )?,
                        _ => {}
                    }
                }
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
            x = x.saturating_add(cell.width as u16);
        }
    }
    for image in &frame.images {
        if !prepared.original.contains(&image.key())
            || graphics.image_ids.contains_key(&image.key())
        {
            continue;
        }
        let id = graphics.allocate()?;
        graphics.image_ids.insert(image.key(), id);
        transmit_image(
            out,
            id,
            image.width,
            image.height,
            image.format,
            &image.pixels,
        )?;
    }
    for p in &prepared.placements {
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
    write!(
        out,
        "\x1b[{};1H\x1b[0;7m{}\x1b[K\x1b[0m",
        viewport.rows + 1,
        safe_text(chrome)
            .chars()
            .take(viewport.cols as usize)
            .collect::<String>()
    )?;
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
        if let Some([r, g, b]) = frame.cursor.color {
            write!(out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x1b\\")?;
        } else {
            write!(out, "\x1b]112\x1b\\")?;
        }
    }
    out.flush()?;
    Ok(())
}

/// Demultiplex terminal-owned OSC/DCS/APC responses before the maintained key
/// parser. A clipboard payload can never become a keystroke, including after
/// timeout. This is input framing, not a second application terminal parser.
enum InputPart {
    Keys(Vec<u8>),
    Reply(Vec<u8>),
}
enum ClientEvent {
    Legacy(InputEvent),
    Reply(Vec<u8>),
}
fn emit_reply(parts: &mut Vec<InputPart>, keys: &mut Vec<u8>, reply: Vec<u8>) {
    if !keys.is_empty() {
        parts.push(InputPart::Keys(std::mem::take(keys)));
    }
    parts.push(InputPart::Reply(reply));
}
fn decode_input(parser: &mut InputParser, parts: Vec<InputPart>, more: bool) -> Vec<ClientEvent> {
    let mut events = Vec::new();
    for part in parts {
        match part {
            InputPart::Keys(bytes) => events.extend(
                parser
                    .parse_as_vec(&bytes, more)
                    .into_iter()
                    .map(ClientEvent::Legacy),
            ),
            InputPart::Reply(reply) => events.push(ClientEvent::Reply(reply)),
        }
    }
    if !more {
        events.extend(
            parser
                .parse_as_vec(&[], false)
                .into_iter()
                .map(ClientEvent::Legacy),
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
    fn feed(&mut self, bytes: &[u8]) -> Vec<InputPart> {
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
                if self.paste_bytes > crate::MAX_INPUT {
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
    fn flush_escape(&mut self) -> Vec<u8> {
        if !self.in_paste && self.buffer == [0x1b] {
            std::mem::take(&mut self.buffer)
        } else {
            Vec::new()
        }
    }
}
struct Consent {
    id: String,
    request: ClipboardRequest,
    started: Instant,
    sent: bool,
    allowed: bool,
    contents: Vec<ClipboardContent>,
    available: Vec<String>,
    // None means the startup DECRQM response has not arrived, not unsupported.
    enhanced_supported: Option<bool>,
    paste: bool,
    password: Option<String>,
}
fn reply(socket: &Path, view: &str, id: &str, response: ClipboardResponse) -> Result<()> {
    transport::checked(
        socket,
        &json!({"op":"clipboard","view_id":view,"request_id":id,"response":response}),
    )?;
    Ok(())
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
fn finish_consent(
    socket: &Path,
    view: &str,
    consent: &Consent,
    response: ClipboardResponse,
    input: &std::sync::mpsc::SyncSender<(Value, bool)>,
) -> Result<()> {
    if !consent.paste {
        return reply(socket, view, &consent.id, response);
    }
    if response.result == ClipboardResult::Success {
        input.try_send((json!({"op":"paste_contents","view_id":view,"contents":response.contents,"location":consent.request.location,"allow_unsafe":false}),true)).context("input queue full")?;
    }
    Ok(())
}
struct PasteOffer {
    password: String,
    mimes: Vec<String>,
    primary: bool,
    started: Instant,
}
fn paste_offer_packet(offer: &mut Option<PasteOffer>, packet: &[u8]) -> Result<Option<Consent>> {
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
fn begin_clipboard(
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
fn clipboard_packet(consent: &mut Consent, packet: &[u8]) -> Result<Option<ClipboardResponse>> {
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
fn key_event(key: termwiz::input::KeyEvent) -> KeyEvent {
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
fn kitty_key(payload: &[u8]) -> Result<KeyEvent> {
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
struct KeyboardPolicy {
    read_only: bool,
    suppressed: HashSet<String>,
}

fn dispatch_key(
    event: KeyEvent,
    out: &mut impl Write,
    socket: &Path,
    view: &str,
    consent: &mut Option<Consent>,
    input: &std::sync::mpsc::SyncSender<(Value, bool)>,
    policy: &mut KeyboardPolicy,
) -> Result<bool> {
    match event.action {
        KeyAction::Release if policy.suppressed.remove(&event.key) => return Ok(false),
        KeyAction::Repeat if policy.suppressed.contains(&event.key) => return Ok(false),
        KeyAction::Press => {
            policy.suppressed.remove(&event.key);
        }
        _ => {}
    }
    if event.key == "BracketRight" && event.modifiers & 2 != 0 && event.action != KeyAction::Release
    {
        return Ok(true);
    }
    if let Some(c) = consent.as_mut().filter(|c| !c.sent) {
        if event.action == KeyAction::Release {
            return Ok(false);
        }
        // Keep releases/repeats local even when y/n immediately completes the
        // request and removes its consent state.
        policy.suppressed.insert(event.key.clone());
        if event.text.eq_ignore_ascii_case("y") && event.modifiers & 6 == 0 {
            c.allowed = true;
            if let Some(response) = begin_clipboard(out, c)? {
                finish_consent(socket, view, c, response, input)?;
                *consent = None;
            }
        } else if event.text.eq_ignore_ascii_case("n") && event.modifiers & 6 == 0 {
            finish_consent(socket, view, c, ClipboardResponse::denied(), input)?;
            *consent = None;
        }
        return Ok(false);
    }
    if !policy.read_only {
        input
            .try_send((json!({"op":"key","view_id":view,"event":event}), true))
            .context("input queue full")?;
    }
    Ok(false)
}
#[derive(Default)]
struct ColorProbe {
    truecolor: bool,
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    cursor: Option<[u8; 3]>,
    palette: HashMap<usize, [u8; 3]>,
    revision: u64,
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
    fn feed(&mut self, bytes: &[u8]) {
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

type InputPump = (
    std::sync::mpsc::SyncSender<(Value, bool)>,
    std::sync::mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
);
fn input_pump(socket: &Path) -> InputPump {
    let (tx, rx) = std::sync::mpsc::sync_channel::<(Value, bool)>(2048);
    let (errors, error_rx) = std::sync::mpsc::sync_channel(1);
    let socket = socket.to_owned();
    let worker = std::thread::spawn(move || {
        while let Ok((request, required)) = rx.recv() {
            if let Err(error) = transport::checked(&socket, &request)
                && required
            {
                let _ = errors.try_send(format!("{error:#}"));
                break;
            }
        }
    });
    (tx, error_rx, worker)
}
/// Exercise the physical keyboard decoder and FIFO IPC path. Acceptance means
/// queued once, not completion or model processing.
pub fn keyboard(socket: &Path, view: &str, bytes: &[u8]) -> Result<()> {
    let (input, errors, worker) = input_pump(socket);
    let mut parser = InputParser::new();
    let mut demux = ReplyDemux::default();
    let mut output = Vec::new();
    let mut consent = None;
    let mut policy = KeyboardPolicy::default();
    let result = (|| -> Result<()> {
        for event in decode_input(&mut parser, demux.feed(bytes), false) {
            match event {
                ClientEvent::Legacy(InputEvent::Key(key)) => {
                    if dispatch_key(
                        key_event(key),
                        &mut output,
                        socket,
                        view,
                        &mut consent,
                        &input,
                        &mut policy,
                    )? {
                        bail!("detach shortcut is not a keyboard injection");
                    }
                }
                ClientEvent::Legacy(InputEvent::Paste(text)) => input
                    .try_send((
                        json!({"op":"paste","view_id":view,"data":text.into_bytes()}),
                        true,
                    ))
                    .context("input queue full")?,
                ClientEvent::Reply(reply) if reply.starts_with(b"key;") => {
                    if dispatch_key(
                        kitty_key(&reply[4..])?,
                        &mut output,
                        socket,
                        view,
                        &mut consent,
                        &input,
                        &mut policy,
                    )? {
                        bail!("detach shortcut is not a keyboard injection");
                    }
                }
                _ => bail!("keyboard diagnostic accepts only keyboard and bracketed paste input"),
            }
        }
        Ok(())
    })();
    drop(input);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("input sender failed"))?;
    result?;
    if let Ok(error) = errors.try_recv() {
        bail!("{error}");
    }
    Ok(())
}
fn mouse_position(col: u16, row: u16, viewport: Size, canonical: Size) -> Option<(f32, f32)> {
    (col < viewport.cols && row < viewport.rows && col < canonical.cols && row < canonical.rows)
        .then_some((
            col as f32 * canonical.cell_width as f32,
            row as f32 * canonical.cell_height as f32,
        ))
}
fn user_input(event: &ClientEvent) -> bool {
    matches!(event, ClientEvent::Legacy(_))
        || matches!(event,ClientEvent::Reply(reply) if reply.starts_with(b"key;")||reply.starts_with(b"focus;"))
}
fn next_input(
    incoming: &mut VecDeque<ClientEvent>,
    deferred: &mut VecDeque<ClientEvent>,
    paste_pending: bool,
) -> Result<Option<ClientEvent>> {
    loop {
        if !paste_pending {
            while let Some(event) = deferred.pop_back() {
                incoming.push_front(event);
            }
        }
        let Some(event) = incoming.pop_front() else {
            return Ok(None);
        };
        if paste_pending && user_input(&event) {
            if deferred.len() >= 64 {
                bail!("enhanced paste input barrier full; following input was not dispatched");
            }
            deferred.push_back(event);
        } else {
            return Ok(Some(event));
        }
    }
}
pub fn tui(socket: &Path, read_only: bool, clipboard_policy: &str) -> Result<()> {
    if !matches!(clipboard_policy, "allow" | "deny" | "ask") {
        bail!("clipboard must be allow, deny, or ask");
    }
    let mut viewport = dimensions()?;
    let spec = |size: Size| json!({"size":size,"interactive":!read_only,"visible":true,"input":!read_only,"clipboard":!read_only && clipboard_policy != "deny"});
    let mut attach = spec(viewport);
    attach["op"] = json!("attach");
    let attached = transport::checked(socket, &attach)?;
    let view = attached["view_id"]
        .as_str()
        .context("missing view id")?
        .to_owned();
    let (input_tx, error_rx, input_thread) = input_pump(socket);
    let result = (|| -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        let mut guard = TerminalGuard {
            graphics: GraphicsState::new(
                ((uuid::Uuid::new_v4().as_u128() as u32) & 0x7ffff800) | 0x80000000,
            ),
            kitty_keyboard: false,
            clipboard_mode: None,
        };
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::with_capacity(256 * 1024, stdout.lock());
        write!(
            out,
            "\x1b[?1049h\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1002h\x1b[?1006h"
        )?;
        out.flush()?;
        write!(
            out,
            "\x1bP+q5463\x1b\\\x1bP+q524742\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\"
        )?;
        write!(out, "\x1b[?u\x1b[?5522$p")?;
        for index in 0..256 {
            write!(out, "\x1b]4;{index};?\x1b\\")?;
        }
        out.flush()?;
        let mut colors = ColorProbe::default();
        let mut seeded = 0;
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let mut revision = 0;
        let mut consent: Option<Consent> = None;
        let mut last_input = Instant::now();
        let mut paste_offer: Option<PasteOffer> = None;
        let mut last_mouse_button = MouseButton::None;
        let mut rendered_consent: Option<(String, bool, bool, Option<bool>)> = None;
        let mut enhanced_supported = None;
        let mut keyboard_policy = KeyboardPolicy {
            read_only,
            ..KeyboardPolicy::default()
        };
        let mut scene_revision = 0;
        let mut scene: Option<Frame> = None;
        let mut received_images = HashMap::<ImageKey, Arc<Image>>::new();
        let mut known_resources = Vec::<ImageKey>::new();
        let mut deferred = VecDeque::<ClientEvent>::new();
        loop {
            if let Ok(error) = error_rx.try_recv() {
                bail!("input request failed: {error}");
            }
            let size = dimensions()?;
            if size != viewport {
                viewport = size;
                let mut update = spec(size);
                update["op"] = json!("update");
                update["view_id"] = json!(view);
                input_tx
                    .try_send((update, true))
                    .context("input queue full")?;
                revision = 0;
            }
            let snapshot = transport::checked(
                socket,
                &json!({"op":"poll","view_id":view,"frame_after":scene_revision,"resources":known_resources}),
            )?;
            if !snapshot["frame"].is_null() {
                let mut frame: Frame = serde_json::from_value(snapshot["frame"].clone())?;
                for image in frame.images.drain(..) {
                    received_images.insert(image.key(), image);
                }
                known_resources = serde_json::from_value(snapshot["image_refs"].clone())?;
                let retained = known_resources.iter().copied().collect::<HashSet<_>>();
                received_images.retain(|key, _| retained.contains(key));
                frame.images = known_resources
                    .iter()
                    .map(|key| {
                        received_images
                            .get(key)
                            .cloned()
                            .context("missing normalized image generation")
                    })
                    .collect::<Result<_>>()?;
                scene_revision = snapshot["revision"].as_u64().unwrap_or(0);
                scene = Some(frame);
            }
            let canonical = scene
                .as_ref()
                .context("missing initial normalized scene")?
                .size;
            if colors.revision != seeded
                && snapshot["size_owner"]["view_id"] == view
                && colors.foreground.is_some()
                && colors.background.is_some()
            {
                let palette = if colors.palette.len() == 256 {
                    Some((0..256).map(|i| colors.palette[&i]).collect::<Vec<_>>())
                } else {
                    None
                };
                input_tx.try_send((json!({"op":"colors","view_id":view,"foreground":colors.foreground,"background":colors.background,"cursor":colors.cursor,"palette":palette}),false)).context("input queue full")?;
                seeded = colors.revision;
            }
            if let Some(pending) = snapshot["clipboard"].as_object() {
                let id = pending["request_id"].as_str().context("clipboard id")?;
                if consent.as_ref().is_some_and(|c| c.paste) {
                    let _ = reply(socket, &view, id, ClipboardResponse::denied());
                } else if consent.as_ref().is_none_or(|c| c.id != id) {
                    consent = Some(Consent {
                        id: id.into(),
                        request: serde_json::from_value(pending["request"].clone())?,
                        started: Instant::now(),
                        sent: false,
                        allowed: clipboard_policy == "allow",
                        contents: Vec::new(),
                        available: Vec::new(),
                        enhanced_supported,
                        paste: false,
                        password: None,
                    });
                    revision = 0;
                }
            } else if consent.as_ref().is_some_and(|c| !c.paste) {
                consent = None;
                revision = 0;
            }
            if let Some(c) = consent.as_mut() {
                c.enhanced_supported = enhanced_supported;
                if clipboard_policy == "allow" {
                    c.allowed = true;
                }
                if c.allowed
                    && !c.sent
                    && let Some(response) = begin_clipboard(&mut out, c)?
                {
                    finish_consent(socket, &view, c, response, &input_tx)?;
                    consent = None;
                }
            }
            if paste_offer
                .as_ref()
                .is_some_and(|offer| offer.started.elapsed() > Duration::from_secs(4))
            {
                bail!("enhanced paste listing timed out; following input was not dispatched");
            }
            if let Some(c) = consent
                .as_ref()
                .filter(|c| c.started.elapsed() > Duration::from_secs(4))
            {
                if c.paste && !deferred.is_empty() {
                    bail!("enhanced paste timed out; following input was not dispatched");
                }
                finish_consent(socket, &view, c, ClipboardResponse::denied(), &input_tx)?;
                consent = None;
            }
            let current = snapshot["revision"].as_u64().unwrap_or(0);
            let local_consent = consent
                .as_ref()
                .map(|c| (c.id.as_str(), c.sent, c.allowed, c.enhanced_supported));
            if rendered_consent
                .as_ref()
                .map(|(id, sent, allowed, supported)| (id.as_str(), *sent, *allowed, *supported))
                != local_consent
            {
                revision = 0;
                rendered_consent = local_consent.map(|(id, sent, allowed, supported)| {
                    (id.to_owned(), sent, allowed, supported)
                });
            }
            if current != revision || revision == 0 {
                if let Some(frame) = scene.as_ref() {
                    let chrome = if consent.as_ref().is_some_and(|c| !c.sent) {
                        if consent.as_ref().is_some_and(|c| c.allowed) {
                            "Clipboard allowed: waiting for terminal capability (pane-local, 5s timeout)".to_owned()
                        } else {
                            "Clipboard request: y allow / n deny (pane-local, 5s timeout)"
                                .to_owned()
                        }
                    } else {
                        format!(
                            "bkpr {} {} | Ctrl-] detach | clipboard {} | {} | {}",
                            &view[..8],
                            if read_only {
                                "read-only"
                            } else {
                                "interactive"
                            },
                            clipboard_policy,
                            snapshot["lifecycle"].as_str().unwrap_or("unknown"),
                            if colors.truecolor {
                                "RGB confirmed"
                            } else {
                                "16-color fallback"
                            }
                        )
                    };
                    draw(
                        &mut out,
                        frame,
                        viewport,
                        &chrome,
                        &colors,
                        &mut guard.graphics,
                    )?;
                }
                revision = current;
            }
            if matches!(
                snapshot["lifecycle"].as_str(),
                Some("exited" | "terminated" | "fault")
            ) {
                break;
            }
            let mut fd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let available = unsafe { libc::poll(&mut fd, 1, 30) };
            let mut bytes = [0u8; 8192];
            let parts = if available > 0 {
                let n = std::io::stdin().read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                last_input = Instant::now();
                demux.feed(&bytes[..n])
            } else if last_input.elapsed() > Duration::from_millis(50) {
                vec![InputPart::Keys(demux.flush_escape())]
            } else {
                Vec::new()
            };
            let mut incoming = VecDeque::from(decode_input(&mut parser, parts, available > 0));
            loop {
                let paste_pending = paste_offer.is_some()
                    || consent
                        .as_ref()
                        .is_some_and(|c| c.paste && (c.sent || clipboard_policy == "allow"));
                let Some(event) = next_input(&mut incoming, &mut deferred, paste_pending)? else {
                    break;
                };
                match event {
                    ClientEvent::Reply(response) => {
                        if response.starts_with(b"G") {
                            if let Some(status) = response.splitn(2, |byte| *byte == b';').nth(1)
                                && status != b"OK"
                            {
                                let detail = String::from_utf8_lossy(status)
                                    .chars()
                                    .filter(|c| !c.is_control())
                                    .take(256)
                                    .collect::<String>();
                                bail!("physical Kitty graphics projection rejected: {detail}");
                            }
                            continue;
                        }
                        if response.starts_with(b"input-error;") {
                            bail!(
                                "bounded physical input framing rejected: {}",
                                String::from_utf8_lossy(&response)
                            );
                        }
                        if let Some(response) = response.strip_prefix(b"clipboard-cap;") {
                            let Some(status) = std::str::from_utf8(response)?
                                .strip_prefix("?5522;")
                                .and_then(|s| s.strip_suffix('$'))
                                .and_then(|s| s.parse::<u8>().ok())
                                .filter(|status| *status <= 4)
                            else {
                                continue;
                            };
                            let supported = (1..=4).contains(&status);
                            enhanced_supported = Some(supported);
                            if let Some(c) = consent.as_mut() {
                                c.enhanced_supported = enhanced_supported;
                            }
                            if supported && guard.clipboard_mode.is_none() {
                                guard.clipboard_mode = Some(matches!(status, 1 | 3));
                                if !read_only && clipboard_policy != "deny" {
                                    write!(out, "\x1b[?5522h")?;
                                    out.flush()?;
                                }
                            }
                            continue;
                        }
                        if response.starts_with(b"keyboard;") {
                            if !guard.kitty_keyboard {
                                write!(out, "\x1b[>31u")?;
                                out.flush()?;
                                guard.kitty_keyboard = true;
                            }
                            continue;
                        }
                        if let Some(payload) = response.strip_prefix(b"key;") {
                            if dispatch_key(
                                kitty_key(payload)?,
                                &mut out,
                                socket,
                                &view,
                                &mut consent,
                                &input_tx,
                                &mut keyboard_policy,
                            )? {
                                return Ok(());
                            }
                            continue;
                        }
                        if response.starts_with(b"focus;") {
                            if !read_only {
                                input_tx.try_send((json!({"op":"focus","view_id":view,"focused":response==b"focus;in"}),true)).context("input queue full")?;
                            }
                            if !read_only && response == b"focus;in" {
                                let mut update = spec(viewport);
                                update["op"] = json!("update");
                                update["view_id"] = json!(view);
                                input_tx
                                    .try_send((update, true))
                                    .context("input queue full")?;
                            }
                            continue;
                        }
                        let color_revision = colors.revision;
                        colors.feed(&response);
                        if color_revision != colors.revision {
                            revision = 0;
                        }
                        if consent.is_none()
                            && !read_only
                            && clipboard_policy != "deny"
                            && let Some(c) = paste_offer_packet(&mut paste_offer, &response)?
                        {
                            consent = Some(c);
                            revision = 0;
                        }
                        if let Some(c) = consent.as_mut().filter(|c| c.sent) {
                            match clipboard_packet(c, &response) {
                                Ok(Some(answer)) => {
                                    if c.paste
                                        && answer.result != ClipboardResult::Success
                                        && !deferred.is_empty()
                                    {
                                        bail!(
                                            "enhanced paste failed; following input was not dispatched"
                                        );
                                    }
                                    finish_consent(socket, &view, c, answer, &input_tx)?;
                                    consent = None;
                                }
                                Ok(None) => {}
                                Err(_) => {
                                    if c.paste && !deferred.is_empty() {
                                        bail!(
                                            "malformed enhanced paste; following input was not dispatched"
                                        );
                                    }
                                    finish_consent(
                                        socket,
                                        &view,
                                        c,
                                        ClipboardResponse::denied(),
                                        &input_tx,
                                    )?;
                                    consent = None;
                                }
                            }
                        }
                    }
                    ClientEvent::Legacy(event) => match event {
                        InputEvent::Key(key) => {
                            if dispatch_key(
                                key_event(key),
                                &mut out,
                                socket,
                                &view,
                                &mut consent,
                                &input_tx,
                                &mut keyboard_policy,
                            )? {
                                return Ok(());
                            }
                        }
                        InputEvent::Paste(text) => {
                            if !read_only {
                                input_tx.try_send((json!({"op":"paste","view_id":view,"data":text.into_bytes()}),true)).context("input queue full")?;
                            }
                        }
                        InputEvent::Mouse(mouse) => {
                            if !read_only
                                && let Some((x, y)) =
                                    mouse_position(mouse.x, mouse.y, viewport, canonical)
                            {
                                use termwiz::input::MouseButtons;
                                let button = if mouse
                                    .mouse_buttons
                                    .contains(MouseButtons::VERT_WHEEL)
                                {
                                    if mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
                                        MouseButton::WheelUp
                                    } else {
                                        MouseButton::WheelDown
                                    }
                                } else if mouse.mouse_buttons.contains(MouseButtons::LEFT) {
                                    MouseButton::Left
                                } else if mouse.mouse_buttons.contains(MouseButtons::MIDDLE) {
                                    MouseButton::Middle
                                } else if mouse.mouse_buttons.contains(MouseButtons::RIGHT) {
                                    MouseButton::Right
                                } else {
                                    MouseButton::None
                                };
                                let wheel = matches!(
                                    button,
                                    MouseButton::WheelUp
                                        | MouseButton::WheelDown
                                        | MouseButton::WheelLeft
                                        | MouseButton::WheelRight
                                );
                                let action = if wheel {
                                    MouseAction::Press
                                } else if button == MouseButton::None {
                                    if last_mouse_button == MouseButton::None {
                                        MouseAction::Motion
                                    } else {
                                        MouseAction::Release
                                    }
                                } else if button == last_mouse_button {
                                    MouseAction::Motion
                                } else {
                                    MouseAction::Press
                                };
                                let event_button = if action == MouseAction::Release {
                                    last_mouse_button
                                } else {
                                    button
                                };
                                if !wheel {
                                    last_mouse_button = button;
                                }
                                let mut modifiers = 0;
                                for (flag, bit) in [
                                    (Modifiers::SHIFT, 1),
                                    (Modifiers::CTRL, 2),
                                    (Modifiers::ALT, 4),
                                    (Modifiers::SUPER, 8),
                                ] {
                                    if mouse.modifiers.contains(flag) {
                                        modifiers |= bit;
                                    }
                                }
                                let event = MouseEvent {
                                    x,
                                    y,
                                    modifiers,
                                    button: event_button,
                                    action,
                                };
                                input_tx
                                    .try_send((
                                        json!({"op":"mouse","view_id":view,"event":event}),
                                        true,
                                    ))
                                    .context("input queue full")?;
                            }
                        }
                        _ => {}
                    },
                }
            }
        }
        Ok(())
    })();
    drop(input_tx);
    let drained = input_thread
        .join()
        .map_err(|_| anyhow::anyhow!("input sender failed"))
        .and_then(|_| {
            if let Ok(error) = error_rx.try_recv() {
                Err(anyhow::anyhow!(error))
            } else {
                Ok(())
            }
        });
    let detached = transport::checked(socket, &json!({"op":"detach","view_id":view}));
    result.and(drained).and(detached.map(|_| ()))
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
}

#[cfg(test)]
#[path = "render_regressions.rs"]
mod regressions;

#[cfg(test)]
#[path = "render_geometry_regressions.rs"]
mod geometry_regressions;
