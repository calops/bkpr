use super::{chat_controls, chat_draft::Draft, chat_images, transport};
use crate::keyguide::{self, Action, ChatAction, Context, HistoryAction, Navigation};
use anyhow::{Result, bail};
use bkpr_protocol::{KeyAction, KeyEvent, Operation, Payload, channels::*};
use bkpr_tui_framework::{Role, UiSurface, safe_text};
use std::collections::{HashMap, VecDeque};

#[path = "chat_view.rs"]
mod view;
use view::{Hit, Paint};

const PAGE: u16 = 32;
const HISTORY_ROWS: usize = 160;
const HISTORY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Anchor {
    sequence: u64,
    line: usize,
}

#[derive(Default)]
struct History {
    entries: VecDeque<view::Entry>,
    bytes: usize,
    old: bool,
    loaded: bool,
    anchor: Option<Anchor>,
    paused_at: Option<u64>,
    load: Option<Load>,
    error: Option<String>,
    visible_through: u64,
    read_failed: Option<u64>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Load {
    Latest,
    Older(u64),
    Newer(u64),
    Jump {
        message: MessageId,
        sequence: u64,
        known_tail: u64,
    },
}

struct Active {
    id: ChannelId,
    history: History,
    roster: Vec<ChannelRosterEntry>,
    roster_next: Option<Option<ParticipantId>>,
    roster_error: Option<String>,
    roster_ready: bool,
}

impl Active {
    fn new(id: ChannelId) -> Self {
        Self {
            id,
            history: History {
                load: Some(Load::Latest),
                ..History::default()
            },
            roster: Vec::new(),
            roster_next: None,
            roster_error: None,
            roster_ready: false,
        }
    }
}

struct Editor {
    token: u64,
    form: chat_controls::Editor,
}

enum Send {
    Pending {
        request: ChannelRequest,
        retry: bool,
    },
    Unknown(ChannelRequest),
}

#[derive(PartialEq, Eq)]
pub(super) enum JobKind {
    History {
        epoch: u64,
        load: Load,
    },
    Roster(u64),
    Mark {
        channel: ChannelId,
        through: u64,
    },
    Send(ChannelId),
    EditorRead(u64),
    EditorWrite(u64),
    Image {
        channel: ChannelId,
        attachment: AttachmentId,
    },
}

pub(super) enum Job {
    Request(ChannelRequest),
    Image(ChannelId, AttachmentId),
}

#[expect(
    clippy::large_enum_variant,
    reason = "bounded job results move the decoded response without a second allocation"
)]
pub(super) enum Completion {
    Response(ChannelResponse),
    Image(chat_images::Loaded),
    History {
        entries: Vec<view::Entry>,
        older: bool,
    },
}

impl Job {
    pub(super) fn run(self, endpoint: &transport::Endpoint) -> Result<Completion> {
        match self {
            Self::Request(request) => {
                let Payload::Channels(response) =
                    endpoint.checked_payload(Operation::Channels { request })?
                else {
                    bail!("endpoint returned a non-channel response");
                };
                Ok(match response {
                    ChannelResponse::History {
                        messages,
                        next_before,
                        ..
                    } => Completion::History {
                        entries: messages.into_iter().map(view::Entry::new).collect(),
                        older: next_before.is_some(),
                    },
                    response => Completion::Response(response),
                })
            }
            Self::Image(channel, attachment) => {
                chat_images::load(endpoint, channel, attachment).map(Completion::Image)
            }
        }
    }
}
// Input dispatch is stack-only; a submitted request is moved directly into its worker.
#[allow(clippy::large_enum_variant)]
pub(super) enum Effect {
    None,
    Native,
    Submit(JobKind, Job),
}

#[derive(Default)]
pub(super) struct Chat {
    catalog: HashMap<ChannelId, ChannelSummary>,
    catalog_ready: bool,
    attention: HashMap<String, usize>,
    catalog_error: Option<String>,
    workspace: Option<String>,
    active: Option<Active>,
    recent_history: VecDeque<(ChannelId, History)>,
    selected: HashMap<String, ChannelId>,
    ids: Vec<ChannelId>,
    channel_chooser: bool,
    list_cursor: usize,
    drafts: HashMap<ChannelId, Draft>,
    sends: HashMap<ChannelId, Send>,
    pending: HashMap<String, JobKind>,
    epoch: u64,
    next: u64,
    editor: Option<Editor>,
    paint: Paint,
    images: chat_images::Cache,
    transports: Vec<bkpr_tui_framework::PreparedTransport>,
    images_wanted: Vec<AttachmentId>,
    mark: Option<(ChannelId, u64)>,
    notice: Option<(Role, String)>,
}

impl Chat {
    pub(super) fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    pub(super) fn visible(&self) -> bool {
        self.workspace.is_some()
    }

    pub(super) fn dialog_open(&self) -> bool {
        self.editor.is_some() || self.channel_chooser
    }

    pub(super) fn is_attending(&self) -> bool {
        self.visible() && !self.dialog_open()
    }

    pub(super) fn channel(&self, workspace: &str, channel: &ChannelId) -> Option<&ChannelSummary> {
        self.catalog
            .get(channel)
            .filter(|entry| entry.workspace_id == workspace)
    }

    pub(super) fn contains_channel(&self, workspace: &str, channel: &ChannelId) -> bool {
        self.channel(workspace, channel).is_some()
    }

    pub(super) fn rendered(&self, channel: &ChannelId, message: &MessageId, sequence: u64) -> bool {
        self.paint.rendered.contains(&sequence)
            && self.active.as_ref().is_some_and(|active| {
                active.id == *channel
                    && active.history.entries.iter().any(|entry| {
                        entry.sequence() == sequence && entry.message.message.id == *message
                    })
            })
    }

    pub(super) fn summary(&self) -> Option<&ChannelSummary> {
        self.catalog.get(&self.active.as_ref()?.id)
    }

    pub(super) fn open(&mut self, workspace: String) {
        if self.workspace.as_ref() == Some(&workspace) {
            self.set_channel_chooser(false);
            self.redraw();
            return;
        }
        self.close();
        self.workspace = Some(workspace);
        self.refresh_channels();
    }

    pub(super) fn close(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        self.retain_active_history();
        self.workspace = None;
        self.channel_chooser = false;
        self.editor = None;
        self.images_wanted.clear();
        self.mark = None;
        self.notice = None;
        self.hover_channel(None);
        self.paint.invalidate_hits();
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(super) fn jump(
        &mut self,
        workspace: String,
        channel: ChannelId,
        message: MessageId,
        sequence: u64,
    ) {
        self.open(workspace);
        self.select(channel.clone());
        let known_tail = self
            .summary()
            .map_or(sequence, |summary| summary.last_sequence);
        if let Some(active) = &mut self.active
            && active.id == channel
        {
            active.history = History {
                load: Some(Load::Jump {
                    message,
                    sequence,
                    known_tail,
                }),
                ..History::default()
            };
            self.epoch = self.epoch.wrapping_add(1);
            self.paint.invalidate_hits();
        }
    }

    pub(super) fn replace_catalog(&mut self, catalog: HashMap<ChannelId, ChannelSummary>) {
        self.attention.clear();
        for entry in catalog.values().filter(|entry| entry.unread_attention) {
            *self
                .attention
                .entry(entry.workspace_id.clone())
                .or_default() += 1;
        }
        self.recent_history
            .retain(|(id, _)| catalog.contains_key(id));
        self.catalog = catalog;
        self.catalog_ready = true;
        self.catalog_error = None;
        self.refresh_channels();
        self.redraw();
    }

    pub(super) fn reset_catalog(&mut self) {
        self.replace_catalog(HashMap::new());
        self.catalog_ready = false;
    }

    pub(super) fn catalog_error(&mut self, error: &anyhow::Error) {
        self.catalog_error = Some(error_text(error));
        self.attention.clear();
        self.redraw();
    }

    pub(super) fn upsert(&mut self, summary: ChannelSummary) -> bool {
        let (badge_changed, rebuild, projected) = self.update_catalog_entry(summary);
        self.refresh_catalog_projection(rebuild, projected);
        badge_changed
    }

    /// Apply ordered catalog changes and refresh the channel projection once per page.
    pub(super) fn apply_catalog_changes(&mut self, changes: Vec<ChannelCatalogChange>) -> bool {
        let (mut badge_changed, mut rebuild, mut projected) = (false, false, false);
        for change in changes {
            match change {
                ChannelCatalogChange::Upsert { channel } => {
                    let (badge, list, visible) = self.update_catalog_entry(channel);
                    badge_changed |= badge;
                    rebuild |= list;
                    projected |= visible;
                }
                ChannelCatalogChange::WorkspaceClosed { workspace_id } => {
                    badge_changed |= self.workspace_closed(&workspace_id);
                }
            }
        }
        self.refresh_catalog_projection(rebuild, projected);
        badge_changed
    }

    fn update_catalog_entry(&mut self, mut summary: ChannelSummary) -> (bool, bool, bool) {
        let old = self.catalog.get(&summary.id);
        if let Some(previous) = old {
            summary.last_sequence = summary.last_sequence.max(previous.last_sequence);
            summary.read_through = summary.read_through.max(previous.read_through);
        }
        let rebuild = old.is_none_or(|old| {
            old.name != summary.name
                || old.workspace_id != summary.workspace_id
                || old.direct != summary.direct
        });
        let before = old.is_some_and(|entry| entry.unread_attention);
        let mut badge_changed = false;
        if before != summary.unread_attention {
            if summary.unread_attention {
                let count = self
                    .attention
                    .entry(summary.workspace_id.clone())
                    .or_default();
                badge_changed = *count == 0;
                *count += 1;
            } else if let Some(count) = self.attention.get_mut(&summary.workspace_id) {
                *count -= 1;
                if *count == 0 {
                    self.attention.remove(&summary.workspace_id);
                    badge_changed = true;
                }
            }
        }
        let projected = self.workspace() == Some(summary.workspace_id.as_str());
        self.catalog.insert(summary.id.clone(), summary);
        (badge_changed, rebuild && projected, projected)
    }

    fn refresh_catalog_projection(&mut self, rebuild: bool, projected: bool) {
        if rebuild {
            self.refresh_channels();
        }
        if projected {
            self.redraw();
        }
    }

    pub(super) fn workspace_closed(&mut self, workspace: &str) -> bool {
        if self.workspace() == Some(workspace) {
            self.close();
        }
        self.selected.remove(workspace);
        self.drafts.retain(|id, _| {
            self.catalog
                .get(id)
                .is_none_or(|entry| entry.workspace_id != workspace)
        });
        self.recent_history.retain(|(id, _)| {
            self.catalog
                .get(id)
                .is_some_and(|entry| entry.workspace_id != workspace)
        });
        self.catalog
            .retain(|_, entry| entry.workspace_id != workspace);
        self.attention.remove(workspace).is_some()
    }

    pub(super) fn attention(&self) -> impl Iterator<Item = &str> {
        self.attention.keys().map(String::as_str)
    }

    fn refresh_channels(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        self.ids.clear();
        self.ids.extend(
            self.catalog
                .values()
                .filter(|entry| entry.workspace_id == *workspace)
                .map(|entry| entry.id.clone()),
        );
        self.ids.sort_unstable_by(|left, right| {
            let left = &self.catalog[left];
            let right = &self.catalog[right];
            channel_order(left).cmp(&channel_order(right))
        });
        let selection = self
            .selected
            .get(workspace)
            .filter(|id| self.catalog.contains_key(*id))
            .or_else(|| self.ids.first())
            .cloned();
        if let Some(id) = selection {
            self.select(id);
        } else {
            self.active = None;
            self.epoch = self.epoch.wrapping_add(1);
            self.paint.invalidate_hits();
        }
    }

    fn select(&mut self, id: ChannelId) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        if !self.contains_channel(workspace, &id) {
            return;
        }
        if self.active.as_ref().is_some_and(|active| active.id == id) {
            return;
        }
        self.selected.insert(workspace.clone(), id.clone());
        self.retain_active_history();
        self.list_cursor = self
            .ids
            .iter()
            .position(|candidate| *candidate == id)
            .unwrap_or(0);
        let mut active = Active::new(id);
        if let Some(index) = self
            .recent_history
            .iter()
            .position(|(id, _)| *id == active.id)
        {
            active.history = self.recent_history.remove(index).expect("cached history").1;
        }
        self.active = Some(active);
        self.editor = None;
        self.notice = None;
        self.images_wanted.clear();
        self.mark = None;
        self.paint.invalidate_hits();
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn retain_active_history(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        if let Some(draft) = self.drafts.get_mut(&active.id) {
            draft.completion = None;
        }
        if active.history.loaded {
            // In-flight reads belong to the old selection epoch. Catalog catch-up
            // resumes from the retained tail once this channel is selected again.
            active.history.load = None;
            active.history.error = None;
            active.history.visible_through = 0;
            self.recent_history.push_front((active.id, active.history));
            self.recent_history.truncate(3);
        }
    }

    pub(super) fn set_channel_chooser(&mut self, open: bool) {
        if self.channel_chooser == open {
            return;
        }
        self.channel_chooser = open;
        self.hover_channel(None);
        if open {
            self.list_cursor = self
                .active
                .as_ref()
                .and_then(|active| self.ids.iter().position(|id| *id == active.id))
                .unwrap_or(0);
        }
        self.paint.invalidate_hits();
        self.redraw();
    }

    fn writable(&self, read_only: bool) -> bool {
        !read_only && self.summary().is_some_and(|channel| !channel.direct)
    }

    fn draft(&mut self) -> Option<&mut Draft> {
        Some(
            self.drafts
                .entry(self.active.as_ref()?.id.clone())
                .or_default(),
        )
    }

    pub(super) fn paste(&mut self, text: &str, read_only: bool) {
        if let Some(editor) = &mut self.editor {
            if !read_only {
                editor.form.paste(text);
            }
        } else if !self.channel_chooser
            && self.writable(read_only)
            && let Some(active) = &self.active
            && !self.sends.contains_key(&active.id)
            && let Some(draft) = self.draft()
        {
            draft.paste(text);
        }
    }

    pub(super) fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    pub(super) fn editor_height(&self, width: u16, read_only: bool) -> Option<u16> {
        Some(
            self.editor
                .as_ref()?
                .form
                .preferred_height(width, !read_only),
        )
    }

    pub(super) fn close_editor(&mut self) {
        if self.editor.take().is_some() {
            self.redraw();
        }
    }

    pub(super) fn cancel_editor_press(&mut self) {
        if let Some(editor) = &mut self.editor {
            editor.form.cancel_press();
        }
    }

    pub(super) fn paint_editor(
        &mut self,
        surfaces: chat_controls::EditorSurfaces<'_>,
        theme: &bkpr_tui_framework::Theme,
        read_only: bool,
        graphics: &crate::chrome_graphics::ChromeGraphics,
    ) -> Option<bkpr_tui_framework::Rect> {
        self.editor
            .as_mut()?
            .form
            .paint(surfaces, theme, !read_only, Some(graphics))
    }

    pub(super) fn editor_mouse(
        &mut self,
        mouse: termwiz::input::MouseEvent,
        action: bkpr_protocol::MouseAction,
        button: bkpr_protocol::MouseButton,
        read_only: bool,
    ) -> Effect {
        let Some(editor) = &mut self.editor else {
            return Effect::None;
        };
        let action = editor.form.mouse(mouse, action, button, !read_only);
        self.editor_action(action)
    }

    fn activate_button(&mut self, hit: Hit, read_only: bool) {
        if self.editor.is_some() {
            return;
        }
        match hit {
            Hit::Create if self.supports(ChatAction::Create, read_only) => self.start_editor(true),
            Hit::Manage if self.writable(read_only) => self.start_editor(false),
            _ => {}
        }
    }

    fn start_editor(&mut self, create: bool) {
        self.cancel_button_press();
        self.redraw();
        self.set_channel_chooser(false);
        let form = if create {
            self.workspace.clone().map(chat_controls::Editor::create)
        } else {
            self.summary().and_then(chat_controls::Editor::manage)
        };
        self.editor = form.map(|form| {
            self.next = self.next.wrapping_add(1);
            Editor {
                token: self.next,
                form,
            }
        });
    }

    fn editor_action(&mut self, action: chat_controls::EditorAction) -> Effect {
        match action {
            chat_controls::EditorAction::None => Effect::None,
            chat_controls::EditorAction::Close => {
                self.close_editor();
                Effect::None
            }
            chat_controls::EditorAction::Submit(request) => {
                let editor = self.editor.as_ref().expect("active controls");
                Effect::Submit(JobKind::EditorWrite(editor.token), Job::Request(request))
            }
        }
    }

    pub(super) fn context(&self) -> Context {
        if self.editor.is_some() {
            Context::ChatControls
        } else if self.channel_chooser {
            Context::ChatChannels
        } else if self.completing() {
            Context::ChatMention
        } else {
            Context::Chat
        }
    }

    pub(super) fn supports(&self, action: ChatAction, read_only: bool) -> bool {
        let send = self
            .active
            .as_ref()
            .and_then(|active| self.sends.get(&active.id));
        match action {
            ChatAction::Create => !read_only && self.catalog_ready && self.catalog_error.is_none(),
            ChatAction::Manage => self.writable(read_only),
            ChatAction::MarkRead => !read_only && self.active.is_some(),
            ChatAction::Retry => self.writable(read_only) && matches!(send, Some(Send::Unknown(_))),
            ChatAction::Send | ChatAction::Newline | ChatAction::Edit(_) => {
                self.writable(read_only) && send.is_none()
            }
            ChatAction::Choose if !self.channel_chooser && self.completing() => {
                self.active.as_ref().is_some_and(|active| {
                    active.roster_ready
                        && self.drafts.get(&active.id).is_some_and(|draft| {
                            draft
                                .choices(&active.roster, self.paint.completion_capacity())
                                .any(|(_, _, chosen)| chosen)
                        })
                })
            }
            ChatAction::Choose => !self.ids.is_empty(),
            ChatAction::ChannelChooser | ChatAction::Back => true,
        }
    }

    pub(super) fn key(&mut self, event: &KeyEvent, read_only: bool) -> Effect {
        if let Some(editor) = &mut self.editor {
            let action = editor.form.key(event, !read_only);
            return self.editor_action(action);
        }
        let action = keyguide::lookup_with(
            self.context(),
            event,
            |action| !matches!(action, Action::Chat(action) if !self.supports(action, read_only)),
        );
        match action {
            Some(Action::Chat(ChatAction::ChannelChooser)) => {
                self.set_channel_chooser(!self.channel_chooser);
            }
            Some(Action::Chat(ChatAction::Back)) => {
                if self.channel_chooser {
                    self.set_channel_chooser(false);
                } else if self.completing() {
                    self.draft().unwrap().completion = None;
                    self.redraw_composer();
                } else {
                    return Effect::Native;
                }
            }
            Some(Action::Chat(ChatAction::Create)) => self.start_editor(true),
            Some(Action::Chat(ChatAction::Manage)) => self.start_editor(false),
            Some(Action::Chat(ChatAction::MarkRead)) => {
                self.redraw();
                self.explicit_read();
            }
            Some(Action::Chat(action @ (ChatAction::Retry | ChatAction::Send))) => {
                self.redraw();
                return self.send(action == ChatAction::Retry);
            }
            Some(Action::Chat(ChatAction::Newline)) => {
                if let Some(draft) = self.draft() {
                    draft.insert("\n", false);
                    self.redraw_composer();
                }
            }
            Some(Action::Chat(ChatAction::Edit(edit))) => {
                if let Some(draft) = self.draft() {
                    draft.edit(edit);
                    self.redraw_composer();
                }
            }
            Some(Action::Chat(ChatAction::Choose)) if !self.channel_chooser => {
                if let Some(active) = &self.active
                    && let Some(draft) = self.drafts.get_mut(&active.id)
                {
                    draft.select_mention(&active.roster, self.paint.completion_capacity(), None);
                    self.redraw_composer();
                }
            }
            Some(Action::Chat(ChatAction::Choose)) => {
                if let Some(id) = self.ids.get(self.list_cursor).cloned() {
                    self.select(id);
                }
                self.set_channel_chooser(false);
            }
            Some(Action::History(HistoryAction::PageUp)) => {
                self.redraw();
                self.scroll(-(self.paint.page_rows().max(1) as isize));
            }
            Some(Action::History(HistoryAction::PageDown)) => {
                self.redraw();
                self.scroll(self.paint.page_rows().max(1) as isize);
            }
            Some(Action::History(HistoryAction::Live)) => {
                self.redraw();
                self.live_tail();
            }
            Some(Action::Navigate(navigation)) => self.navigate(navigation),
            _ if !self.channel_chooser && self.writable(read_only) => self.composer_key(event),
            _ => {}
        }
        Effect::None
    }

    fn navigate(&mut self, navigation: Navigation) {
        if !self.channel_chooser {
            if let Some(active) = &self.active
                && active.roster_ready
                && self.paint.completion_capacity() > 0
                && let Some(draft) = self.drafts.get_mut(&active.id)
            {
                let delta = if navigation == Navigation::Previous {
                    -1
                } else {
                    1
                };
                draft.move_completion(delta, &active.roster);
                self.redraw_composer();
            }
        } else {
            let last = self.ids.len().saturating_sub(1);
            self.list_cursor = match navigation {
                Navigation::Previous => self.list_cursor.saturating_sub(1),
                Navigation::Next => (self.list_cursor + 1).min(last),
                Navigation::First => 0,
                Navigation::Last => last,
                _ => self.list_cursor,
            };
            self.redraw();
        }
    }

    fn completing(&self) -> bool {
        self.active
            .as_ref()
            .and_then(|active| self.drafts.get(&active.id))
            .is_some_and(|draft| draft.completion.is_some())
    }

    fn composer_key(&mut self, event: &KeyEvent) {
        let Some(active) = &mut self.active else {
            return;
        };
        if self.sends.contains_key(&active.id) {
            return;
        }
        let draft = self.drafts.entry(active.id.clone()).or_default();
        let mods = event.modifiers & 15;
        if mods == 0 && matches!(event.key.as_str(), "ArrowUp" | "ArrowDown") {
            let delta = if event.key == "ArrowUp" { -1 } else { 1 };
            draft.move_vertical(usize::from(self.paint.composer_width()), delta);
            self.redraw_composer();
        } else if mods & !1 == 0 && event.text.chars().any(|ch| !ch.is_control()) {
            if draft.insert(&event.text, event.action == KeyAction::Press) {
                active.roster.clear();
                active.roster_next = Some(None);
                active.roster_error = None;
                active.roster_ready = false;
            }
            self.redraw_composer();
        }
    }

    fn send(&mut self, retry: bool) -> Effect {
        let Some(id) = self.active.as_ref().map(|active| active.id.clone()) else {
            return Effect::None;
        };
        let request = match self.sends.get(&id) {
            Some(Send::Unknown(request)) if retry => request.clone(),
            Some(_) => return Effect::None,
            None if retry => return Effect::None,
            None => match self.drafts.entry(id.clone()).or_default().post(id.clone()) {
                Ok(request) => request,
                Err(error) => {
                    self.drafts.get_mut(&id).unwrap().error = Some(error.into());
                    return Effect::None;
                }
            },
        };
        self.sends.insert(
            id.clone(),
            Send::Pending {
                request: request.clone(),
                retry,
            },
        );
        Effect::Submit(JobKind::Send(id), Job::Request(request))
    }

    fn explicit_read(&mut self) {
        let Some(summary) = self.summary() else {
            return;
        };
        if summary.last_sequence > summary.read_through
            && self.should_queue_mark(&summary.id, summary.last_sequence)
        {
            self.mark = Some((summary.id.clone(), summary.last_sequence));
        }
    }

    pub(super) fn attend(&mut self, physical_focus: bool, read_only: bool) {
        if !physical_focus || read_only || !self.is_attending() {
            return;
        }
        let Some(active) = &self.active else { return };
        let Some(summary) = self.catalog.get(&active.id) else {
            return;
        };
        let through = active.history.visible_through;
        if through > summary.read_through
            && active.history.read_failed != Some(through)
            && self.should_queue_mark(&summary.id, through)
        {
            self.mark = Some((summary.id.clone(), through));
        }
    }

    fn should_queue_mark(&self, channel: &ChannelId, through: u64) -> bool {
        !self
            .mark
            .as_ref()
            .is_some_and(|(id, queued)| id == channel && *queued >= through)
            && !self.pending.values().any(|kind| {
                matches!(kind, JobKind::Mark { channel: pending, through: sent }
                if pending == channel && *sent >= through)
            })
    }

    fn live_tail(&mut self) {
        let latest = self.summary().map_or(0, |summary| summary.last_sequence);
        if let Some(active) = &mut self.active {
            active.history.anchor = None;
            active.history.paused_at = None;
            active.history.error = None;
            if !active.history.loaded || active.history.latest() < latest {
                active.history.load = Some(Load::Latest);
            }
        }
    }

    fn scroll(&mut self, delta: isize) {
        let newest = self.summary().map_or(0, |summary| summary.last_sequence);
        let Some(active) = &mut self.active else {
            return;
        };
        let history = &mut active.history;
        let Some((anchor, boundary)) = self.paint.scroll(delta) else {
            return;
        };
        let loaded = history.latest();
        if delta > 0 && boundary && loaded >= newest {
            history.anchor = None;
            history.paused_at = None;
        } else {
            history.anchor = Some(anchor);
            history.paused_at.get_or_insert(loaded);
            if delta > 0 && boundary && loaded < newest {
                history.load = Some(Load::Newer(loaded));
            }
        }
        if boundary
            && delta < 0
            && history.old
            && let Some(front) = history.entries.front()
        {
            history.load = Some(Load::Older(front.sequence()));
        }
        if boundary {
            history.error = None;
        }
    }

    pub(super) fn mouse(
        &mut self,
        mouse: termwiz::input::MouseEvent,
        button: bkpr_protocol::MouseButton,
        action: bkpr_protocol::MouseAction,
        read_only: bool,
    ) -> Effect {
        use bkpr_protocol::{MouseAction as A, MouseButton as B};
        let hit = self.paint.hit(mouse.x, mouse.y);
        let writable = self.writable(read_only);
        match (button, action, hit) {
            (B::WheelUp | B::WheelDown, _, Some(Hit::Channel(_) | Hit::List)) => {
                self.list_cursor = self
                    .list_cursor
                    .saturating_add_signed(if button == B::WheelUp { -3 } else { 3 })
                    .min(self.ids.len().saturating_sub(1));
            }
            (B::WheelUp | B::WheelDown, _, Some(Hit::History(_))) => {
                self.scroll(if button == B::WheelUp { -3 } else { 3 })
            }
            (B::Left, A::Press, Some(Hit::Channel(index))) => {
                if let Some(id) = self.ids.get(index).cloned() {
                    let viewport = self.list_cursor;
                    self.select(id);
                    if !self.channel_chooser {
                        self.list_cursor = viewport;
                    }
                }
                self.set_channel_chooser(false);
            }
            (B::Left, A::Press, Some(Hit::OpenChannels)) => self.set_channel_chooser(true),
            (B::Left, A::Press, Some(Hit::Mention(index))) if writable => {
                if let Some(active) = &self.active
                    && active.roster_ready
                    && let Some(draft) = self.drafts.get_mut(&active.id)
                {
                    draft.select_mention(
                        &active.roster,
                        self.paint.completion_capacity(),
                        Some(index),
                    );
                }
            }
            (B::Left, A::Press, Some(Hit::Latest)) => self.live_tail(),
            (B::Right, A::Press, Some(Hit::History(Some(index)))) if writable => {
                if let Some(active) = &self.active
                    && !self.sends.contains_key(&active.id)
                    && let Some(entry) = active.history.entries.get(index)
                {
                    self.drafts.entry(active.id.clone()).or_default().reply_to =
                        Some(entry.message.message.id.clone());
                }
            }
            _ => {}
        }
        Effect::None
    }

    pub(super) fn next_job(&mut self, read_only: bool) -> Option<(JobKind, Job)> {
        if let Some(editor) = &mut self.editor
            && let Some(request) = editor.form.take_observation()
        {
            return Some((JobKind::EditorRead(editor.token), Job::Request(request)));
        }
        let active = self.active.as_mut()?;
        let channel = &active.id;
        let summary = self.catalog.get(channel)?;
        let epoch = self.epoch;
        if !self.pending.values().any(
            |kind| matches!(kind, JobKind::History { epoch: pending, .. } if *pending == epoch),
        ) {
            let history = &mut active.history;
            if history.load.is_none()
                && history.error.is_none()
                && history.anchor.is_none()
                && history.latest() < summary.last_sequence
            {
                history.load = Some(if history.entries.is_empty() {
                    Load::Latest
                } else {
                    Load::Newer(history.latest())
                });
            }
            if let Some(load) = history.load.take() {
                let (before, after) = match &load {
                    Load::Latest => (None, None),
                    Load::Older(sequence) => (Some(*sequence), None),
                    Load::Newer(sequence) => (None, Some(*sequence)),
                    Load::Jump { sequence, .. } => (None, Some(sequence.saturating_sub(1))),
                };
                return Some((
                    JobKind::History { epoch, load },
                    Job::Request(ChannelRequest::History {
                        channel_id: channel.clone(),
                        before,
                        after,
                        limit: PAGE,
                    }),
                ));
            }
        }
        if self
            .drafts
            .get(channel)
            .is_some_and(|draft| draft.completion.is_some())
            && !self
                .pending
                .values()
                .any(|kind| *kind == JobKind::Roster(epoch))
            && let Some(after) = active.roster_next.take()
        {
            return Some((
                JobKind::Roster(epoch),
                Job::Request(ChannelRequest::UserRoster {
                    workspace_id: summary.workspace_id.clone(),
                    channel_id: Some(channel.clone()),
                    after,
                    limit: MAX_PAGE_LIMIT,
                }),
            ));
        }
        if !read_only
            && !self.pending.values().any(
                |kind| matches!(kind, JobKind::Mark { channel: pending, .. } if pending == channel),
            )
            && let Some((selected, through)) = self.mark.take()
            && selected == *channel
            && through > summary.read_through
        {
            return Some((
                JobKind::Mark {
                    channel: selected.clone(),
                    through,
                },
                Job::Request(ChannelRequest::MarkRead {
                    channel_id: selected,
                    through,
                }),
            ));
        }
        for id in &self.images_wanted {
            if self.images.begin(channel, id) {
                return Some((
                    JobKind::Image {
                        channel: channel.clone(),
                        attachment: id.clone(),
                    },
                    Job::Image(channel.clone(), id.clone()),
                ));
            }
        }
        None
    }

    pub(super) fn track(&mut self, kind: JobKind) -> String {
        self.next = self.next.wrapping_add(1);
        let key = format!("chat:{}", self.next);
        self.pending.insert(key.clone(), kind);
        key
    }

    /// Whether shutdown must drain this job's write receipt.
    pub(super) fn is_shutdown_write(&self, key: &str) -> bool {
        matches!(
            self.pending.get(key),
            Some(JobKind::Send(_) | JobKind::EditorWrite(_) | JobKind::Mark { .. })
        )
    }

    /// Whether an original post still has an unresolved outcome.
    pub(super) fn has_unknown_send(&self) -> bool {
        self.sends
            .values()
            .any(|send| matches!(send, Send::Unknown(_) | Send::Pending { retry: true, .. }))
    }

    pub(super) fn complete(
        &mut self,
        key: &str,
        result: Result<Completion>,
        admitted: bool,
    ) -> Option<(String, anyhow::Error)> {
        let kind = self.pending.remove(key)?;
        if let JobKind::History { epoch, load } = kind {
            if epoch == self.epoch
                && let Some(active) = &mut self.active
            {
                // Repeated scrolling can queue the same boundary before this page arrives.
                if active.history.load.as_ref() == Some(&load) {
                    active.history.load = None;
                }
                match result {
                    Ok(Completion::History { entries, older }) => {
                        active.history.apply(load, entries, older)
                    }
                    Err(error) => active.history.error = Some(error_text(&error)),
                    _ => active.history.error = Some("Unexpected history response".into()),
                }
            }
            return None;
        }
        let result = match kind {
            JobKind::Image {
                channel,
                attachment,
            } => {
                if !admitted {
                    self.images.cancel(&channel, &attachment);
                    return None;
                }
                let image = result.and_then(|result| match result {
                    Completion::Image(image) => Ok(image),
                    _ => bail!("invalid channel image response"),
                });
                if let Some(transport) = self.images.finish(&channel, &attachment, image) {
                    self.transports.push(transport);
                }
                return None;
            }
            _ => result.and_then(|result| match result {
                Completion::Response(result) => Ok(result),
                _ => bail!("invalid channel operation response"),
            }),
        };
        match kind {
            JobKind::Roster(epoch) if epoch == self.epoch => {
                let active = self.active.as_mut().expect("current roster owner");
                if active.roster_next == Some(None) {
                    return None;
                }
                match result {
                    Ok(ChannelResponse::UserRoster {
                        participants,
                        next_after,
                    }) => {
                        active
                            .roster
                            .extend(participants.into_iter().filter(|entry| {
                                entry.participant.kind == ParticipantKind::Agent
                                    && entry.binding.is_some()
                            }));
                        active.roster_next = next_after.map(Some);
                        if active.roster_next.is_none() {
                            active.roster_ready = true;
                            active.roster.sort_unstable_by(|a, b| {
                                a.participant
                                    .display_name
                                    .cmp(&b.participant.display_name)
                                    .then_with(|| a.participant.id.cmp(&b.participant.id))
                            });
                        }
                    }
                    Err(error) => active.roster_error = Some(error_text(&error)),
                    _ => active.roster_error = Some("Unexpected roster response".into()),
                }
            }
            JobKind::Mark { channel, through } => match result {
                Ok(ChannelResponse::ReadPosition { through: applied }) if applied >= through => {
                    if let Some(summary) = self.catalog.get_mut(&channel) {
                        summary.read_through = summary.read_through.max(applied);
                    }
                }
                result => {
                    if let Some(active) = &mut self.active
                        && active.id == channel
                    {
                        active.history.read_failed = Some(through);
                        self.notice =
                            Some((Role::Warning, "Could not mark read; Alt-m retries".into()));
                    }
                    let error = result.err().unwrap_or_else(|| {
                        transport::mark_outcome_unknown(anyhow::anyhow!(
                            "unexpected mark-read response"
                        ))
                    });
                    return Some(("Couldn't confirm a channel read position.".into(), error));
                }
            },
            JobKind::Send(channel) => {
                let Some(Send::Pending { request, retry }) = self.sends.remove(&channel) else {
                    return None;
                };
                match result {
                    Ok(ChannelResponse::Posted {
                        channel_id,
                        message,
                        ..
                    }) if channel_id.as_ref().is_none_or(|id| id == &channel) => {
                        self.drafts.remove(&channel);
                        if let Some(summary) = self.catalog.get_mut(&channel) {
                            summary.last_sequence = summary.last_sequence.max(message.sequence);
                        }
                    }
                    result => {
                        let error = result.err().unwrap_or_else(|| {
                            transport::mark_outcome_unknown(anyhow::anyhow!(
                                "unexpected post response"
                            ))
                        });
                        let unknown = transport::outcome_unknown(&error);
                        let draft = self.drafts.entry(channel.clone()).or_default();
                        draft.error = Some(if retry && !unknown {
                            format!(
                                "Retry rejected: {} · Original outcome unknown · Ctrl-r retries the identical post",
                                error_text(&error)
                            )
                        } else if unknown {
                            "Send outcome unknown · Ctrl-r retries the identical post".into()
                        } else {
                            format!("Send rejected: {}", error_text(&error))
                        });
                        if retry || unknown {
                            self.sends.insert(channel, Send::Unknown(request));
                        }
                        return Some(("Couldn't confirm a channel post.".into(), error));
                    }
                }
            }
            JobKind::EditorRead(token) => {
                if let Some(editor) = &mut self.editor
                    && editor.token == token
                {
                    editor
                        .form
                        .observe(result.map_err(|error| error_text(&error)));
                }
            }
            JobKind::EditorWrite(token) => {
                use chat_controls::EditorResult;
                match result {
                    Ok(ChannelResponse::Channel { channel }) => {
                        let id = channel.id.clone();
                        let select = self
                            .editor
                            .as_mut()
                            .filter(|editor| editor.token == token)
                            .is_some_and(|editor| {
                                editor.form.finish(EditorResult::Applied(&channel));
                                editor.form.is_create()
                            });
                        if select && !self.catalog.contains_key(&id) {
                            let mut provisional = channel;
                            // Let the ordered catalog publish any badge transition.
                            provisional.unread_attention = false;
                            self.upsert(provisional);
                        }
                        if select {
                            self.editor = None;
                            self.select(id);
                        }
                    }
                    result => {
                        let error = result.err().unwrap_or_else(|| {
                            transport::mark_outcome_unknown(anyhow::anyhow!(
                                "unexpected channel settings response"
                            ))
                        });
                        if let Some(editor) = &mut self.editor
                            && editor.token == token
                        {
                            let description = error_text(&error);
                            editor.form.finish(if transport::outcome_unknown(&error) {
                                EditorResult::Unknown(&description)
                            } else {
                                EditorResult::Rejected(&description)
                            });
                        }
                        return Some(("Couldn't confirm a channel change.".into(), error));
                    }
                }
            }
            _ => {}
        }
        None
    }

    pub(super) fn transports(&mut self) -> Vec<bkpr_tui_framework::PreparedTransport> {
        std::mem::take(&mut self.transports)
    }

    pub(super) fn surface(&self) -> &UiSurface {
        &self.paint.surface
    }
}

impl History {
    fn latest(&self) -> u64 {
        self.entries.back().map_or(0, view::Entry::sequence)
    }

    fn apply(&mut self, load: Load, messages: Vec<view::Entry>, older: bool) {
        self.loaded = true;
        self.error = None;
        let prepend = matches!(load, Load::Older(_));
        if matches!(load, Load::Latest | Load::Jump { .. }) {
            self.entries.clear();
            self.bytes = 0;
            self.anchor = None;
            self.paused_at = None;
        }
        if !matches!(load, Load::Newer(_)) {
            self.old = older;
        }
        if let Load::Jump {
            message,
            sequence,
            known_tail,
        } = load
        {
            if messages.first().is_some_and(|entry| {
                entry.message.message.id == message && entry.sequence() == sequence
            }) {
                self.anchor = Some(Anchor { sequence, line: 0 });
                self.paused_at = Some(known_tail);
                self.old = sequence > 1;
            } else {
                self.error = Some("Original channel message is no longer available".into());
            }
        }
        let mut add = |entry: view::Entry| {
            self.bytes += entry.message.content_bytes();
            if prepend {
                self.entries.push_front(entry);
            } else {
                self.entries.push_back(entry);
            }
        };
        if prepend {
            messages.into_iter().rev().for_each(&mut add);
        } else {
            messages.into_iter().for_each(&mut add);
        }
        while self.entries.len() > HISTORY_ROWS || self.bytes > HISTORY_BYTES {
            let front = !prepend
                && self.anchor.is_none_or(|anchor| {
                    self.entries
                        .front()
                        .is_some_and(|entry| entry.sequence() < anchor.sequence)
                });
            let entry = if front {
                self.entries.pop_front()
            } else {
                self.entries.pop_back()
            }
            .expect("nonempty history above bound");
            self.bytes -= entry.message.content_bytes();
            self.old |= front;
        }
    }
}

fn channel_order(channel: &ChannelSummary) -> (u8, &str, &ChannelId) {
    let order = match (channel.direct, channel.name.as_str()) {
        (false, "#desk") => 0,
        (false, "#team") => 1,
        (false, _) => 2,
        (true, _) => 3,
    };
    (order, channel.name.as_str(), &channel.id)
}

fn error_text(error: &anyhow::Error) -> String {
    safe_text(&error.root_cause().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: &str) -> ChannelSummary {
        ChannelSummary {
            id: ChannelId(id.into()),
            workspace_id: "workspace".into(),
            name: format!("#{id}"),
            membership: MembershipMode::WorkspaceWide,
            settings: ChannelSettings::default(),
            last_sequence: 12,
            read_through: 0,
            unread_attention: false,
            direct: false,
        }
    }

    fn open_chat() -> (Chat, ChannelId) {
        let mut chat = Chat::default();
        let summary = channel("desk");
        let id = summary.id.clone();
        chat.replace_catalog(HashMap::from([(id.clone(), summary)]));
        chat.open("workspace".into());
        (chat, id)
    }

    fn posted(sequence: u64) -> MessageSummary {
        MessageSummary {
            id: MessageId(format!("message-{sequence}")),
            sequence,
            author: Participant {
                id: ParticipantId("user".into()),
                kind: ParticipantKind::User,
                display_name: "User".into(),
            },
            posted_at_ms: 0,
            intent: PostIntent::default(),
            mentions: Vec::new(),
            reply_to: None,
            attachments: Vec::new(),
            mention_spans: Vec::new(),
            source: None,
        }
    }

    #[test]
    fn a_rejected_retry_does_not_unlock_an_unknown_original_post() {
        let (mut chat, id) = open_chat();
        chat.drafts
            .entry(id.clone())
            .or_default()
            .insert("hello", false);
        let Effect::Submit(kind, Job::Request(original)) = chat.send(false) else {
            panic!("expected an original post");
        };
        let key = chat.track(kind);
        let error = transport::mark_outcome_unknown(anyhow::anyhow!("lost receipt"));
        assert!(chat.complete(&key, Err(error), true).is_some());

        for error in ["not admitted", "binding changed"] {
            let Effect::Submit(kind, Job::Request(retry)) = chat.send(true) else {
                panic!("expected an idempotent retry");
            };
            assert_eq!(retry, original);
            let key = chat.track(kind);
            assert!(chat.is_shutdown_write(&key));
            assert!(chat.has_unknown_send());
            assert!(
                chat.complete(&key, Err(anyhow::anyhow!("{error}")), true)
                    .is_some()
            );
            assert!(matches!(chat.send(false), Effect::None));
            assert!(chat.has_unknown_send());
        }

        let Effect::Submit(kind, Job::Request(retry)) = chat.send(true) else {
            panic!("expected an idempotent retry");
        };
        assert_eq!(retry, original);
        let key = chat.track(kind);
        let response = ChannelResponse::Posted {
            channel_id: Some(id.clone()),
            message: posted(13),
            wake_count: 0,
            attention: false,
        };
        assert!(
            chat.complete(&key, Ok(Completion::Response(response)), true)
                .is_none()
        );
        assert!(!chat.has_unknown_send());
        assert!(!chat.drafts.contains_key(&id));
    }

    #[test]
    fn management_receipt_cannot_replace_ordered_policy_or_consume_a_badge_transition() {
        let (mut chat, id) = open_chat();
        chat.start_editor(false);
        let key = chat.track(JobKind::EditorWrite(chat.editor.as_ref().unwrap().token));
        let mut latest = channel("desk");
        latest.membership = MembershipMode::Explicit;
        latest.settings.attention_reports = !latest.settings.attention_reports;
        assert!(
            !chat.apply_catalog_changes(vec![ChannelCatalogChange::Upsert {
                channel: latest.clone(),
            }])
        );

        let mut stale = channel("desk");
        stale.unread_attention = true;
        let receipt = Completion::Response(ChannelResponse::Channel { channel: stale });
        assert!(chat.complete(&key, Ok(receipt), true).is_none());
        assert_eq!(chat.catalog[&id], latest);
        assert_eq!(chat.attention().count(), 0);

        latest.unread_attention = true;
        assert!(chat.apply_catalog_changes(vec![ChannelCatalogChange::Upsert { channel: latest }]));
        assert_eq!(chat.attention().collect::<Vec<_>>(), ["workspace"]);
    }

    #[test]
    fn create_receipt_only_seeds_missing_entry_without_publishing_attention() {
        let (mut chat, _) = open_chat();
        chat.start_editor(true);
        let key = chat.track(JobKind::EditorWrite(chat.editor.as_ref().unwrap().token));
        let mut created = channel("created");
        created.unread_attention = true;
        let receipt = Completion::Response(ChannelResponse::Channel {
            channel: created.clone(),
        });
        assert!(chat.complete(&key, Ok(receipt), true).is_none());
        assert_eq!(chat.summary().map(|entry| &entry.id), Some(&created.id));
        assert_eq!(chat.attention().count(), 0);
        assert!(chat.upsert(created.clone()));

        chat.start_editor(true);
        let key = chat.track(JobKind::EditorWrite(chat.editor.as_ref().unwrap().token));
        let receipt = Completion::Response(ChannelResponse::Channel {
            channel: channel("created"),
        });
        assert!(chat.complete(&key, Ok(receipt), true).is_none());
        assert_eq!(chat.summary(), Some(&created));
    }

    #[test]
    fn repeated_history_boundary_does_not_request_the_same_page_twice() {
        let (mut chat, _) = open_chat();
        let boundary = Load::Older(6);
        let history = &mut chat.active.as_mut().unwrap().history;
        history.anchor = Some(Anchor {
            sequence: 6,
            line: 0,
        });
        history.load = Some(boundary.clone());
        let (kind, _) = chat.next_job(false).unwrap();
        let key = chat.track(kind);
        chat.active.as_mut().unwrap().history.load = Some(boundary);
        assert!(chat.next_job(false).is_none());
        let entry = view::Entry::new(ChannelMessage {
            message: posted(5),
            text: "older".into(),
            attachments: Vec::new(),
        });
        let page = Completion::History {
            entries: vec![entry],
            older: true,
        };
        assert!(chat.complete(&key, Ok(page), true).is_none());
        assert!(chat.next_job(false).is_none());
    }

    #[test]
    fn same_mark_read_is_not_requeued_behind_its_in_flight_request() {
        let (mut chat, _) = open_chat();
        let history = &mut chat.active.as_mut().unwrap().history;
        history.load = None;
        history.anchor = Some(Anchor {
            sequence: 12,
            line: 0,
        });
        history.visible_through = 12;
        chat.attend(true, false);
        let (kind, _) = chat.next_job(false).unwrap();
        let key = chat.track(kind);
        chat.attend(true, false);
        chat.explicit_read();
        assert!(
            chat.complete(&key, Err(anyhow::anyhow!("rejected")), true)
                .is_some()
        );
        chat.attend(true, false);
        assert!(chat.next_job(false).is_none());
        chat.explicit_read();
        assert!(matches!(
            chat.next_job(false),
            Some((JobKind::Mark { through: 12, .. }, _))
        ));
    }

    #[test]
    fn mention_roster_follows_empty_and_over_membership_cap_pages() {
        let (mut chat, id) = open_chat();
        let draft = chat.drafts.entry(id).or_default();
        assert!(draft.insert("@", true));
        let active = chat.active.as_mut().unwrap();
        active.history.load = None;
        active.history.anchor = Some(Anchor {
            sequence: 12,
            line: 0,
        });
        active.roster_next = Some(None);

        let limit = usize::from(MAX_PAGE_LIMIT);
        let bound_pages = MAX_MEMBERS.div_ceil(limit);
        let mut cursor = None;
        for page in 0..=bound_pages + 1 {
            let (kind, Job::Request(ChannelRequest::UserRoster { after, limit, .. })) =
                chat.next_job(false).unwrap()
            else {
                panic!("expected a roster continuation");
            };
            assert_eq!(after, cursor);
            assert_eq!(limit, MAX_PAGE_LIMIT);
            let key = chat.track(kind);
            let participants = if page == 0 {
                Vec::new()
            } else {
                (0..usize::from(limit))
                    .map(|index| {
                        let id = ParticipantId(format!("p-{page:04}-{index:04}"));
                        let live = page <= bound_pages || index == 0;
                        ChannelRosterEntry {
                            participant: Participant {
                                id: id.clone(),
                                kind: if live || index == 1 {
                                    ParticipantKind::Agent
                                } else {
                                    ParticipantKind::User
                                },
                                display_name: id.0.clone(),
                            },
                            binding: live.then(|| ChannelMentionBinding {
                                participant_id: id,
                                launch_id: "launch".into(),
                                binding_generation: 1,
                            }),
                        }
                    })
                    .collect()
            };
            cursor = (page <= bound_pages).then(|| ParticipantId(format!("p-{page:04}-zzzz")));
            let response = ChannelResponse::UserRoster {
                participants,
                next_after: cursor.clone(),
            };
            assert!(
                chat.complete(&key, Ok(Completion::Response(response)), true)
                    .is_none()
            );
            assert_eq!(chat.active.as_ref().unwrap().roster_ready, cursor.is_none());
        }
        let roster = &chat.active.as_ref().unwrap().roster;
        assert_eq!(roster.len(), bound_pages * limit + 1);
        assert_eq!(
            roster.last().unwrap().participant.id.0,
            format!("p-{:04}-0000", bound_pages + 1)
        );
        assert!(chat.next_job(false).is_none());
    }

    #[test]
    fn revisiting_a_channel_keeps_its_anchor_but_catalog_reset_retires_history() {
        let mut chat = Chat::default();
        let catalog = [channel("desk"), channel("team")].map(|entry| (entry.id.clone(), entry));
        let [(desk, _), (team, _)] = &catalog;
        chat.replace_catalog(catalog.clone().into_iter().collect());
        chat.open("workspace".into());
        let history = &mut chat.active.as_mut().unwrap().history;
        history.loaded = true;
        history.anchor = Some(Anchor {
            sequence: 6,
            line: 2,
        });
        chat.select(team.clone());
        assert!(!chat.active.as_ref().unwrap().history.loaded);
        chat.select(desk.clone());
        assert!(
            chat.active
                .as_ref()
                .unwrap()
                .history
                .anchor
                .is_some_and(|anchor| { anchor.sequence == 6 && anchor.line == 2 })
        );
        chat.reset_catalog();
        chat.replace_catalog(catalog.into_iter().collect());
        assert!(!chat.active.as_ref().unwrap().history.loaded);
    }
}
