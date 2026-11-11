use super::{Cow, Result, Write, parse_rgb};

/// A client-selected canvas, only while its associated material is ready.
/// `original` is the logical host/pane default, never the derived matte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalBackground {
    /// Logical host default RGB before applying the derived canvas.
    pub original: [u8; 3],
    /// Derived canvas RGB to display while its material is ready.
    pub matte: [u8; 3],
}

#[derive(Default)]
pub(super) struct Background {
    // Retain XParseColor spelling and precision, not just normalized RGB.
    original: Option<String>,
    surface: Option<[u8; 3]>,
    queried_surface: Option<([u8; 3], String)>,
    // A late real theme reply may need to correct a completed fallback restore.
    // Keep failed corrective writes retryable without claiming an active matte.
    pending_restore: bool,
}

impl Background {
    pub(super) fn prepare_query(&mut self) {
        self.queried_surface = self.surface.zip(self.original.clone());
    }

    pub(super) fn surface(&self) -> Option<[u8; 3]> {
        self.surface
    }

    pub(super) fn observe<'a>(
        &mut self,
        reply: &'a [u8],
        out: &mut impl Write,
    ) -> Result<Cow<'a, [u8]>> {
        let Some(value) = reply.strip_prefix(b"11;") else {
            return Ok(Cow::Borrowed(reply));
        };
        let Ok(value) = std::str::from_utf8(value) else {
            return Ok(Cow::Borrowed(reply));
        };
        let Some(rgb) = parse_rgb(value) else {
            return Ok(Cow::Borrowed(reply));
        };
        // Resolve a late reply against the physical default at query time,
        // including replies arriving after fallback restored the host.
        let queried_surface = self.queried_surface.take();
        let restored_during_query = queried_surface.is_some() && self.surface.is_none();
        if let Some((surface, original)) = queried_surface
            && rgb == surface
        {
            return Ok(Cow::Owned(format!("11;{original}").into_bytes()));
        }
        if self.surface == Some(rgb) {
            if let Some(original) = &self.original {
                return Ok(Cow::Owned(format!("11;{original}").into_bytes()));
            }
        } else {
            if self.original.as_deref() != Some(value) {
                self.original = Some(value.to_owned());
            }
            // Theme resets may restore even an unchanged original RGB. Reapply
            // immediately; the client may deduplicate its logical color update.
            if let Some(surface) = self.surface {
                if let Err(error) = write_rgb(out, surface).and_then(|()| out.flush()) {
                    let _ = self.restore(out);
                    return Err(error.into());
                }
            } else if restored_during_query {
                // The query observed a genuine new host theme before fallback
                // restored the old original. Reconcile physical and logical
                // defaults now, without reacquiring graphical ownership.
                self.pending_restore = true;
                self.restore(out)?;
            }
        }
        Ok(Cow::Borrowed(reply))
    }

    pub(super) fn apply(
        &mut self,
        background: Option<TerminalBackground>,
        out: &mut impl Write,
    ) -> Result<()> {
        let Some(background) = background else {
            return self.restore(out);
        };
        self.original
            .get_or_insert_with(|| rgb(background.original));
        if self.surface != Some(background.matte) {
            // Own before writing, so partial-write/error/unwind cleanup can
            // still restore. A failed restore retains ownership for Drop retry.
            self.surface = Some(background.matte);
            if let Err(error) = write_rgb(out, background.matte).and_then(|()| out.flush()) {
                let _ = self.restore(out);
                return Err(error.into());
            }
        }
        Ok(())
    }

    pub(super) fn restore(&mut self, out: &mut impl Write) -> Result<()> {
        if self.surface.is_some() || self.pending_restore {
            if let Some(original) = &self.original {
                write!(out, "\x1b]11;{original}\x1b\\")?;
                out.flush()?;
            }
            self.surface = None;
            self.pending_restore = false;
        }
        Ok(())
    }
}

fn rgb([r, g, b]: [u8; 3]) -> String {
    format!(
        "rgb:{:04x}/{:04x}/{:04x}",
        u16::from(r) * 257,
        u16::from(g) * 257,
        u16::from(b) * 257
    )
}

fn write_rgb(out: &mut impl Write, [r, g, b]: [u8; 3]) -> std::io::Result<()> {
    write!(
        out,
        "\x1b]11;rgb:{:04x}/{:04x}/{:04x}\x1b\\",
        u16::from(r) * 257,
        u16::from(g) * 257,
        u16::from(b) * 257
    )
}

#[cfg(test)]
mod tests {
    use super::super::{ColorProbe, GraphicsState, InputPart, ReplyDemux, TerminalGuard};
    use super::*;

    fn terminal() -> std::mem::ManuallyDrop<TerminalGuard> {
        std::mem::ManuallyDrop::new(TerminalGuard {
            graphics: GraphicsState::new(1000),
            kitty_keyboard: false,
            clipboard_mode: None,
            requested_clipboard_mode: None,
            theme_mode: None,
            background: Background::default(),
        })
    }

    fn receive(
        terminal: &mut TerminalGuard,
        probe: &mut ColorProbe,
        packet: &[u8],
        out: &mut Vec<u8>,
    ) {
        let mut demux = ReplyDemux::default();
        for bytes in packet.chunks(2) {
            for part in demux.feed(bytes) {
                match part {
                    InputPart::Reply(reply) => {
                        let reply = terminal.observe_background_reply(&reply, out).unwrap();
                        probe.feed(&reply);
                    }
                    InputPart::Keys(bytes) => assert!(bytes.is_empty()),
                    InputPart::Mouse { .. } => panic!("background response became mouse input"),
                }
            }
        }
    }

    const CANVAS: TerminalBackground = TerminalBackground {
        original: [17, 34, 51],
        matte: [8, 9, 10],
    };
    const MATTE: &[u8] = b"\x1b]11;rgb:0808/0909/0a0a\x1b\\";

    #[test]
    fn exact_original_survives_owned_echo_and_restores_before_terminal_reset() {
        let mut terminal = terminal();
        let mut probe = ColorProbe::default();
        let mut out = Vec::new();
        terminal.prepare_background_query();
        receive(
            &mut terminal,
            &mut probe,
            b"\x1b]11;rgb:1112/2223/3334\x07",
            &mut out,
        );
        terminal.set_background(&mut out, Some(CANVAS)).unwrap();
        assert_eq!(out, MATTE);
        receive(&mut terminal, &mut probe, MATTE, &mut out);
        assert_eq!(probe.background, Some(CANVAS.original));
        assert_eq!(out, MATTE);
        terminal.prepare_background_query();
        receive(&mut terminal, &mut probe, MATTE, &mut out);
        assert_eq!(probe.background, Some(CANVAS.original));
        assert_eq!(out, MATTE);
        // The same cleanup writer is used by Drop, without writing test bytes
        // into the real test runner's terminal or touching its raw mode.
        terminal.restore_output(&mut out);
        assert!(
            out.starts_with(b"\x1b]11;rgb:0808/0909/0a0a\x1b\\\x1b]11;rgb:1112/2223/3334\x1b\\")
        );
        let reset = b"\x1b[?1049l";
        assert!(out.windows(reset.len()).any(|bytes| bytes == reset));
        assert!(!out.windows(5).any(|bytes| bytes == b"\x1b]10;"));
        assert_eq!(terminal.background.surface(), None);
    }

    #[test]
    fn late_query_echo_after_fallback_cannot_replace_the_original() {
        let mut terminal = terminal();
        let mut probe = ColorProbe::default();
        let mut out = Vec::new();
        receive(&mut terminal, &mut probe, b"\x1b]11;#123\x1b\\", &mut out);
        terminal.set_background(&mut out, Some(CANVAS)).unwrap();
        terminal.prepare_background_query();
        terminal.set_background(&mut out, None).unwrap();
        assert_eq!(&out[MATTE.len()..], b"\x1b]11;#123\x1b\\");
        receive(&mut terminal, &mut probe, MATTE, &mut out);
        assert_eq!(probe.background, Some(CANVAS.original));
        assert_eq!(terminal.background.surface(), None);
        let length = out.len();
        terminal.set_background(&mut out, Some(CANVAS)).unwrap();
        terminal.set_background(&mut out, None).unwrap();
        assert_eq!(&out[length + MATTE.len()..], b"\x1b]11;#123\x1b\\");
    }

    #[test]
    fn fresh_and_initial_late_replies_adopt_real_theme_and_reapply() {
        let mut terminal = terminal();
        let mut probe = ColorProbe::default();
        let mut out = Vec::new();
        terminal.prepare_background_query();
        // The original startup response can arrive after the first material.
        terminal.set_background(&mut out, Some(CANVAS)).unwrap();
        receive(
            &mut terminal,
            &mut probe,
            b"\x1b]11;rgb:1112/2223/3334\x1b\\",
            &mut out,
        );
        assert_eq!(out, [MATTE, MATTE].concat());
        assert_eq!(probe.background, Some(CANVAS.original));
        assert!(probe.theme_changed(b"theme-change;2"));
        terminal.prepare_background_query();
        receive(&mut terminal, &mut probe, b"\x1b]11;#abcdef\x07", &mut out);
        assert_eq!(probe.background, Some([0xab, 0xcd, 0xef]));
        assert_eq!(out, [MATTE, MATTE, MATTE].concat());
        terminal.set_background(&mut out, None).unwrap();
        assert!(out.ends_with(b"\x1b]11;#abcdef\x1b\\"));
    }

    #[test]
    fn failed_partial_apply_restores_and_failed_restore_is_retryable() {
        struct FailsOnce {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for FailsOnce {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                if self.calls == 2 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                let length = bytes.len().min(3);
                self.bytes.extend_from_slice(&bytes[..length]);
                Ok(length)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut terminal = terminal();
        let mut out = FailsOnce {
            bytes: Vec::new(),
            calls: 0,
        };
        assert!(terminal.set_background(&mut out, Some(CANVAS)).is_err());
        assert!(out.bytes.ends_with(b"\x1b]11;rgb:1111/2222/3333\x1b\\"));
        assert_eq!(terminal.background.surface(), None);
        let mut bytes = Vec::new();
        terminal.set_background(&mut bytes, Some(CANVAS)).unwrap();
        out.calls = 1;
        assert!(terminal.set_background(&mut out, None).is_err());
        assert_eq!(terminal.background.surface(), Some(CANVAS.matte));
        terminal.set_background(&mut bytes, None).unwrap();
        assert!(bytes.ends_with(b"\x1b]11;rgb:1111/2222/3333\x1b\\"));
    }

    #[test]
    fn background_ownership_is_per_terminal_and_invalid_replies_do_not_consume_it() {
        let mut a = terminal();
        let mut b = terminal();
        let mut probe = ColorProbe::default();
        let mut out = Vec::new();
        a.set_background(&mut out, Some(CANVAS)).unwrap();
        a.prepare_background_query();
        let length = out.len();
        receive(&mut a, &mut probe, b"\x1b]11;rgb:invalid\x1b\\", &mut out);
        assert_eq!(length, out.len());
        a.set_background(&mut out, None).unwrap();
        receive(&mut a, &mut probe, MATTE, &mut out);
        assert_eq!(probe.background, Some(CANVAS.original));
        receive(&mut b, &mut probe, MATTE, &mut out);
        assert_eq!(probe.background, Some(CANVAS.matte));
    }
}

#[cfg(test)]
mod late_theme_tests {
    use super::super::{ColorProbe, InputPart, ReplyDemux};
    use super::*;
    use bkpr_protocol::{Effect, Size};

    #[test]
    fn delayed_real_theme_query_after_fallback_reconciles_the_physical_host() {
        let size = Size {
            cols: 2,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        };
        let mut host = bkpr_terminal_engine::Terminal::new(size).unwrap();
        host.set_default_colors([255, 255, 255], [17, 34, 51], None)
            .unwrap();
        let mut owner = Background::default();
        let mut logical = ColorProbe::default();
        let mut out = Vec::new();
        let reply = owner.observe(b"11;#123", &mut out).unwrap();
        logical.feed(&reply);
        owner
            .apply(
                Some(TerminalBackground {
                    original: [17, 34, 51],
                    matte: [8, 9, 10],
                }),
                &mut out,
            )
            .unwrap();
        host.feed(&out).unwrap();
        assert_eq!(host.snapshot().unwrap().background, [8, 9, 10]);
        // A real host theme changed before our fresh query. Delay delivery of
        // its actual native OSC response while fallback restores the old color.
        host.feed(b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\").unwrap();
        assert!(logical.theme_changed(b"theme-change;2"));
        owner.prepare_query();
        out.clear();
        out.extend_from_slice(&ColorProbe::query());
        out.flush().unwrap();
        let effects = host.feed(&out).unwrap();
        let replies: Vec<u8> = effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::PtyWrite(bytes) = effect {
                    Some(bytes)
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        out.clear();
        owner.apply(None, &mut out).unwrap();
        assert_eq!(out, b"\x1b]11;#123\x1b\\");
        host.feed(&out).unwrap();
        assert_eq!(host.snapshot().unwrap().background, [17, 34, 51]);
        out.clear();
        let mut demux = ReplyDemux::default();
        let mut expected_restore = Vec::new();
        for part in demux.feed(&replies) {
            if let InputPart::Reply(reply) = part {
                if reply.starts_with(b"11;") {
                    expected_restore.extend_from_slice(b"\x1b]");
                    expected_restore.extend_from_slice(&reply);
                    expected_restore.extend_from_slice(b"\x1b\\");
                }
                let reply = owner.observe(&reply, &mut out).unwrap();
                logical.feed(&reply);
            }
        }
        assert_eq!(logical.background, Some([170, 187, 204]));
        assert_eq!(owner.surface(), None);
        // Corrective write uses the exact reply spelling, not the matte and
        // not a normalized/reconstructed baseline.
        assert!(!expected_restore.is_empty());
        assert_eq!(out, expected_restore);
        host.feed(&out).unwrap();
        assert_eq!(host.snapshot().unwrap().background, [170, 187, 204]);
        out.clear();
        owner.restore(&mut out).unwrap();
        assert!(out.is_empty());
    }
}
