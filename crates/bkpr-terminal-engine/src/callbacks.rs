use super::*;
use sdk::terminal as native;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::LazyLock,
};

type Shared = Rc<RefCell<CallbackState>>;
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn optional(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}
fn guarded(state: &Shared, callback: impl FnOnce() -> Result<()>) {
    if !matches!(catch_unwind(AssertUnwindSafe(callback)), Ok(Ok(()))) {
        state.borrow_mut().failed = true;
    }
}
pub(super) fn install(
    terminal: &mut sdk::Terminal<'static, 'static>,
    shared: &Shared,
    render: &Rc<RefCell<render::Resources>>,
) -> Result<()> {
    let state = Rc::clone(shared);
    terminal.on_pty_write(move |_, bytes| {
        guarded(&state, || {
            if bytes.len() > EFFECT_MAX_BYTES {
                return Err(Error("PTY callback exceeds effect limit".into()));
            }
            state
                .borrow_mut()
                .push(Effect::PtyWrite(bytes.to_vec()), bytes.len());
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    terminal.on_bell(move |_| {
        guarded(&state, || {
            state.borrow_mut().push(Effect::Bell, 0);
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    terminal.on_title_changed(move |terminal| {
        guarded(&state, || {
            let value = text(terminal.title()?);
            let len = value.len();
            state.borrow_mut().push(Effect::Title(value), len);
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    terminal.on_pwd_changed(move |terminal| {
        guarded(&state, || {
            let value = text(terminal.pwd()?);
            let len = value.len();
            state.borrow_mut().push(Effect::Cwd(value), len);
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    let resources = Rc::clone(render);
    terminal.on_render_hold(move |terminal, held| {
        guarded(&state, || {
            if held {
                // Capture while the callback still sees the old image/link state.
                // Subsequent VT may delete those resources before a draw is due.
                let size = state.borrow().size;
                let frame = resources.borrow_mut().capture(terminal, size)?;
                let mut state = state.borrow_mut();
                state.held_frame = Some(frame);
                state.hold_since = Some(std::time::Instant::now());
            } else {
                let mut state = state.borrow_mut();
                state.held_frame = None;
                state.hold_since = None;
            }
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    terminal.on_program_status(move |_, report| {
        guarded(&state, || {
            let name = match report.state()? {
                native::ProgramStatusState::Idle => "idle",
                native::ProgramStatusState::Working => "working",
                native::ProgramStatusState::Done => "done",
                native::ProgramStatusState::Blocked => "blocked",
                native::ProgramStatusState::Error => "error",
                native::ProgramStatusState::Clear => "clear",
                _ => return Ok(()),
            };
            let kind = match report.kind()? {
                native::ProgramStatusKind::Permission => Some("permission".into()),
                native::ProgramStatusKind::Question => Some("question".into()),
                native::ProgramStatusKind::Auth => Some("auth".into()),
                _ => None,
            };
            let id = text(report.id());
            let app = text(report.app());
            let title = text(report.title());
            let message = text(report.message());
            let bytes = id.len() + app.len() + title.len() + message.len();
            state.borrow_mut().push(
                Effect::ProgramStatus {
                    state: name.into(),
                    kind,
                    app: optional(app),
                    message: optional(message),
                    id,
                    title,
                    progress: report.progress().filter(|p| *p <= 100),
                },
                bytes,
            );
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    terminal.on_semantic_prompt(move |_, event| {
        guarded(&state, || {
            if event.kind()? == native::SemanticPromptKind::PromptStart {
                state.borrow_mut().status.prompt_or_exit();
            }
            Ok(())
        })
    })?;
    let state = Rc::clone(shared);
    terminal.on_clipboard_write(move |_, request| {
        let _ = catch_unwind(AssertUnwindSafe(|| clipboard_write(&state, request)));
    })?;
    let state = Rc::clone(shared);
    terminal.on_clipboard_read(move |_, request| {
        let _ = catch_unwind(AssertUnwindSafe(|| clipboard_read(&state, request)));
    })?;
    let state = Rc::clone(shared);
    terminal.on_size(move |_| {
        let size = state.borrow().size;
        Some(native::SizeReportSize {
            rows: size.rows,
            columns: size.cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        })
    })?;
    terminal.on_device_attributes(|_| {
        Some(native::DeviceAttributes {
            primary: native::PrimaryDeviceAttributes::new(
                native::ConformanceLevel::VT220,
                &[
                    native::DeviceAttributeFeature::ANSI_COLOR,
                    native::DeviceAttributeFeature::CLIPBOARD,
                ],
            ),
            secondary: native::SecondaryDeviceAttributes {
                device_type: native::DeviceType::VT220,
                firmware_version: 0,
                rom_cartridge: 0,
            },
            tertiary: native::TertiaryDeviceAttributes::default(),
        })
    })?;
    terminal.on_xtversion(|_| Some("bkpr-terminal-engine (libghostty-vt 93967b7f9478e94dcbb42125c8a90f34feb40d75+paste-offer)"))?;
    Ok(())
}
fn location(value: native::ClipboardLocation) -> ClipboardLocation {
    match value {
        native::ClipboardLocation::Selection => ClipboardLocation::Selection,
        native::ClipboardLocation::Primary => ClipboardLocation::Primary,
        _ => ClipboardLocation::Standard,
    }
}
fn invoke(shared: &Shared, request: ClipboardRequest) -> ClipboardResponse {
    let handler = shared.borrow_mut().clipboard.take();
    if let Some(mut handler) = handler {
        let response = catch_unwind(AssertUnwindSafe(|| handler(request)));
        shared.borrow_mut().clipboard = Some(handler);
        response.unwrap_or_else(|_| ClipboardResponse::denied())
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
fn clipboard_write(shared: &Shared, request: native::ClipboardWrite<'_>) {
    let contents = request.contents();
    if contents.len() > 128 {
        return;
    }
    let mut total = 0usize;
    let mut owned = Vec::with_capacity(contents.len());
    for content in contents {
        total = total
            .saturating_add(content.mime.len())
            .saturating_add(content.data.len());
        if total > CLIPBOARD_MAX_BYTES {
            return;
        }
        // Protocol MIME names are UTF-8 strings. Invalid names are denied rather
        // than lossy-normalized into a different requested representation.
        let Ok(mime) = std::str::from_utf8(content.mime) else {
            return;
        };
        owned.push(ClipboardContent {
            mime: mime.into(),
            data: content.data.to_vec(),
        });
    }
    let can_remember = request.can_remember();
    let answer = invoke(
        shared,
        ClipboardRequest {
            operation: ClipboardOperation::Write { contents: owned },
            location: location(request.location()),
            name: text(request.name()),
            granted: request.granted(),
            can_remember,
            paste_offer_id: None,
        },
    );
    let result = match answer.result {
        ClipboardResult::Success => Ok(()),
        ClipboardResult::Denied => Err(native::ClipboardWriteError::Denied),
        ClipboardResult::Unsupported => Err(native::ClipboardWriteError::Unsupported),
        ClipboardResult::Busy => Err(native::ClipboardWriteError::Busy),
        ClipboardResult::InvalidData => Err(native::ClipboardWriteError::InvalidData),
        ClipboardResult::IoError => Err(native::ClipboardWriteError::IoError),
    };
    request.reply(result, answer.remember && can_remember);
}
fn clipboard_read(shared: &Shared, request: native::ClipboardRead<'_>) {
    let mimes = request.mimes();
    if mimes.len() > 128 {
        return;
    }
    let mut owned = Vec::with_capacity(mimes.len());
    for mime in mimes {
        if mime.len() > 4096 {
            return;
        }
        let Ok(mime) = std::str::from_utf8(mime) else {
            return;
        };
        owned.push(mime.to_owned());
    }
    let offer_id = request.paste_offer_id();
    let offer = if let Some(id) = offer_id {
        let Some(offer) = shared.borrow_mut().paste_offer(id, !owned.is_empty()) else {
            return;
        };
        let offered_location = if offer.location == ClipboardLocation::Selection {
            ClipboardLocation::Primary
        } else {
            offer.location
        };
        if offered_location != location(request.location()) {
            return;
        }
        Some(offer)
    } else {
        None
    };
    let can_remember = request.can_remember();
    let mut answer = invoke(
        shared,
        ClipboardRequest {
            operation: ClipboardOperation::Read {
                mimes: owned,
                list: request.list(),
            },
            location: location(request.location()),
            name: text(request.name()),
            granted: request.granted(),
            can_remember: can_remember && offer.is_none(),
            paste_offer_id: offer_id,
        },
    );
    if let Some(offer) = &offer {
        if offer.expired() {
            answer = ClipboardResponse::denied();
        }
        // Policy grants the original view, never replacement clipboard bytes.
        answer.contents.clear();
        answer.available.clear();
        answer.remember = false;
    } else if !valid_response(&answer) {
        answer = ClipboardResponse::denied();
    }
    let source = offer.as_ref().map_or(answer.contents.as_slice(), |offer| {
        offer.contents.as_slice()
    });
    let contents: Vec<_> = source
        .iter()
        .map(|c| native::ClipboardReplyContent::new(&c.mime, &c.data))
        .collect();
    let available: Vec<_> = if let Some(offer) = &offer {
        offer
            .contents
            .iter()
            .map(|c| native::ClipboardMime::new(&c.mime))
            .collect()
    } else {
        answer
            .available
            .iter()
            .map(|mime| native::ClipboardMime::new(mime))
            .collect()
    };
    let result = match answer.result {
        ClipboardResult::Success => Ok(contents.as_slice()),
        ClipboardResult::Denied | ClipboardResult::InvalidData => {
            Err(native::ClipboardReadError::Denied)
        }
        ClipboardResult::Unsupported => Err(native::ClipboardReadError::Unsupported),
        ClipboardResult::Busy => Err(native::ClipboardReadError::Busy),
        ClipboardResult::IoError => Err(native::ClipboardReadError::IoError),
    };
    // No RefCell borrow survives this synchronous reply: it nests PTY callbacks.
    request.reply(result, &available, answer.remember && can_remember);
}
struct BoundedPng(sdk::kitty::graphics::RustPngDecoder);
impl sdk::kitty::graphics::DecodePng for BoundedPng {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc sdk::alloc::Allocator<'_>,
        data: &[u8],
    ) -> Option<sdk::kitty::graphics::DecodedImage<'alloc>> {
        if data.len() > IMAGE_MAX_BYTES {
            return None;
        }
        // The configured SDK budget rejects the computed RGBA output before
        // allocation or pixel decoding, independently of native allocator caps.
        catch_unwind(AssertUnwindSafe(|| {
            sdk::kitty::graphics::DecodePng::decode_png(&mut self.0, alloc, data)
        }))
        .ok()
        .flatten()
    }
}
pub(super) fn initialize_sys() -> Result<()> {
    static INITIALIZED: LazyLock<std::result::Result<(), String>> = LazyLock::new(|| {
        // Every engine constructor waits for this initialization before any
        // native call; the process-global hook is never changed afterwards.
        unsafe {
            sdk::kitty::graphics::set_png_decoder(Some(Box::new(BoundedPng(
                sdk::kitty::graphics::RustPngDecoder::new().with_max_decoded_bytes(IMAGE_MAX_BYTES),
            ))))
        }
        .map_err(|error| error.to_string())
    });
    INITIALIZED
        .as_ref()
        .map(|_| ())
        .map_err(|error| Error(error.clone()))
}
