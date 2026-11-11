use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Terminal grid dimensions and the pixel size of each cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Size {
    /// Number of terminal columns.
    #[schemars(range(min = 1, max = 500))]
    pub cols: u16,
    /// Number of terminal rows.
    #[schemars(range(min = 1, max = 300))]
    pub rows: u16,
    /// Cell width in pixels.
    #[schemars(range(min = 1, max = 256))]
    pub cell_width: u32,
    /// Cell height in pixels.
    #[schemars(range(min = 1, max = 256))]
    pub cell_height: u32,
}
/// Canonical visible terminal scene, including resources and viewport state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// Geometry used to capture this scene.
    pub size: Size,
    /// Visible rows in top-to-bottom order.
    pub rows: Vec<Row>,
    /// Application cursor projected into this viewport.
    pub cursor: Cursor,
    /// Default foreground RGB color.
    pub foreground: [u8; 3],
    /// Default background RGB color.
    pub background: [u8; 3],
    /// Indexed terminal palette in RGB order.
    pub palette: Vec<[u8; 3]>,
    /// Immutable image generations referenced by the scene.
    pub images: Vec<std::sync::Arc<Image>>,
    /// Image draws positioned relative to this viewport.
    pub placements: Vec<Placement>,
    /// Whether the alternate terminal screen is active.
    pub alternate_screen: bool,
    /// Whether the application has enabled native mouse tracking.
    #[serde(default)]
    pub mouse_reporting: bool,
    /// Total retained rows, including the active visible grid.
    pub scrollback_total: u64,
    /// Distance above live output in rows. Zero is live; larger values are
    /// older history. This is not the engine's absolute-from-oldest row index.
    pub scrollback_offset: u64,
}
/// Receipt key for an immutable native image generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageKey {
    /// Native image identifier, scoped to its pane.
    pub id: u32,
    /// Pixel generation distinguishing replacements of the same image ID.
    pub generation: u64,
}
impl Frame {
    /// Returns resource receipt keys in the order of `images`.
    pub fn image_keys(&self) -> Vec<ImageKey> {
        self.images.iter().map(|image| image.key()).collect()
    }
}
impl Image {
    /// Returns the receipt key for these immutable pixels.
    pub const fn key(&self) -> ImageKey {
        ImageKey {
            id: self.id,
            generation: self.generation,
        }
    }
}
/// One visible terminal row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// Cells in column order, including wide-glyph spacer cells.
    pub cells: Vec<Cell>,
}
/// Text, display width and attributes for one terminal grid position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Renderable grapheme text. Engine-recognized Kitty graphics placeholders
    /// are empty: their resolved graphics are represented by frame placements.
    pub text: CompactString,
    /// Column width: 0 for a wide-glyph spacer, 1 normally, or 2 for a wide glyph.
    pub width: u8,
    /// Text and background attributes for this grid position.
    pub style: Style,
    /// OSC 8 hyperlink URI, when present.
    pub hyperlink: Option<String>,
}
/// Resolved RGB colors and original terminal text attributes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Style {
    /// Resolved foreground RGB color.
    pub foreground: [u8; 3],
    /// Resolved background RGB color.
    pub background: [u8; 3],
    /// Resolved underline RGB color; defaults to the foreground.
    pub underline_color: [u8; 3],
    /// Original foreground color selection.
    pub foreground_source: ColorSource,
    /// Original background color selection.
    pub background_source: ColorSource,
    /// Original underline color selection.
    pub underline_source: ColorSource,
    /// Bold intensity is enabled.
    pub bold: bool,
    /// Italic text is enabled.
    pub italic: bool,
    /// Faint intensity is enabled.
    pub faint: bool,
    /// Text blinking is enabled.
    pub blink: bool,
    /// Foreground and background inversion is enabled.
    pub inverse: bool,
    /// Text is concealed without removing its occupied cells.
    pub invisible: bool,
    /// Strikethrough decoration is enabled.
    pub strikethrough: bool,
    /// Overline decoration is enabled.
    pub overline: bool,
    /// Underline style: 0=none, 1=single, 2=double, 3=curly, 4=dotted, 5=dashed.
    pub underline: u8,
}
/// Original terminal color selection, retained alongside resolved RGB values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorSource {
    /// Terminal default color for the corresponding role.
    Default,
    /// Indexed palette color.
    Palette(
        /// Zero-based terminal palette index.
        u8,
    ),
    /// Explicit RGB color.
    Rgb(
        /// Red, green and blue channels.
        [u8; 3],
    ),
}
/// Application cursor state within the visible terminal grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    /// Zero-based viewport column; meaningful when visible.
    pub x: u16,
    /// Zero-based viewport row; meaningful when visible.
    pub y: u16,
    /// Whether the cursor is visible in this viewport.
    pub visible: bool,
    /// Whether the application requests cursor blinking.
    pub blinking: bool,
    /// Requested cursor shape.
    pub shape: CursorShape,
    /// Explicit cursor RGB color, or the terminal default when absent.
    pub color: Option<[u8; 3]>,
}
/// Requested visual form of the terminal cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    /// Vertical insertion bar.
    Bar,
    /// Filled cell block.
    Block,
    /// Line along the bottom of a cell.
    Underline,
    /// Outlined cell block.
    Hollow,
}
/// Immutable decoded pixel resource identified by a native generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Image {
    /// Native image identifier, scoped to its pane.
    pub id: u32,
    /// Generation distinguishing replacement pixels for the same ID.
    pub generation: u64,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Channel layout of each packed pixel.
    pub format: PixelFormat,
    /// Packed row-major pixel bytes; JSON represents them as bounded standard base64.
    #[serde(with = "pixel_base64")]
    pub pixels: Vec<u8>,
}
/// Packed channel layout with one byte per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// Red, green and blue channels.
    Rgb,
    /// Red, green, blue and alpha channels.
    Rgba,
    /// Grayscale intensity and alpha channels.
    GrayAlpha,
    /// Grayscale intensity channel.
    Gray,
}
impl PixelFormat {
    /// Returns the packed byte count for one pixel.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgba => 4,
            Self::GrayAlpha => 2,
            Self::Gray => 1,
        }
    }
}
/// Draw of an image source rectangle in canonical viewport coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    /// Identifier of the referenced image.
    pub image_id: u32,
    /// Generation of the referenced immutable image.
    pub generation: u64,
    /// Native placement identifier; zero when no explicit identifier is available.
    pub placement_id: u32,
    /// Zero-based viewport column of the anchor; may be negative when clipped.
    pub col: i32,
    /// Zero-based viewport row of the anchor; may be negative when clipped.
    pub row: i32,
    /// Horizontal pixel offset from the anchor cell.
    pub offset_x: u32,
    /// Vertical pixel offset from the anchor cell.
    pub offset_y: u32,
    /// Destination width in canonical pixels.
    pub width: u32,
    /// Destination height in canonical pixels.
    pub height: u32,
    /// Source rectangle left edge in image pixels.
    pub source_x: u32,
    /// Source rectangle top edge in image pixels.
    pub source_y: u32,
    /// Source rectangle width in image pixels.
    pub source_width: u32,
    /// Source rectangle height in image pixels.
    pub source_height: u32,
    /// Compositing position relative to terminal backgrounds and text.
    pub layer: ImageLayer,
    /// Native image z-order.
    pub z: i32,
}
/// Image compositing band relative to terminal cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageLayer {
    /// Draw beneath cell backgrounds.
    BelowBackground,
    /// Draw above backgrounds but beneath text.
    BelowText,
    /// Draw above cell text.
    AboveText,
}
/// Terminal-generated output or application metadata observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// Exact response bytes to send to the PTY, not keyboard input.
    PtyWrite(
        /// Exact terminal-generated response bytes.
        Vec<u8>,
    ),
    /// OSC 7501 semantic status update; independent of process lifetime and title.
    ProgramStatus {
        /// Reported state: idle, working, done, blocked, error, or clear.
        state: String,
        /// Optional blocking reason: permission, question, or auth.
        kind: Option<String>,
        /// Application label, when reported.
        app: Option<String>,
        /// Optional human-readable status detail.
        message: Option<String>,
        /// Status record ID; clear applies to this ID and its slash-delimited descendants.
        id: String,
        /// Human-readable status title.
        title: String,
        /// Completion percentage from 0 through 100, when reported.
        progress: Option<u8>,
    },
    /// Application-reported terminal title.
    Title(
        /// New application-reported title.
        String,
    ),
    /// Application-reported working directory.
    Cwd(
        /// New application-reported working directory.
        String,
    ),
    /// Application bell request.
    Bell,
}
/// Retained OSC 7501 semantic status record, not process or delivery readiness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatus {
    /// Reported state: idle, working, done, blocked, or error.
    pub state: String,
    /// Optional blocking reason: permission, question, or auth.
    pub kind: Option<String>,
    /// Application label, including inherited ancestor labels in native snapshots.
    pub app: Option<String>,
    /// Optional human-readable status detail.
    pub message: Option<String>,
    /// Record identity, with slash-delimited ancestry.
    pub id: String,
    /// Human-readable status title.
    pub title: String,
    /// Completion percentage from 0 through 100, when reported.
    pub progress: Option<u8>,
}
/// One clipboard representation with exact, unnormalized content bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardContent {
    /// UTF-8 MIME representation name.
    pub mime: String,
    /// Exact representation bytes; not necessarily UTF-8.
    pub data: Vec<u8>,
}
/// Application-requested clipboard transfer or format discovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardOperation {
    /// Read requested representations or discover available MIME names.
    Read {
        /// MIME representations requested by the application.
        mimes: Vec<String>,
        /// Whether the application requests a list of available MIME names.
        list: bool,
    },
    /// Replace clipboard contents with exact MIME representations.
    Write {
        /// Representations to write without text normalization.
        contents: Vec<ClipboardContent>,
    },
}
/// Native clipboard or selection namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardLocation {
    /// Standard system clipboard.
    Standard,
    /// Native selection namespace.
    Selection,
    /// Primary selection namespace.
    Primary,
}
/// Clipboard operation requiring a separate policy decision from keyboard input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRequest {
    /// Requested transfer direction, formats and contents.
    pub operation: ClipboardOperation,
    /// Requested clipboard or selection namespace.
    pub location: ClipboardLocation,
    /// Application-provided request name; not an authenticated identity.
    pub name: String,
    /// Whether the native terminal reports an existing grant.
    pub granted: bool,
    /// Whether the native request permits remembering this decision.
    pub can_remember: bool,
    /// Correlation from a validated native one-time paste password, never from
    /// a program-supplied raw ID. The policy handler must revalidate its origin.
    pub paste_offer_id: Option<u64>,
}
/// Outcome of a clipboard operation, not a PTY input receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardResult {
    /// The clipboard operation succeeded.
    Success,
    /// Policy denied the operation.
    Denied,
    /// The requested operation or representation is unsupported.
    Unsupported,
    /// The clipboard is temporarily busy.
    Busy,
    /// The transferred data is invalid.
    InvalidData,
    /// Clipboard I/O failed.
    IoError,
}
/// Policy result and exact clipboard data returned to the native terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardResponse {
    /// Outcome of the requested clipboard operation.
    pub result: ClipboardResult,
    /// Returned MIME representations for a read.
    pub contents: Vec<ClipboardContent>,
    /// Available MIME names for a format-list request.
    pub available: Vec<String>,
    /// Whether to remember the decision, subject to `ClipboardRequest::can_remember`.
    pub remember: bool,
}
impl ClipboardResponse {
    /// Returns denial with no contents, available formats or remembered grant.
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KeyEvent {
    /// Physical key code; unsupported spellings are rejected by the native encoder.
    pub key: String,
    /// Modifier bitmask described on `KeyEvent`; the native encoder also accepts side bits.
    pub modifiers: u16,
    /// Layout-produced text; the native encoder ignores control and reserved function-key text.
    pub text: String,
    /// Key lifecycle transition.
    pub action: KeyAction,
    /// Unicode scalar before Shift transformation, or zero when unavailable.
    pub unshifted_codepoint: u32,
}
/// Physical key lifecycle transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum KeyAction {
    /// Initial key depression.
    Press,
    /// Key release.
    Release,
    /// Repeated key depression while held.
    Repeat,
}
/// Pointer event in pane-local pixels, encoded using current terminal mouse modes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MouseEvent {
    /// Horizontal pixel coordinate; must be finite and within the pane.
    pub x: f32,
    /// Vertical pixel coordinate; must be finite and within the pane.
    pub y: f32,
    /// Native modifier bitmask, with the same layout as `KeyEvent::modifiers`.
    pub modifiers: u16,
    /// Button or wheel direction associated with the event.
    pub button: MouseButton,
    /// Pointer lifecycle transition.
    pub action: MouseAction,
}
/// Native mouse button or wheel direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    /// No associated button, such as unpressed pointer motion.
    None,
    /// Primary mouse button.
    Left,
    /// Middle mouse button.
    Middle,
    /// Secondary mouse button.
    Right,
    /// Vertical wheel toward the top.
    WheelUp,
    /// Vertical wheel toward the bottom.
    WheelDown,
    /// Horizontal wheel toward the left.
    WheelLeft,
    /// Horizontal wheel toward the right.
    WheelRight,
    /// Back navigation button.
    Back,
    /// Forward navigation button.
    Forward,
}
/// Pointer button or motion transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseAction {
    /// Button depression or wheel step.
    Press,
    /// Button release.
    Release,
    /// Pointer movement.
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
