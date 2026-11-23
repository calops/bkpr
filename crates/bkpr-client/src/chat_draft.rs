use bkpr_protocol::{KeyEvent, channels::*};
use bkpr_tui_framework::{EditAction, TextInput, markdown, safe_text, text_width};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

const COMPLETIONS: usize = 5;

struct Mention {
    bytes: Range<usize>,
    binding: ChannelMentionBinding,
    label: String,
}

pub(super) struct Completion {
    start: usize,
    selected: usize,
}

pub(super) struct Draft {
    pub input: TextInput,
    mentions: Vec<Mention>,
    pub completion: Option<Completion>,
    pub reply_to: Option<MessageId>,
    pub error: Option<String>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            input: TextInput::multiline(MAX_POST_BYTES),
            mentions: Vec::new(),
            completion: None,
            reply_to: None,
            error: None,
        }
    }
}

impl Draft {
    pub(super) fn insert(&mut self, value: &str, typed: bool) -> bool {
        let at = self.input.cursor;
        let before = self.input.value.len();
        if !typed {
            self.completion = None;
        }
        if !self.input.insert(value) {
            return false;
        }
        self.error = None;
        self.splice(at..at, self.input.value.len() - before);
        if typed && value == "@" && self.mentions.len() < MAX_MENTIONS {
            let boundary = self.input.value[..at]
                .chars()
                .next_back()
                .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | ':' | ','));
            if boundary && allows_mention(&self.input.value, at) {
                self.completion = Some(Completion { start: at, selected: 0 });
                return true;
            }
        }
        self.update_query();
        false
    }

    pub(super) fn edit(&mut self, action: EditAction) {
        let at = self.input.cursor;
        let removed = match action {
            EditAction::Backspace => self.input.value[..at]
                .grapheme_indices(true)
                .next_back()
                .map(|(offset, _)| offset..at),
            EditAction::Delete => self.input.value[at..]
                .graphemes(true)
                .next()
                .map(|value| at..at + value.len()),
            EditAction::Clear => Some(0..self.input.value.len()),
            _ => None,
        };
        if self.input.edit(action) {
            self.error = None;
            if let Some(removed) = removed {
                self.splice(removed, 0);
            }
        }
        if !matches!(action, EditAction::Backspace) {
            self.completion = None;
        }
        self.update_query();
    }

    fn splice(&mut self, removed: Range<usize>, inserted: usize) {
        self.mentions.retain_mut(|mention| {
            if mention.bytes.end <= removed.start {
                true
            } else if mention.bytes.start >= removed.end {
                let delta = inserted as isize - removed.len() as isize;
                mention.bytes.start = mention.bytes.start.saturating_add_signed(delta);
                mention.bytes.end = mention.bytes.end.saturating_add_signed(delta);
                true
            } else {
                false
            }
        });
    }

    fn query(&self) -> Option<&str> {
        let start = self.completion.as_ref()?.start;
        let source = self.input.value.get(start..self.input.cursor)?.strip_prefix('@')?;
        (source.len() <= MAX_NAME_BYTES && !source.contains('\n')).then_some(source)
    }

    fn update_query(&mut self) {
        if self.query().is_none() {
            self.completion = None;
        } else if let Some(completion) = &mut self.completion {
            completion.selected = 0;
        }
    }

    fn matches<'a>(&self, roster: &'a [ChannelRosterEntry]) -> impl Iterator<Item = &'a ChannelRosterEntry> {
        let query = self.query().map(str::to_lowercase);
        roster.iter().filter(move |entry| {
            query.as_ref().is_some_and(|query| {
                entry.binding.is_some() && (query.is_empty()
                    || entry.participant.display_name.to_lowercase().contains(query)
                    || entry.participant.id.0.contains(query))
            })
        })
    }

    pub(super) fn move_completion(&mut self, delta: isize, roster: &[ChannelRosterEntry]) {
        let count = self.matches(roster).count();
        if let Some(completion) = &mut self.completion {
            completion.selected = completion.selected.saturating_add_signed(delta).min(count.saturating_sub(1));
        }
    }

    pub(super) fn choices<'a>(&self, roster: &'a [ChannelRosterEntry]) -> impl Iterator<Item = (usize, &'a ChannelRosterEntry, bool)> {
        let selected = self.completion.as_ref().map_or(0, |completion| completion.selected);
        self.matches(roster)
            .enumerate()
            .skip(selected.saturating_sub(COMPLETIONS - 1))
            .take(COMPLETIONS)
            .map(move |(index, entry)| (index, entry, index == selected))
    }

    pub(super) fn select_mention(&mut self, roster: &[ChannelRosterEntry], index: Option<usize>) {
        let Some(completion) = &self.completion else { return };
        let Some(entry) = self.matches(roster).nth(index.unwrap_or(completion.selected)) else { return };
        let value = format!("@{}", safe_text(&entry.participant.display_name));
        let range = completion.start..self.input.cursor;
        if self.input.value.len() - range.len() + value.len() + 1 > MAX_POST_BYTES {
            self.error = Some("Mention would exceed the message size limit".into());
            return;
        }
        let binding = entry.binding.as_ref().expect("eligible selection").clone();
        self.input.value.replace_range(range.clone(), &value);
        self.input.cursor = range.start + value.len();
        self.input.value.insert(self.input.cursor, ' ');
        self.input.cursor += 1;
        self.splice(range.clone(), value.len() + 1);
        self.mentions.push(Mention {
            bytes: range.start..self.input.cursor - 1,
            binding,
            label: value,
        });
        self.completion = None;
        self.error = None;
    }

    pub(super) fn selection_label(entry: &ChannelRosterEntry) -> String {
        let identity = entry.binding.as_ref().map_or(entry.participant.id.as_ref(), |binding| binding.launch_id.as_str());
        let short: String = identity.chars().take(8).collect();
        format!("@{} · {}", safe_text(&entry.participant.display_name), safe_text(&short))
    }

    pub(super) fn wake_preview(&self, policy: WakePolicy) -> String {
        let labels = || self.mentions.iter().map(|mention| mention.label.as_str()).collect::<Vec<_>>().join(", ");
        match policy {
            WakePolicy::None => "Wakes: nobody".into(),
            WakePolicy::All if !self.mentions.is_empty() => format!("Wakes: all eligible · addressed: {}", labels()),
            WakePolicy::All | WakePolicy::AddressedOrAll if self.mentions.is_empty() => "Wakes: all eligible".into(),
            WakePolicy::AddressedOnly if self.mentions.is_empty() => "Wakes: nobody (no selected mention)".into(),
            _ => format!("Wakes: {} only", labels()),
        }
    }

    pub(super) fn post(&self, channel_id: ChannelId) -> Result<ChannelRequest, &'static str> {
        if self.input.value.trim().is_empty() {
            return Err("Write a message first");
        }
        let mut bindings = Vec::<ChannelMentionBinding>::new();
        if !self.mentions.is_empty() {
            let document = markdown::Document::parse(&self.input.value);
            let rows = document.wrap(MAX_POST_BYTES);
            for mention in &self.mentions {
                if !allows_in_rows(&rows, mention.bytes.start) {
                    return Err("A selected mention is now in code or a quote. Remove it before sending.");
                }
                if let Some(previous) = bindings.iter().find(|binding| binding.participant_id == mention.binding.participant_id) {
                    if previous != &mention.binding {
                        return Err("The selected mentions refer to different bindings. Pick the agent again.");
                    }
                } else {
                    bindings.push(mention.binding.clone());
                }
            }
        }
        Ok(ChannelRequest::PostUser {
            channel_id,
            post: ChannelPost {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                text: self.input.value.clone(),
                intent: PostIntent::Note,
                mentions: bindings.iter().map(|binding| binding.participant_id.clone()).collect(),
                reply_to: self.reply_to.clone(),
                attachments: Vec::new(),
                source: None,
            },
            bindings,
        })
    }

    pub(super) fn lines(&self, columns: usize) -> (Vec<Range<usize>>, usize) {
        let columns = columns.max(1);
        let mut lines = Vec::new();
        let mut start = 0;
        let mut width = 0;
        for (index, value) in self.input.value.grapheme_indices(true) {
            if value == "\n" {
                lines.push(start..index);
                start = index + 1;
                width = 0;
                continue;
            }
            let cells = text_width(value);
            if width > 0 && width + cells > columns {
                lines.push(start..index);
                start = index;
                width = 0;
            }
            width += cells;
        }
        lines.push(start..self.input.value.len());
        let line = lines.partition_point(|line| line.start <= self.input.cursor).saturating_sub(1);
        (lines, line)
    }

    pub(super) fn highlighted(&self, line: &Range<usize>) -> impl Iterator<Item = Range<usize>> + '_ {
        let line = line.clone();
        self.mentions.iter().filter_map(move |mention| {
            let start = line.start.max(mention.bytes.start);
            let end = line.end.min(mention.bytes.end);
            (start < end).then_some(start..end)
        })
    }
}

fn allows_mention(text: &str, at: usize) -> bool {
    allows_in_rows(&markdown::Document::parse(text).wrap(MAX_POST_BYTES), at)
}

fn allows_in_rows(rows: &[markdown::Row<'_>], at: usize) -> bool {
    rows.iter().flat_map(|row| &row.runs).any(|run| {
        run.source.as_ref().is_some_and(|range| range.contains(&at))
            && !run.style.inline_code && !run.style.fenced_code && !run.style.quote && !run.style.link
    })
}

pub(super) fn edit_action(event: &KeyEvent) -> Option<EditAction> {
    match (event.key.as_str(), event.modifiers & 15) {
        ("ArrowLeft", 0) => Some(EditAction::Left),
        ("ArrowRight", 0) => Some(EditAction::Right),
        ("Home", 0) => Some(EditAction::Home),
        ("End", 0) => Some(EditAction::End),
        ("Backspace", 0) => Some(EditAction::Backspace),
        ("Delete", 0) => Some(EditAction::Delete),
        ("KeyU", 2) => Some(EditAction::Clear),
        _ => None,
    }
}
