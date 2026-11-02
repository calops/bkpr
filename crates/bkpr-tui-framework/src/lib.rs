//! Unified scene projection, damage rendering, host capability and input framing.
//! No application terminal parser or second physical output backend lives here.
mod render;
pub use render::*;
pub mod graphics;
mod surface;
pub mod window;
pub use surface::{Divider, UiSurface};
mod text_input;
pub use text_input::{EditAction, TextInput};
