//! Bkpr scene normalization and device authority policy over the upstream SDK.
//! Native objects, input encoders, callbacks and borrowed views belong to
//! libghostty-vt. A terminal is created and used on its pane worker only.
#![deny(missing_docs)]

mod callbacks;
mod input;
mod offers;
mod render;
mod search;
mod selection;
mod status;
use bkpr_protocol::*;
use libghostty_vt as sdk;
pub use offers::{PASTE_OFFER_MAX_BYTES, PASTE_OFFER_MAX_COUNT, PASTE_OFFER_TIMEOUT};
pub use status::ProgramStatusView;
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt,
    rc::Rc,
    sync::{Arc, LazyLock},
};

/// Native Ghostty source revision required by this adapter.
pub const ENGINE_REVISION: &str = "93967b7f9478e94dcbb42125c8a90f34feb40d75";
/// Safe/sys SDK source revision used with the pinned native ABI.
pub const SDK_REVISION: &str = "575ed6b8ceb79ffaf247ecf7e34333876b0d5be4";
/// SHA-256 of the native clipboard-offer patch.
pub const ENGINE_PATCH_SHA256: &str =
    "196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c";
/// Whether canonical extraction supports Sixel graphics.
pub const SIXEL_SUPPORTED: bool = false;
/// Maximum aggregate MIME payload bytes in a clipboard transfer.
pub const CLIPBOARD_MAX_BYTES: usize = 8 * 1024 * 1024;
const EFFECT_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Fallible adapter operation, defaulting to [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
/// Native adapter failure; a failed mutation need not leave terminal state unchanged.
#[derive(Debug)]
pub struct Error(
    /// Human-readable failure diagnostic.
    pub String,
);
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
/// Synchronous clipboard policy invoked on the pane worker with exact MIME contents.
pub type ClipboardHandler = Box<dyn FnMut(ClipboardRequest) -> ClipboardResponse + Send>;
/// Ordered status changes on the terminal worker; `false` denotes withdrawal,
/// never completion. The borrowed records cannot outlive the callback.
pub type ProgramStatusObserver = Box<dyn FnMut(ProgramStatusView<'_>, bool)>;
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct DefaultColors {
    foreground: Option<sdk::style::RgbColor>,
    background: Option<sdk::style::RgbColor>,
    cursor: Option<sdk::style::RgbColor>,
}
struct CallbackState {
    effects: Vec<Effect>,
    effect_bytes: usize,
    failed: bool,
    color_scheme: Option<sdk::terminal::ColorScheme>,
    default_colors: DefaultColors,
    default_ansi: [[u8; 3]; 16],
    clipboard: Option<ClipboardHandler>,
    hold_since: Option<std::time::Instant>,
    held_frame: Option<(Frame, PaneColorOverrides)>,
    status: status::StatusRecords,
    status_observer: Option<ProgramStatusObserver>,
    size: Size,
    paste_offers: HashMap<u64, Arc<offers::PasteOffer>>,
}
impl CallbackState {
    fn push(&mut self, effect: Effect, bytes: usize) {
        self.status.update(&effect);
        if self.effect_bytes.saturating_add(bytes) > EFFECT_MAX_BYTES || self.effects.len() >= 65536
        {
            self.failed = true;
            return;
        }
        self.effect_bytes += bytes;
        self.effects.push(effect);
    }
    fn color_overrides(
        &self,
        terminal: &sdk::Terminal<'_, '_>,
        palette: &[[u8; 3]],
    ) -> Result<PaneColorOverrides> {
        let ansi_mask = palette
            .iter()
            .zip(&self.default_ansi)
            .enumerate()
            .fold(0, |mask, (index, (effective, default))| {
                mask | (u16::from(effective != default) << index)
            });
        Ok(PaneColorOverrides {
            foreground: terminal.fg_color()? != self.default_colors.foreground,
            background: terminal.bg_color()? != self.default_colors.background,
            ansi_mask,
        })
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
    selections: selection::Selections,
    searches: search::Searches,
}
impl Terminal {
    /// Create a thread-affine terminal with bounded history, images and callbacks.
    /// Rejects invalid geometry and reports native initialization failures.
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
        // Keep PNG/zlib payloads so clients can forward them without re-encoding.
        native.set_kitty_image_retain_source(true)?;
        let callbacks = Rc::new(RefCell::new(CallbackState {
            effects: Vec::new(),
            effect_bytes: 0,
            failed: false,
            color_scheme: None,
            default_colors: DefaultColors::default(),
            default_ansi: *Self::factory_palette().first_chunk::<16>().unwrap(),
            clipboard: None,
            hold_since: None,
            held_frame: None,
            status: status::StatusRecords::default(),
            status_observer: None,
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
            selections: selection::Selections::new()?,
            searches: search::Searches::default(),
        };
        this.resize(size)?;
        Ok(this)
    }
    /// Synchronous pane-worker policy. No handler means deny. Native session
    /// grants and offer correlations never replace current view authority.
    pub fn set_clipboard_handler(&mut self, handler: ClipboardHandler) {
        self.callbacks.borrow_mut().clipboard = Some(handler);
    }
    /// Observe every canonical status replacement before feed/effect coalescing.
    /// Callbacks run synchronously and must not block on storage or process scans.
    pub fn set_program_status_observer(&mut self, observer: ProgramStatusObserver) {
        self.callbacks.borrow_mut().status_observer = Some(observer);
    }
    /// Ghostty's built-in indexed palette, independent of pane and application state.
    pub fn factory_palette() -> &'static [[u8; 3]; 256] {
        static PALETTE: LazyLock<[[u8; 3]; 256]> =
            LazyLock::new(|| sdk::style::Palette::default().0.map(render::rgb));
        &PALETTE
    }
    /// Set virtual RGB defaults (unset on `None`) and optionally replace the indexed
    /// defaults. The SDK preserves application OSC overrides.
    ///
    /// Reserve an opted-in scheme report before changing any native defaults.
    /// A report admission error leaves the theme unchanged; otherwise the caller
    /// receives the reservation to dispatch after application. The boolean reports
    /// whether defaults were changed (a supplied palette must differ).
    pub fn set_theme_colors<R, E: From<Error>>(
        &mut self,
        foreground: Option<[u8; 3]>,
        background: Option<[u8; 3]>,
        cursor: Option<[u8; 3]>,
        palette: Option<&[[u8; 3]; 256]>,
        admit_report: impl FnOnce(Vec<u8>) -> Result<R, E>,
    ) -> Result<(bool, Option<R>), E> {
        use sdk::terminal::{ColorScheme, Mode};
        let defaults = DefaultColors {
            foreground: foreground.map(render::native_rgb),
            background: background.map(render::native_rgb),
            cursor: cursor.map(render::native_rgb),
        };
        let (previous, previous_scheme) = {
            let state = self.callbacks.borrow();
            (state.default_colors, state.color_scheme)
        };
        if previous == defaults && palette.is_none() {
            return Ok((false, None));
        }
        let scheme = if previous.background == defaults.background {
            previous_scheme
        } else {
            defaults.background.map(|rgb| {
                if rgb.perceived_luminance() > 0.5 {
                    ColorScheme::Light
                } else {
                    ColorScheme::Dark
                }
            })
        };
        let reservation = if let Some(scheme) = scheme
            && Some(scheme) != previous_scheme
            && self
                .native
                .mode(Mode::COLOR_SCHEME_REPORT)
                .map_err(Error::from)?
        {
            let mut report = vec![0; 16];
            let len = scheme.encode_report(&mut report).map_err(Error::from)?;
            report.truncate(len);
            Some(admit_report(report)?)
        } else {
            None
        };
        if let Some(palette) = palette {
            self.native
                .set_default_color_palette(Some(sdk::style::Palette(
                    palette.map(render::native_rgb),
                )))
                .map_err(Error::from)?;
            self.callbacks
                .borrow_mut()
                .default_ansi
                .copy_from_slice(&palette[..16]);
        }
        if previous.foreground != defaults.foreground {
            self.native
                .set_default_fg_color(defaults.foreground)
                .map_err(Error::from)?;
            self.callbacks.borrow_mut().default_colors.foreground = defaults.foreground;
        }
        if previous.cursor != defaults.cursor {
            self.native
                .set_default_cursor_color(defaults.cursor)
                .map_err(Error::from)?;
            self.callbacks.borrow_mut().default_colors.cursor = defaults.cursor;
        }
        if previous.background != defaults.background {
            self.native
                .set_default_bg_color(defaults.background)
                .map_err(Error::from)?;
            let mut state = self.callbacks.borrow_mut();
            state.default_colors.background = defaults.background;
            state.color_scheme = scheme;
        }
        Ok((true, reservation))
    }
    /// Resize and reconcile inspection state; rejects invalid geometry or native failures.
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
        self.selections.reconcile(&self.native)?;
        self.searches.invalidate();
        Ok(())
    }
    /// Process at most 1 MiB of PTY output and return queued effects in order.
    /// Native/callback errors may follow partial processing; do not replay the chunk.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Effect>> {
        if bytes.len() > 1024 * 1024 {
            return Err(Error("feed chunk exceeds 1 MiB".into()));
        }
        self.native.vt_write(bytes);
        self.selections.reconcile(&self.native)?;
        self.searches.invalidate();
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
    /// Take pending effects without feeding output; reject a faulted callback queue.
    pub fn drain_effects(&mut self) -> Result<Vec<Effect>> {
        if self.callbacks.borrow().failed {
            return Err(Error("terminal callback resource limit failed".into()));
        }
        Ok(self.take_effects())
    }
    /// Move the canonical viewport by signed rows; negative values select older history.
    pub fn scroll(&mut self, delta: i64) -> Result<()> {
        let delta = isize::try_from(delta).map_err(|_| Error("scroll delta overflow".into()))?;
        self.native
            .scroll_viewport(sdk::terminal::ScrollViewport::Delta(delta));
        Ok(())
    }
    /// Extract an owned canonical scene, sharing immutable image generations.
    /// A synchronized-output hold may return held content until its bounded deadline.
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
        if let Some((frame, _)) = &self.callbacks.borrow().held_frame {
            let mut frame = frame.clone();
            // Input policy follows current native modes even while rendering
            // is held. The immutable held scene itself remains untouched.
            frame.mouse_reporting = self.native.is_mouse_tracking()?;
            frame.alternate_screen = self.native.active_screen()? == sdk::screen::Screen::Alternate;
            return Ok(frame);
        }
        self.render.borrow_mut().capture(&self.native, self.size)
    }
    /// Extract the canonical scene and its detectable application color overrides.
    /// Overrides equal to their native defaults are indistinguishable in the SDK.
    pub fn snapshot_with_color_overrides(&mut self) -> Result<(Frame, PaneColorOverrides)> {
        let frame = self.snapshot()?;
        let state = self.callbacks.borrow();
        let overrides = if let Some((_, overrides)) = &state.held_frame {
            *overrides
        } else {
            state.color_overrides(&self.native, &frame.palette)?
        };
        Ok((frame, overrides))
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
        self.selections.retain(&self.native, attached);
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
    /// Remaining synchronized-output hold duration, or `None` when no hold is active.
    pub fn sync_output_remaining(&self) -> Option<std::time::Duration> {
        self.callbacks
            .borrow()
            .hold_since
            .map(|since| std::time::Duration::from_secs(1).saturating_sub(since.elapsed()))
    }
    /// Advance native animation using one monotonic millisecond clock for this terminal's lifetime.
    /// Returns the next absolute deadline on that clock, or `None` when no animation is playing.
    pub fn tick_images(&mut self, now_ms: u64) -> Result<Option<u64>> {
        Ok(self.native.kitty_graphics_animation_tick(now_ms)?)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.clear_view_selections();
        self.searches.retain(|_| None);
    }
}
