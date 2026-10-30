use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub size: Size,
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: Vec<[u8; 3]>,
    pub images: Vec<std::sync::Arc<Image>>,
    pub placements: Vec<Placement>,
    pub alternate_screen: bool,
    /// Total retained rows, including the active visible grid.
    pub scrollback_total: u64,
    /// Distance above live output in rows. Zero is live; larger values are
    /// older history. This is not the engine's absolute-from-oldest row index.
    pub scrollback_offset: u64,
}
/// Receipt key for an immutable native image generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageKey {
    pub id: u32,
    pub generation: u64,
}
pub struct FrameWire<'a> {
    frame: &'a Frame,
    known: std::collections::HashSet<ImageKey>,
}
impl Frame {
    pub fn wire(&self, known: &[ImageKey]) -> FrameWire<'_> {
        FrameWire {
            frame: self,
            known: known.iter().copied().collect(),
        }
    }
    pub fn image_keys(&self) -> Vec<ImageKey> {
        self.images.iter().map(|image| image.key()).collect()
    }
}
impl Image {
    pub const fn key(&self) -> ImageKey {
        ImageKey {
            id: self.id,
            generation: self.generation,
        }
    }
}
impl Serialize for FrameWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeSeq, SerializeStruct};
        struct Images<'a> {
            images: &'a [std::sync::Arc<Image>],
            known: &'a std::collections::HashSet<ImageKey>,
        }
        impl Serialize for Images<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(None)?;
                for image in self.images {
                    if !self.known.contains(&image.key()) {
                        sequence.serialize_element(image)?;
                    }
                }
                sequence.end()
            }
        }
        let frame = self.frame;
        let mut output = serializer.serialize_struct("Frame", 11)?;
        output.serialize_field("size", &frame.size)?;
        output.serialize_field("rows", &frame.rows)?;
        output.serialize_field("cursor", &frame.cursor)?;
        output.serialize_field("foreground", &frame.foreground)?;
        output.serialize_field("background", &frame.background)?;
        output.serialize_field("palette", &frame.palette)?;
        output.serialize_field(
            "images",
            &Images {
                images: &frame.images,
                known: &self.known,
            },
        )?;
        output.serialize_field("placements", &frame.placements)?;
        output.serialize_field("alternate_screen", &frame.alternate_screen)?;
        output.serialize_field("scrollback_total", &frame.scrollback_total)?;
        output.serialize_field("scrollback_offset", &frame.scrollback_offset)?;
        output.end()
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Renderable grapheme text. Engine-recognized Kitty graphics placeholders
    /// are empty: their resolved graphics are represented by frame placements.
    pub text: String,
    pub width: u8,
    pub style: Style,
    pub hyperlink: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub underline_color: [u8; 3],
    pub foreground_source: ColorSource,
    pub background_source: ColorSource,
    pub underline_source: ColorSource,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorSource {
    Default,
    Palette(u8),
    Rgb([u8; 3]),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub blinking: bool,
    pub shape: CursorShape,
    pub color: Option<[u8; 3]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Bar,
    Block,
    Underline,
    Hollow,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub id: u32,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    #[serde(with = "pixel_base64")]
    pub pixels: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Rgb,
    Rgba,
    GrayAlpha,
    Gray,
}
impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgba => 4,
            Self::GrayAlpha => 2,
            Self::Gray => 1,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub image_id: u32,
    pub generation: u64,
    pub placement_id: u32,
    pub col: i32,
    pub row: i32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub width: u32,
    pub height: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub layer: ImageLayer,
    pub z: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageLayer {
    BelowBackground,
    BelowText,
    AboveText,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    PtyWrite(Vec<u8>),
    ProgramStatus {
        state: String,
        kind: Option<String>,
        app: Option<String>,
        message: Option<String>,
        id: String,
        title: String,
        progress: Option<u8>,
    },
    Title(String),
    Cwd(String),
    Bell,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatus {
    pub state: String,
    pub kind: Option<String>,
    pub app: Option<String>,
    pub message: Option<String>,
    pub id: String,
    pub title: String,
    pub progress: Option<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardContent {
    pub mime: String,
    pub data: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardOperation {
    Read { mimes: Vec<String>, list: bool },
    Write { contents: Vec<ClipboardContent> },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardLocation {
    Standard,
    Selection,
    Primary,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRequest {
    pub operation: ClipboardOperation,
    pub location: ClipboardLocation,
    pub name: String,
    pub granted: bool,
    pub can_remember: bool,
    /// Correlation from a validated native one-time paste password, never from
    /// a program-supplied raw ID. The policy handler must revalidate its origin.
    pub paste_offer_id: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardResult {
    Success,
    Denied,
    Unsupported,
    Busy,
    InvalidData,
    IoError,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardResponse {
    pub result: ClipboardResult,
    pub contents: Vec<ClipboardContent>,
    pub available: Vec<String>,
    pub remember: bool,
}
impl ClipboardResponse {
    pub fn denied() -> Self {
        Self {
            result: ClipboardResult::Denied,
            contents: Vec::new(),
            available: Vec::new(),
            remember: false,
        }
    }
}
/// Physical key uses W3C code spelling: KeyA, Digit0, Enter, ArrowUp, F1, etc.
/// Modifiers: shift=1, control=2, alt=4, super=8, caps-lock=16, num-lock=32.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: String,
    pub modifiers: u16,
    pub text: String,
    pub action: KeyAction,
    pub unshifted_codepoint: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyAction {
    Press,
    Release,
    Repeat,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MouseEvent {
    pub x: f32,
    pub y: f32,
    pub modifiers: u16,
    pub button: MouseButton,
    pub action: MouseAction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    Back,
    Forward,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

mod pixel_base64 {
    use base64::{Engine, display::Base64Display, engine::general_purpose::STANDARD};
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    use std::fmt;
    const MAX_BYTES: usize = super::super::IMAGE_MAX_BYTES;
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Base64Display::new(bytes, &STANDARD))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        struct Pixels;
        impl Visitor<'_> for Pixels {
            type Value = Vec<u8>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bounded base64 image pixels")
            }
            fn visit_str<E: Error>(self, text: &str) -> Result<Self::Value, E> {
                if text.len() > MAX_BYTES.div_ceil(3) * 4 {
                    return Err(E::custom("encoded image exceeds resource limit"));
                }
                let bytes = STANDARD.decode(text).map_err(E::custom)?;
                if bytes.len() > MAX_BYTES {
                    return Err(E::custom("decoded image exceeds resource limit"));
                }
                Ok(bytes)
            }
        }
        deserializer.deserialize_str(Pixels)
    }
}
