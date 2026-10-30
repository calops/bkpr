//! Owned, normalized boundary around one exact official libghostty-vt C API.
//! A terminal is exclusively accessed by its pane worker. Callback requests and
//! snapshots own their bytes; no engine pointer or borrowed buffer escapes.
mod model;
pub use model::*;
mod callbacks;
mod held;
mod input;
mod offers;
mod render;
mod status;
pub use offers::{PASTE_OFFER_MAX_BYTES, PASTE_OFFER_MAX_COUNT, PASTE_OFFER_TIMEOUT};
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/ghostty.rs"));
}
use ffi::*;
use std::{ffi::c_void, fmt, ptr};

pub const ENGINE_REVISION: &str = "93967b7f9478e94dcbb42125c8a90f34feb40d75";
pub const ENGINE_PATCH_SHA256: &str =
    "196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c";
pub const SIXEL_SUPPORTED: bool = false;
pub const CLIPBOARD_MAX_BYTES: usize = 8 * 1024 * 1024;
const IMAGE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub type Result<T, E = Error> = std::result::Result<T, E>;
const EFFECT_MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
fn check(value: GhosttyResult) -> Result<()> {
    if value == GHOSTTY_SUCCESS {
        Ok(())
    } else {
        Err(Error(format!("libghostty-vt result {value}")))
    }
}
fn sized<T: Default>() -> T {
    let mut value = T::default();
    // All callers use official sized C structs whose first field is size_t.
    unsafe {
        (&mut value as *mut T)
            .cast::<usize>()
            .write(std::mem::size_of::<T>());
    }
    value
}
fn out<T>(v: &mut T) -> *mut c_void {
    (v as *mut T).cast()
}
fn rgb(v: GhosttyColorRgb) -> [u8; 3] {
    [v.r, v.g, v.b]
}
fn valid_size(size: Size) -> Result<()> {
    if size.cols == 0
        || size.rows == 0
        || size.cols > 1000
        || size.rows > 1000
        || size.cell_width == 0
        || size.cell_height == 0
        || size.cell_width > 1024
        || size.cell_height > 1024
    {
        return Err(Error("terminal size exceeds bounded pane geometry".into()));
    }
    Ok(())
}
pub type ClipboardHandler = Box<dyn FnMut(ClipboardRequest) -> ClipboardResponse + Send>;
struct CallbackState {
    effects: Vec<Effect>,
    effect_bytes: usize,
    failed: bool,
    clipboard: Option<ClipboardHandler>,
    render: GhosttyRenderState,
    hold_since: Option<std::time::Instant>,
    status: status::StatusRecords,
    rows: GhosttyRenderStateRowIterator,
    cells: GhosttyRenderStateRowCells,
    images: GhosttyRenderStateImageIterator,
    image_cache: held::ImageCache,
    held_links: std::collections::HashMap<(u16, u32), String>,
    size: Size,
    rebuild_after_hold: bool,
    paste_offers: std::collections::HashMap<u64, std::sync::Arc<offers::PasteOffer>>,
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
/// May be moved to a pane worker, but never accessed concurrently. The official
/// API has no thread affinity; sys callbacks are installed process-wide once.
pub struct Terminal {
    terminal: GhosttyTerminal,
    rows: GhosttyRenderStateRowIterator,
    cells: GhosttyRenderStateRowCells,
    images: GhosttyRenderStateImageIterator,
    key_encoder: GhosttyKeyEncoder,
    key_event: GhosttyKeyEvent,
    mouse_encoder: GhosttyMouseEncoder,
    mouse_event: GhosttyMouseEvent,
    callbacks: Box<CallbackState>,
    size: Size,
    mouse_buttons: u16,
    last_offer_id: u64,
}
// SAFETY: every handle and callback belongs exclusively to this value, callback
// context is boxed, and the handler is Send. No simultaneous or reentrant calls.
unsafe impl Send for Terminal {}
impl Terminal {
    pub fn new(size: Size) -> Result<Self> {
        valid_size(size)?;
        callbacks::initialize_sys()?;
        let mut this = Self {
            terminal: ptr::null_mut(),
            rows: ptr::null_mut(),
            cells: ptr::null_mut(),
            images: ptr::null_mut(),
            key_encoder: ptr::null_mut(),
            key_event: ptr::null_mut(),
            mouse_encoder: ptr::null_mut(),
            mouse_event: ptr::null_mut(),
            callbacks: Box::new(CallbackState {
                effects: Vec::new(),
                effect_bytes: 0,
                failed: false,
                clipboard: None,
                render: ptr::null_mut(),
                hold_since: None,
                status: status::StatusRecords::default(),
                rows: ptr::null_mut(),
                cells: ptr::null_mut(),
                images: ptr::null_mut(),
                image_cache: std::collections::HashMap::new(),
                held_links: std::collections::HashMap::new(),
                size,
                rebuild_after_hold: false,
                paste_offers: std::collections::HashMap::new(),
            }),
            size,
            mouse_buttons: 0,
            last_offer_id: 0,
        };
        unsafe {
            check(ghostty_terminal_new(
                ptr::null(),
                &mut this.terminal,
                size.cols,
                size.rows,
            ))?;
            check(ghostty_render_state_new(
                ptr::null(),
                &mut this.callbacks.render,
            ))?;
            check(ghostty_render_state_row_iterator_new(
                ptr::null(),
                &mut this.rows,
            ))?;
            check(ghostty_render_state_row_cells_new(
                ptr::null(),
                &mut this.cells,
            ))?;
            check(ghostty_render_state_image_iterator_new(
                ptr::null(),
                &mut this.images,
            ))?;
            this.callbacks.rows = this.rows;
            this.callbacks.cells = this.cells;
            this.callbacks.images = this.images;
            check(ghostty_key_encoder_new(ptr::null(), &mut this.key_encoder))?;
            check(ghostty_key_event_new(ptr::null(), &mut this.key_event))?;
            check(ghostty_mouse_encoder_new(
                ptr::null(),
                &mut this.mouse_encoder,
            ))?;
            check(ghostty_mouse_event_new(ptr::null(), &mut this.mouse_event))?;
            check(ghostty_terminal_set(
                this.terminal,
                GHOSTTY_TERMINAL_OPT_USERDATA,
                (&mut *this.callbacks as *mut CallbackState).cast(),
            ))?;
            for (option, callback) in callbacks::registrations() {
                check(ghostty_terminal_set(this.terminal, option, callback))?;
            }
        }
        this.set_value(
            GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
            &(16 * 1024 * 1024usize),
        )?;
        this.set_value(GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &10000usize)?;
        this.set_value(
            GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
            &(IMAGE_MAX_BYTES as u64),
        )?;
        this.set_value(GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES, &(16 * 1024 * 1024usize))?;
        this.set_value(
            GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES_KITTY,
            &(16 * 1024 * 1024usize),
        )?;
        this.set_value(
            GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES,
            &CLIPBOARD_MAX_BYTES,
        )?;
        for option in [
            GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE,
            GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM,
        ] {
            this.set_value(option, &false)?;
        }
        // Unlike the file/shared-memory bool options, temporary-file medium
        // takes a GhosttyString directory; its documented disable value is NULL.
        unsafe {
            check(ghostty_terminal_set(
                this.terminal,
                GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE,
                ptr::null(),
            ))?;
        }
        this.resize(size)?;
        Ok(this)
    }
    fn set_value<T>(&mut self, option: GhosttyTerminalOption, value: &T) -> Result<()> {
        unsafe {
            check(ghostty_terminal_set(
                self.terminal,
                option,
                (value as *const T).cast(),
            ))
        }
    }
    /// Handler runs synchronously on the pane worker. A remote consent handler
    /// must use a bounded wait/cancellation channel, never a global runtime lock.
    /// No handler means deny. Engine session grants are still policy inputs;
    /// the handler remains responsible for current pane/client authority.
    pub fn set_clipboard_handler(&mut self, handler: ClipboardHandler) {
        self.callbacks.clipboard = Some(handler);
    }
    pub fn set_default_colors(
        &mut self,
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: Option<[u8; 3]>,
    ) -> Result<()> {
        let color = |[r, g, b]: [u8; 3]| GhosttyColorRgb { r, g, b };
        self.set_value(GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &color(foreground))?;
        self.set_value(GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &color(background))?;
        if let Some(cursor) = cursor {
            self.set_value(GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &color(cursor))?;
        } else {
            unsafe {
                check(ghostty_terminal_set(
                    self.terminal,
                    GHOSTTY_TERMINAL_OPT_COLOR_CURSOR,
                    ptr::null(),
                ))?;
            }
        }
        Ok(())
    }
    pub fn set_palette(&mut self, palette: &[[u8; 3]; 256]) -> Result<()> {
        let native = palette.map(|[r, g, b]| GhosttyColorRgb { r, g, b });
        self.set_value(GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, &native)
    }
    pub fn resize(&mut self, size: Size) -> Result<()> {
        valid_size(size)?;
        self.callbacks.size = size;
        let result = unsafe {
            ghostty_terminal_resize(
                self.terminal,
                size.cols,
                size.rows,
                size.cell_width,
                size.cell_height,
            )
        };
        if result != GHOSTTY_SUCCESS {
            self.callbacks.size = self.size;
            check(result)?;
        }
        self.size = size;
        Ok(())
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Effect>> {
        if bytes.len() > 1024 * 1024 {
            return Err(Error("feed chunk exceeds 1 MiB".into()));
        }
        unsafe {
            ghostty_terminal_vt_write(self.terminal, bytes.as_ptr(), bytes.len());
        }
        let mut failed = false;
        unsafe {
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_VT_PROCESSING_ERROR,
                out(&mut failed),
            ))?;
        }
        if failed || self.callbacks.failed {
            return Err(Error(
                "terminal processing or callback resource limit failed".into(),
            ));
        }
        Ok(self.take_effects())
    }
    fn take_effects(&mut self) -> Vec<Effect> {
        self.callbacks.effect_bytes = 0;
        std::mem::take(&mut self.callbacks.effects)
    }
    /// Flush callbacks caused by out-of-band operations such as mode-2048
    /// resize reporting. Feed already drains its own effects.
    pub fn drain_effects(&mut self) -> Result<Vec<Effect>> {
        if self.callbacks.failed {
            return Err(Error("terminal callback resource limit failed".into()));
        }
        Ok(self.take_effects())
    }
    pub fn scroll(&mut self, delta: i64) -> Result<()> {
        let delta = isize::try_from(delta).map_err(|_| Error("scroll delta overflow".into()))?;
        unsafe {
            ghostty_terminal_scroll_viewport(
                self.terminal,
                GhosttyTerminalScrollViewport {
                    tag: GHOSTTY_SCROLL_VIEWPORT_DELTA,
                    value: GhosttyTerminalScrollViewportValue { delta },
                },
            );
        }
        Ok(())
    }
    /// Read a viewport `offset` rows above live output: zero is live, increasing
    /// values show older rows, and values beyond retained history clamp oldest.
    /// The canonical viewport is restored so independent views cannot steal it.
    pub fn read_scrollback(&mut self, offset: u64) -> Result<Frame> {
        let mut scrollbar = GhosttyTerminalScrollbar::default();
        unsafe {
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_SCROLLBAR,
                out(&mut scrollbar),
            ))?;
        }
        let live_start = scrollbar.total.saturating_sub(scrollbar.len);
        let offset = usize::try_from(live_start.saturating_sub(offset))
            .map_err(|_| Error("scroll offset overflow".into()))?;
        let held = self.callbacks.hold_since;
        let saved = if held.is_some() {
            let mut temporary = ptr::null_mut();
            unsafe {
                check(ghostty_render_state_new(ptr::null(), &mut temporary))?;
            }
            let render = std::mem::replace(&mut self.callbacks.render, temporary);
            self.callbacks.hold_since = None;
            Some((render, std::mem::take(&mut self.callbacks.image_cache)))
        } else {
            None
        };
        unsafe {
            ghostty_terminal_scroll_viewport(
                self.terminal,
                GhosttyTerminalScrollViewport {
                    tag: GHOSTTY_SCROLL_VIEWPORT_ROW,
                    value: GhosttyTerminalScrollViewportValue { row: offset },
                },
            );
        }
        let result = self.snapshot();
        let (tag, value) = if scrollbar.offset + scrollbar.len >= scrollbar.total {
            (
                GHOSTTY_SCROLL_VIEWPORT_BOTTOM,
                GhosttyTerminalScrollViewportValue::default(),
            )
        } else {
            (
                GHOSTTY_SCROLL_VIEWPORT_ROW,
                GhosttyTerminalScrollViewportValue {
                    row: scrollbar.offset as usize,
                },
            )
        };
        unsafe {
            ghostty_terminal_scroll_viewport(
                self.terminal,
                GhosttyTerminalScrollViewport { tag, value },
            );
        }
        if let Some((render, images)) = saved {
            let temporary = std::mem::replace(&mut self.callbacks.render, render);
            unsafe {
                ghostty_render_state_free(temporary);
            }
            self.callbacks.image_cache = images;
            self.callbacks.hold_since = held;
            // The temporary renderer consumed native dirty flags. The held
            // render remains immutable until release, then must capture afresh.
            self.callbacks.rebuild_after_hold = true;
        }
        result
    }
    /// Relative deadline for the pane worker's next scheduled snapshot, even
    /// when the application produces no more bytes after enabling mode 2026.
    pub fn sync_output_remaining(&self) -> Option<std::time::Duration> {
        self.callbacks
            .hold_since
            .map(|since| std::time::Duration::from_secs(1).saturating_sub(since.elapsed()))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            if !self.terminal.is_null() {
                for (option, _) in callbacks::registrations() {
                    ghostty_terminal_set(self.terminal, option, ptr::null());
                }
            }
            ghostty_mouse_event_free(self.mouse_event);
            ghostty_mouse_encoder_free(self.mouse_encoder);
            ghostty_key_event_free(self.key_event);
            ghostty_key_encoder_free(self.key_encoder);
            ghostty_render_state_image_iterator_free(self.images);
            ghostty_render_state_row_cells_free(self.cells);
            ghostty_render_state_row_iterator_free(self.rows);
            ghostty_render_state_free(self.callbacks.render);
            ghostty_terminal_free(self.terminal);
        }
    }
}
