//! Bounded, read-only failure incidents; recording never retries an operation.
use crate::{keyguide::Navigation, metadata};
use bkpr_tui_framework::{Rect, Role, RoleSpan, Theme, UiSurface, toast::ToastId, window};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

const MAX_BYTES: usize = 64 * 1024;
const MAX_RECORDS: usize = 64;
const MAX_DETAILS: usize = 8 * 1024;
const MAX_SUMMARY: usize = 512;
const TRUNCATED: &str = " … [truncated]";

/// Bound formatting of approved technical context before retaining or sanitizing it.
/// This does not redact secrets or authorize source bodies as diagnostic content.
pub(crate) fn technical(arguments: std::fmt::Arguments<'_>) -> String {
    struct Bounded(String);
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let remaining = (MAX_DETAILS - TRUNCATED.len()).saturating_sub(self.0.len());
            let mut end = value.len().min(remaining);
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.0.push_str(&value[..end]);
            if end == value.len() {
                Ok(())
            } else {
                Err(std::fmt::Error)
            }
        }
    }
    let mut output = Bounded(String::new());
    if std::fmt::write(&mut output, arguments).is_err() {
        output.0.push_str(TRUNCATED);
    }
    output.0
}

/// Monotonic identity within one frontend's incident store.
pub(crate) type DiagnosticId = u64;

/// Typed dispatch knowledge, independent of the wording of an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// No mutation result is asserted by this incident.
    NotApplicable,
    /// The operation was not dispatched.
    NotDispatched,
    /// Dispatch may have occurred, but its result cannot be confirmed.
    Unknown,
}

impl Outcome {
    fn description(self) -> &'static str {
        match self {
            Self::NotApplicable => "Outcome: NotApplicable — no mutation result is asserted.",
            Self::NotDispatched => "Outcome: NotDispatched — the operation was not dispatched.",
            Self::Unknown => {
                "Outcome: Unknown — the result could not be confirmed. Do not replay the operation."
            }
        }
    }

    fn role(self) -> Role {
        match self {
            Self::NotApplicable => Role::Muted,
            Self::NotDispatched => Role::Info,
            Self::Unknown => Role::Warning,
        }
    }
}

/// Sanitized technical context retained independently of notification lifetime.
pub(crate) struct Incident {
    /// Store-issued identity; identical text still represents a new incident.
    pub(crate) id: DiagnosticId,
    /// Single-line, control-safe human summary, at most 512 bytes.
    pub(crate) summary: String,
    /// Bounded technical text including explicit typed outcome knowledge.
    pub(crate) details: String,
    /// Original dispatch knowledge, never inferred from text.
    pub(crate) outcome: Outcome,
}

impl Incident {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.summary.capacity() + self.details.capacity()
    }
}

/// At most 64 incidents and 64 toast bindings within a 64 KiB retained-data budget.
/// Callers supply approved error context, never source bodies or credentials.
#[derive(Default)]
pub(crate) struct Store {
    incidents: VecDeque<Incident>,
    toasts: VecDeque<(ToastId, DiagnosticId)>,
    next_id: DiagnosticId,
    incident_bytes: usize,
}

impl Store {
    /// Record a distinct incident, evicting oldest records and their toast bindings.
    /// Panics only if all `u64` incident identities have been exhausted.
    pub(crate) fn record(
        &mut self,
        summary: &str,
        details: &str,
        outcome: Outcome,
    ) -> DiagnosticId {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("diagnostic IDs exhausted");
        let mut summary = bounded(summary, MAX_SUMMARY, false, "");
        if summary.trim().is_empty() {
            summary = "Diagnostic incident".into();
        }
        let mut prefix = String::from(outcome.description());
        prefix.push('\n');
        let details = bounded(details, MAX_DETAILS, true, &prefix);
        let incident = Incident {
            id: self.next_id,
            summary,
            details,
            outcome,
        };
        if self.incidents.len() == MAX_RECORDS {
            self.evict_oldest();
        }
        self.incident_bytes += incident.bytes();
        self.incidents.push_back(incident);
        self.trim();
        self.next_id
    }

    /// Look up an incident without extending its retention or changing authority.
    pub(crate) fn get(&self, id: DiagnosticId) -> Option<&Incident> {
        self.incidents.iter().find(|incident| incident.id == id)
    }

    /// Return the newest retained incident, even after its toast has expired.
    pub(crate) fn latest(&self) -> Option<DiagnosticId> {
        self.incidents.back().map(|incident| incident.id)
    }

    /// Bind a live notification identity to retained details; missing incidents are ignored.
    /// Multiple toasts may refer to an incident; oldest bindings are bounded independently.
    pub(crate) fn bind_toast(&mut self, id: DiagnosticId, toast: ToastId) {
        if self.get(id).is_none() {
            return;
        }
        self.toasts.retain(|(existing, _)| *existing != toast);
        if self.toasts.len() == MAX_RECORDS {
            self.toasts.pop_front();
        }
        self.toasts.push_back((toast, id));
        self.trim();
    }

    /// Resolve a toast action only while its incident and bounded binding survive.
    pub(crate) fn for_toast(&self, toast: ToastId) -> Option<DiagnosticId> {
        self.toasts
            .iter()
            .find_map(|(candidate, id)| (*candidate == toast).then_some(*id))
    }

    fn bytes(&self) -> usize {
        self.incident_bytes
            + (self.incidents.capacity() - self.incidents.len()) * std::mem::size_of::<Incident>()
            + self.toasts.capacity() * std::mem::size_of::<(ToastId, DiagnosticId)>()
            + std::mem::size_of::<Self>()
    }

    fn trim(&mut self) {
        while self.incidents.len() > MAX_RECORDS || self.bytes() > MAX_BYTES {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(incident) = self.incidents.pop_front() {
            self.incident_bytes -= incident.bytes();
            self.toasts.retain(|(_, id)| *id != incident.id);
        }
    }
}

fn bounded(value: &str, limit: usize, multiline: bool, prefix: &str) -> String {
    let available = limit - prefix.len();
    let mut end = value.len().min(available);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let shortened = end < value.len();
    let source = &value[..end];
    let mut text = String::with_capacity(prefix.len() + source.len());
    text.push_str(prefix);
    let mut changed = shortened;
    let mut graphemes = source.graphemes(true).peekable();
    while let Some(grapheme) = graphemes.next() {
        // Do not retain a possibly partial final cluster from the bounded prefix.
        if shortened && graphemes.peek().is_none() {
            break;
        }
        if grapheme == "\n" || grapheme == "\t" || grapheme == "\r\n" {
            text.push(if multiline {
                if grapheme == "\t" { '\t' } else { '\n' }
            } else {
                ' '
            });
        } else if grapheme.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{10eeee}' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        }) {
            changed = true;
        } else {
            text.push_str(grapheme);
        }
    }
    if changed {
        while text.len() + TRUNCATED.len() > limit {
            let last = text[prefix.len()..]
                .grapheme_indices(true)
                .next_back()
                .map(|(index, _)| prefix.len() + index)
                .expect("diagnostic prefix leaves room for truncation marker");
            text.truncate(last);
        }
        text.push_str(TRUNCATED);
    }
    text
}

struct Line {
    text: String,
    role: Role,
}

/// Read-only incident presentation; the parent owns dismissal, input and focus.
pub(crate) struct Viewer {
    /// Requested identity; eviction is displayed rather than substituted with another incident.
    pub(crate) id: DiagnosticId,
    cached: Option<(Option<DiagnosticId>, u16)>,
    rows: Vec<Line>,
    scroll: usize,
    page: usize,
}

impl Viewer {
    /// Open the selected incident at its summary, without retaining backend resources.
    pub(crate) fn new(id: DiagnosticId) -> Self {
        Self {
            id,
            cached: None,
            rows: Vec::new(),
            scroll: 0,
            page: 1,
        }
    }

    /// Scroll within cached content; page size is caller-supplied visible content rows.
    pub(crate) fn navigate(&mut self, navigation: Navigation, page: usize) {
        let page = page.max(1);
        let last = self.rows.len().saturating_sub(page);
        self.scroll = match navigation {
            Navigation::Previous => self.scroll.saturating_sub(1),
            Navigation::Next => self.scroll.saturating_add(1),
            Navigation::First => 0,
            Navigation::Last => last,
            Navigation::PagePrevious => self.scroll.saturating_sub(page),
            Navigation::PageNext => self.scroll.saturating_add(page),
        }
        .min(last);
        self.page = page;
    }

    /// Paint cached, grapheme-safe diagnostic text in the surface's current bounds.
    /// Preserve its cursor and resources; this UI owns neither terminal input nor output.
    pub(crate) fn paint(
        &mut self,
        surface: &mut UiSurface,
        theme: &Theme,
        incident: Option<&Incident>,
    ) {
        let size = surface.frame.size;
        let body = window::content(size);
        let incident = incident.filter(|incident| incident.id == self.id);
        self.reflow(body.width, incident);
        self.page = usize::from(body.height).max(1);
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(self.page));
        let mut lines = window::border_rows(size, " Diagnostics ", window::Border::Rounded);
        let mut spans = window::theme_spans(size).to_vec();
        for (offset, line) in self
            .rows
            .iter()
            .skip(self.scroll)
            .take(self.page)
            .enumerate()
        {
            let rect = Rect {
                x: body.x,
                y: body.y.saturating_add(offset as u16),
                width: body.width,
                height: 1,
            };
            lines.push((rect, line.text.clone(), false));
            spans.push(RoleSpan {
                rect,
                role: line.role,
            });
        }
        let cursor = surface.frame.cursor.clone();
        surface.paint(size, lines, Some(cursor));
        surface.apply_theme(theme, Role::Panel, &spans);
    }

    fn reflow(&mut self, width: u16, incident: Option<&Incident>) {
        let key = (incident.map(|incident| incident.id), width);
        if self.cached == Some(key) {
            return;
        }
        let old_last = self.rows.len().saturating_sub(self.page);
        let old_scroll = self.scroll;
        let same_incident = self.cached.is_some_and(|cached| cached.0 == key.0);
        self.cached = Some(key);
        self.rows.clear();
        if width > 0 {
            let mut paragraphs = Vec::new();
            if let Some(incident) = incident {
                paragraphs.push(metadata::Paragraph::new(&incident.summary, Role::Title));
                paragraphs.push(metadata::Paragraph::new("", Role::Panel));
                paragraphs.push(metadata::Paragraph::new(
                    incident.outcome.description(),
                    incident.outcome.role(),
                ));
                paragraphs.push(metadata::Paragraph::new("", Role::Panel));
                paragraphs.push(metadata::Paragraph::new(
                    "Technical diagnostics",
                    Role::Title,
                ));
                for line in incident.details.split('\n') {
                    paragraphs.push(metadata::Paragraph::new(
                        line.replace('\t', "    "),
                        Role::Panel,
                    ));
                }
                if incident.details.trim() == incident.outcome.description() {
                    paragraphs.push(metadata::Paragraph::new(
                        "No further technical details were recorded.",
                        Role::Muted,
                    ));
                }
            } else {
                paragraphs.push(metadata::Paragraph::new(
                    "Diagnostic no longer available",
                    Role::Title,
                ));
                paragraphs.push(metadata::Paragraph::new(
                    "This incident is no longer retained in bounded history. Its operation result is not known from this viewer.",
                    Role::Muted,
                ));
            }
            let mut wrapped = Vec::new();
            metadata::wrap(&paragraphs, width, &mut wrapped);
            self.rows.extend(wrapped.into_iter().map(|line| Line {
                text: line.text,
                role: line.role,
            }));
        }
        let last = self.rows.len().saturating_sub(self.page);
        self.scroll = if same_incident && old_last > 0 {
            old_scroll.saturating_mul(last) / old_last
        } else {
            0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::Size;
    use bkpr_tui_framework::text_width;

    fn surface(cols: u16, rows: u16) -> UiSurface {
        UiSurface::new(Size {
            cols,
            rows,
            cell_width: 8,
            cell_height: 16,
        })
    }

    fn visible(surface: &UiSurface) -> String {
        surface
            .frame
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn identical_operations_remain_distinct_and_ids_survive_eviction() {
        let mut store = Store::default();
        assert_eq!(store.latest(), None);
        let first = store.record("Failed", "Receipt missing", Outcome::Unknown);
        let second = store.record("Failed", "Receipt missing", Outcome::Unknown);
        assert_ne!(first, second);
        for _ in 0..MAX_RECORDS {
            store.record("Another operation", "", Outcome::NotApplicable);
        }
        assert!(store.get(first).is_none());
        assert!(store.get(second).is_none());
        assert_eq!(store.incidents.len(), MAX_RECORDS);
        assert!(store.latest().unwrap() > second);
    }

    #[test]
    fn byte_budget_evicts_records_and_their_toast_actions() {
        let mut store = Store::default();
        let first = store.record("Image failed", "context", Outcome::NotApplicable);
        store.bind_toast(first, 10);
        store.bind_toast(first, 11);
        assert_eq!(store.for_toast(10), Some(first));
        assert_eq!(store.for_toast(11), Some(first));
        for _ in 0..MAX_RECORDS {
            let id = store.record(&"s".repeat(1024), &"d".repeat(16384), Outcome::Unknown);
            store.bind_toast(id, id + 100);
            assert!(store.bytes() <= MAX_BYTES);
        }
        assert!(store.incidents.len() < MAX_RECORDS);
        assert!(store.get(first).is_none());
        assert_eq!(store.for_toast(10), None);
        assert_eq!(store.for_toast(11), None);
        assert!(store.get(store.latest().unwrap()).is_some());
    }

    #[test]
    fn toast_rebinding_and_binding_capacity_never_leave_dangling_ids() {
        let mut store = Store::default();
        let first = store.record("First", "", Outcome::NotApplicable);
        let second = store.record("Second", "", Outcome::NotApplicable);
        store.bind_toast(first, 1);
        store.bind_toast(second, 1);
        assert_eq!(store.for_toast(1), Some(second));
        store.bind_toast(u64::MAX, 1);
        assert_eq!(store.for_toast(1), Some(second));
        for toast in 2..100 {
            store.bind_toast(first, toast);
        }
        assert_eq!(store.toasts.len(), MAX_RECORDS);
        assert_eq!(store.for_toast(1), None);
        assert_eq!(store.for_toast(99), Some(first));
    }

    #[test]
    fn unicode_and_controls_are_bounded_without_splitting_graphemes() {
        let mut store = Store::default();
        let cluster = "👩🏽‍💻e\u{301}";
        let id = store.record(
            &cluster.repeat(512),
            &format!(
                "stage\n\tcontext\u{1b}\u{7}\u{202e}\n{}",
                cluster.repeat(1024)
            ),
            Outcome::Unknown,
        );
        let incident = store.get(id).unwrap();
        assert!(incident.summary.len() <= MAX_SUMMARY);
        assert!(incident.details.len() <= MAX_DETAILS);
        assert!(incident.summary.ends_with(TRUNCATED));
        assert!(incident.details.ends_with(TRUNCATED));
        assert!(incident.details.contains("stage\n\tcontext"));
        assert!(!incident.details.contains(['\u{1b}', '\u{7}', '\u{202e}']));
        let summary = incident.summary.strip_suffix(TRUNCATED).unwrap();
        assert!(cluster.repeat(512).starts_with(summary));
        assert!(
            summary
                .graphemes(true)
                .all(|glyph| matches!(glyph, "👩🏽‍💻" | "e\u{301}"))
        );
        let id = store.record("one\ntwo\tthree", "\u{7}", Outcome::NotApplicable);
        assert_eq!(store.get(id).unwrap().summary, "one two three");
        assert!(store.get(id).unwrap().details.ends_with(TRUNCATED));
    }

    #[test]
    fn typed_outcomes_override_misleading_error_wording() {
        let mut store = Store::default();
        let unknown = store.record("Not dispatched", "Success", Outcome::Unknown);
        let incident = store.get(unknown).unwrap();
        assert_eq!(incident.outcome, Outcome::Unknown);
        assert!(incident.details.starts_with("Outcome: Unknown"));
        assert!(incident.details.contains("Do not replay"));
        let mut viewer = Viewer::new(unknown);
        let mut surface = surface(100, 20);
        viewer.paint(&mut surface, &Theme::default(), Some(incident));
        assert!(visible(&surface).contains("Outcome: Unknown"));
        let neutral = store.record("Observation failed", "", Outcome::NotApplicable);
        let incident = store.get(neutral).unwrap();
        assert!(incident.details.contains("no mutation result is asserted"));
        assert!(!incident.details.contains("not dispatched"));
        let rejected = store.record("Unknown", "", Outcome::NotDispatched);
        assert!(
            store
                .get(rejected)
                .unwrap()
                .details
                .starts_with("Outcome: NotDispatched")
        );
    }

    #[test]
    fn navigation_reflow_and_eviction_remain_readable() {
        let mut store = Store::default();
        let details = (0..80)
            .map(|n| format!("stage {n}: 👩🏽‍💻 context\n"))
            .collect::<String>();
        let id = store.record("Image could not load", &details, Outcome::Unknown);
        let mut viewer = Viewer::new(id);
        let mut surface = surface(42, 12);
        let theme = Theme::default();
        viewer.paint(&mut surface, &theme, store.get(id));
        assert!(visible(&surface).contains("Image could not load"));
        viewer.navigate(Navigation::PageNext, 10);
        assert_eq!(viewer.scroll, 10);
        viewer.navigate(Navigation::Previous, 10);
        assert_eq!(viewer.scroll, 9);
        viewer.navigate(Navigation::Next, 10);
        viewer.navigate(Navigation::PagePrevious, 10);
        assert_eq!(viewer.scroll, 0);
        viewer.navigate(Navigation::Last, 10);
        viewer.paint(&mut surface, &theme, store.get(id));
        assert!(visible(&surface).contains("stage 79"));
        let mut narrow = self::surface(12, 12);
        viewer.paint(&mut narrow, &theme, store.get(id));
        assert_eq!(viewer.scroll, viewer.rows.len() - 10);
        assert!(viewer.rows.iter().all(|line| text_width(&line.text) <= 10));
        viewer.navigate(Navigation::First, 10);
        assert_eq!(viewer.scroll, 0);
        viewer.paint(&mut surface, &theme, None);
        assert_eq!(viewer.scroll, 0);
        assert!(visible(&surface).contains("Diagnostic no longer available"));
        assert!(!visible(&surface).contains("Image could not load"));
    }

    #[test]
    fn tiny_views_clip_whole_graphemes_and_preserve_cursor_and_resources() {
        let mut store = Store::default();
        let id = store.record("👩🏽‍💻 e\u{301}", "\n\tline", Outcome::Unknown);
        let mut viewer = Viewer::new(id);
        let theme = Theme::default();
        for (cols, rows) in [(0, 0), (1, 1), (2, 2), (3, 3), (8, 4)] {
            let mut surface = surface(cols, rows);
            surface.frame.cursor.visible = true;
            let image = std::sync::Arc::new(bkpr_protocol::Image {
                id: 7,
                generation: 3,
                width: 1,
                height: 1,
                format: bkpr_protocol::PixelFormat::Rgb,
                pixels: vec![12, 34, 56],
            });
            surface.frame.images.push(image.clone());
            surface.frame.placements.push(bkpr_protocol::Placement {
                image_id: 7,
                generation: 3,
                placement_id: 1,
                col: 0,
                row: 0,
                offset_x: 0,
                offset_y: 0,
                width: 1,
                height: 1,
                source_x: 0,
                source_y: 0,
                source_width: 1,
                source_height: 1,
                layer: bkpr_protocol::ImageLayer::BelowText,
                z: 0,
            });
            let cursor = surface.frame.cursor.clone();
            let images = surface.frame.images.clone();
            let placements = surface.frame.placements.clone();
            viewer.paint(&mut surface, &theme, store.get(id));
            assert_eq!(surface.frame.cursor, cursor);
            assert_eq!(surface.frame.images, images);
            assert!(std::sync::Arc::ptr_eq(&surface.frame.images[0], &image));
            assert_eq!(surface.frame.placements, placements);
            assert_eq!(surface.frame.rows.len(), usize::from(rows));
            let width = window::content(surface.frame.size).width;
            assert!(
                viewer
                    .rows
                    .iter()
                    .all(|line| text_width(&line.text) <= usize::from(width))
            );
        }
    }
}

#[cfg(test)]
mod formatting_tests {
    use super::*;

    #[test]
    fn oversized_technical_formatting_stops_early_and_marks_utf8_truncation() {
        struct Oversized;
        impl std::fmt::Display for Oversized {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                for _ in 0..100_000 {
                    formatter.write_str("λ")?;
                }
                panic!("bounded formatting must stop before consuming the full source");
            }
        }
        let details = technical(format_args!("Stage: fixture\n{Oversized}"));
        assert!(details.starts_with("Stage: fixture\n"));
        assert!(details.ends_with(TRUNCATED));
        assert!(details.len() <= MAX_DETAILS);
    }
}
