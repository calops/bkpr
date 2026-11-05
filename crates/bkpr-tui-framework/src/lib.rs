//! Unified scene projection, damage rendering, host capability and input framing.
//! No application terminal parser or second physical output backend lives here.
mod render;
pub use render::*;
pub mod decoration;
pub mod graphics;
pub mod joined;
mod joined_contour;
pub mod overlay;
mod surface;
pub mod toast;
pub mod window;
pub use surface::{BackgroundSpan, ForegroundSpan, RoleSpan, UiSurface};
mod theme;
pub use theme::{Role, Theme, ThemeStyles};
mod text_input;
pub use text_input::{EditAction, TextInput};
