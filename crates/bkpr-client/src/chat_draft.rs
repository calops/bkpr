use bkpr_protocol::channels::*;
use bkpr_tui_framework::{EditAction, TextInput, markdown, safe_text, text_width};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

struct Mention {
    bytes: Range<usize>,
    binding: ChannelMentionBinding,
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
    pub(super) fn paste(&mut self, text: &str) {
        if text.len() <= 2 * MAX_POST_BYTES && text.contains('\r') {
            // Xterm sends CR for selection line breaks, even in bracketed paste.
            let mut parts = text.split('\r');
            let mut normalized = String::with_capacity(text.len());
            normalized.push_str(parts.next().unwrap_or_default());
            for part in parts {
                normalized.push('\n');
                normalized.push_str(part.strip_prefix('\n').unwrap_or(part));
            }
            self.insert(&normalized, false);
        } else {
            self.insert(text, false);
        }
    }

    pub(super) fn insert(&mut self, value: &str, typed: bool) -> bool {
        if !typed {
            self.completion = None;
        }
        let Some(inserted) = self.input.insert_with_range(value) else {
            return false;
        };
        self.error = None;
        let at = inserted.start;
        self.splice(at..at, inserted.len());
        if typed && value == "@" && self.mentions.len() < MAX_MENTIONS {
            let boundary = self.input.value[..at]
                .chars()
                .next_back()
                .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | ':' | ','));
            if boundary && allows_mention(&self.input.value, at) {
                self.completion = Some(Completion {
                    start: at,
                    selected: 0,
                });
                return true;
            }
        }
        self.update_query();
        false
    }

    pub(super) fn edit(&mut self, action: EditAction) {
        if let Some(removed) = self.input.edit_with_range(action) {
            self.error = None;
            if self
                .completion
                .as_ref()
                .is_some_and(|completion| removed.start <= completion.start)
            {
                self.completion = None;
            }
            self.splice(removed, 0);
        }
        if !matches!(
            action,
            EditAction::Backspace | EditAction::DeletePreviousWord
        ) {
            self.completion = None;
        }
        self.update_query();
    }

    fn splice(&mut self, removed: Range<usize>, inserted: usize) {
        let delta = inserted as isize - removed.len() as isize;
        self.mentions.retain_mut(|mention| {
            if mention.bytes.end <= removed.start {
                true
            } else if mention.bytes.start >= removed.end {
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
        let source = self
            .input
            .value
            .get(start..self.input.cursor)?
            .strip_prefix('@')?;
        (source.len() <= MAX_NAME_BYTES && !source.contains('\n')).then_some(source)
    }

    fn update_query(&mut self) {
        if self.query().is_none() {
            self.completion = None;
        } else if let Some(completion) = &mut self.completion {
            completion.selected = 0;
        }
    }

    fn matches<'a>(
        &self,
        roster: &'a [ChannelRosterEntry],
    ) -> impl Iterator<Item = &'a ChannelRosterEntry> {
        let query = self.query().map(str::to_lowercase);
        roster.iter().filter(move |entry| {
            query.as_ref().is_some_and(|query| {
                entry.binding.is_some()
                    && (query.is_empty()
                        || entry
                            .participant
                            .display_name
                            .to_lowercase()
                            .contains(query)
                        || entry.participant.id.0.contains(query))
            })
        })
    }

    pub(super) fn move_completion(&mut self, delta: isize, roster: &[ChannelRosterEntry]) {
        let count = self.matches(roster).count();
        if let Some(completion) = &mut self.completion {
            completion.selected = completion
                .selected
                .saturating_add_signed(delta)
                .min(count.saturating_sub(1));
        }
    }

    pub(super) fn choices<'a>(
        &self,
        roster: &'a [ChannelRosterEntry],
        capacity: usize,
    ) -> impl Iterator<Item = (usize, &'a ChannelRosterEntry, bool)> {
        let selected = self
            .completion
            .as_ref()
            .map_or(0, |completion| completion.selected);
        self.matches(roster)
            .enumerate()
            .skip(selected.saturating_sub(capacity.saturating_sub(1)))
            .take(capacity)
            .map(move |(index, entry)| (index, entry, index == selected))
    }

    pub(super) fn select_mention(
        &mut self,
        roster: &[ChannelRosterEntry],
        capacity: usize,
        index: Option<usize>,
    ) {
        if capacity == 0 {
            return;
        }
        let Some(completion) = &self.completion else {
            return;
        };
        let Some((_, entry, _)) = self
            .choices(roster, capacity)
            .find(|&(choice, _, chosen)| index.map_or(chosen, |index| choice == index))
        else {
            return;
        };
        let value = format!("@{} ", safe_text(&entry.participant.display_name));
        let range = completion.start..self.input.cursor;
        let Some(inserted) = self.input.replace_range(range.clone(), &value) else {
            self.error = Some("Mention would exceed the message size limit".into());
            return;
        };
        let binding = entry.binding.as_ref().expect("eligible selection").clone();
        self.splice(range, inserted.len());
        self.mentions.push(Mention {
            bytes: inserted.start..inserted.end - 1,
            binding,
        });
        self.completion = None;
        self.error = None;
    }

    pub(super) fn selection_label(entry: &ChannelRosterEntry, index: usize) -> (String, String) {
        let short: String = entry.participant.id.as_ref().chars().take(8).collect();
        (
            format!(
                "{} @{}",
                index + 1,
                safe_text(&entry.participant.display_name)
            ),
            format!(" · {}", safe_text(&short)),
        )
    }

    pub(super) fn completion_wake_preview(&self, policy: WakePolicy) -> Option<&'static str> {
        self.completion.as_ref()?;
        Some(match policy {
            WakePolicy::None => "No wake",
            WakePolicy::All => "Wakes all eligible",
            WakePolicy::AddressedOnly | WakePolicy::AddressedOrAll => "Wakes selected agents",
        })
    }

    pub(super) fn post(&self, channel_id: ChannelId) -> Result<ChannelRequest, &'static str> {
        if self.input.value.trim().is_empty() {
            return Err("Write a message first");
        }
        let mut bindings = Vec::<ChannelMentionBinding>::new();
        let mut mention_spans = Vec::with_capacity(self.mentions.len());
        if !self.mentions.is_empty() {
            let document = markdown::Document::parse(&self.input.value);
            let rows = document.wrap(MAX_POST_BYTES);
            for mention in &self.mentions {
                if !allows_in_rows(&rows, mention.bytes.start) {
                    return Err(
                        "A selected mention is now in code or a quote. Remove it before sending.",
                    );
                }
                if let Some(previous) = bindings
                    .iter()
                    .find(|binding| binding.participant_id == mention.binding.participant_id)
                {
                    if previous != &mention.binding {
                        return Err(
                            "The selected mentions refer to different bindings. Pick the agent again.",
                        );
                    }
                } else {
                    bindings.push(mention.binding.clone());
                }
                mention_spans.push(ChannelMentionSpan {
                    start: mention.bytes.start as u32,
                    end: mention.bytes.end as u32,
                    participant_id: mention.binding.participant_id.clone(),
                });
            }
        }
        mention_spans.sort_unstable_by_key(|span| span.start);
        Ok(ChannelRequest::PostUser {
            channel_id,
            post: ChannelPost {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                text: self.input.value.clone(),
                intent: PostIntent::Note,
                mentions: bindings
                    .iter()
                    .map(|binding| binding.participant_id.clone())
                    .collect(),
                mention_spans,
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
        let line = lines
            .partition_point(|line| line.start <= self.input.cursor)
            .saturating_sub(1);
        (lines, line)
    }

    pub(super) fn move_vertical(&mut self, columns: usize, delta: isize) {
        let (lines, current) = self.lines(columns);
        let index = current.saturating_add_signed(delta).min(lines.len() - 1);
        if current == index {
            return;
        }
        let target = &lines[index];
        let mut cells = text_width(&self.input.value[lines[current].start..self.input.cursor]);
        let mut cursor = target.start;
        for grapheme in self.input.value[target.clone()].graphemes(true) {
            let width = text_width(grapheme);
            if cells < width {
                break;
            }
            cells -= width;
            cursor += grapheme.len();
        }
        self.input.cursor = cursor;
        self.completion = None;
    }

    pub(super) fn highlighted(
        &self,
        line: &Range<usize>,
    ) -> impl Iterator<Item = Range<usize>> + '_ {
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
            && !run.style.inline_code
            && !run.style.fenced_code
            && !run.style.quote
            && !run.style.link
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn twin_roster() -> Vec<ChannelRosterEntry> {
        [("one", "launch-a", 1), ("two", "launch-b", 2)]
            .into_iter()
            .map(|(id, launch, binding_generation)| ChannelRosterEntry {
                participant: Participant {
                    id: ParticipantId(id.into()),
                    kind: ParticipantKind::Agent,
                    display_name: "Twin".into(),
                },
                binding: Some(ChannelMentionBinding {
                    participant_id: ParticipantId(id.into()),
                    launch_id: launch.into(),
                    binding_generation,
                }),
                role: None,
            })
            .collect()
    }

    fn assert_literal(draft: &Draft) {
        let ChannelRequest::PostUser { post, bindings, .. } =
            draft.post(ChannelId("desk".into())).unwrap()
        else {
            panic!("expected user post");
        };
        assert!(post.mentions.is_empty() && bindings.is_empty());
    }

    #[test]
    fn pasted_and_code_at_signs_do_not_pick_agents() {
        let mut pasted = Draft::default();
        pasted.paste("@Twin\rsecond line\r\nthird line");
        assert_eq!(pasted.input.value, "@Twin\nsecond line\nthird line");
        assert!(pasted.completion.is_none());
        assert_literal(&pasted);
        let mut code = Draft::default();
        code.insert("```\n", false);
        assert!(!code.insert("@", true));
        code.insert("Twin\n```", false);
        assert!(code.completion.is_none());
        assert_literal(&code);
    }

    #[test]
    fn wake_scope_only_previews_an_open_mention_choice() {
        let mut draft = Draft::default();
        assert_eq!(draft.completion_wake_preview(WakePolicy::All), None);
        assert!(draft.insert("@", true));
        assert_eq!(
            draft.completion_wake_preview(WakePolicy::None),
            Some("No wake")
        );
        assert_eq!(
            draft.completion_wake_preview(WakePolicy::All),
            Some("Wakes all eligible")
        );
        for policy in [WakePolicy::AddressedOnly, WakePolicy::AddressedOrAll] {
            assert_eq!(
                draft.completion_wake_preview(policy),
                Some("Wakes selected agents")
            );
        }
        draft.select_mention(&twin_roster(), 2, Some(1));
        assert_eq!(
            draft.completion_wake_preview(WakePolicy::AddressedOnly),
            None
        );
    }

    #[test]
    fn matching_titles_choose_one_binding_and_edits_withdraw_the_selection() {
        let roster = twin_roster();
        let mut draft = Draft::default();
        assert!(draft.insert("@", true));
        draft.select_mention(&roster, 2, Some(1));
        let ChannelRequest::PostUser { post, bindings, .. } =
            draft.post(ChannelId("desk".into())).unwrap()
        else {
            panic!("expected user post")
        };
        assert_eq!(post.mentions, [roster[1].participant.id.clone()]);
        assert_eq!(
            post.mention_spans,
            [ChannelMentionSpan {
                start: 0,
                end: "@Twin".len() as u32,
                participant_id: roster[1].participant.id.clone(),
            }]
        );
        assert_eq!(bindings.first(), roster[1].binding.as_ref());
        draft.edit(EditAction::Backspace);
        draft.edit(EditAction::Backspace);
        assert_literal(&draft);
    }

    #[test]
    fn a_short_picker_scrolls_to_the_selected_binding_and_rejects_hidden_choices() {
        let roster = twin_roster();
        let mut draft = Draft::default();
        assert!(draft.insert("@", true));
        draft.move_completion(1, &roster);
        let choices: Vec<_> = draft
            .choices(&roster, 1)
            .map(|(index, _, chosen)| (index, chosen))
            .collect();
        assert_eq!(choices, [(1, true)]);
        draft.select_mention(&roster, 0, None);
        draft.select_mention(&roster, 1, Some(0));
        assert!(draft.completion.is_some());
        draft.select_mention(&roster, 1, None);
        assert_eq!(
            draft.mentions[0].binding,
            *roster[1].binding.as_ref().unwrap()
        );
    }

    #[test]
    fn word_deletion_can_edit_a_query_but_cannot_keep_a_deleted_mention_marker() {
        let mut draft = Draft::default();
        assert!(draft.insert("@", true));
        draft.insert("Twin extra", true);
        draft.edit(EditAction::DeletePreviousWord);
        assert_eq!(draft.query(), Some("Twin "));
        draft.edit(EditAction::DeletePreviousWord);
        assert!(draft.input.value.is_empty());
        assert!(draft.completion.is_none());
    }

    #[test]
    fn line_edits_shift_and_withdraw_only_exactly_affected_mentions() {
        let roster = twin_roster();
        let mut draft = Draft::default();
        draft.insert("intro\né ", false);
        assert!(draft.insert("@", true));
        draft.select_mention(&roster, 2, Some(0));
        draft.insert("tail\n", false);
        assert!(draft.insert("@", true));
        draft.select_mention(&roster, 2, Some(1));
        draft.insert("after", false);

        draft.input.cursor = "intro\né ".len();
        draft.edit(EditAction::DeleteToLineStart);
        draft.edit(EditAction::End);
        draft.edit(EditAction::DeletePreviousWord);
        assert_eq!(draft.input.value, "intro\n@Twin \n@Twin after");
        for mention in &draft.mentions {
            assert_eq!(&draft.input.value[mention.bytes.clone()], "@Twin");
        }
        assert_eq!(draft.mentions.len(), 2);

        draft.edit(EditAction::Home);
        draft.edit(EditAction::DeleteToLineEnd);
        assert_eq!(draft.input.value, "intro\n\n@Twin after");
        let ChannelRequest::PostUser { post, bindings, .. } =
            draft.post(ChannelId("desk".into())).unwrap()
        else {
            panic!("expected user post")
        };
        assert_eq!(post.mentions, [roster[1].participant.id.clone()]);
        assert_eq!(bindings.first(), roster[1].binding.as_ref());
        assert_eq!(
            draft.mentions[0].bytes,
            "intro\n\n".len().."intro\n\n@Twin".len()
        );
    }
}
