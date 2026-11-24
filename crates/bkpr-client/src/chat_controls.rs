//! Client-local channel drafts; the app owns endpoint authority and write receipts.
use crate::keyguide::{self, Action, Context};
use bkpr_protocol::channels::{
    ChannelId, ChannelRequest, ChannelResponse, ChannelSettings, ChannelSummary, MAX_MEMBERS,
    MAX_NAME_BYTES, MAX_PAGE_LIMIT, MembershipMode, Participant, ParticipantId, WakePolicy,
};
use bkpr_protocol::{
    Cursor, CursorShape, Frame, KeyAction, KeyEvent, MouseAction, MouseButton, Size,
};
use bkpr_tui_framework::{
    Navigation, NavigationRow, Rect, Role, RoleSpan, TextInput, Theme, UiSurface,
    button::{Button, State as ButtonState},
    checkbox::Checkbox,
    dropdown::Dropdown,
    text_width, window,
};
use std::collections::{BTreeMap, BTreeSet};
use termwiz::input::MouseEvent;

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

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlHit {
    rect: Rect,
    field: Field,
    choice: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Wake(WakePolicy),
    Membership(MembershipMode),
}

const WAKE: Dropdown<'_, Choice> = Dropdown::new(&[
    (Choice::Wake(WakePolicy::All), "All"),
    (Choice::Wake(WakePolicy::AddressedOrAll), "Mentions or all"),
    (Choice::Wake(WakePolicy::AddressedOnly), "Mentions only"),
    (Choice::Wake(WakePolicy::None), "None"),
]);
const MEMBERSHIP: Dropdown<'_, Choice> = Dropdown::new(&[
    (
        Choice::Membership(MembershipMode::WorkspaceWide),
        "Workspace-wide",
    ),
    (
        Choice::Membership(MembershipMode::Explicit),
        "Explicit members",
    ),
]);

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
    painted_size: Option<Size>,
    painted_hits: Vec<ControlHit>,
    pointer: Option<(u16, u16)>,
    pressed: Option<ControlHit>,
    dropdown: Option<(Field, usize)>,
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
                format!(
                    "Manage #{}",
                    summary.name.strip_prefix('#').unwrap_or(&summary.name)
                ),
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
            painted_size: None,
            painted_hits: Vec::new(),
            pointer: None,
            pressed: None,
            dropdown: None,
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
        if self.handle_dropdown_key(key, writable) {
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
                    if writable && self.editable() && self.open_dropdown(&field) {
                        return EditorAction::None;
                    }
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

    /// Cancel the gesture and pointer highlight when modal ownership changes.
    pub(super) fn cancel_press(&mut self) {
        self.pressed = None;
        self.pointer = None;
        self.nav.hover(None);
    }

    /// Route modal-local presses, releases, hover, and wheel events.
    pub(super) fn mouse(
        &mut self,
        mouse: MouseEvent,
        action: MouseAction,
        button: MouseButton,
        writable: bool,
    ) -> EditorAction {
        self.pointer = Some((mouse.x, mouse.y));
        if matches!(button, MouseButton::WheelUp | MouseButton::WheelDown) {
            self.cancel_press();
            let delta = if button == MouseButton::WheelUp {
                -3
            } else {
                3
            };
            if let Some((field, index)) = &self.dropdown {
                let (menu, _) = self.choices(field).expect("open dropdown");
                self.dropdown = Some((field.clone(), menu.move_by(*index, delta)));
            } else {
                self.nav.scroll_by(delta);
            }
            return EditorAction::None;
        }
        let target = self
            .painted_hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(mouse.x, mouse.y));
        self.nav.hover(target.map(|hit| &hit.field));
        match (button, action) {
            (MouseButton::Left, MouseAction::Press) => self.pressed = target.cloned(),
            (MouseButton::Left, MouseAction::Release) => {
                let pressed = self.pressed.take();
                let Some(hit) = pressed.filter(|hit| Some(hit) == target) else {
                    return EditorAction::None;
                };
                self.nav.select(hit.field.clone());
                if let Some(choice) = hit.choice {
                    if writable && self.editable() {
                        self.commit_dropdown(&hit.field, choice);
                    }
                } else if !(writable && self.editable() && self.open_dropdown(&hit.field)) {
                    self.dropdown = None;
                    return self.activate(hit.field, writable, 1);
                }
            }
            _ => {}
        }
        EditorAction::None
    }

    fn choices(&self, field: &Field) -> Option<(&'static Dropdown<'static, Choice>, Choice)> {
        match field {
            Field::UserWake => Some((&WAKE, Choice::Wake(self.settings.user_wake))),
            Field::AgentWake => Some((&WAKE, Choice::Wake(self.settings.agent_wake))),
            Field::Mode => Some((&MEMBERSHIP, Choice::Membership(self.membership))),
            _ => None,
        }
    }

    fn open_dropdown(&mut self, field: &Field) -> bool {
        let Some((menu, value)) = self.choices(field) else {
            return false;
        };
        if self
            .dropdown
            .as_ref()
            .is_some_and(|(current, _)| current == field)
        {
            self.dropdown = None;
        } else {
            self.dropdown = Some((field.clone(), menu.index(value).expect("current choice")));
        }
        true
    }

    fn commit_dropdown(&mut self, field: &Field, index: usize) {
        self.dropdown = None;
        if let Some(value) = self.choices(field).and_then(|(menu, _)| menu.value(index)) {
            self.set_choice(field, value);
        }
    }

    fn set_choice(&mut self, field: &Field, value: Choice) {
        match (field, value) {
            (Field::UserWake, Choice::Wake(wake)) => self.settings.user_wake = wake,
            (Field::AgentWake, Choice::Wake(wake)) => self.settings.agent_wake = wake,
            (Field::Mode, Choice::Membership(mode)) => {
                self.membership = mode;
                self.rebuild_rows();
            }
            _ => unreachable!("choice belongs to field"),
        }
        self.error = None;
    }

    fn handle_dropdown_key(&mut self, key: &KeyEvent, writable: bool) -> bool {
        let Some((field, index)) = self.dropdown.clone() else {
            return false;
        };
        if key.modifiers & 15 != 0 && key.key != "Tab" {
            return true;
        }
        let (menu, _) = self.choices(&field).expect("open dropdown");
        match key.key.as_str() {
            "Escape" if key.action == KeyAction::Press => self.dropdown = None,
            "Enter" | "Space" if key.action == KeyAction::Press && writable && self.editable() => {
                self.commit_dropdown(&field, index);
            }
            "ArrowUp" | "ArrowDown" | "Home" | "End" => {
                let delta = match key.key.as_str() {
                    "ArrowUp" => -1,
                    "ArrowDown" => 1,
                    "Home" => isize::MIN,
                    _ => isize::MAX,
                };
                self.dropdown = Some((field, menu.move_by(index, delta)));
            }
            "Tab" => {
                self.dropdown = None;
                return false;
            }
            _ => {}
        }
        true
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
            field @ (Field::UserWake | Field::AgentWake | Field::Mode) => {
                let (menu, value) = self.choices(&field).expect("dropdown field");
                if let Some(value) = menu.cycle(value, direction < 0) {
                    self.set_choice(&field, value);
                }
            }
            Field::Questions => {
                self.settings.attention_questions = !self.settings.attention_questions
            }
            Field::Reports => self.settings.attention_reports = !self.settings.attention_reports,
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
        self.pressed = None;
        self.painted_hits.clear();
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
        effects: &mut Frame,
        mut place: impl FnMut(&mut Frame, Rect, ButtonState) -> bool,
    ) {
        if self.painted_size.replace(size) != Some(size) {
            self.cancel_press();
        }
        let status = self.status(writable).is_some();
        let header = u16::from(size.rows >= if status { 3 } else { 2 });
        let status_rows = u16::from(status && size.rows >= 2);
        let body = size.rows - header - status_rows;
        self.nav.viewport(usize::from(body));
        let mut lines = Vec::with_capacity(2 * usize::from(size.rows));
        let mut spans = Vec::with_capacity(lines.capacity());
        let mut buttons = Vec::new();
        let mut hits = std::mem::take(&mut self.painted_hits);
        hits.clear();
        let mut cursor = None;
        let mut popup = None;
        let viewport = Rect {
            x: 0,
            y: header,
            width: size.cols,
            height: body,
        };
        let inset = u16::from(size.cols >= 5);
        let control_x = if size.cols >= 43 { 23 } else { size.cols / 2 };
        let full_row = |y| form_row(inset, y, size.cols - 2 * inset);
        let aligned = |y| form_row(control_x, y, size.cols.saturating_sub(control_x + inset));
        let mut add = |line: window::TextLine, role| {
            spans.push(RoleSpan { rect: line.0, role });
            lines.push(line);
        };
        if header > 0 {
            add((full_row(0), self.title.clone(), false), Role::Title);
        }
        let start = self.nav.scroll;
        for (index, row) in self
            .nav
            .rows
            .iter()
            .skip(start)
            .take(usize::from(body))
            .enumerate()
        {
            let y = header + index as u16;
            let Row::Control(field) = row else {
                let Row::Heading(title) = row else {
                    unreachable!()
                };
                add(
                    (
                        form_row(inset, y, control_x.saturating_sub(inset)),
                        (*title).into(),
                        false,
                    ),
                    Role::Info,
                );
                continue;
            };
            let focused = self.nav.cursor.as_ref() == Some(field);
            let hovered = self.nav.hover.as_ref() == Some(field);
            let editable = writable && self.editable();
            let rect = if let Some(label) = field_label(field) {
                let label_rect = form_row(inset, y, control_x.saturating_sub(inset + 1));
                add((label_rect, label.into(), false), Role::Muted);
                aligned(y)
            } else if matches!(field, Field::Member(_)) {
                full_row(y)
            } else {
                aligned(y)
            };
            let role = if !editable {
                Role::Muted
            } else if focused {
                Role::Accent
            } else if hovered {
                Role::Hover
            } else {
                Role::Base
            };
            if *field == Field::Name {
                let frame_pad = u16::from(rect.width >= 4);
                let input = rect.inset_horizontal(frame_pad);
                let (text, caret) = self.name.visible(usize::from(input.width));
                // Reserve the same field edge and cursor position in all modes.
                if frame_pad != 0 {
                    add((form_row(rect.x, y, 1), "[".into(), false), role);
                    add(
                        (form_row(rect.x + rect.width - 1, y, 1), "]".into(), false),
                        role,
                    );
                }
                add((input, text, false), role);
                if focused && editable && input.width > 0 {
                    cursor = Some(Cursor {
                        x: input.x + (caret as u16).min(input.width - 1),
                        y,
                        visible: true,
                        blinking: true,
                        shape: CursorShape::Bar,
                        color: None,
                    });
                }
            } else if let Some((menu, value)) = self.choices(field) {
                let open = self
                    .dropdown
                    .as_ref()
                    .filter(|(current, _)| current == field);
                add(menu.trigger(rect, value, open.is_some()), role);
                if let Some((_, active)) = open
                    && let Some(layout) = menu.layout(rect, viewport, *active)
                {
                    popup = Some((field, menu, value, layout, *active));
                }
            } else if let Some((checked, label)) =
                self.checkbox(field, start + index + 1, rect.width)
            {
                let role = if editable && checked && !focused && !hovered {
                    Role::Success
                } else {
                    role
                };
                add(Checkbox::new(checked, &label).line(rect), role);
            } else if let Some((label, enabled)) = self.action_label(field, editable) {
                let hit = ControlHit {
                    rect,
                    field: field.clone(),
                    choice: None,
                };
                let pointer =
                    ButtonState::at(rect, self.pointer, self.pressed.as_ref() == Some(&hit));
                let state = if !enabled {
                    ButtonState::Disabled
                } else if focused && pointer == ButtonState::Idle {
                    ButtonState::Hover
                } else {
                    pointer
                };
                add((rect, String::new(), false), Role::Panel);
                buttons.push((rect, label, state));
            }
            if rect.width != 0 {
                hits.push(ControlHit {
                    rect,
                    field: field.clone(),
                    choice: None,
                });
            }
        }
        if status_rows > 0
            && let Some((message, role)) = self.status(writable)
        {
            add((full_row(size.rows - 1), message.into(), false), role);
        }
        if let Some((field, menu, value, layout, active)) = popup {
            for index in layout.indices() {
                if let Some((rect, mut label, _)) = menu.option(layout, index, value) {
                    let width = text_width(&label);
                    label.extend(std::iter::repeat_n(
                        ' ',
                        usize::from(rect.width).saturating_sub(width),
                    ));
                    let selected = self
                        .pointer
                        .filter(|&(x, y)| layout.bounds().contains(x, y))
                        .map_or(index == active, |(x, y)| rect.contains(x, y));
                    add(
                        (rect, label, selected),
                        if selected { Role::Accent } else { Role::Title },
                    );
                    hits.push(ControlHit {
                        rect,
                        field: field.clone(),
                        choice: Some(index),
                    });
                }
            }
        }
        surface.paint(size, lines, cursor);
        surface.apply_theme(theme, Role::Panel, &spans);
        for (rect, label, state) in buttons {
            if popup.is_some_and(|(_, _, _, layout, _)| layout.bounds().contains(rect.x, rect.y)) {
                continue;
            }
            let graphical = place(effects, rect, state);
            Button::new(label).paint(
                &mut surface.frame,
                rect,
                theme.style(Role::Accent),
                state,
                graphical,
            );
        }
        self.painted_hits = hits;
    }

    fn checkbox(&self, field: &Field, ordinal: usize, width: u16) -> Option<(bool, String)> {
        match field {
            Field::Questions => Some((self.settings.attention_questions, String::new())),
            Field::Reports => Some((self.settings.attention_reports, String::new())),
            Field::Member(id) => {
                let label = self
                    .roster_member(id)
                    .map(|p| p.display_name.as_str())
                    .or_else(|| self.selected_labels.get(id).map(String::as_str))
                    .unwrap_or("Saved member");
                Some((
                    self.selected.contains(id),
                    member_label(ordinal, label, id, width.saturating_sub(4)),
                ))
            }
            _ => None,
        }
    }

    fn action_label(&self, field: &Field, editable: bool) -> Option<(&'static str, bool)> {
        Some(match field {
            Field::SaveSettings => (
                "\u{f0193} Save settings",
                editable && self.settings != self.saved_settings,
            ),
            Field::SaveMembership => (
                "\u{f0193} Save membership",
                editable && self.membership_dirty() && self.saved_ready(),
            ),
            Field::Create => (
                "\u{f0415} Create channel",
                editable && !self.name.value.trim().is_empty() && self.name.error().is_none(),
            ),
            Field::Close => ("\u{f0156} Close", true),
            Field::RetrySaved => (
                "Retry saved members",
                matches!(self.saved_fetch, Fetch::Failed(..)),
            ),
            Field::RetryRoster => (
                "Retry workspace roster",
                matches!(self.roster_fetch, Fetch::Failed(..)),
            ),
            Field::FirstRoster => (
                "First roster page",
                !matches!(self.in_flight, Some((Read::Roster, _))),
            ),
            Field::NextRoster(_) => (
                "Next roster page",
                matches!(self.roster_fetch, Fetch::Idle)
                    && !matches!(self.in_flight, Some((Read::Roster, _))),
            ),
            _ => return None,
        })
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

fn form_row(x: u16, y: u16, width: u16) -> Rect {
    Rect {
        x,
        y,
        width,
        height: 1,
    }
}

fn field_label(field: &Field) -> Option<&'static str> {
    Some(match field {
        Field::Name => "\u{f03eb} Name",
        Field::UserWake => "\u{f0004} User wake",
        Field::AgentWake => "\u{f06a9} Agent wake",
        Field::Questions => "\u{f0596} Question attention",
        Field::Reports => "\u{f0219} Report attention",
        Field::Mode => "\u{f0849} Membership mode",
        _ => return None,
    })
}

fn member_label(ordinal: usize, label: &str, id: &ParticipantId, width: u16) -> String {
    let prefix = format!("{ordinal} ");
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

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::channels::{ChannelRosterEntry, ParticipantKind};
    use termwiz::input::{Modifiers, MouseButtons};

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

    fn paint(editor: &mut Editor, size: Size) {
        editor.paint(
            &mut UiSurface::new(size),
            size,
            &Theme::default(),
            true,
            &mut bkpr_tui_framework::graphics_frame(size),
            |_, _, _| false,
        );
    }

    fn hit(editor: &Editor, field: Field) -> Rect {
        editor
            .painted_hits
            .iter()
            .find(|hit| hit.field == field)
            .expect("field must have been painted")
            .rect
    }

    fn pointer(editor: &mut Editor, rect: Rect, action: MouseAction) -> EditorAction {
        editor.mouse(
            MouseEvent {
                x: rect.x,
                y: rect.y,
                mouse_buttons: MouseButtons::LEFT,
                modifiers: Modifiers::empty(),
            },
            action,
            MouseButton::Left,
            true,
        )
    }

    fn choose(editor: &mut Editor, field: Field, steps: usize) {
        assert!(matches!(enter(editor, field), EditorAction::None));
        for _ in 0..steps {
            editor.key(&event("ArrowDown"), true);
        }
        editor.key(&event("Enter"), true);
    }

    #[test]
    fn create_submits_initial_policy_and_the_exact_roster_choice_once() {
        let mut editor = Editor::create("exact-workspace".into());
        assert!(editor.take_observation().is_none());
        editor.paste("Incident review");
        choose(&mut editor, Field::UserWake, 1);
        choose(&mut editor, Field::AgentWake, 2);
        enter(&mut editor, Field::Questions);
        enter(&mut editor, Field::Reports);
        choose(&mut editor, Field::Mode, 1);
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
        paint(&mut editor, size);
        let member = hit(&editor, Field::Member(id("agent-b")));
        pointer(&mut editor, member, MouseAction::Press);
        pointer(&mut editor, member, MouseAction::Release);
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
        choose(&mut editor, Field::Mode, 1);
        assert!(editor.take_observation().is_some());
        editor.observe(Ok(roster(&["agent-a"], Some("agent-a"))));
        let size = Size {
            cols: 22,
            rows: 20,
            cell_width: 8,
            cell_height: 16,
        };
        paint(&mut editor, size);
        let member = hit(&editor, Field::Member(id("agent-a")));
        pointer(&mut editor, member, MouseAction::Press);
        enter(&mut editor, Field::NextRoster(id("agent-a")));
        assert!(matches!(
            editor.take_observation(),
            Some(ChannelRequest::UserRoster { after: Some(cursor), .. }) if cursor == id("agent-a")
        ));
        editor.observe(Ok(roster(&["agent-b"], None)));
        paint(&mut editor, size);
        pointer(&mut editor, member, MouseAction::Release);
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
