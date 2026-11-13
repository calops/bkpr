//! Unified scene projection, damage rendering, host capability and input framing.
//! No application terminal parser or second physical output backend lives here.
#![deny(missing_docs)]

mod render;
pub use render::*;
pub mod animation;
pub mod chord;
pub mod decoration;
pub mod graphics;
pub mod joined;
mod joined_contour;
mod navigation;
pub use navigation::{Navigation, NavigationRow};
pub mod overlay;
pub mod search_list;
mod spinner;
pub use spinner::Spinner;
mod surface;
pub mod toast;
pub mod window;
pub use surface::{BackgroundSpan, ForegroundSpan, RoleSpan, UiSurface};
mod theme;
pub use theme::{Role, Theme, ThemeStyles};
mod text_input;
pub use search_list::{SearchList, SearchListIntent};
pub use text_input::{EditAction, TextInput};

/// Measure sanitized UI text in terminal cells without allocating.
///
/// Each extended grapheme cluster is measured as a whole using `unicode-width`,
/// so joined emoji, presentation selectors and combining marks share the cell
/// geometry used by [`text`], [`window::fit`] and [`TextInput::visible`].
/// This does not sanitize text or reinterpret canonical native pane cells.
pub fn text_width(value: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    value.graphemes(true).map(UnicodeWidthStr::width).sum()
}
