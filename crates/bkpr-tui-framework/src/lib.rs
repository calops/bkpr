//! Unified scene projection, damage rendering, host capability and input framing.
//! No application terminal parser or second physical output backend lives here.
mod render;
pub use render::*;
pub mod decoration;
pub mod graphics;
pub mod overlay;
mod surface;
pub mod window;
pub use surface::{RoleSpan, UiSurface};
mod theme;
pub use theme::{Role, Theme, ThemeStyles};
mod text_input;
pub use text_input::{EditAction, TextInput};
