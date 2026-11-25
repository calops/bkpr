//! Correlated OSC 99 capabilities, safe notification output and activation replies.
//!
//! The caller owns notification policy and validated source navigation.
//! Successful terminal output is not provider acceptance or proof of display.
use std::{
    fmt,
    io::{self, Write},
    time::{Duration, Instant},
};

use base64::{
    Engine,
    display::Base64Display,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use uuid::Uuid;

pub(crate) const REPLY_PREFIX: &[u8] = b"notification;";

/// Maximum live notifications owned by one physical-terminal attachment.
pub const MAX_ACTIVE: usize = 64;
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REPLY_BYTES: usize = 4096;
const MAX_CHUNK_BYTES: usize = 2048;

/// Opaque, globally unique terminal correlation; not a desktop destination or grant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Id(Uuid);

impl Id {
    /// Create a fresh correlation using the operating system's random source.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse a previously retained UUID correlation, without creating authority.
    pub fn parse(value: &str) -> Option<Self> {
        Uuid::parse_str(value).ok().map(Self)
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Features positively reported by the physical terminal's OSC 99 query response.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    /// Title payloads are supported; false prevents unconfirmed presentation.
    pub title: bool,
    /// Separate body payloads are supported.
    pub body: bool,
    /// Separate image icon payloads are supported.
    pub icon: bool,
    /// Activation can be reported back to the originating terminal application.
    pub activation: bool,
    /// Activation can focus the originating physical terminal window.
    pub focus: bool,
    /// A notification can be explicitly closed by its correlation.
    pub close: bool,
    /// Closure reports can be requested; an OS may still report them as untracked.
    pub close_events: bool,
    /// Low urgency is supported; normal urgency is the protocol default.
    pub low_urgency: bool,
    /// Critical urgency is supported.
    pub critical_urgency: bool,
}

/// A bounded host reply; correlation alone never authorizes navigation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reply {
    /// Capability evidence to be checked against the currently pending query.
    Capabilities {
        /// Echoed query correlation.
        id: Id,
        /// Reported terminal features, not provider delivery evidence.
        capabilities: Capabilities,
    },
    /// The user activated a notification whose source the caller must validate.
    Activated {
        /// Notification correlation supplied by its originating caller.
        id: Id,
    },
    /// Closure tracking changed; this is not application task completion.
    Closed {
        /// Notification correlation supplied by its originating caller.
        id: Id,
        /// False means the terminal cannot observe OS closure, not that it closed.
        tracked: bool,
    },
}

/// One asynchronous physical-terminal capability query, independent of TERM names.
#[derive(Default)]
pub struct Probe {
    id: Option<Id>,
    deadline: Option<Instant>,
    capabilities: Option<Capabilities>,
}

impl Probe {
    /// Issue a fresh query through the existing output owner, without blocking input.
    /// A write error can mean partial output; it is not negative capability evidence.
    pub fn begin(&mut self, out: &mut impl Write, now: Instant) -> io::Result<()> {
        let deadline = now
            .checked_add(QUERY_TIMEOUT)
            .ok_or_else(|| io::Error::other("notification query deadline overflow"))?;
        let id = Id::new();
        self.id = Some(id);
        self.deadline = Some(deadline);
        self.capabilities = None;
        write!(out, "\x1b]99;i={id}:p=?;\x1b\\")
    }

    /// Deadline for the caller's existing event loop; no periodic polling is needed.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Expire a pending query, retaining unknown rather than inventing support.
    pub fn expire(&mut self, now: Instant) {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
        }
    }

    /// Consume only this probe's capability reply; stale replies cannot enable it.
    pub fn consume(&mut self, reply: Reply, now: Instant) -> bool {
        let Reply::Capabilities { id, capabilities } = reply else {
            return false;
        };
        if Some(id) != self.id {
            return false;
        }
        self.expire(now);
        if self.deadline.take().is_some() {
            self.capabilities = Some(capabilities);
        }
        true
    }

    /// Positively confirmed features, or unknown before/after an unanswered query.
    pub fn capabilities(&self) -> Option<Capabilities> {
        self.capabilities
    }
}

/// Requested physical-window focus and application activation reporting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Activation {
    /// Neither focus the terminal nor report activation.
    None,
    /// Report activation without focusing the physical terminal window.
    Report,
    /// Focus the physical terminal window without reporting activation.
    Focus,
    /// Focus the physical terminal and report activation for source navigation.
    ReportAndFocus,
}

/// Requested desktop urgency; no level implies a successful application outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Urgency {
    /// Low-priority desktop presentation.
    Low,
    /// The terminal/OS default priority.
    Normal,
    /// Critical desktop presentation.
    Critical,
}

/// Plain UTF-8 content and caller-selected policy for one terminal notification.
pub struct Notification<'a> {
    /// Stable presentation correlation; reuse only for an intentional update.
    pub id: Id,
    /// Application filter name, at most 256 UTF-8 bytes.
    pub app_name: &'a str,
    /// Plain title; title and body cannot both be empty.
    pub title: &'a str,
    /// Plain body; nonempty content requires confirmed body support.
    pub body: &'a str,
    /// Optional PNG, JPEG, or GIF bytes; omitted when icon support is unconfirmed.
    pub icon: Option<&'a [u8]>,
    /// Activation behavior, requiring the corresponding confirmed capabilities.
    pub activation: Activation,
    /// Request closure reports; requires positively confirmed close-event support.
    pub report_close: bool,
    /// Requested urgency; unsupported non-default levels are rejected.
    pub urgency: Urgency,
}

/// Write a notification through the existing output owner using safe base64 chunks.
/// Validate unsupported policy before output. An I/O error may follow partial output;
/// even a successful write does not acknowledge provider acceptance or display.
pub fn show(
    out: &mut impl Write,
    capabilities: Capabilities,
    notification: &Notification<'_>,
) -> io::Result<()> {
    validate(capabilities, notification)?;
    show_validated(out, capabilities, notification)
}

/// Reject unsupported policy before any terminal output or resource ownership change.
pub(crate) fn validate(
    capabilities: Capabilities,
    notification: &Notification<'_>,
) -> io::Result<()> {
    let unsupported = |message| io::Error::new(io::ErrorKind::Unsupported, message);
    if !capabilities.title {
        return Err(unsupported("terminal notification support is unconfirmed"));
    }
    if notification.app_name.len() > 256
        || (notification.title.is_empty() && notification.body.is_empty())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "notification requires content and an application name of at most 256 bytes",
        ));
    }
    if !notification.body.is_empty() && !capabilities.body {
        return Err(unsupported("terminal does not support notification bodies"));
    }
    let (report, focus) = match notification.activation {
        Activation::None => (false, false),
        Activation::Report => (true, false),
        Activation::Focus => (false, true),
        Activation::ReportAndFocus => (true, true),
    };
    if (report && !capabilities.activation) || (focus && !capabilities.focus) {
        return Err(unsupported(
            "terminal does not support requested activation",
        ));
    }
    if notification.report_close && !capabilities.close_events {
        return Err(unsupported(
            "terminal does not support notification closure reports",
        ));
    }
    match notification.urgency {
        Urgency::Low if capabilities.low_urgency => {}
        Urgency::Normal => {}
        Urgency::Critical if capabilities.critical_urgency => {}
        _ => return Err(unsupported("terminal does not support requested urgency")),
    }
    Ok(())
}

/// Encode a request whose semantics were checked by `validate`.
pub(crate) fn show_validated(
    out: &mut impl Write,
    capabilities: Capabilities,
    notification: &Notification<'_>,
) -> io::Result<()> {
    let action = match notification.activation {
        Activation::None => "-focus",
        Activation::Report => "-focus,report",
        Activation::Focus => "focus",
        Activation::ReportAndFocus => "report,focus",
    };
    let urgency = match notification.urgency {
        Urgency::Low => ":u=0",
        Urgency::Normal => "",
        Urgency::Critical => ":u=2",
    };
    let mut app_name = [0; 344];
    let count = STANDARD_NO_PAD
        .encode_slice(notification.app_name, &mut app_name)
        .expect("bounded application name fits base64 buffer");
    let app_name = std::str::from_utf8(&app_name[..count]).expect("base64 is ASCII");
    if capabilities.icon
        && let Some(icon) = notification.icon
    {
        for chunk in icon.chunks(MAX_CHUNK_BYTES) {
            write!(
                out,
                "\x1b]99;i={}:p=icon:e=1:d=0:a={action};{}\x1b\\",
                notification.id,
                Base64Display::new(chunk, &STANDARD),
            )?;
        }
    }
    for (kind, text, final_payload) in [
        ("title", notification.title, notification.body.is_empty()),
        ("body", notification.body, true),
    ] {
        if kind == "body" && text.is_empty() {
            continue;
        }
        let mut start = 0_usize;
        loop {
            let mut end = start.saturating_add(MAX_CHUNK_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let done = u8::from(final_payload && end == text.len());
            write!(
                out,
                "\x1b]99;i={}:p={kind}:e=1:d={done}:f={app_name}:a={action}:c={}{};{}\x1b\\",
                notification.id,
                u8::from(notification.report_close && capabilities.close_events),
                urgency,
                Base64Display::new(&text.as_bytes()[start..end], &STANDARD),
            )?;
            if end == text.len() {
                break;
            }
            start = end;
        }
    }
    Ok(())
}

/// Request closure of an existing presentation; output is not a closure receipt.
pub fn close(out: &mut impl Write, capabilities: Capabilities, id: Id) -> io::Result<()> {
    if !capabilities.close {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "terminal does not support notification closure",
        ));
    }
    write!(out, "\x1b]99;i={id}:p=close;\x1b\\")
}

/// Decode only the shared demultiplexer's OSC-99-tagged replies, never raw input.
/// Malformed, oversized and unsupported replies are ignored. The caller must still
/// match activation IDs against its live, generation-fenced presentation records.
pub fn parse_reply(packet: &[u8]) -> Option<Reply> {
    if packet.len() > MAX_REPLY_BYTES {
        return None;
    }
    let text = std::str::from_utf8(packet.strip_prefix(REPLY_PREFIX)?).ok()?;
    let (metadata, payload) = text.strip_prefix("99;")?.split_once(';')?;
    let mut id = None;
    let mut kind = None;
    for entry in metadata.split(':') {
        let (key, value) = entry.split_once('=')?;
        match key {
            "i" if id.is_none() => id = Some(Id::parse(value)?),
            "p" if kind.is_none() => kind = Some(value),
            "i" | "p" => return None,
            _ => {}
        }
    }
    let id = id?;
    match kind {
        Some("?") => Some(Reply::Capabilities {
            id,
            capabilities: parse_capabilities(payload)?,
        }),
        Some("close") if payload.is_empty() || payload == "untracked" => Some(Reply::Closed {
            id,
            tracked: payload.is_empty(),
        }),
        None if payload.is_empty() => Some(Reply::Activated { id }),
        _ => None,
    }
}

fn parse_capabilities(payload: &str) -> Option<Capabilities> {
    let mut capabilities = Capabilities::default();
    let mut seen = 0_u8;
    for entry in payload.split(':') {
        let (key, value) = entry.split_once('=')?;
        let bit = match key {
            "p" => 1,
            "a" => 2,
            "c" => 4,
            "u" => 8,
            _ => continue,
        };
        if seen & bit != 0 {
            return None;
        }
        seen |= bit;
        if key == "c" {
            capabilities.close_events = value == "1";
            continue;
        }
        for value in value.split(',') {
            match (key, value) {
                ("p", "title") => capabilities.title = true,
                ("p", "body") => capabilities.body = true,
                ("p", "icon") => capabilities.icon = true,
                ("p", "close") => capabilities.close = true,
                ("a", "report") => capabilities.activation = true,
                ("a", "focus") => capabilities.focus = true,
                ("u", "0") => capabilities.low_urgency = true,
                ("u", "2") => capabilities.critical_urgency = true,
                _ => {}
            }
        }
    }
    capabilities.title.then_some(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputPart, ReplyDemux};

    fn replies(wire: &[u8], split: usize) -> (Vec<u8>, Vec<Reply>) {
        let mut demux = ReplyDemux::default();
        let mut keys = Vec::new();
        let mut replies = Vec::new();
        for bytes in [&wire[..split], &wire[split..]] {
            for part in demux.feed(bytes) {
                match part {
                    InputPart::Keys(bytes) => keys.extend(bytes),
                    InputPart::Reply(bytes) => replies.extend(parse_reply(&bytes)),
                    InputPart::Mouse { .. } => panic!("unexpected mouse event"),
                }
            }
        }
        (keys, replies)
    }

    fn query_reply(id: Id, capabilities: &str) -> Reply {
        let wire = format!("\x1b]99;i={id}:p=?;{capabilities}\x1b\\");
        replies(wire.as_bytes(), 0).1[0]
    }

    #[test]
    fn capability_evidence_is_query_scoped_and_cannot_revive_after_expiry() {
        let now = Instant::now();
        let mut probe = Probe::default();
        probe.begin(&mut Vec::new(), now).unwrap();
        let first = query_reply(
            probe.id.unwrap(),
            "a=report,focus:c=1:p=title,body,close:u=0,1,2",
        );
        assert!(probe.consume(first, now));
        let capabilities = probe.capabilities().unwrap();
        assert!(capabilities.activation && capabilities.focus && capabilities.body);
        assert!(capabilities.close && capabilities.close_events);

        probe.begin(&mut Vec::new(), now).unwrap();
        assert!(!probe.consume(first, now));
        assert_eq!(probe.capabilities(), None);
        let second = query_reply(probe.id.unwrap(), "p=title:a=report");
        assert!(probe.consume(second, now + QUERY_TIMEOUT));
        assert_eq!(probe.capabilities(), None);
        assert!(probe.consume(second, now));
        assert_eq!(probe.capabilities(), None);
    }

    #[test]
    fn activation_is_framed_without_keys_and_cannot_be_forged_by_another_string_kind() {
        let id = Id::new();
        let wire = format!("a\x1b]99;i={id};\x1b\\z");
        for split in 0..=wire.len() {
            let (keys, events) = replies(wire.as_bytes(), split);
            assert_eq!(keys, b"az");
            assert_eq!(events, [Reply::Activated { id }]);
        }
        for wire in [
            format!("a\x1bP99;i={id};\x1b\\z"),
            format!("a\x1b]notification;99;i={id};\x1b\\z"),
            format!("a\x1bPnotification;99;i={id};\x1b\\z"),
        ] {
            for split in 0..=wire.len() {
                let (keys, events) = replies(wire.as_bytes(), split);
                assert_eq!(keys, b"az");
                assert!(events.is_empty());
            }
        }
    }

    #[test]
    fn ambiguous_capabilities_and_unrequested_actions_do_not_create_events() {
        let id = Id::new();
        for fields in ["p=body", "p=title:p=title,body", "p=title:a=report:a=focus"] {
            let wire = format!("\x1b]99;i={id}:p=?;{fields}\x1b\\");
            assert!(replies(wire.as_bytes(), 0).1.is_empty());
        }
        let wire = format!("\x1b]99;i={id};1\x1b\\");
        assert!(replies(wire.as_bytes(), 0).1.is_empty());
        let wire = format!("\x1b]99;i={id}:p=close;untracked\x1b\\");
        assert_eq!(
            replies(wire.as_bytes(), 0).1,
            [Reply::Closed { id, tracked: false }]
        );
    }

    #[test]
    fn unsafe_unicode_content_stays_literal_in_complete_bounded_chunks() {
        let title = format!(
            "{}\x1b]9;;injected\x07🦀{}",
            "x".repeat(2047),
            "é".repeat(1400)
        );
        let body = "body & <literal>\nnext line";
        let notification = Notification {
            id: Id::new(),
            app_name: "Bkpr",
            title: &title,
            body,
            icon: None,
            activation: Activation::ReportAndFocus,
            report_close: false,
            urgency: Urgency::Normal,
        };
        let capabilities = Capabilities {
            title: true,
            body: true,
            activation: true,
            focus: true,
            ..Capabilities::default()
        };
        let mut wire = Vec::new();
        show(&mut wire, capabilities, &notification).unwrap();
        let wire = std::str::from_utf8(&wire).unwrap();
        assert!(wire.starts_with("\x1b]99;"));
        let mut title_bytes = Vec::new();
        let mut body_bytes = Vec::new();
        let mut completed = false;
        for packet in wire.split("\x1b]99;").skip(1) {
            assert!(!completed, "data followed the completed notification");
            let packet = packet.strip_suffix("\x1b\\").unwrap();
            assert!(!packet.contains('\x1b') && !packet.contains('\x07'));
            let (metadata, encoded) = packet.split_once(';').unwrap();
            assert!(metadata.split(':').any(|field| field == "e=1"));
            completed = metadata.split(':').any(|field| field == "d=1");
            let bytes = STANDARD.decode(encoded).unwrap();
            assert!(bytes.len() <= MAX_CHUNK_BYTES);
            std::str::from_utf8(&bytes).unwrap();
            let kind = metadata
                .split(':')
                .find_map(|field| field.strip_prefix("p="))
                .unwrap();
            match kind {
                "title" => title_bytes.extend(bytes),
                "body" => body_bytes.extend(bytes),
                _ => panic!("unexpected payload kind"),
            }
        }
        assert!(completed);
        assert_eq!(title_bytes, title.as_bytes());
        assert_eq!(body_bytes, body.as_bytes());
    }

    #[test]
    fn icon_chunks_precede_completion_and_require_advertised_support() {
        let id = Id::new();
        let icon = vec![0xff; MAX_CHUNK_BYTES + 1];
        let notice = Notification {
            id,
            app_name: "Bkpr",
            title: "Ready",
            body: "",
            icon: Some(&icon),
            activation: Activation::None,
            report_close: false,
            urgency: Urgency::Normal,
        };
        for (features, expected_chunks) in [("p=title", 0), ("p=title,icon", 2)] {
            let mut wire = Vec::new();
            show(&mut wire, parse_capabilities(features).unwrap(), &notice).unwrap();
            let wire = std::str::from_utf8(&wire).unwrap();
            let (last, chunks) = wire
                .trim_end_matches("\x1b\\")
                .rsplit_once("\x1b\\")
                .map_or((wire.trim_end_matches("\x1b\\"), ""), |(head, last)| {
                    (last, head)
                });
            assert!(last.starts_with(&format!("\x1b]99;i={id}:p=title:e=1:d=1:")));
            let prefix = format!("\x1b]99;i={id}:p=icon:e=1:d=0:a=-focus;");
            let mut decoded = Vec::new();
            let mut count = 0;
            for chunk in chunks.split("\x1b\\").filter(|chunk| !chunk.is_empty()) {
                let bytes = STANDARD
                    .decode(chunk.strip_prefix(&prefix).unwrap())
                    .unwrap();
                assert!(!bytes.is_empty() && bytes.len() <= MAX_CHUNK_BYTES);
                decoded.extend(bytes);
                count += 1;
            }
            assert_eq!(count, expected_chunks);
            assert_eq!(decoded, if count == 0 { &[][..] } else { &icon });
        }
    }

    #[test]
    fn unconfirmed_or_unsupported_policy_never_starts_a_partial_notification() {
        let mut notification = Notification {
            id: Id::new(),
            app_name: "Bkpr",
            title: "Needs attention",
            body: "",
            icon: None,
            activation: Activation::None,
            report_close: false,
            urgency: Urgency::Normal,
        };
        let mut wire = b"existing output".to_vec();
        assert_eq!(
            show(&mut wire, Capabilities::default(), &notification)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        let capabilities = Capabilities {
            title: true,
            ..Capabilities::default()
        };
        notification.activation = Activation::ReportAndFocus;
        assert_eq!(
            show(&mut wire, capabilities, &notification)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(wire, b"existing output");
    }

    #[test]
    fn closure_reporting_is_explicit_and_rejected_before_output_when_unsupported() {
        let mut notification = Notification {
            id: Id::new(),
            app_name: "test",
            title: "attention",
            body: "",
            icon: None,
            activation: Activation::None,
            report_close: false,
            urgency: Urgency::Normal,
        };
        let mut capabilities = Capabilities {
            title: true,
            close_events: true,
            ..Capabilities::default()
        };
        let mut wire = Vec::new();
        show(&mut wire, capabilities, &notification).unwrap();
        assert!(std::str::from_utf8(&wire).unwrap().contains(":c=0;"));
        notification.report_close = true;
        wire.clear();
        show(&mut wire, capabilities, &notification).unwrap();
        assert!(std::str::from_utf8(&wire).unwrap().contains(":c=1;"));
        capabilities.close_events = false;
        wire.clear();
        assert_eq!(
            show(&mut wire, capabilities, &notification)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(wire.is_empty());
    }
}
