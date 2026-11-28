use bkpr_protocol::channels::*;
use bkpr_protocol::roles::{RoleGrantId, WorkspaceRole};
use bkpr_tui_framework::{EditAction, TextInput, markdown, safe_text, text_width};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

struct Mention {
    bytes: Range<usize>,
    binding: ChannelMentionBinding,
    coordinator_grant: Option<RoleGrantId>,
}

pub(super) struct Completion {
    start: usize,
    selected: usize,
    slash: bool,
}

#[derive(Clone, Copy)]
pub(super) enum Choice<'a> {
    Agent(&'a ChannelRosterEntry),
    Coordinator(&'a ChannelRosterEntry, &'a RoleGrantId),
    Slash(&'static str),
}

fn slash_options(value: &str) -> Option<(usize, &'static [&'static str])> {
    if !value.starts_with('/') || value.contains('\n') || value == "/role coordinator none" {
        return None;
    }
    let (start, options): (_, &[_]) = if value.starts_with("/role coordinator ") {
        ("/role coordinator ".len(), &["none", "@"])
    } else if value.starts_with("/role ") {
        ("/role ".len(), &["coordinator"])
    } else {
        (0, &["/role"])
    };
    options
        .iter()
        .any(|choice| choice.starts_with(&value[start..]))
        .then_some((start, options))
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
    pub(super) fn role_target(
        &self,
    ) -> Option<Result<Option<&ChannelMentionBinding>, &'static str>> {
        let target = role_argument(&self.input.value)?;
        Some(if self.input.value.contains('\n') {
            Err("A role command must be on one line")
        } else if let Some((_, "none")) = target {
            Ok(None)
        } else if let Some((start, text)) = target.filter(|(_, text)| text.starts_with('@')) {
            self.mentions
                .iter()
                .find(|mention| mention.bytes == (start..start + text.len()))
                .map(|mention| Some(&mention.binding))
                .ok_or("Select an exact agent from the @ mention picker")
        } else {
            Err("Use /role coordinator @agent or /role coordinator none")
        })
    }

    pub(super) fn completing_role_target(&self) -> bool {
        self.completion.as_ref().is_some_and(|completion| {
            !completion.slash
                && !self.input.value.contains('\n')
                && role_argument(&self.input.value)
                    .flatten()
                    .is_some_and(|(start, _)| start == completion.start)
        })
    }

    pub(super) fn slash_completing(&self) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|completion| completion.slash)
    }

    pub(super) fn invalidate_completion(&mut self) -> bool {
        if let Some(completion) = &mut self.completion {
            completion.selected = 0;
            true
        } else {
            false
        }
    }

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
                    slash: false,
                });
                return true;
            }
        }
        if typed
            && self.input.cursor == self.input.value.len()
            && (self.slash_completing()
                || value == "/"
                || matches!(self.input.value.as_str(), "/role " | "/role coordinator "))
            && let Some((start, _)) = slash_options(&self.input.value)
        {
            self.completion = Some(Completion {
                start,
                selected: 0,
                slash: true,
            });
            return false;
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
        if self.slash_completing() {
            return None;
        }
        let source = self
            .input
            .value
            .get(start..self.input.cursor)?
            .strip_prefix('@')?;
        (source.len() <= MAX_NAME_BYTES && !source.contains('\n')).then_some(source)
    }

    fn update_query(&mut self) {
        if !(self.slash_completing()
            && self.input.cursor == self.input.value.len()
            && slash_options(&self.input.value).is_some())
            && self.query().is_none()
        {
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
        let count = self.candidates(roster).count();
        if let Some(completion) = &mut self.completion {
            completion.selected = completion
                .selected
                .saturating_add_signed(delta)
                .min(count.saturating_sub(1));
        }
    }

    fn candidates<'a>(&self, roster: &'a [ChannelRosterEntry]) -> impl Iterator<Item = Choice<'a>> {
        let slash = if self.slash_completing() {
            slash_options(&self.input.value)
        } else {
            None
        };
        let commands = slash.into_iter().flat_map(move |(start, options)| {
            options
                .iter()
                .copied()
                .filter(move |choice| choice.starts_with(&self.input.value[start..]))
        });
        let coordinator = self
            .query()
            .filter(|query| {
                !self.completing_role_target()
                    && "coordinator".starts_with(&query.to_ascii_lowercase())
            })
            .and_then(|_| {
                roster.iter().find_map(|entry| {
                    let role = entry.role.as_ref()?;
                    (role.role == WorkspaceRole::Coordinator && entry.binding.is_some())
                        .then_some(Choice::Coordinator(entry, &role.grant_id))
                })
            });
        commands
            .map(Choice::Slash)
            .chain(coordinator)
            .chain(self.matches(roster).map(Choice::Agent))
    }

    pub(super) fn choices<'a>(
        &self,
        roster: &'a [ChannelRosterEntry],
        capacity: usize,
    ) -> impl Iterator<Item = (usize, Choice<'a>, bool)> {
        let selected = self
            .completion
            .as_ref()
            .map_or(0, |completion| completion.selected);
        self.candidates(roster)
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
    ) -> bool {
        if capacity == 0 {
            return false;
        }
        let Some(completion) = &self.completion else {
            return false;
        };
        let Some((_, entry, _)) = self
            .choices(roster, capacity)
            .find(|&(choice, _, chosen)| index.map_or(chosen, |index| choice == index))
        else {
            return false;
        };
        if let Choice::Slash(command) = entry {
            let range = completion.start..self.input.cursor;
            if command == "@" {
                return self.insert("@", true);
            }
            let Some(inserted) = self.input.replace_range(range.clone(), command) else {
                return false;
            };
            self.splice(range, inserted.len());
            self.completion = None;
            return self.insert(" ", true);
        }
        let (entry, coordinator_grant) = match entry {
            Choice::Agent(entry) => (entry, None),
            Choice::Coordinator(entry, grant) => (entry, Some(grant.clone())),
            Choice::Slash(_) => unreachable!(),
        };
        let value = if coordinator_grant.is_some() {
            "@coordinator ".into()
        } else {
            format!("@{} ", safe_text(&entry.participant.display_name))
        };
        let range = completion.start..self.input.cursor;
        let Some(inserted) = self.input.replace_range(range.clone(), &value) else {
            self.error = Some("Mention would exceed the message size limit".into());
            return false;
        };
        let binding = entry.binding.as_ref().expect("eligible selection").clone();
        self.splice(range, inserted.len());
        self.mentions.push(Mention {
            bytes: inserted.start..inserted.end - 1,
            binding,
            coordinator_grant,
        });
        self.completion = None;
        self.error = None;
        false
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
        if self.slash_completing() || self.completing_role_target() {
            return None;
        }
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
        let mut coordinator_grant_id = None;
        if !self.mentions.is_empty() {
            let document = markdown::Document::parse(&self.input.value);
            let rows = document.wrap(MAX_POST_BYTES);
            for mention in &self.mentions {
                if let Some(grant) = &mention.coordinator_grant {
                    if coordinator_grant_id.is_some_and(|previous| previous != grant) {
                        return Err(
                            "Coordinator changed. Remove and reselect the @coordinator mentions.",
                        );
                    }
                    coordinator_grant_id = Some(grant);
                }
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
        let post = ChannelPost {
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
        };
        Ok(match coordinator_grant_id {
            Some(grant) => ChannelRequest::PostUserWithCoordinator {
                channel_id,
                post,
                bindings,
                coordinator_grant_id: grant.clone(),
            },
            None => ChannelRequest::PostUser {
                channel_id,
                post,
                bindings,
            },
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

// Outer None means ordinary chat; inner None is a reserved command with invalid syntax.
fn role_argument(text: &str) -> Option<Option<(usize, &str)>> {
    let command = text.trim_start().strip_prefix("/role")?;
    if !command.is_empty() && !command.starts_with(char::is_whitespace) {
        return None;
    }
    let argument = command
        .trim_start()
        .strip_prefix("coordinator")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map(str::trim_start)
        .map(|rest| (text.len() - rest.len(), rest.trim_end()));
    Some(argument)
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
    fn coordinator_alias_freezes_selected_tenure_and_plain_text_has_no_binding() {
        let mut roster = twin_roster();
        let old = RoleGrantId("old-tenure".into());
        roster[0].role = Some(bkpr_protocol::roles::RoleAttribution {
            role: WorkspaceRole::Coordinator,
            grant_id: old.clone(),
        });
        let binding = roster[0].binding.clone().unwrap();
        let mut draft = Draft::default();
        draft.insert("@", true);
        draft.select_mention(&roster, 2, None);
        roster[0].role.as_mut().unwrap().grant_id = RoleGrantId("new-tenure".into());
        assert!(matches!(
            draft.post(ChannelId("desk".into())).unwrap(),
            ChannelRequest::PostUserWithCoordinator { post, bindings, coordinator_grant_id, .. }
                if post.text == "@coordinator " && bindings == [binding] && coordinator_grant_id == old
        ));
        let mut plain = Draft::default();
        plain.paste("@coordinator");
        assert!(matches!(
            plain.post(ChannelId("desk".into())).unwrap(),
            ChannelRequest::PostUser { bindings, .. } if bindings.is_empty()
        ));
    }

    #[test]
    fn slash_completion_edits_without_creating_a_mention_binding() {
        let mut draft = Draft::default();
        draft.insert("/", true);
        draft.select_mention(&[], 2, None);
        assert_eq!(draft.input.value, "/role ");
        draft.select_mention(&[], 2, None);
        assert_eq!(draft.input.value, "/role coordinator ");
        draft.move_completion(1, &[]);
        assert!(draft.select_mention(&[], 2, None));
        assert_eq!(draft.input.value, "/role coordinator @");
        assert!(matches!(draft.role_target(), Some(Err(_))));
        assert!(draft.completing_role_target());
        let mut ordinary = Draft::default();
        ordinary.insert("path", true);
        ordinary.insert("/", true);
        assert!(ordinary.completion.is_none());
    }

    #[test]
    fn role_target_requires_a_whole_structured_mention() {
        let roster = twin_roster();
        let mut draft = Draft::default();
        draft.paste("/role coordinator @Twin");
        assert!(matches!(draft.role_target(), Some(Err(_))));

        let mut draft = Draft::default();
        draft.insert("/role coordinator ", true);
        assert!(draft.insert("@", true));
        assert!(draft.completing_role_target());
        assert_eq!(draft.completion_wake_preview(WakePolicy::All), None);
        draft.select_mention(&roster, 2, Some(1));
        assert_eq!(draft.role_target(), Some(Ok(roster[1].binding.as_ref())));
        draft.insert("tail", true);
        assert!(matches!(draft.role_target(), Some(Err(_))));
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
