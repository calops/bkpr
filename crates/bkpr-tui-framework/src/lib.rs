//! Unified scene projection, damage rendering, host capability and input framing.
//! No application terminal parser or second physical output backend lives here.
mod render;
pub use render::*;
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
