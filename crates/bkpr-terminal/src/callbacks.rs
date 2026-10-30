use super::*;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    slice,
    sync::LazyLock,
};

// All raw inputs below are borrowed by the pinned C API for this callback only.
// No callback unwinds into Zig. Clipboard replies execute before returning and
// after releasing any exclusive Rust borrow of callback state (reply may write
// PTY bytes through another callback).
unsafe fn bytes<'a>(value: GhosttyString) -> &'a [u8] {
    if value.len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(value.ptr, value.len) }
    }
}
unsafe fn text(value: GhosttyString) -> String {
    String::from_utf8_lossy(unsafe { bytes(value) }).into_owned()
}
fn optional(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}
fn guarded(userdata: *mut c_void, callback: impl FnOnce(&mut CallbackState)) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        callback(unsafe { &mut *userdata.cast::<CallbackState>() })
    }));
    if result.is_err() {
        unsafe {
            (*userdata.cast::<CallbackState>()).failed = true;
        }
    }
}
pub(super) fn registrations() -> [(GhosttyTerminalOption, *const c_void); 12] {
    [
        (
            GHOSTTY_TERMINAL_OPT_WRITE_PTY,
            write_pty as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_BELL,
            bell as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_TITLE_CHANGED,
            title as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_PWD_CHANGED,
            cwd as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_PROGRAM_STATUS,
            status as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ,
            clipboard_read as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE,
            clipboard_write as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_RENDER_HOLD,
            hold as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT,
            semantic_prompt as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_SIZE,
            size as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES,
            device_attributes as *const () as *const c_void,
        ),
        (
            GHOSTTY_TERMINAL_OPT_XTVERSION,
            version as *const () as *const c_void,
        ),
    ]
}
unsafe extern "C" fn semantic_prompt(
    _: GhosttyTerminal,
    userdata: *mut c_void,
    event: *const GhosttyTerminalSemanticPrompt,
) {
    guarded(userdata, |state| {
        let event = unsafe { &*event };
        if event.size >= std::mem::size_of::<GhosttyTerminalSemanticPrompt>()
            && event.kind == GHOSTTY_SEMANTIC_PROMPT_PROMPT_START
        {
            state.status.prompt_or_exit();
        }
    });
}
unsafe extern "C" fn write_pty(
    _: GhosttyTerminal,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    guarded(userdata, |state| {
        if len > EFFECT_MAX_BYTES {
            state.failed = true;
            return;
        }
        let data = if len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(data, len) }.to_vec()
        };
        state.push(Effect::PtyWrite(data), len);
    });
}
unsafe extern "C" fn bell(_: GhosttyTerminal, userdata: *mut c_void) {
    guarded(userdata, |state| state.push(Effect::Bell, 0));
}
unsafe fn string_event(
    terminal: GhosttyTerminal,
    userdata: *mut c_void,
    key: GhosttyTerminalData,
    is_title: bool,
) {
    guarded(userdata, |state| {
        let mut value = GhosttyString::default();
        if unsafe { ghostty_terminal_get(terminal, key, out(&mut value)) } != GHOSTTY_SUCCESS {
            state.failed = true;
            return;
        }
        let text = unsafe { text(value) };
        let len = text.len();
        state.push(
            if is_title {
                Effect::Title(text)
            } else {
                Effect::Cwd(text)
            },
            len,
        );
    });
}
unsafe extern "C" fn title(terminal: GhosttyTerminal, userdata: *mut c_void) {
    unsafe {
        string_event(terminal, userdata, GHOSTTY_TERMINAL_DATA_TITLE, true);
    }
}
unsafe extern "C" fn cwd(terminal: GhosttyTerminal, userdata: *mut c_void) {
    unsafe {
        string_event(terminal, userdata, GHOSTTY_TERMINAL_DATA_PWD, false);
    }
}
unsafe extern "C" fn hold(terminal: GhosttyTerminal, userdata: *mut c_void, held: bool) {
    guarded(userdata, |state| {
        if held {
            if unsafe { ghostty_render_state_update(state.render, terminal) } != GHOSTTY_SUCCESS
                || unsafe { held::capture(terminal, state) }.is_err()
            {
                state.failed = true;
            }
            state.hold_since = Some(std::time::Instant::now());
        } else if held::release(state).is_err() {
            state.failed = true;
        }
    });
}
unsafe extern "C" fn status(
    _: GhosttyTerminal,
    userdata: *mut c_void,
    report: *const GhosttyTerminalProgramStatus,
) {
    guarded(userdata, |state| {
        let report = unsafe { &*report };
        if report.size < std::mem::size_of::<GhosttyTerminalProgramStatus>() {
            state.failed = true;
            return;
        }
        let name = match report.state {
            GHOSTTY_PROGRAM_STATUS_STATE_IDLE => "idle",
            GHOSTTY_PROGRAM_STATUS_STATE_WORKING => "working",
            GHOSTTY_PROGRAM_STATUS_STATE_DONE => "done",
            GHOSTTY_PROGRAM_STATUS_STATE_BLOCKED => "blocked",
            GHOSTTY_PROGRAM_STATUS_STATE_ERROR => "error",
            GHOSTTY_PROGRAM_STATUS_STATE_CLEAR => "clear",
            _ => return,
        };
        let kind = match report.kind {
            GHOSTTY_PROGRAM_STATUS_KIND_PERMISSION => Some("permission".into()),
            GHOSTTY_PROGRAM_STATUS_KIND_QUESTION => Some("question".into()),
            GHOSTTY_PROGRAM_STATUS_KIND_AUTH => Some("auth".into()),
            _ => None,
        };
        let id = unsafe { text(report.id) };
        let app = unsafe { text(report.app) };
        let title = unsafe { text(report.title) };
        let message = unsafe { text(report.message) };
        let bytes = id.len() + app.len() + title.len() + message.len();
        state.push(
            Effect::ProgramStatus {
                state: name.into(),
                kind,
                app: optional(app),
                message: optional(message),
                id,
                title,
                progress: u8::try_from(report.progress).ok().filter(|v| *v <= 100),
            },
            bytes,
        );
    });
}
fn location(value: GhosttyClipboardLocation) -> ClipboardLocation {
    match value {
        GHOSTTY_CLIPBOARD_LOCATION_SELECTION => ClipboardLocation::Selection,
        GHOSTTY_CLIPBOARD_LOCATION_PRIMARY => ClipboardLocation::Primary,
        _ => ClipboardLocation::Standard,
    }
}
fn invoke(userdata: *mut c_void, request: ClipboardRequest) -> ClipboardResponse {
    let handler = unsafe { (*userdata.cast::<CallbackState>()).clipboard.take() };
    if let Some(mut handler) = handler {
        let result = catch_unwind(AssertUnwindSafe(|| handler(request)));
        unsafe {
            (*userdata.cast::<CallbackState>()).clipboard = Some(handler);
        }
        result.unwrap_or_else(|_| ClipboardResponse::denied())
    } else {
        ClipboardResponse::denied()
    }
}
fn valid_response(value: &ClipboardResponse) -> bool {
    value.contents.len() <= 128
        && value.available.len() <= 128
        && value
            .contents
            .iter()
            .try_fold(0usize, |n, c| {
                n.checked_add(c.mime.len())?.checked_add(c.data.len())
            })
            .is_some_and(|n| n <= CLIPBOARD_MAX_BYTES)
        && value.available.iter().all(|v| v.len() <= 4096)
}
fn string(value: &[u8]) -> GhosttyString {
    GhosttyString {
        ptr: value.as_ptr(),
        len: value.len(),
    }
}
unsafe extern "C" fn clipboard_write(
    _: GhosttyTerminal,
    userdata: *mut c_void,
    request: *const GhosttyClipboardWrite,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let req = unsafe { &*request };
        if req.size < std::mem::size_of::<GhosttyClipboardWrite>() || req.contents_len > 128 {
            return;
        }
        let contents = if req.contents_len == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(req.contents, req.contents_len) }
        };
        let mut total = 0usize;
        let mut owned = Vec::with_capacity(contents.len());
        for content in contents {
            total = total
                .saturating_add(content.mime.len)
                .saturating_add(content.data.len);
            if total > CLIPBOARD_MAX_BYTES {
                return;
            }
            owned.push(ClipboardContent {
                mime: unsafe { text(content.mime) },
                data: unsafe { bytes(content.data) }.to_vec(),
            });
        }
        let answer = invoke(
            userdata,
            ClipboardRequest {
                operation: ClipboardOperation::Write { contents: owned },
                location: location(req.location),
                name: unsafe { text(req.name) },
                granted: req.granted,
                can_remember: req.can_remember,
                paste_offer_id: None,
            },
        );
        let mut reply = sized::<GhosttyClipboardWriteReply>();
        reply.result = match answer.result {
            ClipboardResult::Success => GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS,
            ClipboardResult::Denied => GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED,
            ClipboardResult::Unsupported => GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED,
            ClipboardResult::Busy => GHOSTTY_CLIPBOARD_WRITE_RESULT_BUSY,
            ClipboardResult::InvalidData => GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA,
            ClipboardResult::IoError => GHOSTTY_CLIPBOARD_WRITE_RESULT_IO_ERROR,
        };
        reply.remember = answer.remember && req.can_remember;
        if let Some(reply_fn) = req.reply {
            unsafe {
                reply_fn(request, &reply);
            }
        }
    }));
}
unsafe extern "C" fn clipboard_read(
    _: GhosttyTerminal,
    userdata: *mut c_void,
    request: *const GhosttyClipboardRead,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let req = unsafe { &*request };
        if req.size < std::mem::size_of::<GhosttyClipboardRead>() || req.mimes_len > 128 {
            return;
        }
        let mimes = if req.mimes_len == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(req.mimes, req.mimes_len) }
        };
        if mimes.iter().any(|m| m.len > 4096) {
            return;
        }
        let offer = if req.paste_offer_id != 0 {
            // Native validated the exact one-time password before returning
            // this correlation. Missing/stale offers must never fall through
            // to another view's current clipboard.
            let Some(offer) = (unsafe { &mut *userdata.cast::<CallbackState>() })
                .paste_offer(req.paste_offer_id, req.mimes_len != 0)
            else {
                return;
            };
            let offered_location = if offer.location == ClipboardLocation::Selection {
                ClipboardLocation::Primary
            } else {
                offer.location
            };
            if offered_location != location(req.location) {
                return;
            }
            Some(offer)
        } else {
            None
        };
        let request_owned = ClipboardRequest {
            operation: ClipboardOperation::Read {
                mimes: mimes.iter().map(|m| unsafe { text(*m) }).collect(),
                list: req.list,
            },
            location: location(req.location),
            name: unsafe { text(req.name) },
            granted: req.granted,
            can_remember: req.can_remember && offer.is_none(),
            paste_offer_id: (req.paste_offer_id != 0).then_some(req.paste_offer_id),
        };
        let mut answer = invoke(userdata, request_owned);
        if let Some(offer) = &offer {
            if offer.expired() {
                answer = ClipboardResponse::denied();
            }
            // The handler authorizes the originating view, not a replacement
            // clipboard value. A one-time offer cannot become a session grant.
            answer.contents.clear();
            answer.available.clear();
            answer.remember = false;
        } else if !valid_response(&answer) {
            answer = ClipboardResponse::denied();
        }
        let source = offer.as_ref().map_or(answer.contents.as_slice(), |offer| {
            offer.contents.as_slice()
        });
        let contents: Vec<_> = if answer.result == ClipboardResult::Success {
            source
                .iter()
                .map(|c| GhosttyClipboardContent {
                    mime: string(c.mime.as_bytes()),
                    data: string(&c.data),
                })
                .collect()
        } else {
            Vec::new()
        };
        let available: Vec<_> = if let Some(offer) = &offer {
            offer
                .contents
                .iter()
                .map(|c| string(c.mime.as_bytes()))
                .collect()
        } else {
            answer
                .available
                .iter()
                .map(|m| string(m.as_bytes()))
                .collect()
        };
        let mut reply = sized::<GhosttyClipboardReadReply>();
        reply.result = match answer.result {
            ClipboardResult::Success => GHOSTTY_CLIPBOARD_READ_RESULT_SUCCESS,
            ClipboardResult::Denied | ClipboardResult::InvalidData => {
                GHOSTTY_CLIPBOARD_READ_RESULT_DENIED
            }
            ClipboardResult::Unsupported => GHOSTTY_CLIPBOARD_READ_RESULT_UNSUPPORTED,
            ClipboardResult::Busy => GHOSTTY_CLIPBOARD_READ_RESULT_BUSY,
            ClipboardResult::IoError => GHOSTTY_CLIPBOARD_READ_RESULT_IO_ERROR,
        };
        reply.contents = contents.as_ptr();
        reply.contents_len = contents.len();
        reply.available = available.as_ptr();
        reply.available_len = available.len();
        reply.remember = answer.remember && req.can_remember;
        if let Some(reply_fn) = req.reply {
            unsafe {
                reply_fn(request, &reply);
            }
        }
    }));
}
pub(super) fn initialize_sys() -> Result<()> {
    static INITIALIZED: LazyLock<GhosttyResult> = LazyLock::new(|| unsafe {
        ghostty_sys_set(
            GHOSTTY_SYS_OPT_DECODE_PNG,
            decode_png as *const () as *const c_void,
        )
    });
    check(*INITIALIZED)
}
unsafe extern "C" fn decode_png(
    _: *mut c_void,
    allocator: *const GhosttyAllocator,
    data: *const u8,
    len: usize,
    output: *mut GhosttySysImage,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| -> Option<()> {
        if len > IMAGE_MAX_BYTES {
            return None;
        }
        let data = if len == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(data, len) }
        };
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
        decoder.set_limits(png::Limits {
            bytes: IMAGE_MAX_BYTES,
        });
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let width = reader.info().width;
        let height = reader.info().height;
        let rgba_len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if rgba_len == 0 || rgba_len > IMAGE_MAX_BYTES {
            return None;
        }
        let buffer_len = reader.output_buffer_size()?;
        if buffer_len > IMAGE_MAX_BYTES {
            return None;
        }
        let mut decoded = vec![0; buffer_len];
        let info = reader.next_frame(&mut decoded).ok()?;
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Grayscale => 1,
            _ => return None,
        };
        if info.bit_depth != png::BitDepth::Eight
            || info.buffer_size()
                != (width as usize)
                    .checked_mul(height as usize)?
                    .checked_mul(channels)?
        {
            return None;
        }
        let pixels = unsafe { ghostty_alloc(allocator, rgba_len) }.cast::<u8>();
        if pixels.is_null() {
            return None;
        }
        let target = unsafe { slice::from_raw_parts_mut(pixels, rgba_len) };
        for (src, dst) in decoded[..info.buffer_size()]
            .chunks_exact(channels)
            .zip(target.chunks_exact_mut(4))
        {
            match channels {
                4 => dst.copy_from_slice(src),
                3 => dst.copy_from_slice(&[src[0], src[1], src[2], 255]),
                2 => dst.copy_from_slice(&[src[0], src[0], src[0], src[1]]),
                _ => dst.copy_from_slice(&[src[0], src[0], src[0], 255]),
            }
        }
        unsafe {
            *output = GhosttySysImage {
                width,
                height,
                data: pixels,
                data_len: rgba_len,
            };
        }
        Some(())
    }))
    .ok()
    .flatten()
    .is_some()
}
unsafe extern "C" fn size(
    _: GhosttyTerminal,
    userdata: *mut c_void,
    output: *mut GhosttySizeReportSize,
) -> bool {
    let size = unsafe { (*userdata.cast::<CallbackState>()).size };
    unsafe {
        *output = GhosttySizeReportSize {
            rows: size.rows,
            columns: size.cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        };
    }
    true
}
unsafe extern "C" fn device_attributes(
    _: GhosttyTerminal,
    _: *mut c_void,
    output: *mut GhosttyDeviceAttributes,
) -> bool {
    let mut attributes = GhosttyDeviceAttributes::default();
    attributes.primary.conformance_level = GHOSTTY_DA_CONFORMANCE_VT220 as u16;
    // Only advertise implemented product capabilities. In particular, never
    // advertise Sixel (DA feature 4) merely because the host terminal has it.
    attributes.primary.features[0] = GHOSTTY_DA_FEATURE_ANSI_COLOR as u16;
    attributes.primary.features[1] = GHOSTTY_DA_FEATURE_CLIPBOARD as u16;
    attributes.primary.num_features = 2;
    attributes.secondary.device_type = GHOSTTY_DA_DEVICE_TYPE_VT220 as u16;
    unsafe {
        *output = attributes;
    }
    true
}
unsafe extern "C" fn version(_: GhosttyTerminal, _: *mut c_void) -> GhosttyString {
    let version = b"bkpr-terminal (libghostty-vt 93967b7f9478e94dcbb42125c8a90f34feb40d75+paste-offer-a7921c7)";
    GhosttyString {
        ptr: version.as_ptr(),
        len: version.len(),
    }
}
