//! Bkpr scene normalization and device authority policy over the upstream SDK.
//! Native objects, input encoders, callbacks and borrowed views belong to
//! libghostty-vt. A terminal is created and used on its pane worker only.
mod callbacks;
mod input;
mod offers;
mod render;
mod status;
use bkpr_protocol::*;
use libghostty_vt as sdk;
pub use offers::{PASTE_OFFER_MAX_BYTES, PASTE_OFFER_MAX_COUNT, PASTE_OFFER_TIMEOUT};
use std::{cell::RefCell, collections::HashMap, fmt, rc::Rc, sync::Arc};

pub const ENGINE_REVISION: &str = "93967b7f9478e94dcbb42125c8a90f34feb40d75";
pub const SDK_REVISION: &str = "575ed6b8ceb79ffaf247ecf7e34333876b0d5be4";
pub const ENGINE_PATCH_SHA256: &str =
    "196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c";
pub const SIXEL_SUPPORTED: bool = false;
pub const CLIPBOARD_MAX_BYTES: usize = 8 * 1024 * 1024;
const EFFECT_MAX_BYTES: usize = 16 * 1024 * 1024;
pub type Result<T, E = Error> = std::result::Result<T, E>;
#[derive(Debug)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<sdk::Error> for Error {
    fn from(value: sdk::Error) -> Self {
        Self(value.to_string())
    }
}
fn valid_size(size: Size) -> Result<()> {
    bkpr_protocol::valid_size(size).map_err(|error| Error(error.to_string()))
}
pub type ClipboardHandler = Box<dyn FnMut(ClipboardRequest) -> ClipboardResponse + Send>;
struct CallbackState {
    effects: Vec<Effect>,
    effect_bytes: usize,
    failed: bool,
    clipboard: Option<ClipboardHandler>,
    hold_since: Option<std::time::Instant>,
    held_frame: Option<Frame>,
    status: status::StatusRecords,
    size: Size,
    paste_offers: HashMap<u64, Arc<offers::PasteOffer>>,
}
impl CallbackState {
    fn push(&mut self, effect: Effect, bytes: usize) {
        if self.effect_bytes.saturating_add(bytes) > EFFECT_MAX_BYTES || self.effects.len() >= 65536
        {
            self.failed = true;
            return;
        }
        self.effect_bytes += bytes;
        self.status.update(&effect);
        self.effects.push(effect);
    }
}
/// Neither Send nor Sync, matching the upstream SDK's thread-affinity contract.
pub struct Terminal {
    native: sdk::Terminal<'static, 'static>,
    callbacks: Rc<RefCell<CallbackState>>,
    render: Rc<RefCell<render::Resources>>,
    key_encoder: sdk::key::Encoder<'static>,
    key_event: sdk::key::Event<'static>,
    mouse_encoder: sdk::mouse::Encoder<'static>,
    mouse_event: sdk::mouse::Event<'static>,
    size: Size,
    mouse_buttons: u16,
    last_offer_id: u64,
    history: HashMap<String, sdk::screen::TrackedGridRef>,
}
impl Terminal {
    pub fn new(size: Size) -> Result<Self> {
        valid_size(size)?;
        callbacks::initialize_sys()?;
        let mut native = sdk::Terminal::new(size.cols, size.rows)?;
        native.set_scrollback_max_bytes(Some(16 * 1024 * 1024))?;
        native.set_scrollback_max_lines(Some(10000))?;
        native.set_kitty_image_storage_limit(IMAGE_MAX_BYTES as u64)?;
        native.set_apc_max_bytes(Some(16 * 1024 * 1024))?;
        native.set_apc_max_bytes_kitty(Some(16 * 1024 * 1024))?;
        native.set_clipboard_write_max_bytes(Some(CLIPBOARD_MAX_BYTES))?;
        native.set_kitty_image_from_file_allowed(false)?;
        native.set_kitty_image_from_shared_mem_allowed(false)?;
        native.set_kitty_image_temp_file_dir(None)?;
        let callbacks = Rc::new(RefCell::new(CallbackState {
            effects: Vec::new(),
            effect_bytes: 0,
            failed: false,
            clipboard: None,
            hold_since: None,
            held_frame: None,
            status: status::StatusRecords::default(),
            size,
            paste_offers: HashMap::new(),
        }));
        let render = Rc::new(RefCell::new(render::Resources::new()?));
        callbacks::install(&mut native, &callbacks, &render)?;
        let mut this = Self {
            native,
            callbacks,
            render,
            key_encoder: sdk::key::Encoder::new()?,
            key_event: sdk::key::Event::new()?,
            mouse_encoder: sdk::mouse::Encoder::new()?,
            mouse_event: sdk::mouse::Event::new()?,
            size,
            mouse_buttons: 0,
            last_offer_id: 0,
            history: HashMap::new(),
        };
        this.resize(size)?;
        Ok(this)
    }
    /// Synchronous pane-worker policy. No handler means deny. Native session
    /// grants and offer correlations never replace current view authority.
    pub fn set_clipboard_handler(&mut self, handler: ClipboardHandler) {
        self.callbacks.borrow_mut().clipboard = Some(handler);
    }
    pub fn set_default_colors(
        &mut self,
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: Option<[u8; 3]>,
    ) -> Result<()> {
        self.native
            .set_default_fg_color(Some(render::native_rgb(foreground)))?;
        self.native
            .set_default_bg_color(Some(render::native_rgb(background)))?;
        self.native
            .set_default_cursor_color(cursor.map(render::native_rgb))?;
        Ok(())
    }
    pub fn set_palette(&mut self, palette: &[[u8; 3]; 256]) -> Result<()> {
        self.native
            .set_default_color_palette(Some(sdk::style::Palette(
                palette.map(render::native_rgb),
            )))?;
        Ok(())
    }
    pub fn resize(&mut self, size: Size) -> Result<()> {
        valid_size(size)?;
        self.callbacks.borrow_mut().size = size;
        if let Err(error) =
            self.native
                .resize(size.cols, size.rows, size.cell_width, size.cell_height)
        {
            self.callbacks.borrow_mut().size = self.size;
            return Err(error.into());
        }
        self.size = size;
        Ok(())
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Effect>> {
        if bytes.len() > 1024 * 1024 {
            return Err(Error("feed chunk exceeds 1 MiB".into()));
        }
        self.native.vt_write(bytes);
        // Alternate screens have no primary-screen history projection. Do not
        // resurrect a stale inspection position when the application returns.
        if self.native.active_screen()? == sdk::screen::Screen::Alternate {
            self.history.clear();
        }
        if self.native.vt_processing_error()? || self.callbacks.borrow().failed {
            return Err(Error(
                "terminal processing or callback resource limit failed".into(),
            ));
        }
        Ok(self.take_effects())
    }
    fn take_effects(&mut self) -> Vec<Effect> {
        let mut state = self.callbacks.borrow_mut();
        state.effect_bytes = 0;
        std::mem::take(&mut state.effects)
    }
    pub fn drain_effects(&mut self) -> Result<Vec<Effect>> {
        if self.callbacks.borrow().failed {
            return Err(Error("terminal callback resource limit failed".into()));
        }
        Ok(self.take_effects())
    }
    pub fn scroll(&mut self, delta: i64) -> Result<()> {
        let delta = isize::try_from(delta).map_err(|_| Error("scroll delta overflow".into()))?;
        self.native
            .scroll_viewport(sdk::terminal::ScrollViewport::Delta(delta));
        Ok(())
    }
    pub fn snapshot(&mut self) -> Result<Frame> {
        if self
            .callbacks
            .borrow()
            .hold_since
            .is_some_and(|since| since.elapsed() >= std::time::Duration::from_secs(1))
        {
            self.native
                .set_mode(sdk::terminal::Mode::SYNC_OUTPUT, false)?;
            let mut state = self.callbacks.borrow_mut();
            state.hold_since = None;
            state.held_frame = None;
        }
        if let Some(frame) = &self.callbacks.borrow().held_frame {
            let mut frame = frame.clone();
            // Input policy follows current native modes even while rendering
            // is held. The immutable held scene itself remains untouched.
            frame.mouse_reporting = self.native.is_mouse_tracking()?;
            frame.alternate_screen = self.native.active_screen()? == sdk::screen::Screen::Alternate;
            return Ok(frame);
        }
        self.render.borrow_mut().capture(&self.native, self.size)
    }
    /// Move one attached view through native history without changing PTY input
    /// or the canonical live viewport. Extremes mean live/oldest respectively.
    pub fn scroll_view(&mut self, view_id: &str, delta: i64) -> Result<u64> {
        if delta == i64::MIN {
            self.history.remove(view_id);
            return Ok(0);
        }
        if self.native.active_screen()? == sdk::screen::Screen::Alternate {
            return Err(Error("scrollback unavailable on alternate screen".into()));
        }
        let scrollbar = self.native.scrollbar()?;
        let oldest = scrollbar.total.saturating_sub(scrollbar.len);
        let offset = self
            .view_scrollback_offset(view_id)?
            .saturating_add_signed(delta)
            .min(oldest);
        if offset == 0 {
            self.history.remove(view_id);
        } else {
            let point = sdk::terminal::Point::Screen(sdk::terminal::PointCoordinate {
                x: 0,
                y: u32::try_from(oldest - offset)
                    .map_err(|_| Error("history row overflow".into()))?,
            });
            if let Some(anchor) = self.history.get_mut(view_id) {
                anchor.set(&mut self.native, point)?;
            } else {
                self.history
                    .insert(view_id.to_owned(), self.native.track_grid_ref(point)?);
            }
        }
        Ok(offset)
    }
    /// Resolve a bounded native pin after output, pruning or resize/reflow.
    /// If retention removed its row, inspection clamps to oldest retained data.
    pub fn view_scrollback_offset(&self, view_id: &str) -> Result<u64> {
        if self.native.active_screen()? == sdk::screen::Screen::Alternate {
            return Ok(0);
        }
        let Some(anchor) = self.history.get(view_id) else {
            return Ok(0);
        };
        let scrollbar = self.native.scrollbar()?;
        let oldest = scrollbar.total.saturating_sub(scrollbar.len);
        let row = anchor
            .point(sdk::terminal::PointSpace::Screen)?
            .map_or(0, |point| u64::from(point.y));
        Ok(oldest.saturating_sub(row))
    }
    /// Drop native pins with their detached views; never retain terminal text.
    pub fn retain_scrollback_views(&mut self, mut attached: impl FnMut(&str) -> bool) {
        self.history.retain(|view, _| attached(view));
    }
    /// Inspect current native data, independently of the published render hold,
    /// without stealing the canonical viewport or mutating the held frame.
    pub fn read_scrollback(&mut self, offset: u64) -> Result<Frame> {
        if offset == 0 || self.native.active_screen()? == sdk::screen::Screen::Alternate {
            return self.render.borrow_mut().capture(&self.native, self.size);
        }
        let scrollbar = self.native.scrollbar()?;
        let row = usize::try_from(
            scrollbar
                .total
                .saturating_sub(scrollbar.len)
                .saturating_sub(offset),
        )
        .map_err(|_| Error("scroll offset overflow".into()))?;
        // The same reusable native render state follows this temporary viewport.
        // Restoring the viewport dirties its rows for the next live extraction;
        // no second renderer, scrollback copy or image-cache clone is needed.
        self.native
            .scroll_viewport(sdk::terminal::ScrollViewport::Row(row));
        let mut result = self.render.borrow_mut().capture(&self.native, self.size);
        let restore = if scrollbar.offset + scrollbar.len >= scrollbar.total {
            sdk::terminal::ScrollViewport::Bottom
        } else {
            sdk::terminal::ScrollViewport::Row(scrollbar.offset as usize)
        };
        self.native.scroll_viewport(restore);
        if let Ok(frame) = &mut result
            && frame.scrollback_offset > 0
        {
            frame.cursor.visible = false;
        }
        result
    }
    pub fn sync_output_remaining(&self) -> Option<std::time::Duration> {
        self.callbacks
            .borrow()
            .hold_since
            .map(|since| std::time::Duration::from_secs(1).saturating_sub(since.elapsed()))
    }
    pub fn tick_images(&mut self, now_ms: u64) -> Result<Option<u64>> {
        Ok(self.native.kitty_graphics_animation_tick(now_ms)?)
    }
}
