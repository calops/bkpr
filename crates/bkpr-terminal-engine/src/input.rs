use super::*;
use sdk::{
    key::{Key, Mods},
    mouse,
};

fn key_code(key: &str) -> Result<Key> {
    let numeric = if let Some(letter) = key
        .strip_prefix("Key")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_uppercase())
    {
        Some(Key::A as i32 + i32::from(letter.as_bytes()[0] - b'A'))
    } else if let Some(digit) = key
        .strip_prefix("Digit")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
    {
        Some(Key::Digit0 as i32 + i32::from(digit.as_bytes()[0] - b'0'))
    } else if let Some(digit) = key
        .strip_prefix("Numpad")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
    {
        Some(Key::Numpad0 as i32 + i32::from(digit.as_bytes()[0] - b'0'))
    } else {
        key.strip_prefix('F')
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|n| (1..=25).contains(n))
            .map(|n| Key::F1 as i32 + n - 1)
    };
    if let Some(code) = numeric {
        return Key::try_from(code)
            .map_err(|_| Error(format!("unsupported physical key code {key}")));
    }
    Ok(match key {
        "Unidentified" => Key::Unidentified,
        "Backquote" => Key::Backquote,
        "Backslash" => Key::Backslash,
        "BracketLeft" => Key::BracketLeft,
        "BracketRight" => Key::BracketRight,
        "Comma" => Key::Comma,
        "Equal" => Key::Equal,
        "IntlBackslash" => Key::IntlBackslash,
        "IntlRo" => Key::IntlRo,
        "IntlYen" => Key::IntlYen,
        "Minus" => Key::Minus,
        "Period" => Key::Period,
        "Quote" => Key::Quote,
        "Semicolon" => Key::Semicolon,
        "Slash" => Key::Slash,
        "AltLeft" => Key::AltLeft,
        "AltRight" => Key::AltRight,
        "Backspace" => Key::Backspace,
        "CapsLock" => Key::CapsLock,
        "ContextMenu" => Key::ContextMenu,
        "ControlLeft" => Key::ControlLeft,
        "ControlRight" => Key::ControlRight,
        "Enter" => Key::Enter,
        "MetaLeft" => Key::MetaLeft,
        "MetaRight" => Key::MetaRight,
        "ShiftLeft" => Key::ShiftLeft,
        "ShiftRight" => Key::ShiftRight,
        "Space" => Key::Space,
        "Tab" => Key::Tab,
        "Convert" => Key::Convert,
        "KanaMode" => Key::KanaMode,
        "NonConvert" => Key::NonConvert,
        "Delete" => Key::Delete,
        "End" => Key::End,
        "Help" => Key::Help,
        "Home" => Key::Home,
        "Insert" => Key::Insert,
        "PageDown" => Key::PageDown,
        "PageUp" => Key::PageUp,
        "ArrowDown" => Key::ArrowDown,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "ArrowUp" => Key::ArrowUp,
        "NumLock" => Key::NumLock,
        "NumpadAdd" => Key::NumpadAdd,
        "NumpadBackspace" => Key::NumpadBackspace,
        "NumpadClear" => Key::NumpadClear,
        "NumpadClearEntry" => Key::NumpadClearEntry,
        "NumpadComma" => Key::NumpadComma,
        "NumpadDecimal" => Key::NumpadDecimal,
        "NumpadDivide" => Key::NumpadDivide,
        "NumpadEnter" => Key::NumpadEnter,
        "NumpadEqual" => Key::NumpadEqual,
        "NumpadMemoryAdd" => Key::NumpadMemoryAdd,
        "NumpadMemoryClear" => Key::NumpadMemoryClear,
        "NumpadMemoryRecall" => Key::NumpadMemoryRecall,
        "NumpadMemoryStore" => Key::NumpadMemoryStore,
        "NumpadMemorySubtract" => Key::NumpadMemorySubtract,
        "NumpadMultiply" => Key::NumpadMultiply,
        "NumpadParenLeft" => Key::NumpadParenLeft,
        "NumpadParenRight" => Key::NumpadParenRight,
        "NumpadSubtract" => Key::NumpadSubtract,
        "NumpadSeparator" => Key::NumpadSeparator,
        "NumpadUp" => Key::NumpadUp,
        "NumpadDown" => Key::NumpadDown,
        "NumpadLeft" => Key::NumpadLeft,
        "NumpadRight" => Key::NumpadRight,
        "NumpadBegin" => Key::NumpadBegin,
        "NumpadHome" => Key::NumpadHome,
        "NumpadEnd" => Key::NumpadEnd,
        "NumpadInsert" => Key::NumpadInsert,
        "NumpadDelete" => Key::NumpadDelete,
        "NumpadPageUp" => Key::NumpadPageUp,
        "NumpadPageDown" => Key::NumpadPageDown,
        "Escape" => Key::Escape,
        "Fn" => Key::Fn,
        "FnLock" => Key::FnLock,
        "PrintScreen" => Key::PrintScreen,
        "ScrollLock" => Key::ScrollLock,
        "Pause" => Key::Pause,
        "BrowserBack" => Key::BrowserBack,
        "BrowserFavorites" => Key::BrowserFavorites,
        "BrowserForward" => Key::BrowserForward,
        "BrowserHome" => Key::BrowserHome,
        "BrowserRefresh" => Key::BrowserRefresh,
        "BrowserSearch" => Key::BrowserSearch,
        "BrowserStop" => Key::BrowserStop,
        "Eject" => Key::Eject,
        "LaunchApp1" => Key::LaunchApp1,
        "LaunchApp2" => Key::LaunchApp2,
        "LaunchMail" => Key::LaunchMail,
        "MediaPlayPause" => Key::MediaPlayPause,
        "MediaSelect" => Key::MediaSelect,
        "MediaStop" => Key::MediaStop,
        "MediaTrackNext" => Key::MediaTrackNext,
        "MediaTrackPrevious" => Key::MediaTrackPrevious,
        "Power" => Key::Power,
        "Sleep" => Key::Sleep,
        "AudioVolumeDown" => Key::AudioVolumeDown,
        "AudioVolumeMute" => Key::AudioVolumeMute,
        "AudioVolumeUp" => Key::AudioVolumeUp,
        "WakeUp" => Key::WakeUp,
        "Copy" => Key::Copy,
        "Cut" => Key::Cut,
        "Paste" => Key::Paste,
        _ => return Err(Error(format!("unsupported physical key code {key}"))),
    })
}
fn encoded(mut encode: impl FnMut(&mut [u8]) -> sdk::error::Result<usize>) -> Result<Vec<u8>> {
    let mut buffer = vec![0; 256];
    let written = match encode(&mut buffer) {
        Err(sdk::Error::OutOfSpace { required }) => {
            if required > 1024 * 1024 {
                return Err(Error("encoded input exceeds 1 MiB".into()));
            }
            buffer.resize(required, 0);
            encode(&mut buffer)?
        }
        result => result?,
    };
    buffer.truncate(written);
    Ok(buffer)
}
fn native_location(location: ClipboardLocation) -> sdk::terminal::ClipboardLocation {
    match location {
        ClipboardLocation::Standard => sdk::terminal::ClipboardLocation::Standard,
        ClipboardLocation::Selection => sdk::terminal::ClipboardLocation::Selection,
        ClipboardLocation::Primary => sdk::terminal::ClipboardLocation::Primary,
    }
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
        self.key_encoder.set_options_from_terminal(&self.native);
        self.key_event
            .set_key(key_code(&event.key)?)
            .set_action(match event.action {
                KeyAction::Press => sdk::key::Action::Press,
                KeyAction::Release => sdk::key::Action::Release,
                KeyAction::Repeat => sdk::key::Action::Repeat,
            })
            .set_mods(Mods::from_bits_retain(event.modifiers))
            .set_consumed_mods(Mods::empty())
            .set_composing(false)
            .set_unshifted_codepoint(char::from_u32(event.unshifted_codepoint).unwrap_or('\0'));
        let text = if event.text.is_empty()
            || event
                .text
                .chars()
                .any(|c| c <= '\u{1f}' || c == '\u{7f}' || ('\u{f700}'..='\u{f8ff}').contains(&c))
        {
            None
        } else {
            Some(event.text.as_str())
        };
        self.key_event.set_utf8(text);
        let result = encoded(|buffer| self.key_encoder.encode(&self.key_event, buffer));
        self.key_event.set_utf8(None::<String>);
        result
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
        self.mouse_encoder
            .set_options_from_terminal(&self.native)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(self.size.cols) * self.size.cell_width,
                screen_height: u32::from(self.size.rows) * self.size.cell_height,
                cell_width: self.size.cell_width,
                cell_height: self.size.cell_height,
                padding_top: 0,
                padding_bottom: 0,
                padding_left: 0,
                padding_right: 0,
            });
        let button = match event.button {
            MouseButton::None => None,
            MouseButton::Left => Some(mouse::Button::Left),
            MouseButton::Middle => Some(mouse::Button::Middle),
            MouseButton::Right => Some(mouse::Button::Right),
            MouseButton::WheelUp => Some(mouse::Button::Four),
            MouseButton::WheelDown => Some(mouse::Button::Five),
            MouseButton::WheelLeft => Some(mouse::Button::Six),
            MouseButton::WheelRight => Some(mouse::Button::Seven),
            MouseButton::Back => Some(mouse::Button::Eight),
            MouseButton::Forward => Some(mouse::Button::Nine),
        };
        self.mouse_event
            .set_button(button)
            .set_action(match event.action {
                MouseAction::Press => mouse::Action::Press,
                MouseAction::Release => mouse::Action::Release,
                MouseAction::Motion => mouse::Action::Motion,
            })
            .set_mods(Mods::from_bits_retain(event.modifiers))
            .set_position(mouse::Position {
                x: event.x,
                y: event.y,
            });
        if let Some(button) = button.filter(|b| {
            (*b as i32) <= mouse::Button::Middle as i32
                || (*b as i32) >= mouse::Button::Eight as i32
        }) {
            let mask = 1u16 << button as u16;
            match event.action {
                MouseAction::Press => self.mouse_buttons |= mask,
                MouseAction::Release => self.mouse_buttons &= !mask,
                MouseAction::Motion => {}
            }
        }
        self.mouse_encoder
            .set_any_button_pressed(self.mouse_buttons != 0);
        encoded(|buffer| self.mouse_encoder.encode(&self.mouse_event, buffer))
    }
    pub fn encode_focus(&mut self, focused: bool) -> Result<Vec<u8>> {
        if !self.native.mode(sdk::terminal::Mode::FOCUS_EVENT)? {
            return Ok(Vec::new());
        }
        let event = if focused {
            sdk::focus::Event::Gained
        } else {
            sdk::focus::Event::Lost
        };
        encoded(|buffer| event.encode(buffer))
    }
    pub fn encode_paste(&mut self, text: &[u8]) -> Result<Vec<u8>> {
        if text.len() > CLIPBOARD_MAX_BYTES {
            return Err(Error("paste exceeds clipboard limit".into()));
        }
        let options = sdk::paste::Options::new().with_source(sdk::paste::Source::Text);
        self.native.paste(
            options,
            &[sdk::terminal::ClipboardMime::new("text/plain")],
            |_, out| out.write_all(text),
        )?;
        self.paste_output()
    }
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
        let paste_events = self.native.mode(sdk::terminal::Mode::PASTE_EVENTS)?;
        {
            let mut state = self.callbacks.borrow_mut();
            state.prune_paste_offers();
            let retained = state
                .paste_offers
                .values()
                .fold(0usize, |n, offer| n.saturating_add(offer.bytes()));
            if paste_events
                && (state.paste_offers.len() >= PASTE_OFFER_MAX_COUNT
                    || retained.saturating_add(bytes) > PASTE_OFFER_MAX_BYTES)
            {
                return Err(Error("pending paste offer quota exceeded".into()));
            }
        }
        let mimes: Vec<_> = contents
            .iter()
            .map(|c| sdk::terminal::ClipboardMime::new(&c.mime))
            .collect();
        let options = sdk::paste::Options::new()
            .with_location(native_location(location))
            .with_allow_unsafe(allow_unsafe)
            .with_offer_id(offer_id);
        self.native.paste(options, &mimes, |mime, out| {
            let content = contents
                .iter()
                .find(|c| c.mime == mime)
                .ok_or_else(|| std::io::Error::other("unoffered MIME type"))?;
            out.write_all(&content.data)
        })?;
        let output = self.paste_output()?;
        self.last_offer_id = offer_id;
        if paste_events && !output.is_empty() {
            self.callbacks.borrow_mut().paste_offers.insert(
                offer_id,
                Arc::new(offers::PasteOffer {
                    created: std::time::Instant::now(),
                    location,
                    contents,
                }),
            );
        }
        Ok(output)
    }
    fn paste_output(&mut self) -> Result<Vec<u8>> {
        if self.callbacks.borrow().failed {
            return Err(Error("paste callback output limit exceeded".into()));
        }
        let mut output = Vec::new();
        for effect in self.take_effects() {
            if let Effect::PtyWrite(bytes) = effect {
                if output.is_empty() {
                    output = bytes;
                } else {
                    output.extend_from_slice(&bytes);
                }
            }
        }
        Ok(output)
    }
}
