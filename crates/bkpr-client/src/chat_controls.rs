//! Client-local channel drafts; the app owns endpoint authority and write receipts.
use crate::keyguide::{self, Action, Context};
use bkpr_protocol::channels::{
    ChannelId, ChannelRequest, ChannelResponse, ChannelSettings, ChannelSummary, MAX_MEMBERS,
    MAX_NAME_BYTES, MAX_PAGE_LIMIT, MembershipMode, Participant, ParticipantId, WakePolicy,
};
use bkpr_protocol::{Cursor, CursorShape, KeyAction, KeyEvent, Size};
use bkpr_tui_framework::{
    Navigation, NavigationRow, Rect, Role, RoleSpan, TextInput, Theme, UiSurface, text_width,
    window,
};
use std::collections::{BTreeMap, BTreeSet};
use termwiz::input::{MouseButtons, MouseEvent};

/// A local editor action for the app to handle against its current owner.
pub(super) enum EditorAction {
    /// No change to the app's view or requests.
    None,
    /// Dismiss the visible draft without canceling any dispatched request.
    Close,
    /// One explicitly requested write; resolve it with a matching receipt.
    Submit(ChannelRequest),
}

/// The receipt for an editor's one outstanding write, qualified by its owner in the app.
pub(super) enum EditorResult<'a> {
    /// The backend confirmed the new channel state.
    Applied(&'a ChannelSummary),
    /// The write was rejected and can be explicitly reconsidered.
    Rejected(&'a str),
    /// The write may have committed and cannot be repeated from this draft.
    Unknown(&'a str),
}

#[derive(Clone, Copy)]
enum Write {
    Create,
    Settings,
    Membership,
}

enum SubmitState {
    Editable,
    Pending(Write),
    Unknown(String),
    Created,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Read {
    Saved,
    Roster,
}

enum Fetch {
    Queued(Option<ParticipantId>),
    Idle,
    Failed(Option<ParticipantId>, String),
}

impl Fetch {
    fn take(&mut self) -> Option<Option<ParticipantId>> {
        if !matches!(self, Self::Queued(_)) {
            return None;
        }
        match std::mem::replace(self, Self::Idle) {
            Self::Queued(after) => Some(after),
            _ => unreachable!(),
        }
    }

    fn retry(&mut self) {
        if let Self::Failed(after, _) = self {
            *self = Self::Queued(after.take());
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Field {
    Name,
    UserWake,
    AgentWake,
    Questions,
    Reports,
    SaveSettings,
    Mode,
    SaveMembership,
    Create,
    Close,
    Member(ParticipantId),
    RetrySaved,
    RetryRoster,
    FirstRoster,
    NextRoster(ParticipantId),
}

enum Row {
    Control(Field),
    Heading(&'static str),
}

impl NavigationRow<Field> for Row {
    fn target(&self) -> Option<&Field> {
        match self {
            Self::Control(field) => Some(field),
            Self::Heading(_) => None,
        }
    }
}

/// A create or management draft tied to one exact workspace and optional channel.
pub(super) struct Editor {
    workspace_id: String,
    channel_id: Option<ChannelId>,
    title: String,
    name: TextInput,
    settings: ChannelSettings,
    saved_settings: ChannelSettings,
    membership: MembershipMode,
    saved_mode: MembershipMode,
    saved: BTreeSet<ParticipantId>,
    selected: BTreeSet<ParticipantId>,
    selected_labels: BTreeMap<ParticipantId, String>,
    saved_fetch: Fetch,
    roster_fetch: Fetch,
    in_flight: Option<(Read, Option<ParticipantId>)>,
    roster: Vec<Participant>,
    roster_after: Option<ParticipantId>,
    roster_next: Option<ParticipantId>,
    submit: SubmitState,
    error: Option<String>,
    nav: Navigation<Field, Row>,
    painted_cols: u16,
    painted_hits: Vec<Option<Field>>,
}

impl Editor {
    /// Open a new draft in the current, exact workspace.
    pub(super) fn create(workspace_id: String) -> Self {
        Self::new(
            workspace_id,
            None,
            "Create channel".into(),
            MembershipMode::WorkspaceWide,
            ChannelSettings::default(),
        )
    }

    /// Open a draft for an ordinary channel; direct histories are never editable.
    pub(super) fn manage(summary: &ChannelSummary) -> Option<Self> {
        (!summary.direct).then(|| {
            Self::new(
                summary.workspace_id.clone(),
                Some(summary.id.clone()),
                format!("Manage #{}", summary.name),
                summary.membership,
                summary.settings,
            )
        })
    }

    fn new(
        workspace_id: String,
        channel_id: Option<ChannelId>,
        title: String,
        membership: MembershipMode,
        settings: ChannelSettings,
    ) -> Self {
        let saved_fetch = if channel_id.is_some() && membership == MembershipMode::Explicit {
            Fetch::Queued(None)
        } else {
            Fetch::Idle
        };
        let mut editor = Self {
            workspace_id,
            channel_id,
            title,
            name: TextInput::new(String::new(), MAX_NAME_BYTES),
            settings,
            saved_settings: settings,
            membership,
            saved_mode: membership,
            saved: BTreeSet::new(),
            selected: BTreeSet::new(),
            selected_labels: BTreeMap::new(),
            saved_fetch,
            roster_fetch: Fetch::Queued(None),
            in_flight: None,
            roster: Vec::new(),
            roster_after: None,
            roster_next: None,
            submit: SubmitState::Editable,
            error: None,
            nav: Navigation::default(),
            painted_cols: 0,
            painted_hits: Vec::new(),
        };
        editor.rebuild_rows();
        editor
    }

    /// Whether this editor creates rather than updates a channel.
    pub(super) fn is_create(&self) -> bool {
        self.channel_id.is_none()
    }

    /// Take one page request for the current draft, if no older page is in flight.
    pub(super) fn take_observation(&mut self) -> Option<ChannelRequest> {
        if self.in_flight.is_some() {
            return None;
        }
        if let Some(after) = self.saved_fetch.take() {
            let channel_id = self
                .channel_id
                .clone()
                .expect("saved members need a channel");
            self.in_flight = Some((Read::Saved, after.clone()));
            return Some(ChannelRequest::ListExplicitMembers {
                channel_id,
                after,
                limit: MAX_PAGE_LIMIT,
            });
        }
        if self.membership != MembershipMode::Explicit {
            return None;
        }
        let after = self.roster_fetch.take()?;
        self.in_flight = Some((Read::Roster, after.clone()));
        Some(ChannelRequest::UserRoster {
            workspace_id: self.workspace_id.clone(),
            channel_id: None,
            after,
            limit: MAX_PAGE_LIMIT,
        })
    }

    /// Apply the matching observational page or expose a retryable read error.
    pub(super) fn observe(&mut self, result: Result<ChannelResponse, String>) {
        let Some((read, after)) = self.in_flight.take() else {
            return;
        };
        match (read, result) {
            (
                Read::Saved,
                Ok(ChannelResponse::ExplicitMembers {
                    members,
                    next_after,
                }),
            ) if page_advances(members.iter(), after.as_ref(), next_after.as_ref())
                && self.saved.len() + members.len() <= MAX_MEMBERS =>
            {
                self.saved.extend(members);
                if let Some(next) = next_after {
                    self.saved_fetch = Fetch::Queued(Some(next));
                } else {
                    self.selected.clone_from(&self.saved);
                }
            }
            (
                Read::Roster,
                Ok(ChannelResponse::UserRoster {
                    participants,
                    next_after,
                }),
            ) if participants.len() <= usize::from(MAX_PAGE_LIMIT)
                && page_advances(
                    participants.iter().map(|entry| &entry.participant.id),
                    after.as_ref(),
                    next_after.as_ref(),
                ) =>
            {
                self.roster = participants
                    .into_iter()
                    .map(|entry| entry.participant)
                    .collect();
                for participant in &self.roster {
                    if self.saved.contains(&participant.id)
                        || self.selected.contains(&participant.id)
                    {
                        self.selected_labels
                            .insert(participant.id.clone(), participant.display_name.clone());
                    }
                }
                self.roster_after = after;
                self.roster_next = next_after;
                if matches!(
                    self.nav.cursor,
                    Some(Field::NextRoster(_) | Field::FirstRoster | Field::RetryRoster)
                ) && let Some(first) = self.roster.first()
                {
                    self.nav.select(Field::Member(first.id.clone()));
                }
            }
            (read, result) => {
                let error = result
                    .err()
                    .unwrap_or_else(|| "Unexpected channel roster page".into());
                let fetch = match read {
                    Read::Saved => &mut self.saved_fetch,
                    Read::Roster => &mut self.roster_fetch,
                };
                *fetch = Fetch::Failed(after, error);
            }
        }
        self.rebuild_rows();
    }

    /// Resolve only the matching active editor write, leaving the other draft section intact.
    pub(super) fn finish(&mut self, result: EditorResult<'_>) {
        let SubmitState::Pending(write) = self.submit else {
            return;
        };
        self.submit = SubmitState::Editable;
        self.error = None;
        match result {
            EditorResult::Rejected(reason) => self.error = Some(reason.into()),
            EditorResult::Unknown(reason) => {
                self.submit =
                    SubmitState::Unknown(format!("Channel write outcome unknown: {reason}"));
            }
            EditorResult::Applied(summary)
                if summary.workspace_id == self.workspace_id
                    && !summary.direct
                    && self.channel_id.as_ref().is_none_or(|id| id == &summary.id) =>
            {
                match write {
                    Write::Create => self.submit = SubmitState::Created,
                    Write::Settings => {
                        self.settings = summary.settings;
                        self.saved_settings = summary.settings;
                    }
                    Write::Membership => {
                        self.membership = summary.membership;
                        self.saved_mode = summary.membership;
                        if summary.membership == MembershipMode::WorkspaceWide {
                            self.selected.clear();
                        }
                        self.saved.clone_from(&self.selected);
                        self.selected_labels
                            .retain(|id, _| self.selected.contains(id));
                    }
                }
            }
            EditorResult::Applied(_) => {
                self.submit = SubmitState::Unknown("Unexpected channel write receipt".into());
            }
        }
        self.rebuild_rows();
    }

    /// Route one focused key, emitting a write only on deliberate submit.
    pub(super) fn key(&mut self, key: &KeyEvent, writable: bool) -> EditorAction {
        if key.action == KeyAction::Release {
            return EditorAction::None;
        }
        let plain = key.modifiers & 15 == 0;
        let name_focused = self.nav.cursor == Some(Field::Name);
        match key.key.as_str() {
            "Escape" if plain && key.action == KeyAction::Press => return EditorAction::Close,
            "Tab" if key.modifiers & 14 == 0 => self.tab(key.modifiers & 1 != 0),
            "ArrowUp" if plain => self.nav.navigate(-1),
            "ArrowDown" if plain => self.nav.navigate(1),
            "PageUp" if plain => self.nav.navigate(-self.page_step()),
            "PageDown" if plain => self.nav.navigate(self.page_step()),
            "Home" if plain && !name_focused => self.nav.navigate(isize::MIN),
            "End" if plain && !name_focused => self.nav.navigate(isize::MAX),
            "Enter" if plain && key.action == KeyAction::Press => {
                if let Some(field) = self.nav.cursor.clone() {
                    return self.activate(
                        if name_focused { Field::Create } else { field },
                        writable,
                        1,
                    );
                }
            }
            "Space" if plain && !name_focused && key.action == KeyAction::Press => {
                if let Some(field) = self.nav.cursor.clone() {
                    return self.activate(field, writable, 1);
                }
            }
            "ArrowLeft" | "ArrowRight" if plain && !name_focused => {
                if let Some(field @ (Field::UserWake | Field::AgentWake | Field::Mode)) =
                    self.nav.cursor.clone()
                {
                    return self.activate(
                        field,
                        writable,
                        if key.key == "ArrowLeft" { -1 } else { 1 },
                    );
                }
            }
            _ if name_focused && writable && self.editable() => {
                let changed = match keyguide::lookup(Context::Edit, key) {
                    Some(Action::Edit(action)) => self.name.edit(action),
                    None if key.modifiers & 14 == 0 => self.name.insert(&key.text),
                    _ => false,
                };
                if changed {
                    self.error = None;
                }
            }
            _ => {}
        }
        EditorAction::None
    }

    /// Route surface-local pointer coordinates; `left_pressed` is the initial press edge.
    pub(super) fn mouse(
        &mut self,
        mouse: MouseEvent,
        left_pressed: bool,
        writable: bool,
    ) -> EditorAction {
        let index = usize::from(mouse.y);
        let inside = mouse.x < self.painted_cols && index < self.painted_hits.len();
        if inside && mouse.mouse_buttons.contains(MouseButtons::VERT_WHEEL) {
            let up = mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE);
            self.nav.hover(None);
            self.nav.scroll_by(if up { -3 } else { 3 });
            return EditorAction::None;
        }
        let target = if inside {
            self.painted_hits[index].as_ref()
        } else {
            None
        };
        self.nav.hover(target);
        if left_pressed
            && let Some(field) = target
            && self.nav.rows.iter().any(|row| row.target() == Some(field))
        {
            let field = field.clone();
            self.nav.select(field.clone());
            return self.activate(field, writable, 1);
        }
        EditorAction::None
    }

    /// Paste into the channel name only; other rows do not interpret pasted text.
    pub(super) fn paste(&mut self, text: &str) {
        if self.editable() && self.nav.cursor == Some(Field::Name) && self.name.insert(text) {
            self.error = None;
        }
    }

    fn editable(&self) -> bool {
        matches!(self.submit, SubmitState::Editable)
    }

    fn saved_ready(&self) -> bool {
        matches!(self.saved_fetch, Fetch::Idle) && !matches!(self.in_flight, Some((Read::Saved, _)))
    }

    fn membership_dirty(&self) -> bool {
        self.membership != self.saved_mode
            || (self.membership == MembershipMode::Explicit && self.selected != self.saved)
    }

    fn page_step(&self) -> isize {
        self.nav.viewport_rows().max(1) as isize
    }

    fn tab(&mut self, previous: bool) {
        let mut fields = self.nav.rows.iter().filter_map(Row::target);
        let edge = if previous {
            fields.clone().next()
        } else {
            fields.clone().next_back()
        };
        if self.nav.cursor.as_ref() == edge {
            let other = if previous {
                fields.next_back()
            } else {
                fields.next()
            };
            if let Some(field) = other {
                self.nav.select(field.clone());
            }
        } else {
            self.nav.navigate(if previous { -1 } else { 1 });
        }
    }

    fn activate(&mut self, field: Field, writable: bool, direction: i8) -> EditorAction {
        match field {
            Field::Close => return EditorAction::Close,
            Field::RetrySaved => self.saved_fetch.retry(),
            Field::RetryRoster => self.roster_fetch.retry(),
            Field::FirstRoster
                if self.roster_after.is_some()
                    && !matches!(self.in_flight, Some((Read::Roster, _))) =>
            {
                self.roster_fetch = Fetch::Queued(None);
            }
            Field::NextRoster(after)
                if matches!(self.roster_fetch, Fetch::Idle)
                    && self.roster_next.as_ref() == Some(&after)
                    && !matches!(self.in_flight, Some((Read::Roster, _))) =>
            {
                self.roster_fetch = Fetch::Queued(Some(after));
            }
            _ if !writable || !self.editable() => return EditorAction::None,
            Field::UserWake => {
                self.settings.user_wake = cycle_wake(self.settings.user_wake, direction);
            }
            Field::AgentWake => {
                self.settings.agent_wake = cycle_wake(self.settings.agent_wake, direction);
            }
            Field::Questions => {
                self.settings.attention_questions = !self.settings.attention_questions
            }
            Field::Reports => self.settings.attention_reports = !self.settings.attention_reports,
            Field::Mode => {
                self.membership = match self.membership {
                    MembershipMode::Explicit => MembershipMode::WorkspaceWide,
                    MembershipMode::WorkspaceWide => MembershipMode::Explicit,
                };
                self.rebuild_rows();
            }
            Field::Member(id)
                if self.membership == MembershipMode::Explicit && self.saved_ready() =>
            {
                if self.selected.remove(&id) {
                    if !self.saved.contains(&id) {
                        self.selected_labels.remove(&id);
                        if self.roster_member(&id).is_none() {
                            self.rebuild_rows();
                        }
                    }
                } else if self.selected.len() == MAX_MEMBERS {
                    self.error = Some(format!(
                        "At most {MAX_MEMBERS} explicit members are allowed"
                    ));
                    return EditorAction::None;
                } else if let Some(label) = self
                    .roster_member(&id)
                    .map(|participant| participant.display_name.clone())
                {
                    self.selected_labels.insert(id.clone(), label);
                    self.selected.insert(id);
                } else if self.saved.contains(&id) {
                    self.selected.insert(id);
                }
            }
            Field::Create => return self.submit(Write::Create),
            Field::SaveSettings => return self.submit(Write::Settings),
            Field::SaveMembership => return self.submit(Write::Membership),
            _ => return EditorAction::None,
        }
        self.error = None;
        EditorAction::None
    }

    fn submit(&mut self, write: Write) -> EditorAction {
        let members = || {
            if self.membership == MembershipMode::Explicit {
                self.selected.iter().cloned().collect()
            } else {
                Vec::new()
            }
        };
        let request = match (write, &self.channel_id) {
            (Write::Create, None) => {
                if self.name.error().is_some() {
                    self.nav.select(Field::Name);
                    return EditorAction::None;
                }
                let name = self.name.value.trim();
                if name.is_empty() {
                    self.error = Some("Enter a channel name".into());
                    self.nav.select(Field::Name);
                    return EditorAction::None;
                }
                ChannelRequest::Create {
                    workspace_id: self.workspace_id.clone(),
                    name: name.to_owned(),
                    membership: self.membership,
                    members: members(),
                    settings: self.settings,
                }
            }
            (Write::Settings, Some(channel_id)) if self.settings != self.saved_settings => {
                ChannelRequest::SetSettings {
                    channel_id: channel_id.clone(),
                    settings: self.settings,
                }
            }
            (Write::Membership, Some(channel_id)) if self.membership_dirty() => {
                if !self.saved_ready() {
                    self.error = Some("Load the saved membership before replacing it".into());
                    return EditorAction::None;
                }
                ChannelRequest::SetMembership {
                    channel_id: channel_id.clone(),
                    membership: self.membership,
                    members: members(),
                }
            }
            _ => return EditorAction::None,
        };
        if let Err(error) = request.validate() {
            self.error = Some(error.to_string());
            return EditorAction::None;
        }
        self.error = None;
        self.submit = SubmitState::Pending(write);
        EditorAction::Submit(request)
    }

    fn roster_member(&self, id: &ParticipantId) -> Option<&Participant> {
        self.roster
            .binary_search_by(|participant| participant.id.cmp(id))
            .ok()
            .map(|index| &self.roster[index])
    }

    fn rebuild_rows(&mut self) {
        let create = self.is_create();
        let saved_ready = self.saved_ready();
        let rows = &mut self.nav.rows;
        rows.clear();
        if create {
            rows.push(Row::Control(Field::Name));
        }
        rows.push(Row::Heading("Wake & attention"));
        rows.extend(
            [
                Field::UserWake,
                Field::AgentWake,
                Field::Questions,
                Field::Reports,
            ]
            .map(Row::Control),
        );
        if !create {
            rows.push(Row::Control(Field::SaveSettings));
        }
        rows.push(Row::Heading("Membership"));
        rows.extend(
            [
                Field::Mode,
                if create {
                    Field::Create
                } else {
                    Field::SaveMembership
                },
            ]
            .map(Row::Control),
        );
        if matches!(self.saved_fetch, Fetch::Failed(..)) {
            rows.push(Row::Control(Field::RetrySaved));
        }
        if self.membership == MembershipMode::Explicit && saved_ready {
            let outside = self.saved.union(&self.selected).filter(|id| {
                self.roster
                    .binary_search_by(|participant| participant.id.cmp(id))
                    .is_err()
            });
            for (index, id) in outside.enumerate() {
                if index == 0 {
                    rows.push(Row::Heading("Saved / selected outside this page"));
                }
                rows.push(Row::Control(Field::Member(id.clone())));
            }
            rows.push(Row::Heading("Current workspace participants"));
            rows.extend(
                self.roster
                    .iter()
                    .map(|participant| Row::Control(Field::Member(participant.id.clone()))),
            );
            if matches!(self.roster_fetch, Fetch::Failed(..)) {
                rows.push(Row::Control(Field::RetryRoster));
            }
            if self.roster_after.is_some() {
                rows.push(Row::Control(Field::FirstRoster));
            }
            if let Some(next) = &self.roster_next {
                rows.push(Row::Control(Field::NextRoster(next.clone())));
            }
        }
        rows.push(Row::Control(Field::Close));
        if !rows
            .iter()
            .any(|row| row.target().is_some() && row.target() == self.nav.cursor.as_ref())
        {
            let field = if self.nav.cursor.is_some() {
                Field::Mode
            } else if create {
                Field::Name
            } else {
                Field::UserWake
            };
            self.nav.select(field);
        }
        self.nav.refresh_viewport();
    }

    fn status(&self, writable: bool) -> Option<(&str, Role)> {
        match &self.submit {
            SubmitState::Pending(_) => {
                return Some(("Waiting for channel write receipt", Role::Info));
            }
            SubmitState::Unknown(reason) => return Some((reason, Role::Warning)),
            SubmitState::Created => return Some(("Channel created", Role::Success)),
            SubmitState::Editable => {}
        }
        if !writable {
            return Some(("Channel controls are read-only", Role::Muted));
        }
        if let Some(error) = self.error.as_deref().or(self.name.error()) {
            return Some((error, Role::Error));
        }
        if let Fetch::Failed(_, error) = &self.saved_fetch {
            return Some((error, Role::Error));
        }
        if !self.saved_ready() {
            return Some(("Loading saved membership…", Role::Info));
        }
        if self.membership == MembershipMode::Explicit {
            if let Fetch::Failed(_, error) = &self.roster_fetch {
                return Some((error, Role::Error));
            }
            if matches!(self.roster_fetch, Fetch::Queued(_))
                || matches!(self.in_flight, Some((Read::Roster, _)))
            {
                return Some(("Loading workspace participants…", Role::Info));
            }
        }
        None
    }

    /// Paint the local editor viewport and retain its exact pointer hit identities.
    pub(super) fn paint(
        &mut self,
        surface: &mut UiSurface,
        size: Size,
        theme: &Theme,
        writable: bool,
    ) {
        let status = self.status(writable).is_some();
        let header_rows = u16::from(size.rows >= if status { 3 } else { 2 });
        let status_rows = u16::from(status && size.rows >= 2);
        let body_rows = usize::from(size.rows - header_rows - status_rows);
        self.nav.viewport(body_rows);
        self.painted_cols = size.cols;
        self.painted_hits.resize(usize::from(size.rows), None);
        let mut lines: Vec<window::TextLine> = Vec::with_capacity(usize::from(size.rows));
        let mut spans = Vec::with_capacity(usize::from(size.rows));
        let mut cursor = None;
        let mut line = |y, label, selected, role| {
            let rect = Rect {
                x: 0,
                y,
                width: size.cols,
                height: 1,
            };
            lines.push((rect, label, selected));
            if role != Role::Panel {
                spans.push(RoleSpan { rect, role });
            }
        };
        if header_rows > 0 {
            line(0, self.title.clone(), false, Role::Title);
            self.painted_hits[0] = None;
        }
        let start = self.nav.scroll;
        let visible_rows = self.nav.rows.len().saturating_sub(start).min(body_rows);
        self.painted_hits[usize::from(header_rows) + visible_rows..].fill(None);
        for (visible, row) in self.nav.rows.iter().skip(start).take(body_rows).enumerate() {
            let y = header_rows + visible as u16;
            let field = match row {
                Row::Control(field) => field,
                Row::Heading(heading) => {
                    self.painted_hits[usize::from(y)] = None;
                    line(y, (*heading).into(), false, Role::Title);
                    continue;
                }
            };
            let selected = self.nav.cursor.as_ref() == Some(field);
            let hovered = !selected && self.nav.hover.as_ref() == Some(field);
            let mut role = if hovered { Role::Hover } else { Role::Panel };
            let label = if *field == Field::Name {
                let prefix = if size.cols > 6 { "Name: " } else { "" };
                let (text, caret) = self.name.visible(usize::from(size.cols) - prefix.len());
                if selected && size.cols > 0 && writable && self.editable() {
                    cursor = Some(Cursor {
                        x: (prefix.len() + caret) as u16,
                        y,
                        visible: true,
                        blinking: true,
                        shape: CursorShape::Bar,
                        color: None,
                    });
                }
                format!("{prefix}{text}")
            } else {
                let (label, enabled) = self.label(field, start + visible + 1, size.cols, writable);
                if !enabled && !hovered {
                    role = Role::Muted;
                }
                label
            };
            line(y, label, selected, role);
            let hit = &mut self.painted_hits[usize::from(y)];
            if hit.as_ref() != Some(field) {
                *hit = Some(field.clone());
            }
        }
        if status_rows > 0
            && let Some((message, role)) = self.status(writable)
        {
            line(size.rows - 1, message.into(), false, role);
        }
        surface.paint(size, lines, cursor);
        surface.apply_theme(theme, Role::Panel, &spans);
    }

    fn label(&self, field: &Field, ordinal: usize, width: u16, writable: bool) -> (String, bool) {
        let editable = writable && self.editable();
        match field {
            Field::UserWake => (
                format!("User wake: {}", wake_label(self.settings.user_wake)),
                editable,
            ),
            Field::AgentWake => (
                format!("Agent wake: {}", wake_label(self.settings.agent_wake)),
                editable,
            ),
            Field::Questions => (
                format!(
                    "{} Question attention",
                    check(self.settings.attention_questions)
                ),
                editable,
            ),
            Field::Reports => (
                format!(
                    "{} Report attention",
                    check(self.settings.attention_reports)
                ),
                editable,
            ),
            Field::Mode => (
                match self.membership {
                    MembershipMode::WorkspaceWide => "Mode: Workspace-wide",
                    MembershipMode::Explicit => "Mode: Explicit members",
                }
                .into(),
                editable,
            ),
            Field::SaveSettings => (
                "[ Save settings ]".into(),
                editable && self.settings != self.saved_settings,
            ),
            Field::SaveMembership => (
                "[ Save membership ]".into(),
                editable && self.membership_dirty() && self.saved_ready(),
            ),
            Field::Create => (
                "[ Create channel ]".into(),
                editable && !self.name.value.trim().is_empty() && self.name.error().is_none(),
            ),
            Field::Close => ("[ Close ]".into(), true),
            Field::Member(id) => {
                let label = self
                    .roster_member(id)
                    .map(|p| p.display_name.as_str())
                    .or_else(|| self.selected_labels.get(id).map(String::as_str))
                    .unwrap_or("Saved member");
                (
                    member_label(self.selected.contains(id), ordinal, label, id, width),
                    editable,
                )
            }
            Field::RetrySaved => (
                "[ Retry saved members ]".into(),
                matches!(self.saved_fetch, Fetch::Failed(..)),
            ),
            Field::RetryRoster => (
                "[ Retry workspace roster ]".into(),
                matches!(self.roster_fetch, Fetch::Failed(..)),
            ),
            Field::FirstRoster => (
                "[ First roster page ]".into(),
                !matches!(self.in_flight, Some((Read::Roster, _))),
            ),
            Field::NextRoster(_) => (
                "[ Next roster page ]".into(),
                matches!(self.roster_fetch, Fetch::Idle)
                    && !matches!(self.in_flight, Some((Read::Roster, _))),
            ),
            Field::Name => unreachable!(),
        }
    }
}

fn page_advances<'a>(
    ids: impl Iterator<Item = &'a ParticipantId>,
    mut previous: Option<&'a ParticipantId>,
    next: Option<&ParticipantId>,
) -> bool {
    let mut saw_record = false;
    for id in ids {
        if previous.is_some_and(|previous| previous >= id) {
            return false;
        }
        previous = Some(id);
        saw_record = true;
    }
    next.is_none_or(|next| saw_record && previous == Some(next))
}

fn check(selected: bool) -> &'static str {
    if selected { "[x]" } else { "[ ]" }
}

fn member_label(
    selected: bool,
    ordinal: usize,
    label: &str,
    id: &ParticipantId,
    width: u16,
) -> String {
    let prefix = format!("{} {ordinal} ", check(selected));
    let room = width.saturating_sub(prefix.len() as u16);
    if room < 12 {
        return format!("{prefix}{label}");
    }
    let id = id.as_ref();
    let label_cells = text_width(label).min(18) as u16;
    let full = window::fit(id, room.saturating_sub(label_cells + 1).max(7));
    let identity = if full == id {
        full
    } else {
        let tail = id.char_indices().rev().nth(7).map_or(0, |(index, _)| index);
        format!("…{}", window::fit(&id[tail..], (room - 6).min(8) - 1))
    };
    let label = window::fit(label, room.saturating_sub(text_width(&identity) as u16 + 1));
    format!("{prefix}{label} {identity}")
}

fn wake_label(wake: WakePolicy) -> &'static str {
    match wake {
        WakePolicy::All => "All",
        WakePolicy::AddressedOrAll => "Mentions or all",
        WakePolicy::AddressedOnly => "Mentions only",
        WakePolicy::None => "None",
    }
}

fn cycle_wake(wake: WakePolicy, direction: i8) -> WakePolicy {
    use WakePolicy::{AddressedOnly, AddressedOrAll, All, None};
    match (wake, direction < 0) {
        (All, false) | (AddressedOnly, true) => AddressedOrAll,
        (AddressedOrAll, false) | (None, true) => AddressedOnly,
        (AddressedOnly, false) | (All, true) => None,
        (None, false) | (AddressedOrAll, true) => All,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::channels::{ChannelRosterEntry, ParticipantKind};
    use termwiz::input::Modifiers;

    fn id(value: &str) -> ParticipantId {
        ParticipantId(value.into())
    }

    fn event(key: &str) -> KeyEvent {
        KeyEvent {
            key: key.into(),
            modifiers: 0,
            text: String::new(),
            action: KeyAction::Press,
            unshifted_codepoint: 0,
        }
    }

    fn focus(editor: &mut Editor, field: Field) {
        assert!(
            editor
                .nav
                .rows
                .iter()
                .any(|row| row.target() == Some(&field))
        );
        editor.nav.select(field);
    }

    fn enter(editor: &mut Editor, field: Field) -> EditorAction {
        focus(editor, field);
        editor.key(&event("Enter"), true)
    }

    fn summary(membership: MembershipMode) -> ChannelSummary {
        ChannelSummary {
            id: ChannelId("channel".into()),
            workspace_id: "exact-workspace".into(),
            name: "Review".into(),
            membership,
            settings: ChannelSettings::default(),
            last_sequence: 0,
            read_through: 0,
            unread_attention: false,
            direct: false,
        }
    }

    fn roster(entries: &[&str], next: Option<&str>) -> ChannelResponse {
        ChannelResponse::UserRoster {
            participants: entries
                .iter()
                .map(|participant| ChannelRosterEntry {
                    participant: Participant {
                        id: id(participant),
                        kind: ParticipantKind::Agent,
                        display_name: "Duplicate title".into(),
                    },
                    binding: None,
                })
                .collect(),
            next_after: next.map(id),
        }
    }

    fn click(editor: &mut Editor, field: Field) -> EditorAction {
        let y = editor
            .painted_hits
            .iter()
            .position(|hit| hit.as_ref() == Some(&field))
            .expect("field must have been painted");
        editor.mouse(
            MouseEvent {
                x: 0,
                y: y as u16,
                mouse_buttons: MouseButtons::LEFT,
                modifiers: Modifiers::empty(),
            },
            true,
            true,
        )
    }

    #[test]
    fn create_submits_initial_policy_and_the_exact_roster_choice_once() {
        let mut editor = Editor::create("exact-workspace".into());
        assert!(editor.take_observation().is_none());
        editor.paste("Incident review");
        enter(&mut editor, Field::UserWake);
        enter(&mut editor, Field::AgentWake);
        enter(&mut editor, Field::AgentWake);
        enter(&mut editor, Field::Questions);
        enter(&mut editor, Field::Reports);
        enter(&mut editor, Field::Mode);
        assert_eq!(
            editor.take_observation(),
            Some(ChannelRequest::UserRoster {
                workspace_id: "exact-workspace".into(),
                channel_id: None,
                after: None,
                limit: MAX_PAGE_LIMIT,
            })
        );
        assert!(editor.take_observation().is_none());
        editor.observe(Ok(roster(&["agent-a", "agent-b"], None)));
        let size = Size {
            cols: 24,
            rows: 20,
            cell_width: 8,
            cell_height: 16,
        };
        editor.paint(&mut UiSurface::new(size), size, &Theme::default(), true);
        click(&mut editor, Field::Member(id("agent-b")));
        let EditorAction::Submit(request) = enter(&mut editor, Field::Create) else {
            panic!("expected one create write");
        };
        assert_eq!(
            request,
            ChannelRequest::Create {
                workspace_id: "exact-workspace".into(),
                name: "Incident review".into(),
                membership: MembershipMode::Explicit,
                members: vec![id("agent-b")],
                settings: ChannelSettings {
                    user_wake: WakePolicy::AddressedOrAll,
                    agent_wake: WakePolicy::AddressedOnly,
                    attention_questions: true,
                    attention_reports: true,
                },
            }
        );
        assert!(matches!(
            enter(&mut editor, Field::Create),
            EditorAction::None
        ));
        editor.finish(EditorResult::Applied(&summary(MembershipMode::Explicit)));
        assert!(matches!(
            enter(&mut editor, Field::Create),
            EditorAction::None
        ));
    }

    #[test]
    fn manage_saves_policy_and_membership_separately_and_preserves_inactive_ids() {
        let mut channel = summary(MembershipMode::Explicit);
        let mut editor = Editor::manage(&channel).unwrap();
        assert_eq!(
            editor.take_observation(),
            Some(ChannelRequest::ListExplicitMembers {
                channel_id: channel.id.clone(),
                after: None,
                limit: MAX_PAGE_LIMIT,
            })
        );
        enter(&mut editor, Field::Questions);
        let EditorAction::Submit(ChannelRequest::SetSettings { settings, .. }) =
            enter(&mut editor, Field::SaveSettings)
        else {
            panic!("policy save should not wait for the membership read");
        };
        channel.settings = settings;
        editor.finish(EditorResult::Applied(&channel));

        editor.observe(Ok(ChannelResponse::ExplicitMembers {
            members: vec![id("inactive-keep")],
            next_after: Some(id("inactive-keep")),
        }));
        assert_eq!(
            editor.take_observation(),
            Some(ChannelRequest::ListExplicitMembers {
                channel_id: channel.id.clone(),
                after: Some(id("inactive-keep")),
                limit: MAX_PAGE_LIMIT,
            })
        );
        editor.observe(Ok(ChannelResponse::ExplicitMembers {
            members: vec![id("inactive-remove")],
            next_after: None,
        }));
        assert!(matches!(
            editor.take_observation(),
            Some(ChannelRequest::UserRoster { .. })
        ));
        editor.observe(Ok(roster(&["current"], None)));
        enter(&mut editor, Field::Member(id("inactive-remove")));
        enter(&mut editor, Field::Member(id("current")));
        enter(&mut editor, Field::Reports);
        let EditorAction::Submit(request) = enter(&mut editor, Field::SaveMembership) else {
            panic!("expected a membership-only write");
        };
        assert_eq!(
            request,
            ChannelRequest::SetMembership {
                channel_id: channel.id.clone(),
                membership: MembershipMode::Explicit,
                members: vec![id("current"), id("inactive-keep")],
            }
        );
        editor.finish(EditorResult::Applied(&channel));
        let EditorAction::Submit(ChannelRequest::SetSettings { settings, .. }) =
            enter(&mut editor, Field::SaveSettings)
        else {
            panic!("membership receipt must leave the policy draft intact");
        };
        assert!(settings.attention_questions && settings.attention_reports);
    }

    #[test]
    fn a_roster_page_receipt_cannot_retarget_a_click_on_the_previous_page() {
        let mut editor = Editor::create("exact-workspace".into());
        enter(&mut editor, Field::Mode);
        assert!(editor.take_observation().is_some());
        editor.observe(Ok(roster(&["agent-a"], Some("agent-a"))));
        let size = Size {
            cols: 22,
            rows: 20,
            cell_width: 8,
            cell_height: 16,
        };
        editor.paint(&mut UiSurface::new(size), size, &Theme::default(), true);
        enter(&mut editor, Field::NextRoster(id("agent-a")));
        assert!(matches!(
            editor.take_observation(),
            Some(ChannelRequest::UserRoster { after: Some(cursor), .. }) if cursor == id("agent-a")
        ));
        editor.observe(Ok(roster(&["agent-b"], None)));
        click(&mut editor, Field::Member(id("agent-a")));
        assert!(editor.selected.is_empty());
    }

    #[test]
    fn direct_read_only_and_unknown_outcomes_cannot_dispatch_writes() {
        let mut channel = summary(MembershipMode::WorkspaceWide);
        channel.direct = true;
        assert!(Editor::manage(&channel).is_none());

        let mut editor = Editor::create("exact-workspace".into());
        editor.paste("Review");
        assert!(matches!(
            editor.key(&event("Enter"), false),
            EditorAction::None
        ));
        assert!(matches!(
            editor.key(&event("Enter"), true),
            EditorAction::Submit(_)
        ));
        editor.finish(EditorResult::Rejected("No longer allowed"));
        assert!(matches!(
            editor.key(&event("Enter"), true),
            EditorAction::Submit(_)
        ));
        editor.finish(EditorResult::Unknown("connection lost"));
        assert!(matches!(
            editor.key(&event("Enter"), true),
            EditorAction::None
        ));
        assert!(matches!(
            editor.key(&event("Escape"), true),
            EditorAction::Close
        ));
    }
}
