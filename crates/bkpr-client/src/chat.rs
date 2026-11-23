use super::{chat_controls, chat_draft::{self, Draft}, chat_images, transport};
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
pub(super) enum Focus { Composer, Channels }

#[derive(Clone, Copy, PartialEq, Eq)]
struct Anchor { sequence: u64, line: usize }

#[derive(Default)]
struct History {
    entries: VecDeque<view::Entry>,
    bytes: usize,
    old: bool,
    loaded: bool,
    anchor: Option<Anchor>,
    load: Option<Load>,
    error: Option<String>,
    visible_through: u64,
    read_failed: Option<u64>,
}

#[derive(Clone, PartialEq, Eq)]
enum Load {
    Latest,
    Older(u64),
    Newer(u64),
    Jump { message: MessageId, sequence: u64 },
}

struct Active {
    id: ChannelId,
    history: History,
    roster: Vec<ChannelRosterEntry>,
    roster_next: Option<Option<ParticipantId>>,
    roster_error: Option<String>,
}

impl Active {
    fn new(id: ChannelId) -> Self {
        Self {
            id, history: History { load: Some(Load::Latest), ..History::default() },
            roster: Vec::new(), roster_next: None, roster_error: None,
        }
    }
}

struct Editor {
    token: u64,
    form: chat_controls::Editor,
}

enum Send {
    Pending(ChannelRequest),
    Unknown(ChannelRequest),
}

#[derive(PartialEq, Eq)]
pub(super) enum JobKind {
    History { epoch: u64, load: Load },
    Roster(u64),
    Mark { channel: ChannelId, through: u64 },
    Send(ChannelId),
    EditorRead(u64),
    EditorWrite(u64),
    Image { channel: ChannelId, attachment: AttachmentId },
}

pub(super) enum Job {
    Request(ChannelRequest),
    Image(ChannelId, AttachmentId),
}

pub(super) enum Completion {
    Response(ChannelResponse),
    Image(chat_images::Loaded),
}

impl Job {
    pub(super) fn run(self, endpoint: &transport::Endpoint) -> Result<Completion> {
        match self {
            Self::Request(request) => {
                let Payload::Channels(response) = endpoint.checked_payload(Operation::Channels { request })? else {
                    bail!("endpoint returned a non-channel response");
                };
                Ok(Completion::Response(response))
            }
            Self::Image(channel, attachment) => chat_images::load(endpoint, channel, attachment).map(Completion::Image),
        }
    }
}

pub(super) enum Effect {
    None,
    Native,
    Submit(JobKind, Job),
}

#[derive(Default)]
pub(super) struct Chat {
    catalog: HashMap<ChannelId, ChannelSummary>,
    catalog_ready: bool,
    catalog_error: Option<String>,
    workspace: Option<String>,
    active: Option<Active>,
    selected: HashMap<String, ChannelId>,
    ids: Vec<ChannelId>,
    focus: Option<Focus>,
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
    pub(super) fn workspace(&self) -> Option<&str> { self.workspace.as_deref() }

    pub(super) fn visible(&self) -> bool { self.workspace.is_some() }

    pub(super) fn is_editing(&self) -> bool { self.editor.is_some() }

    pub(super) fn contains_channel(&self, workspace: &str, channel: &ChannelId) -> bool {
        self.catalog.get(channel).is_some_and(|entry| entry.workspace_id == workspace)
    }

    pub(super) fn rendered(&self, channel: &ChannelId, sequence: u64) -> bool {
        self.active.as_ref().is_some_and(|active| active.id == *channel) && self.paint.rendered.contains(&sequence)
    }

    pub(super) fn summary(&self) -> Option<&ChannelSummary> {
        self.catalog.get(&self.active.as_ref()?.id)
    }

    pub(super) fn open(&mut self, workspace: String) {
        if self.workspace.as_ref() == Some(&workspace) {
            self.focus = Some(Focus::Composer);
            return;
        }
        self.close();
        self.workspace = Some(workspace);
        self.focus = Some(Focus::Composer);
        self.refresh_channels();
    }

    pub(super) fn close(&mut self) {
        if let Some(active) = self.active.take()
            && let Some(draft) = self.drafts.get_mut(&active.id) {
            draft.completion = None;
        }
        self.workspace = None;
        self.focus = None;
        self.editor = None;
        self.images_wanted.clear();
        self.mark = None;
        self.notice = None;
        self.paint.invalidate_hits();
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(super) fn jump(&mut self, workspace: String, channel: ChannelId, message: MessageId, sequence: u64) {
        self.open(workspace);
        self.select(channel);
        if let Some(active) = &mut self.active {
            active.history.load = Some(Load::Jump { message, sequence });
            active.history.error = None;
        }
    }

    pub(super) fn replace_catalog(&mut self, catalog: HashMap<ChannelId, ChannelSummary>) {
        self.catalog = catalog;
        self.catalog_ready = true;
        self.catalog_error = None;
        self.refresh_channels();
    }

    pub(super) fn catalog_error(&mut self, error: &anyhow::Error) {
        self.catalog_error = Some(error_text(error));
    }

    pub(super) fn upsert(&mut self, mut summary: ChannelSummary) {
        if let Some(previous) = self.catalog.get(&summary.id) {
            summary.last_sequence = summary.last_sequence.max(previous.last_sequence);
            summary.read_through = summary.read_through.max(previous.read_through);
        }
        let rebuild = self.catalog.get(&summary.id).is_none_or(|old| old.name != summary.name || old.workspace_id != summary.workspace_id);
        self.catalog.insert(summary.id.clone(), summary);
        if rebuild { self.refresh_channels(); }
    }

    pub(super) fn workspace_closed(&mut self, workspace: &str) {
        if self.workspace() == Some(workspace) { self.close(); }
        self.selected.remove(workspace);
        self.drafts.retain(|id, _| self.catalog.get(id).is_none_or(|entry| entry.workspace_id != workspace));
        self.catalog.retain(|_, entry| entry.workspace_id != workspace);
    }

    pub(super) fn attention(&self) -> impl Iterator<Item = &str> {
        self.catalog.values().filter(|summary| summary.unread_attention).map(|summary| summary.workspace_id.as_str())
    }

    fn refresh_channels(&mut self) {
        let Some(workspace) = &self.workspace else { return };
        self.ids.clear();
        self.ids.extend(self.catalog.values().filter(|entry| entry.workspace_id == *workspace).map(|entry| entry.id.clone()));
        self.ids.sort_unstable_by(|left, right| {
            let left = &self.catalog[left];
            let right = &self.catalog[right];
            channel_order(left).cmp(&channel_order(right))
        });
        let selection = self.selected.get(workspace).filter(|id| self.catalog.contains_key(*id)).or_else(|| self.ids.first()).cloned();
        if let Some(id) = selection {
            self.select(id);
        } else {
            self.active = None;
        }
    }

    fn select(&mut self, id: ChannelId) {
        let Some(workspace) = &self.workspace else { return };
        if !self.contains_channel(workspace, &id) { return; }
        if self.active.as_ref().is_some_and(|active| active.id == id) { return; }
        if let Some(active) = self.active.take() && let Some(draft) = self.drafts.get_mut(&active.id) {
            draft.completion = None;
        }
        self.list_cursor = self.ids.iter().position(|candidate| *candidate == id).unwrap_or(0);
        self.selected.insert(workspace.clone(), id.clone());
        self.active = Some(Active::new(id));
        self.editor = None;
        self.notice = None;
        self.images_wanted.clear();
        self.mark = None;
        self.paint.invalidate_hits();
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn writable(&self, read_only: bool) -> bool {
        !read_only && self.summary().is_some_and(|channel| !channel.direct)
    }

    fn draft(&mut self) -> Option<&mut Draft> {
        Some(self.drafts.entry(self.active.as_ref()?.id.clone()).or_default())
    }

    pub(super) fn paste(&mut self, text: &str, read_only: bool) {
        if let Some(editor) = &mut self.editor {
            if !read_only { editor.form.paste(text); }
        } else if self.focus == Some(Focus::Composer) && self.writable(read_only)
            && let Some(active) = &self.active && !self.sends.contains_key(&active.id)
            && let Some(draft) = self.draft()
        {
            // The outer terminal's bracketed-paste framing is already removed.
            if text.len() <= MAX_POST_BYTES && text.contains("\r\n") {
                draft.insert(&text.replace("\r\n", "\n"), false);
            } else {
                draft.insert(text, false);
            }
        }
    }

    fn start_editor(&mut self, create: bool) {
        let form = if create {
            self.workspace.clone().map(chat_controls::Editor::create)
        } else {
            self.summary().and_then(chat_controls::Editor::manage)
        };
        self.editor = form.map(|form| {
            self.next = self.next.wrapping_add(1);
            Editor { token: self.next, form }
        });
    }

    fn editor_action(&mut self, action: chat_controls::EditorAction) -> Effect {
        match action {
            chat_controls::EditorAction::None => Effect::None,
            chat_controls::EditorAction::Close => { self.editor = None; Effect::None }
            chat_controls::EditorAction::Submit(request) => {
                let editor = self.editor.as_ref().expect("active controls");
                Effect::Submit(JobKind::EditorWrite(editor.token), Job::Request(request))
            }
        }
    }

    pub(super) fn key(&mut self, event: &KeyEvent, read_only: bool) -> Effect {
        if let Some(editor) = &mut self.editor {
            let action = editor.form.key(event, !read_only);
            return self.editor_action(action);
        }
        let mods = event.modifiers & 15;
        let press = event.action == KeyAction::Press;
        let key = event.key.as_str();
        let writable = self.writable(read_only);
        let composing = self.focus == Some(Focus::Composer);
        if key == "Tab" && mods == 0 && press && !self.completing() {
            self.focus = Some(if composing { Focus::Channels } else { Focus::Composer });
            return Effect::None;
        }
        if key == "Escape" && mods == 0 && press {
            if self.completing() {
                self.draft().unwrap().completion = None;
            } else if !composing {
                self.focus = Some(Focus::Composer);
            } else {
                return Effect::Native;
            }
            return Effect::None;
        }
        match (key, mods) {
            ("KeyN", 2) if press && !read_only => self.start_editor(true),
            ("KeyE", 2) if press && writable => self.start_editor(false),
            ("KeyM", 2) if press => self.explicit_read(),
            ("KeyR", 2) if press && writable => return self.send(true),
            ("PageUp", 0) => self.scroll(-(self.paint.page_rows().max(1) as isize)),
            ("PageDown", 0) => self.scroll(self.paint.page_rows().max(1) as isize),
            ("End", 2) if press => self.live_tail(),
            ("ArrowUp", 0) if !composing => self.list_cursor = self.list_cursor.saturating_sub(1),
            ("ArrowDown", 0) if !composing => self.list_cursor = (self.list_cursor + 1).min(self.ids.len().saturating_sub(1)),
            ("Home", 0) if !composing => self.list_cursor = 0,
            ("End", 0) if !composing => self.list_cursor = self.ids.len().saturating_sub(1),
            ("Enter", 0) if !composing && press => {
                if let Some(id) = self.ids.get(self.list_cursor).cloned() { self.select(id); }
                self.focus = Some(Focus::Composer);
            }
            _ if !composing || !writable => {}
            _ => return self.composer_key(event),
        }
        Effect::None
    }

    fn completing(&self) -> bool {
        self.active.as_ref().and_then(|active| self.drafts.get(&active.id)).is_some_and(|draft| draft.completion.is_some())
    }

    fn composer_key(&mut self, event: &KeyEvent) -> Effect {
        let Some(active) = &mut self.active else { return Effect::None };
        if self.sends.contains_key(&active.id) { return Effect::None; }
        let draft = self.drafts.entry(active.id.clone()).or_default();
        let mods = event.modifiers & 15;
        let press = event.action == KeyAction::Press;
        match (event.key.as_str(), mods) {
            ("Enter" | "Tab", 0) if draft.completion.is_some() && press => draft.select_mention(&active.roster, None),
            ("ArrowUp", 0) if draft.completion.is_some() => draft.move_completion(-1, &active.roster),
            ("ArrowDown", 0) if draft.completion.is_some() => draft.move_completion(1, &active.roster),
            ("Enter", 0) if press => return self.send(false),
            ("Enter", 1) if press => { draft.insert("\n", false); }
            _ => {
                if let Some(action) = chat_draft::edit_action(event) {
                    draft.edit(action);
                } else if mods & !1 == 0 && event.text.chars().any(|ch| !ch.is_control())
                    && draft.insert(&event.text, press)
                {
                    active.roster.clear();
                    active.roster_next = Some(None);
                    active.roster_error = None;
                }
            }
        }
        Effect::None
    }

    fn send(&mut self, retry: bool) -> Effect {
        let Some(id) = self.active.as_ref().map(|active| active.id.clone()) else { return Effect::None };
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
        self.sends.insert(id.clone(), Send::Pending(request.clone()));
        Effect::Submit(JobKind::Send(id), Job::Request(request))
    }

    fn explicit_read(&mut self) {
        let Some(summary) = self.summary() else { return };
        self.mark = Some((summary.id.clone(), summary.last_sequence));
    }

    pub(super) fn attend(&mut self, physical_focus: bool, read_only: bool) {
        if !physical_focus || read_only || self.editor.is_some() { return; }
        let Some(active) = &self.active else { return };
        let Some(summary) = self.catalog.get(&active.id) else { return };
        let through = active.history.visible_through;
        if through > summary.read_through && active.history.read_failed != Some(through) {
            self.mark = Some((summary.id.clone(), through));
        }
    }

    fn live_tail(&mut self) {
        let latest = self.summary().map_or(0, |summary| summary.last_sequence);
        if let Some(active) = &mut self.active {
            active.history.anchor = None;
            active.history.error = None;
            if !active.history.loaded || active.history.latest() < latest {
                active.history.load = Some(Load::Latest);
            }
        }
    }

    fn scroll(&mut self, delta: isize) {
        let Some(active) = &mut self.active else { return };
        let history = &mut active.history;
        let Some((anchor, boundary)) = self.paint.scroll(delta) else { return };
        if delta > 0 && boundary {
            history.anchor = None;
        } else {
            history.anchor = Some(anchor);
        }
        if boundary && delta < 0 && history.old && let Some(front) = history.entries.front() {
            history.load = Some(Load::Older(front.sequence()));
        }
        if boundary { history.error = None; }
    }

    pub(super) fn mouse(&mut self, mouse: termwiz::input::MouseEvent, button: bkpr_protocol::MouseButton, action: bkpr_protocol::MouseAction, read_only: bool) -> Effect {
        use bkpr_protocol::{MouseAction as A, MouseButton as B};
        if let Some(editor) = &mut self.editor {
            let action = editor.form.mouse(mouse, button == B::Left && action == A::Press, !read_only);
            return self.editor_action(action);
        }
        let hit = self.paint.hit(mouse.x, mouse.y);
        let writable = self.writable(read_only);
        match (button, action, hit) {
            (B::WheelUp | B::WheelDown, _, Some(Hit::Channel(_) | Hit::List)) => {
                self.list_cursor = self.list_cursor.saturating_add_signed(if button == B::WheelUp { -3 } else { 3 }).min(self.ids.len().saturating_sub(1));
            }
            (B::WheelUp | B::WheelDown, _, Some(Hit::History(_))) => self.scroll(if button == B::WheelUp { -3 } else { 3 }),
            (B::Left, A::Press, Some(Hit::Channel(index))) => {
                if let Some(id) = self.ids.get(index).cloned() { self.select(id); }
                self.focus = Some(Focus::Composer);
            }
            (B::Left, A::Press, Some(Hit::List)) => self.focus = Some(Focus::Channels),
            (B::Left, A::Press, Some(Hit::Composer)) => self.focus = Some(Focus::Composer),
            (B::Left, A::Press, Some(Hit::Mention(index))) if writable => {
                if let Some(active) = &self.active && let Some(draft) = self.drafts.get_mut(&active.id) {
                    draft.select_mention(&active.roster, Some(index));
                }
            }
            (B::Left, A::Press, Some(Hit::Latest)) => self.live_tail(),
            (B::Left, A::Press, Some(Hit::Create)) if !read_only => self.start_editor(true),
            (B::Left, A::Press, Some(Hit::Manage)) if writable => self.start_editor(false),
            (B::Right, A::Press, Some(Hit::History(Some(index)))) if writable => {
                if let Some(active) = &self.active && !self.sends.contains_key(&active.id)
                    && let Some(entry) = active.history.entries.get(index) {
                    self.drafts.entry(active.id.clone()).or_default().reply_to = Some(entry.message.message.id.clone());
                    self.focus = Some(Focus::Composer);
                }
            }
            _ => {}
        }
        Effect::None
    }

    pub(super) fn next_job(&mut self, read_only: bool) -> Option<(JobKind, Job)> {
        if let Some(editor) = &mut self.editor && let Some(request) = editor.form.take_observation() {
            return Some((JobKind::EditorRead(editor.token), Job::Request(request)));
        }
        let active = self.active.as_mut()?;
        let channel = &active.id;
        let summary = self.catalog.get(channel)?;
        let epoch = self.epoch;
        if !self.pending.values().any(|kind| matches!(kind, JobKind::History { epoch: pending, .. } if *pending == epoch)) {
            let history = &mut active.history;
            if history.load.is_none() && history.error.is_none() && history.anchor.is_none() && history.latest() < summary.last_sequence {
                history.load = Some(if history.entries.is_empty() { Load::Latest } else { Load::Newer(history.latest()) });
            }
            if let Some(load) = history.load.take() {
                let (before, after) = match &load {
                    Load::Latest => (None, None),
                    Load::Older(sequence) => (Some(*sequence), None),
                    Load::Newer(sequence) => (None, Some(*sequence)),
                    Load::Jump { sequence, .. } => (None, Some(sequence.saturating_sub(1))),
                };
                return Some((JobKind::History { epoch, load }, Job::Request(ChannelRequest::History {
                    channel_id: channel.clone(), before, after, limit: PAGE,
                })));
            }
        }
        if self.drafts.get(channel).is_some_and(|draft| draft.completion.is_some())
            && !self.pending.values().any(|kind| *kind == JobKind::Roster(epoch))
            && let Some(after) = active.roster_next.take() {
            return Some((JobKind::Roster(epoch), Job::Request(ChannelRequest::UserRoster {
                workspace_id: summary.workspace_id.clone(), channel_id: Some(channel.clone()), after, limit: MAX_PAGE_LIMIT,
            })));
        }
        if !read_only && !self.pending.values().any(|kind| matches!(kind, JobKind::Mark { channel: pending, .. } if pending == channel))
            && let Some((selected, through)) = self.mark.take() && selected == *channel && through > summary.read_through {
            return Some((JobKind::Mark { channel: selected.clone(), through }, Job::Request(ChannelRequest::MarkRead { channel_id: selected, through })));
        }
        for id in &self.images_wanted {
            if self.images.begin(channel, id) {
                return Some((JobKind::Image { channel: channel.clone(), attachment: id.clone() }, Job::Image(channel.clone(), id.clone())));
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

    pub(super) fn complete(&mut self, key: &str, result: Result<Completion>) -> Option<(String, anyhow::Error)> {
        let kind = self.pending.remove(key)?;
        let result = match kind {
            JobKind::Image { channel, attachment } => {
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
            JobKind::History { epoch, load } if epoch == self.epoch => {
                match result {
                    Ok(ChannelResponse::History { messages, next_before, .. }) => {
                        let active = self.active.as_mut().expect("current history owner");
                        active.history.apply(load, messages, next_before.is_some());
                    }
                    Err(error) => self.active.as_mut().unwrap().history.error = Some(error_text(&error)),
                    _ => self.active.as_mut().unwrap().history.error = Some("Unexpected history response".into()),
                }
            }
            JobKind::Roster(epoch) if epoch == self.epoch => {
                let active = self.active.as_mut().expect("current roster owner");
                match result {
                    Ok(ChannelResponse::UserRoster { mut participants, next_after }) => {
                        if active.roster_next != Some(None) {
                            participants.truncate(MAX_MEMBERS.saturating_sub(active.roster.len()));
                            active.roster.extend(participants);
                            active.roster_next = next_after.filter(|_| active.roster.len() < MAX_MEMBERS).map(Some);
                            if active.roster_next.is_none() {
                                active.roster.sort_by(|a, b| a.participant.display_name.cmp(&b.participant.display_name).then_with(|| a.participant.id.cmp(&b.participant.id)));
                            }
                        }
                    }
                    Err(error) => active.roster_error = Some(error_text(&error)),
                    _ => active.roster_error = Some("Unexpected roster response".into()),
                }
            }
            JobKind::Mark { channel, through } => match result {
                Ok(ChannelResponse::ReadPosition { through }) => {
                    if let Some(summary) = self.catalog.get_mut(&channel) { summary.read_through = summary.read_through.max(through); }
                }
                _ => {
                    if let Some(active) = &mut self.active && active.id == channel {
                        active.history.read_failed = Some(through);
                        self.notice = Some((Role::Warning, "Could not mark read; Ctrl-m retries".into()));
                    }
                }
            },
            JobKind::Send(channel) => {
                let Some(Send::Pending(request)) = self.sends.remove(&channel) else { return None };
                match result {
                    Ok(ChannelResponse::Posted { message, .. }) => {
                        self.drafts.remove(&channel);
                        if let Some(summary) = self.catalog.get_mut(&channel) { summary.last_sequence = summary.last_sequence.max(message.sequence); }
                    }
                    result => {
                        let error = result.err().unwrap_or_else(|| transport::mark_outcome_unknown(anyhow::anyhow!("unexpected post response")));
                        let unknown = transport::outcome_unknown(&error);
                        let draft = self.drafts.entry(channel.clone()).or_default();
                        draft.error = Some(if unknown { "Send outcome unknown · Ctrl-r retries the identical post".into() } else { format!("Send rejected: {}", error_text(&error)) });
                        if unknown { self.sends.insert(channel, Send::Unknown(request)); }
                        return Some(("Couldn't confirm a channel post.".into(), error));
                    }
                }
            }
            JobKind::EditorRead(token) => {
                if let Some(editor) = &mut self.editor && editor.token == token {
                    editor.form.observe(result.map_err(|error| error_text(&error)));
                }
            }
            JobKind::EditorWrite(token) => {
                use chat_controls::EditorResult;
                match result {
                    Ok(ChannelResponse::Channel { channel }) => {
                        let id = channel.id.clone();
                        let select = self.editor.as_mut().filter(|editor| editor.token == token).is_some_and(|editor| {
                            editor.form.finish(EditorResult::Applied(&channel));
                            editor.form.is_create()
                        });
                        self.upsert(channel);
                        if select { self.editor = None; self.select(id); self.focus = Some(Focus::Composer); }
                    }
                    result => {
                        let error = result.err().unwrap_or_else(|| transport::mark_outcome_unknown(anyhow::anyhow!("unexpected channel settings response")));
                        if let Some(editor) = &mut self.editor && editor.token == token {
                            let description = error_text(&error);
                            editor.form.finish(if transport::outcome_unknown(&error) { EditorResult::Unknown(&description) } else { EditorResult::Rejected(&description) });
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

    pub(super) fn surface(&self) -> &UiSurface { &self.paint.surface }
}

impl History {
    fn latest(&self) -> u64 { self.entries.back().map_or(0, view::Entry::sequence) }

    fn apply(&mut self, load: Load, messages: Vec<ChannelMessage>, older: bool) {
        self.loaded = true;
        self.error = None;
        let prepend = matches!(load, Load::Older(_));
        if matches!(load, Load::Latest | Load::Jump { .. }) {
            self.entries.clear();
            self.bytes = 0;
            self.anchor = None;
        }
        if !matches!(load, Load::Newer(_)) { self.old = older; }
        if let Load::Jump { message, sequence } = load {
            if messages.first().is_some_and(|entry| entry.message.id == message && entry.message.sequence == sequence) {
                self.anchor = Some(Anchor { sequence, line: 0 });
                self.old = sequence > 1;
            } else {
                self.error = Some("Original channel message is no longer available".into());
            }
        }
        let mut add = |message: ChannelMessage| {
            self.bytes += message.content_bytes();
            let entry = view::Entry::new(message);
            if prepend { self.entries.push_front(entry); } else { self.entries.push_back(entry); }
        };
        if prepend { messages.into_iter().rev().for_each(&mut add); } else { messages.into_iter().for_each(&mut add); }
        while self.entries.len() > HISTORY_ROWS || self.bytes > HISTORY_BYTES {
            let front = !prepend;
            let entry = if front { self.entries.pop_front() } else { self.entries.pop_back() }.expect("nonempty history above bound");
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
