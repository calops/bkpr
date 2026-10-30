use super::*;

fn key_code(key: &str) -> Result<GhosttyKey> {
    if let Some(letter) = key
        .strip_prefix("Key")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_uppercase())
    {
        return Ok(GHOSTTY_KEY_A + i32::from(letter.as_bytes()[0] - b'A'));
    }
    if let Some(digit) = key
        .strip_prefix("Digit")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
    {
        return Ok(GHOSTTY_KEY_DIGIT_0 + i32::from(digit.as_bytes()[0] - b'0'));
    }
    if let Some(digit) = key
        .strip_prefix("Numpad")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
    {
        return Ok(GHOSTTY_KEY_NUMPAD_0 + i32::from(digit.as_bytes()[0] - b'0'));
    }
    if let Some(number) = key
        .strip_prefix('F')
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|n| (1..=25).contains(n))
    {
        return Ok(GHOSTTY_KEY_F1 + number - 1);
    }
    Ok(match key {
        "Unidentified" => GHOSTTY_KEY_UNIDENTIFIED,
        "Backquote" => GHOSTTY_KEY_BACKQUOTE,
        "Backslash" => GHOSTTY_KEY_BACKSLASH,
        "BracketLeft" => GHOSTTY_KEY_BRACKET_LEFT,
        "BracketRight" => GHOSTTY_KEY_BRACKET_RIGHT,
        "Comma" => GHOSTTY_KEY_COMMA,
        "Equal" => GHOSTTY_KEY_EQUAL,
        "IntlBackslash" => GHOSTTY_KEY_INTL_BACKSLASH,
        "IntlRo" => GHOSTTY_KEY_INTL_RO,
        "IntlYen" => GHOSTTY_KEY_INTL_YEN,
        "Minus" => GHOSTTY_KEY_MINUS,
        "Period" => GHOSTTY_KEY_PERIOD,
        "Quote" => GHOSTTY_KEY_QUOTE,
        "Semicolon" => GHOSTTY_KEY_SEMICOLON,
        "Slash" => GHOSTTY_KEY_SLASH,
        "AltLeft" => GHOSTTY_KEY_ALT_LEFT,
        "AltRight" => GHOSTTY_KEY_ALT_RIGHT,
        "Backspace" => GHOSTTY_KEY_BACKSPACE,
        "CapsLock" => GHOSTTY_KEY_CAPS_LOCK,
        "ContextMenu" => GHOSTTY_KEY_CONTEXT_MENU,
        "ControlLeft" => GHOSTTY_KEY_CONTROL_LEFT,
        "ControlRight" => GHOSTTY_KEY_CONTROL_RIGHT,
        "Enter" => GHOSTTY_KEY_ENTER,
        "MetaLeft" => GHOSTTY_KEY_META_LEFT,
        "MetaRight" => GHOSTTY_KEY_META_RIGHT,
        "ShiftLeft" => GHOSTTY_KEY_SHIFT_LEFT,
        "ShiftRight" => GHOSTTY_KEY_SHIFT_RIGHT,
        "Space" => GHOSTTY_KEY_SPACE,
        "Tab" => GHOSTTY_KEY_TAB,
        "Convert" => GHOSTTY_KEY_CONVERT,
        "KanaMode" => GHOSTTY_KEY_KANA_MODE,
        "NonConvert" => GHOSTTY_KEY_NON_CONVERT,
        "Delete" => GHOSTTY_KEY_DELETE,
        "End" => GHOSTTY_KEY_END,
        "Help" => GHOSTTY_KEY_HELP,
        "Home" => GHOSTTY_KEY_HOME,
        "Insert" => GHOSTTY_KEY_INSERT,
        "PageDown" => GHOSTTY_KEY_PAGE_DOWN,
        "PageUp" => GHOSTTY_KEY_PAGE_UP,
        "ArrowDown" => GHOSTTY_KEY_ARROW_DOWN,
        "ArrowLeft" => GHOSTTY_KEY_ARROW_LEFT,
        "ArrowRight" => GHOSTTY_KEY_ARROW_RIGHT,
        "ArrowUp" => GHOSTTY_KEY_ARROW_UP,
        "NumLock" => GHOSTTY_KEY_NUM_LOCK,
        "NumpadAdd" => GHOSTTY_KEY_NUMPAD_ADD,
        "NumpadBackspace" => GHOSTTY_KEY_NUMPAD_BACKSPACE,
        "NumpadClear" => GHOSTTY_KEY_NUMPAD_CLEAR,
        "NumpadClearEntry" => GHOSTTY_KEY_NUMPAD_CLEAR_ENTRY,
        "NumpadComma" => GHOSTTY_KEY_NUMPAD_COMMA,
        "NumpadDecimal" => GHOSTTY_KEY_NUMPAD_DECIMAL,
        "NumpadDivide" => GHOSTTY_KEY_NUMPAD_DIVIDE,
        "NumpadEnter" => GHOSTTY_KEY_NUMPAD_ENTER,
        "NumpadEqual" => GHOSTTY_KEY_NUMPAD_EQUAL,
        "NumpadMemoryAdd" => GHOSTTY_KEY_NUMPAD_MEMORY_ADD,
        "NumpadMemoryClear" => GHOSTTY_KEY_NUMPAD_MEMORY_CLEAR,
        "NumpadMemoryRecall" => GHOSTTY_KEY_NUMPAD_MEMORY_RECALL,
        "NumpadMemoryStore" => GHOSTTY_KEY_NUMPAD_MEMORY_STORE,
        "NumpadMemorySubtract" => GHOSTTY_KEY_NUMPAD_MEMORY_SUBTRACT,
        "NumpadMultiply" => GHOSTTY_KEY_NUMPAD_MULTIPLY,
        "NumpadParenLeft" => GHOSTTY_KEY_NUMPAD_PAREN_LEFT,
        "NumpadParenRight" => GHOSTTY_KEY_NUMPAD_PAREN_RIGHT,
        "NumpadSubtract" => GHOSTTY_KEY_NUMPAD_SUBTRACT,
        "NumpadSeparator" => GHOSTTY_KEY_NUMPAD_SEPARATOR,
        "NumpadUp" => GHOSTTY_KEY_NUMPAD_UP,
        "NumpadDown" => GHOSTTY_KEY_NUMPAD_DOWN,
        "NumpadLeft" => GHOSTTY_KEY_NUMPAD_LEFT,
        "NumpadRight" => GHOSTTY_KEY_NUMPAD_RIGHT,
        "NumpadBegin" => GHOSTTY_KEY_NUMPAD_BEGIN,
        "NumpadHome" => GHOSTTY_KEY_NUMPAD_HOME,
        "NumpadEnd" => GHOSTTY_KEY_NUMPAD_END,
        "NumpadInsert" => GHOSTTY_KEY_NUMPAD_INSERT,
        "NumpadDelete" => GHOSTTY_KEY_NUMPAD_DELETE,
        "NumpadPageUp" => GHOSTTY_KEY_NUMPAD_PAGE_UP,
        "NumpadPageDown" => GHOSTTY_KEY_NUMPAD_PAGE_DOWN,
        "Escape" => GHOSTTY_KEY_ESCAPE,
        "Fn" => GHOSTTY_KEY_FN,
        "FnLock" => GHOSTTY_KEY_FN_LOCK,
        "PrintScreen" => GHOSTTY_KEY_PRINT_SCREEN,
        "ScrollLock" => GHOSTTY_KEY_SCROLL_LOCK,
        "Pause" => GHOSTTY_KEY_PAUSE,
        "BrowserBack" => GHOSTTY_KEY_BROWSER_BACK,
        "BrowserFavorites" => GHOSTTY_KEY_BROWSER_FAVORITES,
        "BrowserForward" => GHOSTTY_KEY_BROWSER_FORWARD,
        "BrowserHome" => GHOSTTY_KEY_BROWSER_HOME,
        "BrowserRefresh" => GHOSTTY_KEY_BROWSER_REFRESH,
        "BrowserSearch" => GHOSTTY_KEY_BROWSER_SEARCH,
        "BrowserStop" => GHOSTTY_KEY_BROWSER_STOP,
        "Eject" => GHOSTTY_KEY_EJECT,
        "LaunchApp1" => GHOSTTY_KEY_LAUNCH_APP_1,
        "LaunchApp2" => GHOSTTY_KEY_LAUNCH_APP_2,
        "LaunchMail" => GHOSTTY_KEY_LAUNCH_MAIL,
        "MediaPlayPause" => GHOSTTY_KEY_MEDIA_PLAY_PAUSE,
        "MediaSelect" => GHOSTTY_KEY_MEDIA_SELECT,
        "MediaStop" => GHOSTTY_KEY_MEDIA_STOP,
        "MediaTrackNext" => GHOSTTY_KEY_MEDIA_TRACK_NEXT,
        "MediaTrackPrevious" => GHOSTTY_KEY_MEDIA_TRACK_PREVIOUS,
        "Power" => GHOSTTY_KEY_POWER,
        "Sleep" => GHOSTTY_KEY_SLEEP,
        "AudioVolumeDown" => GHOSTTY_KEY_AUDIO_VOLUME_DOWN,
        "AudioVolumeMute" => GHOSTTY_KEY_AUDIO_VOLUME_MUTE,
        "AudioVolumeUp" => GHOSTTY_KEY_AUDIO_VOLUME_UP,
        "WakeUp" => GHOSTTY_KEY_WAKE_UP,
        "Copy" => GHOSTTY_KEY_COPY,
        "Cut" => GHOSTTY_KEY_CUT,
        "Paste" => GHOSTTY_KEY_PASTE,
        _ => return Err(Error(format!("unsupported physical key code {key}"))),
    })
}
fn encoded(
    mut encode: impl FnMut(*mut std::ffi::c_char, usize, &mut usize) -> GhosttyResult,
) -> Result<Vec<u8>> {
    let mut buffer = vec![0; 256];
    let mut written = 0;
    let mut result = encode(buffer.as_mut_ptr().cast(), buffer.len(), &mut written);
    if result == GHOSTTY_OUT_OF_SPACE {
        if written > 1024 * 1024 {
            return Err(Error("encoded input exceeds 1 MiB".into()));
        }
        buffer.resize(written, 0);
        result = encode(buffer.as_mut_ptr().cast(), buffer.len(), &mut written);
    }
    check(result)?;
    if written > buffer.len() {
        return Err(Error("engine returned invalid input length".into()));
    }
    buffer.truncate(written);
    Ok(buffer)
}
impl Terminal {
    pub fn encode_key(&mut self, event: &KeyEvent) -> Result<Vec<u8>> {
        if event.text.len() > 65536
            || event.key.len() > 128
            || event.modifiers & !0x3ff != 0
            || (event.unshifted_codepoint != 0
                && char::from_u32(event.unshifted_codepoint).is_none())
        {
            return Err(Error("invalid key text/modifiers/codepoint".into()));
        }
        let key = key_code(&event.key)?;
        unsafe {
            ghostty_key_encoder_setopt_from_terminal(self.key_encoder, self.terminal);
            ghostty_key_event_set_key(self.key_event, key);
            ghostty_key_event_set_action(
                self.key_event,
                match event.action {
                    KeyAction::Press => GHOSTTY_KEY_ACTION_PRESS,
                    KeyAction::Release => GHOSTTY_KEY_ACTION_RELEASE,
                    KeyAction::Repeat => GHOSTTY_KEY_ACTION_REPEAT,
                },
            );
            ghostty_key_event_set_mods(self.key_event, event.modifiers);
            ghostty_key_event_set_consumed_mods(self.key_event, 0);
            ghostty_key_event_set_composing(self.key_event, false);
            ghostty_key_event_set_unshifted_codepoint(self.key_event, event.unshifted_codepoint);
            // C0/DEL text is not logical text; let native key+mods derive it.
            let text =
                if event.text.chars().any(|c| {
                    c <= '\u{1f}' || c == '\u{7f}' || ('\u{f700}'..='\u{f8ff}').contains(&c)
                }) {
                    ""
                } else {
                    &event.text
                };
            ghostty_key_event_set_utf8(
                self.key_event,
                if text.is_empty() {
                    ptr::null()
                } else {
                    text.as_ptr().cast()
                },
                text.len(),
            );
            let result = encoded(|buffer, len, written| {
                ghostty_key_encoder_encode(self.key_encoder, self.key_event, buffer, len, written)
            });
            // The reusable native event must not retain a pointer into this
            // caller's text after encoding, including an encoder error.
            ghostty_key_event_set_utf8(self.key_event, ptr::null(), 0);
            result
        }
    }
    pub fn encode_mouse(&mut self, event: &MouseEvent) -> Result<Vec<u8>> {
        if !event.x.is_finite()
            || !event.y.is_finite()
            || event.x < 0.0
            || event.y < 0.0
            || event.x >= (u32::from(self.size.cols) * self.size.cell_width) as f32
            || event.y >= (u32::from(self.size.rows) * self.size.cell_height) as f32
            || event.modifiers & !0x3ff != 0
        {
            return Err(Error("invalid mouse coordinates/modifiers".into()));
        }
        unsafe {
            ghostty_mouse_encoder_setopt_from_terminal(self.mouse_encoder, self.terminal);
            let mut size = sized::<GhosttyMouseEncoderSize>();
            size.screen_width = u32::from(self.size.cols) * self.size.cell_width;
            size.screen_height = u32::from(self.size.rows) * self.size.cell_height;
            size.cell_width = self.size.cell_width;
            size.cell_height = self.size.cell_height;
            ghostty_mouse_encoder_setopt(
                self.mouse_encoder,
                GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                (&size as *const GhosttyMouseEncoderSize).cast(),
            );
            let button = match event.button {
                MouseButton::None => None,
                MouseButton::Left => Some(GHOSTTY_MOUSE_BUTTON_LEFT),
                MouseButton::Middle => Some(GHOSTTY_MOUSE_BUTTON_MIDDLE),
                MouseButton::Right => Some(GHOSTTY_MOUSE_BUTTON_RIGHT),
                MouseButton::WheelUp => Some(GHOSTTY_MOUSE_BUTTON_FOUR),
                MouseButton::WheelDown => Some(GHOSTTY_MOUSE_BUTTON_FIVE),
                MouseButton::WheelLeft => Some(GHOSTTY_MOUSE_BUTTON_SIX),
                MouseButton::WheelRight => Some(GHOSTTY_MOUSE_BUTTON_SEVEN),
                MouseButton::Back => Some(GHOSTTY_MOUSE_BUTTON_EIGHT),
                MouseButton::Forward => Some(GHOSTTY_MOUSE_BUTTON_NINE),
            };
            if let Some(button) = button {
                ghostty_mouse_event_set_button(self.mouse_event, button);
            } else {
                ghostty_mouse_event_clear_button(self.mouse_event);
            }
            ghostty_mouse_event_set_action(
                self.mouse_event,
                match event.action {
                    MouseAction::Press => GHOSTTY_MOUSE_ACTION_PRESS,
                    MouseAction::Release => GHOSTTY_MOUSE_ACTION_RELEASE,
                    MouseAction::Motion => GHOSTTY_MOUSE_ACTION_MOTION,
                },
            );
            ghostty_mouse_event_set_mods(self.mouse_event, event.modifiers);
            ghostty_mouse_event_set_position(
                self.mouse_event,
                GhosttyMousePosition {
                    x: event.x,
                    y: event.y,
                },
            );
            if let Some(button) = button
                .filter(|b| *b <= GHOSTTY_MOUSE_BUTTON_MIDDLE || *b >= GHOSTTY_MOUSE_BUTTON_EIGHT)
            {
                let mask = 1u16 << button;
                match event.action {
                    MouseAction::Press => self.mouse_buttons |= mask,
                    MouseAction::Release => self.mouse_buttons &= !mask,
                    MouseAction::Motion => {}
                }
            }
            let pressed = self.mouse_buttons != 0;
            ghostty_mouse_encoder_setopt(
                self.mouse_encoder,
                GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED,
                (&pressed as *const bool).cast(),
            );
            encoded(|buffer, len, written| {
                ghostty_mouse_encoder_encode(
                    self.mouse_encoder,
                    self.mouse_event,
                    buffer,
                    len,
                    written,
                )
            })
        }
    }
    pub fn encode_focus(&mut self, focused: bool) -> Result<Vec<u8>> {
        let mut mode = GhosttyTerminalModeConfig {
            mode: unsafe { bkpr_terminal_focus_mode() },
            value: false,
        };
        unsafe {
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_MODE,
                out(&mut mode),
            ))?;
            if !mode.value {
                return Ok(Vec::new());
            }
            encoded(|buffer, len, written| {
                ghostty_focus_encode(
                    if focused {
                        GHOSTTY_FOCUS_GAINED
                    } else {
                        GHOSTTY_FOCUS_LOST
                    },
                    buffer,
                    len,
                    written,
                )
            })
        }
    }
    /// Insert the provided text itself. This is not a current-clipboard event:
    /// mode 5522 must never replace explicit bytes with another clipboard.
    pub fn encode_paste(&mut self, text: &[u8]) -> Result<Vec<u8>> {
        if text.len() > CLIPBOARD_MAX_BYTES {
            return Err(Error("paste exceeds clipboard limit".into()));
        }
        let mime = GhosttyString {
            ptr: b"text/plain".as_ptr(),
            len: 10,
        };
        let mut text_ref = text;
        let mut paste = sized::<GhosttyPaste>();
        paste.location = GHOSTTY_CLIPBOARD_LOCATION_STANDARD;
        paste.source = GHOSTTY_PASTE_SOURCE_TEXT;
        paste.mimes = &mime;
        paste.mimes_len = 1;
        paste.reader = GhosttyMimeReader {
            read: Some(read_text),
            userdata: (&mut text_ref as *mut &[u8]).cast(),
        };
        self.run_paste(&paste)
    }
    /// Offer owned MIME contents with a caller-assigned, monotonically
    /// increasing nonzero identity. Native mode 5522 binds that identity to its
    /// validated one-time password. Later reads still require the policy
    /// handler to authorize the originating view, then use these exact bytes.
    /// Without mode 5522, the engine inserts its first text representation.
    pub fn paste_contents(
        &mut self,
        offer_id: u64,
        contents: Vec<ClipboardContent>,
        location: ClipboardLocation,
        allow_unsafe: bool,
    ) -> Result<Vec<u8>> {
        let bytes = contents.iter().fold(0usize, |n, c| {
            n.saturating_add(c.data.len()).saturating_add(c.mime.len())
        });
        if contents.len() > 128 || bytes > CLIPBOARD_MAX_BYTES {
            return Err(Error("paste exceeds clipboard limit".into()));
        }
        if offer_id == 0 || offer_id <= self.last_offer_id {
            return Err(Error(
                "paste offer IDs must be nonzero and never reused".into(),
            ));
        }
        let mut mode = GhosttyTerminalModeConfig {
            mode: unsafe { bkpr_terminal_paste_events_mode() },
            value: false,
        };
        unsafe {
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_MODE,
                out(&mut mode),
            ))?;
        }
        self.callbacks.prune_paste_offers();
        if mode.value {
            let retained = self
                .callbacks
                .paste_offers
                .values()
                .fold(0usize, |n, offer| n.saturating_add(offer.bytes()));
            if self.callbacks.paste_offers.len() >= PASTE_OFFER_MAX_COUNT
                || retained.saturating_add(bytes) > PASTE_OFFER_MAX_BYTES
            {
                return Err(Error("pending paste offer quota exceeded".into()));
            }
        }
        let mimes: Vec<_> = contents
            .iter()
            .map(|c| GhosttyString {
                ptr: c.mime.as_ptr(),
                len: c.mime.len(),
            })
            .collect();
        let mut contents_ref = contents.as_slice();
        let mut paste = sized::<GhosttyPaste>();
        paste.location = match location {
            ClipboardLocation::Standard => GHOSTTY_CLIPBOARD_LOCATION_STANDARD,
            ClipboardLocation::Selection => GHOSTTY_CLIPBOARD_LOCATION_SELECTION,
            ClipboardLocation::Primary => GHOSTTY_CLIPBOARD_LOCATION_PRIMARY,
        };
        paste.source = GHOSTTY_PASTE_SOURCE_CLIPBOARD;
        paste.mimes = mimes.as_ptr();
        paste.mimes_len = mimes.len();
        paste.allow_unsafe = allow_unsafe;
        paste.offer_id = offer_id;
        paste.reader = GhosttyMimeReader {
            read: Some(read_mime),
            userdata: (&mut contents_ref as *mut &[ClipboardContent]).cast(),
        };
        let encoded = self.run_paste(&paste)?;
        self.last_offer_id = offer_id;
        if mode.value && !encoded.is_empty() {
            self.callbacks.paste_offers.insert(
                offer_id,
                std::sync::Arc::new(offers::PasteOffer {
                    created: std::time::Instant::now(),
                    location,
                    contents,
                }),
            );
        }
        Ok(encoded)
    }
    fn run_paste(&mut self, paste: &GhosttyPaste) -> Result<Vec<u8>> {
        unsafe {
            check(ghostty_terminal_paste(
                self.terminal,
                paste,
                ptr::null_mut(),
            ))?;
        }
        if self.callbacks.failed {
            return Err(Error("paste callback output limit exceeded".into()));
        }
        let mut result = Vec::new();
        for effect in self.take_effects() {
            if let Effect::PtyWrite(bytes) = effect {
                if result.is_empty() {
                    result = bytes;
                } else {
                    result.extend_from_slice(&bytes);
                }
            }
        }
        Ok(result)
    }
}
unsafe extern "C" fn read_mime(
    userdata: *mut c_void,
    mime: GhosttyString,
    writer: GhosttyWriter,
) -> bool {
    let contents = unsafe { &*userdata.cast::<&[ClipboardContent]>() };
    let mime = if mime.len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(mime.ptr, mime.len) }
    };
    let Some(content) = contents.iter().find(|c| c.mime.as_bytes() == mime) else {
        return false;
    };
    let Some(write) = writer.write else {
        return false;
    };
    if content.data.is_empty() {
        return true;
    }
    unsafe { write(writer.userdata, content.data.as_ptr(), content.data.len()) }
}
unsafe extern "C" fn read_text(
    userdata: *mut c_void,
    _: GhosttyString,
    writer: GhosttyWriter,
) -> bool {
    let text = unsafe { &*userdata.cast::<&[u8]>() };
    let Some(write) = writer.write else {
        return false;
    };
    if text.is_empty() {
        return true;
    }
    unsafe { write(writer.userdata, text.as_ptr(), text.len()) }
}
