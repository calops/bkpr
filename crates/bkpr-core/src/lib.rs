//! Native pane ownership, terminal state and independent client projections for Bkpr.
pub mod authority;
#[cfg(target_os = "linux")]
mod process_tree;
use bkpr_protocol::scene_wire;
mod environment;
pub mod registry;
pub mod transport;
mod writer;

use anyhow::{Context, Result, bail};
use authority::Authority;
#[cfg(test)]
use bkpr_protocol::ViewSpec;
use bkpr_protocol::{
    ClipboardContent, ClipboardLocation, ClipboardPending, ClipboardResponse, Effect, Event, Frame,
    ImageKey, KeyEvent, MouseEvent, ProgramStatus, Size,
};
use bkpr_protocol::{MAX_INPUT_BYTES, PaneRequest as Request, valid_size};
use bkpr_terminal_engine::Terminal;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(5);

struct ClipboardWait {
    info: ClipboardPending,
    response: mpsc::SyncSender<ClipboardResponse>,
}
struct PasteOrigin {
    view_id: String,
    expires: Instant,
}
#[derive(Default)]
struct PasteGrants {
    next: u64,
    origins: BTreeMap<u64, PasteOrigin>,
    revoked: Vec<u64>,
}
struct Published {
    frame: Option<Arc<Frame>>,
    revision: u64,
    events: VecDeque<Event>,
    event_sequence: u64,
    scroll_frames: BTreeMap<String, Arc<Frame>>,
    status: Vec<ProgramStatus>,
    lifecycle: String,
    exit_code: Option<u32>,
    error: Option<String>,
    pid: Option<u32>,
}
impl Default for Published {
    fn default() -> Self {
        Self {
            frame: None,
            scroll_frames: BTreeMap::new(),
            status: Vec::new(),
            revision: 0,
            events: VecDeque::new(),
            event_sequence: 0,
            lifecycle: "starting".into(),
            exit_code: None,
            error: None,
            pid: None,
        }
    }
}
enum Mutation {
    Colors {
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: Option<[u8; 3]>,
        palette: Option<Box<[[u8; 3]; 256]>>,
    },
    Raw(Vec<u8>),
    Paste(Vec<u8>),
    PasteContents(String, Vec<ClipboardContent>, ClipboardLocation, bool),
    Key(KeyEvent),
    Mouse(MouseEvent),
    Focus(bool),
    Refresh,
    Terminate,
}
enum CommandOutcome {
    Applied(Option<u64>),
    Completed,
}
impl CommandOutcome {
    fn response(self) -> Value {
        match self {
            Self::Applied(id) => {
                json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"})
            }
            Self::Completed => {
                json!({"accepted":false,"write_id":null,"semantics":"execution_completed"})
            }
        }
    }
}
type CommandResult = std::result::Result<CommandOutcome, String>;
struct Command {
    mutation: Mutation,
    result: mpsc::SyncSender<CommandResult>,
}
fn completed_command(published: &Published, mutation: &Mutation) -> CommandResult {
    if published.lifecycle == "fault" {
        return Err(published
            .error
            .clone()
            .unwrap_or_else(|| "pane execution faulted".into()));
    }
    if matches!(
        published.lifecycle.as_str(),
        "draining" | "exited" | "terminated"
    ) {
        if matches!(mutation, Mutation::Terminate | Mutation::Focus(_))
            || matches!(mutation, Mutation::Mouse(event)
                if event.action == bkpr_protocol::MouseAction::Release)
            || matches!(mutation, Mutation::Key(event)
                if event.action == bkpr_protocol::KeyAction::Release)
        {
            return Ok(CommandOutcome::Completed);
        }
        return Err("pane execution has completed; input was not accepted".into());
    }
    Err("pane worker stopped without a completed execution".into())
}
#[derive(Default)]
struct SnapshotChanges {
    revision: Mutex<u64>,
    wake: Condvar,
}
impl SnapshotChanges {
    fn revision(&self) -> u64 {
        *self.revision.lock().unwrap()
    }
    fn notify(&self) {
        let mut revision = self.revision.lock().unwrap();
        *revision = revision.wrapping_add(1);
        self.wake.notify_all();
    }
    fn wait(&self, after: u64, wait_ms: u64) {
        if wait_ms == 0 {
            return;
        }
        let revision = self.revision.lock().unwrap();
        let _ = self
            .wake
            .wait_timeout_while(
                revision,
                Duration::from_millis(wait_ms.min(1000)),
                |revision| *revision == after,
            )
            .unwrap();
    }
}
#[cfg(test)]
struct ResizeBarrier {
    entered: mpsc::SyncSender<Size>,
    resume: Mutex<mpsc::Receiver<()>>,
}
struct Shared {
    authority: Mutex<Authority>,
    published: Mutex<Published>,
    clipboard: Mutex<Option<ClipboardWait>>,
    // Preserve native/input command FIFO without blocking view-policy acceptance.
    requests: Mutex<()>,
    // Submission and final queue retirement are atomic; never hold while awaiting a reply.
    command_gate: Mutex<()>,
    view_policy: Mutex<()>,
    pane_id: String,
    writer: Mutex<Option<Arc<writer::Queue>>>,
    paste: Mutex<PasteGrants>,
    changes: Arc<SnapshotChanges>,
    delta_frames: Mutex<BTreeMap<String, (u64, Arc<Frame>)>>,
    finished: AtomicBool,
    forgotten: AtomicBool,
    on_finish: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    resize_wake: crossbeam_channel::Sender<()>,
    #[cfg(test)]
    resize_barrier: Mutex<Option<Arc<ResizeBarrier>>>,
}
#[derive(Clone)]
pub struct Pane {
    shared: Arc<Shared>,
    commands: crossbeam_channel::Sender<Command>,
}
pub struct SpawnSpec {
    pub argv: Vec<String>,
    pub home: PathBuf,
    pub size: Size,
    pub isolated_process: bool,
    pub cwd: PathBuf,
    pub isolated_environment: bool,
}
impl Pane {
    pub fn spawn(spec: SpawnSpec) -> Result<Self> {
        Self::spawn_with_id(spec, Uuid::new_v4().to_string(), Arc::default())
    }
    fn spawn_with_id(
        spec: SpawnSpec,
        pane_id: String,
        view_budget: Arc<authority::ViewBudget>,
    ) -> Result<Self> {
        valid_size(spec.size)?;
        if spec.argv.is_empty() {
            bail!("empty shell argv");
        }
        if spec.isolated_environment {
            prepare_home(&spec.home)?;
        } else if !spec.home.is_absolute() || !spec.home.is_dir() {
            bail!("HOME must be an existing absolute directory");
        }
        if !spec.cwd.is_absolute() || !spec.cwd.is_dir() {
            bail!("cwd must be an existing absolute directory");
        }
        #[cfg(target_os = "linux")]
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let (resize_wake, resize_events) = crossbeam_channel::bounded(1);
        let shared = Arc::new(Shared {
            authority: Mutex::new(Authority::with_budget(view_budget)),
            published: Mutex::new(Published::default()),
            clipboard: Mutex::new(None),
            requests: Mutex::new(()),
            command_gate: Mutex::new(()),
            view_policy: Mutex::new(()),
            pane_id,
            writer: Mutex::new(None),
            paste: Mutex::new(PasteGrants::default()),
            changes: Arc::new(SnapshotChanges::default()),
            delta_frames: Mutex::new(BTreeMap::new()),
            finished: AtomicBool::new(false),
            forgotten: AtomicBool::new(false),
            on_finish: Mutex::new(None),
            resize_wake,
            #[cfg(test)]
            resize_barrier: Mutex::new(None),
        });
        let (commands, rx) = crossbeam_channel::bounded(64);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let state = shared.clone();
        thread::Builder::new()
            .name("bkpr-pane".into())
            .spawn(move || {
                let mut termination_reply = None;
                let result = run_pane(
                    spec,
                    state.clone(),
                    &rx,
                    resize_events,
                    ready_tx,
                    &mut termination_reply,
                );
                let _gate = state.command_gate.lock().unwrap();
                let mut published = state.published.lock().unwrap();
                if let Err(error) = &result {
                    published.lifecycle = "fault".into();
                    published.error = Some(format!("{error:#}"));
                } else if termination_reply.is_some() {
                    published.lifecycle = "terminated".into();
                }
                state.finished.store(true, Ordering::Release);
                if let Some(reply) = termination_reply {
                    let _ = reply.send(
                        result
                            .map(|()| CommandOutcome::Applied(None))
                            .map_err(|error| format!("{error:#}")),
                    );
                }
                for command in rx.try_iter() {
                    let _ = command
                        .result
                        .send(completed_command(&published, &command.mutation));
                }
                drop(published);
                drop(_gate);
                state.changes.notify();
                let notify = state.on_finish.lock().unwrap().clone();
                if let Some(notify) = notify {
                    notify();
                }
            })?;
        ready_rx
            .recv()
            .context("pane worker stopped during spawn")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self { shared, commands })
    }
    fn command(&self, mutation: Mutation) -> Result<CommandOutcome> {
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let _gate = self.shared.command_gate.lock().unwrap();
            let published = self.shared.published.lock().unwrap();
            if self.shared.finished.load(Ordering::Acquire)
                || (published.lifecycle != "running" && !matches!(&mutation, Mutation::Terminate))
            {
                return completed_command(&published, &mutation).map_err(anyhow::Error::msg);
            }
            drop(published);
            self.commands
                .try_send(Command {
                    mutation,
                    result: tx,
                })
                .map_err(|_| anyhow::anyhow!("pane command queue full or closed"))?;
        }
        rx.recv_timeout(Duration::from_secs(8))
            .context("pane command timed out; outcome unknown, do not retry input")?
            .map_err(anyhow::Error::msg)
    }
    fn cancel_clipboard(&self, view: Option<&str>) {
        let mut pending = self.shared.clipboard.lock().unwrap();
        if pending
            .as_ref()
            .is_some_and(|p| view.is_none_or(|id| p.info.view_id == id))
            && let Some(wait) = pending.take()
        {
            let _ = wait.response.try_send(ClipboardResponse::denied());
            drop(pending);
            self.shared.changes.notify();
        }
    }
    pub fn request(&self, request: Request) -> Result<Value> {
        if self.shared.forgotten.load(Ordering::Acquire) {
            bail!("pane was forgotten");
        }
        match &request {
            Request::Detach { view_id } => revoke_paste(&self.shared, Some(view_id)),
            Request::Update { view_id, spec }
                if !spec.input || !spec.clipboard || !spec.visible || !spec.interactive =>
            {
                revoke_paste(&self.shared, Some(view_id))
            }
            Request::Shutdown => revoke_paste(&self.shared, None),
            Request::Terminate { view_id } => {
                self.shared.authority.lock().unwrap().input(view_id)?;
                revoke_paste(&self.shared, None);
            }
            _ => {}
        }
        match &request {
            Request::Inspect => return self.snapshot(None, 0, 0, &[], false, false),
            Request::Poll {
                view_id,
                after,
                frame_after,
                resources,
                snapshot_after,
                wait_ms,
                row_deltas,
                scene_encoding,
            } => {
                if scene_encoding
                    .as_deref()
                    .is_some_and(|encoding| encoding != "compact_v1")
                {
                    bail!("unsupported scene encoding");
                }
                if resources.len() > 1024 {
                    bail!("image receipt limit");
                }
                self.shared.authority.lock().unwrap().touch(view_id)?;
                self.shared.changes.wait(*snapshot_after, *wait_ms);
                return self.snapshot(
                    Some(view_id),
                    *after,
                    *frame_after,
                    resources,
                    *row_deltas,
                    scene_encoding.is_some(),
                );
            }
            Request::Detach { view_id } => self.cancel_clipboard(Some(view_id)),
            Request::Update { view_id, spec }
                if !spec.clipboard || !spec.visible || !spec.interactive =>
            {
                self.cancel_clipboard(Some(view_id))
            }
            Request::Shutdown => self.cancel_clipboard(None),
            Request::Terminate { view_id } => {
                self.shared.authority.lock().unwrap().input(view_id)?;
                self.cancel_clipboard(None);
            }
            _ => {}
        }
        // Clipboard responses must bypass command serialization while feed is synchronously waiting.
        if let Request::Clipboard {
            view_id,
            request_id,
            response,
        } = request
        {
            if response
                .contents
                .iter()
                .map(|c| c.data.len())
                .sum::<usize>()
                > MAX_INPUT_BYTES
            {
                bail!("clipboard payload limit");
            }
            let authority = self.shared.authority.lock().unwrap();
            let view = authority.views.get(&view_id).context("stale view")?;
            if !view.spec.clipboard || !view.spec.visible || !view.spec.interactive {
                bail!("clipboard authority denied");
            }
            let mut pending = self.shared.clipboard.lock().unwrap();
            let wait = pending.as_ref().context("clipboard request expired")?;
            if wait.info.view_id != view_id || wait.info.request_id != request_id {
                bail!("clipboard grant mismatch");
            }
            let wait = pending.take().unwrap();
            let result = wait
                .response
                .try_send(response)
                .context("clipboard request expired");
            drop(pending);
            drop(authority);
            self.shared.changes.notify();
            result?;
            return Ok(json!({"accepted":true}));
        }
        let is_policy = matches!(
            &request,
            Request::Attach { .. }
                | Request::Update { .. }
                | Request::Pin { .. }
                | Request::Detach { .. }
        );
        let _policy = if is_policy {
            Some(self.shared.view_policy.lock().unwrap())
        } else {
            None
        };
        let _serial = if is_policy {
            None
        } else {
            Some(self.shared.requests.lock().unwrap())
        };
        // A policy request may have cloned this runtime before PaneForget committed.
        if self.shared.forgotten.load(Ordering::Acquire) {
            bail!("pane was forgotten");
        }
        match request {
            Request::Inspect | Request::Poll { .. } => unreachable!(),
            Request::Attach { spec } => {
                let id = self.shared.authority.lock().unwrap().attach(spec)?;
                self.shared.changes.notify();
                // This acknowledges desired policy, not an applied native resize.
                // A completed retained scene has no receiver and remains viewable.
                let _ = self.shared.resize_wake.try_send(());
                Ok(json!({"view_id":id,"pane_id":self.shared.pane_id}))
            }
            Request::Update { view_id, spec } => {
                self.shared
                    .authority
                    .lock()
                    .unwrap()
                    .update(&view_id, spec)?;
                self.shared.changes.notify();
                let _ = self.shared.resize_wake.try_send(());
                Ok(json!({"updated":true}))
            }
            Request::Detach { view_id } => {
                self.shared.authority.lock().unwrap().detach(&view_id)?;
                self.shared.delta_frames.lock().unwrap().remove(&view_id);
                self.shared.changes.notify();
                let _ = self.shared.resize_wake.try_send(());
                Ok(json!({"detached":true}))
            }
            Request::Pin { view_id, size } => {
                let size = {
                    let mut a = self.shared.authority.lock().unwrap();
                    a.pin(&view_id, size)?;
                    a.owner().map(|(_, size)| size)
                };
                self.shared.changes.notify();
                let _ = self.shared.resize_wake.try_send(());
                Ok(json!({"pinned":size}))
            }
            Request::Input { view_id, data } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                if data.len() > MAX_INPUT_BYTES {
                    bail!("input payload limit");
                }
                Ok(self.command(Mutation::Raw(data))?.response())
            }
            Request::Paste { view_id, data } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                if data.len() > MAX_INPUT_BYTES {
                    bail!("input payload limit");
                }
                Ok(self.command(Mutation::Paste(data))?.response())
            }
            Request::PasteContents {
                view_id,
                contents,
                location,
                allow_unsafe,
            } => {
                {
                    let a = self.shared.authority.lock().unwrap();
                    a.input(&view_id)?;
                    let view = a.views.get(&view_id).context("stale view")?;
                    if !view.spec.clipboard || !view.spec.visible || !view.spec.interactive {
                        bail!("enhanced MIME paste requires an eligible clipboard grant");
                    }
                }
                if contents.len() > 16
                    || contents.iter().map(|c| c.data.len()).sum::<usize>() > MAX_INPUT_BYTES
                {
                    bail!("paste payload limits");
                }
                Ok(self
                    .command(Mutation::PasteContents(
                        view_id,
                        contents,
                        location,
                        allow_unsafe,
                    ))?
                    .response())
            }
            Request::Key { view_id, event } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                Ok(self.command(Mutation::Key(event))?.response())
            }
            Request::Mouse { view_id, event } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                Ok(self.command(Mutation::Mouse(event))?.response())
            }
            Request::Focus { view_id, focused } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                Ok(self.command(Mutation::Focus(focused))?.response())
            }
            Request::Scroll { view_id, delta } => {
                {
                    let mut a = self.shared.authority.lock().unwrap();
                    let view = a.views.get_mut(&view_id).context("stale view")?;
                    view.scrollback_offset = view
                        .scrollback_offset
                        .saturating_add_signed(delta)
                        .min(100_000);
                    view.last_seen = Instant::now();
                }
                self.shared.changes.notify();
                self.command(Mutation::Refresh)?;
                Ok(json!({"scrolled":true}))
            }
            Request::Colors {
                view_id,
                foreground,
                background,
                cursor,
                palette,
            } => {
                if self
                    .shared
                    .authority
                    .lock()
                    .unwrap()
                    .owner()
                    .is_none_or(|(owner, _)| owner != view_id)
                {
                    bail!("only current size owner may seed terminal colors");
                }
                let palette = match palette {
                    Some(palette) => {
                        Some(Box::new(palette.try_into().map_err(|_| {
                            anyhow::anyhow!("palette must contain 256 colors")
                        })?))
                    }
                    None => None,
                };
                self.command(Mutation::Colors {
                    foreground,
                    background,
                    cursor,
                    palette,
                })?;
                Ok(json!({"updated":true}))
            }
            Request::Terminate { view_id } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                self.cancel_clipboard(None);
                let completed = matches!(
                    self.command(Mutation::Terminate)?,
                    CommandOutcome::Completed
                );
                Ok(json!({"terminated":!completed,"already_completed":completed}))
            }
            Request::Shutdown => {
                self.cancel_clipboard(None);
                let completed = matches!(
                    self.command(Mutation::Terminate)?,
                    CommandOutcome::Completed
                );
                self.shared.changes.notify();
                Ok(json!({"shutdown":true,"already_completed":completed}))
            }
            Request::Clipboard { .. } => unreachable!(),
        }
    }
    fn snapshot(
        &self,
        view: Option<&str>,
        after: u64,
        frame_after: u64,
        resources: &[ImageKey],
        row_deltas: bool,
        compact_scene: bool,
    ) -> Result<Value> {
        if resources.len() > 1024 {
            bail!("image receipt limit");
        }
        // Capture the token before the state: concurrent updates can cause an
        // extra response, but can never be hidden behind a newer change token.
        let snapshot_revision = self.shared.changes.revision();
        let (
            size_owner,
            lease_generation,
            views,
            frame,
            baseline,
            revision,
            pid,
            lifecycle,
            exit_code,
            error,
            status,
            events,
            event_sequence,
        ) = {
            let mut authority = self.shared.authority.lock().unwrap();
            if let Some(id) = view {
                authority.touch(id)?;
            }
            let published = self.shared.published.lock().unwrap();
            let frame = view
                .and_then(|id| published.scroll_frames.get(id))
                .or(published.frame.as_ref())
                .cloned();
            let baseline = if (row_deltas || compact_scene)
                && let Some(id) = view
                && let Some(frame) = &frame
            {
                self.shared
                    .delta_frames
                    .lock()
                    .unwrap()
                    .insert(id.to_owned(), (published.revision, frame.clone()))
            } else {
                None
            };
            (
                authority.owner().map(|(id, size)| (id.to_owned(), size)),
                authority.generation,
                authority
                    .views
                    .values()
                    .map(|view| view.snapshot())
                    .collect::<Vec<_>>(),
                frame,
                baseline,
                published.revision,
                published.pid,
                published.lifecycle.clone(),
                published.exit_code,
                published.error.clone(),
                published.status.clone(),
                published
                    .events
                    .iter()
                    .filter(|event| event.sequence > after)
                    .cloned()
                    .collect::<Vec<_>>(),
                published.event_sequence,
            )
        };
        let (clipboard, clipboard_pending) = {
            let pending = self.shared.clipboard.lock().unwrap();
            (
                pending
                    .as_ref()
                    .filter(|wait| view == Some(wait.info.view_id.as_str()))
                    .map(|wait| wait.info.clone()),
                pending.is_some(),
            )
        };
        let writer = self.shared.writer.lock().unwrap().clone();
        let writer = writer.as_ref().map(|writer| writer.snapshot());
        let image_refs = frame
            .as_ref()
            .map(|frame| frame.image_keys())
            .unwrap_or_default();
        let changed_frame = frame.as_deref().filter(|_| frame_after != revision);
        let (frame, row_delta, scene) = if compact_scene {
            (
                Value::Null,
                None,
                changed_frame
                    .map(|frame| {
                        scene_wire::encode(
                            frame,
                            revision,
                            baseline.as_ref(),
                            frame_after,
                            resources,
                        )
                    })
                    .transpose()?,
            )
        } else {
            let (frame, delta) =
                frame_payload(changed_frame, baseline.as_ref(), frame_after, resources);
            (frame, delta, None)
        };
        let mut snapshot = json!({
            "pane_id": self.shared.pane_id,
            "pid": pid,
            "revision": revision,
            "snapshot_revision": snapshot_revision,
            "frame": frame,
            "image_refs": image_refs,
            "resource_protocol": 1,
            "writer": writer,
            "lifecycle": lifecycle,
            "exit_code": exit_code,
            "error": error,
            "program_status": status,
            "events": events,
            "event_sequence": event_sequence,
            "size_owner": size_owner.map(|(id, size)| json!({"view_id": id, "size": size})),
            "lease_generation": lease_generation,
            "views": views,
            "clipboard": clipboard,
            "clipboard_pending": clipboard_pending,
            "capabilities": {
                "sixel": false,
                "normalized_frames": true,
                "clipboard_timeout_ms": CLIPBOARD_TIMEOUT.as_millis(),
                "view_timeout_ms": 10000,
                "poll_wait_max_ms": 1000,
                "row_deltas": true,
                "scene_encodings": ["compact_v1"],
            },
        });
        if let Some(row_delta) = row_delta {
            snapshot["row_delta"] = row_delta;
        }
        if let Some(scene) = scene {
            snapshot["scene"] = scene;
        }
        Ok(snapshot)
    }
}
fn frame_payload(
    frame: Option<&Frame>,
    baseline: Option<&(u64, Arc<Frame>)>,
    frame_after: u64,
    resources: &[ImageKey],
) -> (Value, Option<Value>) {
    let Some(frame) = frame else {
        return (Value::Null, None);
    };
    let Some((base_revision, baseline)) = baseline.filter(|(revision, baseline)| {
        *revision == frame_after
            && baseline.size == frame.size
            && baseline.rows.len() == frame.rows.len()
    }) else {
        return (json!(frame.wire(resources)), None);
    };
    let rows = frame
        .rows
        .iter()
        .zip(&baseline.rows)
        .enumerate()
        .filter(|(_, (row, previous))| row != previous)
        .map(|(index, (row, _))| (index, row))
        .collect::<Vec<_>>();
    let known = resources
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let images = frame
        .images
        .iter()
        .filter(|image| !known.contains(&image.key()))
        .collect::<Vec<_>>();
    // Keep metadata complete; only unchanged rows and receipted resources are
    // omitted. Whole-row replacement preserves wide and combining cell groups.
    (
        json!({
            "size": frame.size,
            "rows": [],
            "cursor": frame.cursor,
            "foreground": frame.foreground,
            "background": frame.background,
            "palette": frame.palette,
            "images": images,
            "placements": frame.placements,
            "alternate_screen": frame.alternate_screen,
            "scrollback_total": frame.scrollback_total,
            "scrollback_offset": frame.scrollback_offset,
        }),
        Some(json!({"base_revision": base_revision, "rows": rows})),
    )
}
fn revoke_paste(shared: &Shared, view: Option<&str>) {
    let mut grants = shared.paste.lock().unwrap();
    let ids = grants
        .origins
        .iter()
        .filter(|(_, origin)| view.is_none_or(|view| origin.view_id == view))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    for id in ids {
        grants.origins.remove(&id);
        grants.revoked.push(id);
    }
}
fn prepare_home(home: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !home.is_absolute() {
        bail!("isolated HOME must be absolute");
    }
    if home.exists()
        && !home.join(".bkpr-native-home").is_file()
        && std::fs::read_dir(home)?.next().is_some()
    {
        bail!("refusing nonempty HOME without native isolation marker");
    }
    std::fs::create_dir_all(home)?;
    std::fs::write(
        home.join(".bkpr-native-home"),
        b"isolated native pane runtime\n",
    )?;
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    for dir in ["config", "cache", "data", "state", "runtime"] {
        std::fs::create_dir_all(home.join(dir))?;
        std::fs::set_permissions(home.join(dir), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn publish(terminal: &mut Terminal, shared: &Shared) -> Result<()> {
    let views = shared
        .authority
        .lock()
        .unwrap()
        .views
        .values()
        .map(|v| (v.id.clone(), v.scrollback_offset))
        .collect::<Vec<_>>();
    let frame = Arc::new(terminal.snapshot()?);
    let mut cache = BTreeMap::new();
    cache.insert(0, frame.clone());
    let mut frames = BTreeMap::new();
    for (id, offset) in &views {
        if !cache.contains_key(offset) {
            cache.insert(*offset, Arc::new(terminal.read_scrollback(*offset)?));
        }
        frames.insert(id.clone(), cache[offset].clone());
    }
    {
        let mut authority = shared.authority.lock().unwrap();
        for (id, requested) in views {
            if let Some(view) = authority.views.get_mut(&id)
                && view.scrollback_offset == requested
            {
                view.scrollback_offset = frames[&id].scrollback_offset;
            }
        }
    }
    let mut p = shared.published.lock().unwrap();
    p.frame = Some(frame);
    p.scroll_frames = frames;
    p.status = terminal.program_status();
    p.revision += 1;
    drop(p);
    shared.changes.notify();
    Ok(())
}
fn route_effects(effects: Vec<Effect>, writer: &writer::Queue, shared: &Shared) -> Result<()> {
    let mut replies = Vec::new();
    for effect in effects {
        match effect {
            Effect::PtyWrite(data) => {
                if replies.len().saturating_add(data.len()) > 2 * MAX_INPUT_BYTES {
                    bail!("terminal reply aggregate limit exceeded");
                }
                replies.extend(data);
            }
            effect => {
                let mut p = shared.published.lock().unwrap();
                p.event_sequence += 1;
                let sequence = p.event_sequence;
                p.events.push_back(Event { sequence, effect });
                if p.events.len() > 128 {
                    p.events.pop_front();
                }
                drop(p);
                shared.changes.notify();
            }
        }
    }
    if !replies.is_empty() {
        writer.enqueue(replies, true)?;
    }
    Ok(())
}
fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: (u32::from(size.cols) * size.cell_width).min(u16::MAX.into()) as u16,
        pixel_height: (u32::from(size.rows) * size.cell_height).min(u16::MAX.into()) as u16,
    }
}
fn reconcile_size(
    master: &dyn portable_pty::MasterPty,
    terminal: &mut Terminal,
    shared: &Shared,
    current_size: &mut Size,
) -> Result<()> {
    let desired = shared
        .authority
        .lock()
        .unwrap()
        .owner()
        .map(|(_, size)| size);
    if let Some(size) = desired
        && size != *current_size
    {
        #[cfg(test)]
        {
            let barrier = shared.resize_barrier.lock().unwrap().take();
            if let Some(barrier) = barrier {
                barrier
                    .entered
                    .send(size)
                    .context("resize test barrier receiver closed")?;
                barrier
                    .resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .context("resize test barrier was not released")?;
            }
        }
        master.resize(pty_size(size))?;
        terminal.resize(size)?;
        *current_size = size;
    }
    Ok(())
}
fn run_pane(
    spec: SpawnSpec,
    shared: Arc<Shared>,
    commands: &crossbeam_channel::Receiver<Command>,
    resize_events: crossbeam_channel::Receiver<()>,
    ready: mpsc::SyncSender<std::result::Result<(), String>>,
    termination_reply: &mut Option<mpsc::SyncSender<CommandResult>>,
) -> Result<()> {
    let setup = (|| -> Result<_> {
        let pair = native_pty_system().openpty(pty_size(spec.size))?;
        let mut cmd = CommandBuilder::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..]);
        if spec.isolated_environment {
            cmd.env_clear();
            cmd.env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            );
            cmd.env("LANG", "C.UTF-8");
            for (key, dir) in [
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_RUNTIME_DIR", "runtime"),
            ] {
                cmd.env(key, spec.home.join(dir));
            }
        }
        if !spec.isolated_environment {
            environment::strip_parent_context(&mut cmd);
        }
        cmd.env("BKPR_PANE_ID", &shared.pane_id);
        cmd.env("HOME", &spec.home);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // SHELL describes the user's shell, not arbitrary argv[0] (eg. an editor).
        cmd.cwd(&spec.cwd);
        let terminal = Terminal::new(spec.size)?;
        let raw_child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let pid = raw_child.process_id();
        #[cfg(target_os = "linux")]
        let tree = pid.map(|pid| process_tree::Tree::new(pid, spec.isolated_process));
        let child = OwnedChild {
            pid,
            inner: raw_child,
            cleaned: false,
            #[cfg(target_os = "linux")]
            tree,
            #[cfg(target_os = "linux")]
            exit_observed: false,
            last_observed: Instant::now(),
        };
        Ok((pair.master, child, terminal))
    })();
    let (master, mut child, mut terminal) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return Err(e);
        }
    };
    let pid = child.pid;
    let fd = master
        .as_raw_fd()
        .context("pane runtime requires a native PTY descriptor")?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let changes = shared.changes.clone();
    let (writer_notify, writer_events) = crossbeam_channel::bounded(1);
    let (writer, writer_thread) = writer::Queue::start(
        master.take_writer()?,
        Box::new(move || {
            changes.notify();
            let _ = writer_notify.try_send(());
        }),
    )?;
    let mut writer_thread = Some(writer_thread);
    *shared.writer.lock().unwrap() = Some(writer.clone());
    let mut reader = master.try_clone_reader()?;
    let (output_tx, output) = crossbeam_channel::bounded::<Vec<u8>>(64);
    let reader_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if reader_fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let reader_ready =
        unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(reader_fd) };
    let reader_stop = ReaderStop(Arc::new(AtomicBool::new(false)));
    let reader_cancel = reader_stop.0.clone();
    let reader_thread = thread::Builder::new()
        .name("bkpr-pty-reader".into())
        .spawn(move || {
            let mut buffer = [0; 16384];
            while !reader_cancel.load(Ordering::Acquire) {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output_tx.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        use std::os::fd::AsRawFd;
                        let mut ready = libc::pollfd {
                            fd: reader_ready.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // Only cancellation has a timer; PTY output wakes immediately.
                        let result = unsafe { libc::poll(&mut ready, 1, 100) };
                        if result < 0
                            && std::io::Error::last_os_error().kind()
                                != std::io::ErrorKind::Interrupted
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })?;
    let clipboard_state = shared.clone();
    terminal.set_clipboard_handler(Box::new(move |request| {
        let authority = clipboard_state.authority.lock().unwrap();
        if let Some(id) = request.paste_offer_id {
            let grants = clipboard_state.paste.lock().unwrap();
            let allowed = grants.origins.get(&id).is_some_and(|origin| {
                origin.expires > Instant::now()
                    && authority.views.get(&origin.view_id).is_some_and(|view| {
                        view.spec.input
                            && view.spec.clipboard
                            && view.spec.visible
                            && view.spec.interactive
                    })
            });
            return if allowed {
                ClipboardResponse {
                    result: bkpr_protocol::ClipboardResult::Success,
                    contents: Vec::new(),
                    available: Vec::new(),
                    remember: false,
                }
            } else {
                ClipboardResponse::denied()
            };
        }
        let Some(view_id) = authority.clipboard_owner() else {
            return ClipboardResponse::denied();
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let request_id = Uuid::new_v4().to_string();
        *clipboard_state.clipboard.lock().unwrap() = Some(ClipboardWait {
            info: ClipboardPending {
                request_id: request_id.clone(),
                view_id,
                request,
            },
            response: tx,
        });
        drop(authority);
        clipboard_state.changes.notify();
        let response = rx
            .recv_timeout(CLIPBOARD_TIMEOUT)
            .unwrap_or_else(|_| ClipboardResponse::denied());
        let mut pending = clipboard_state.clipboard.lock().unwrap();
        if pending
            .as_ref()
            .is_some_and(|p| p.info.request_id == request_id)
        {
            *pending = None;
            drop(pending);
            clipboard_state.changes.notify();
        }
        response
    }));
    {
        let mut p = shared.published.lock().unwrap();
        p.pid = pid;
        p.lifecycle = "running".into();
        p.frame = Some(Arc::new(terminal.snapshot()?));
        p.revision += 1;
    }
    shared.changes.notify();
    let _ = ready.send(Ok(()));
    let result = (|| -> Result<()> {
        let mut eof = false;
        let clock = Instant::now();
        let mut next_image_tick = None;
        let mut current_size = spec.size;
        let mut pending_output = None;
        let no_output = crossbeam_channel::never();
        loop {
            child.observe();
            let mut dirty = false;
            let (stale, size) = {
                let mut a = shared.authority.lock().unwrap();
                let stale = a.expire(Instant::now());
                let size = if stale.is_empty() {
                    None
                } else {
                    a.owner().map(|(_, size)| size)
                };
                (stale, size)
            };
            for view in &stale {
                revoke_paste(&shared, Some(view));
                shared.delta_frames.lock().unwrap().remove(view);
            }
            {
                let mut grants = shared.paste.lock().unwrap();
                for id in std::mem::take(&mut grants.revoked) {
                    terminal.cancel_paste_offer(id);
                }
                let expired = grants
                    .origins
                    .iter()
                    .filter(|(id, origin)| {
                        origin.expires <= Instant::now() || !terminal.has_paste_offer(**id)
                    })
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                for id in expired {
                    grants.origins.remove(&id);
                    terminal.cancel_paste_offer(id);
                }
            }
            if !stale.is_empty() {
                shared.changes.notify();
                if let Some(size) = size
                    && size != current_size
                {
                    master.resize(pty_size(size))?;
                    terminal.resize(size)?;
                    route_effects(terminal.drain_effects()?, &writer, &shared)?;
                    current_size = size;
                }
                dirty = true;
            }
            // A bounded drain gives controls a turn even under an output flood.
            for _ in 0..16 {
                if writer.congested() {
                    break;
                }
                let next = pending_output.take().map_or_else(|| output.try_recv(), Ok);
                match next {
                    Ok(bytes) => {
                        route_effects(terminal.feed(&bytes)?, &writer, &shared)?;
                        dirty = true;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        eof = true;
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                }
            }
            let now = clock.elapsed().as_millis().min(u64::MAX.into()) as u64;
            if dirty || next_image_tick.is_some_and(|deadline| now >= deadline) {
                next_image_tick = terminal.tick_images(now)?;
                dirty = true;
            }
            if terminal
                .sync_output_remaining()
                .is_some_and(|remaining| remaining.is_zero())
            {
                dirty = true;
            }
            if dirty {
                publish(&mut terminal, &shared)?;
            }
            // Maintenance is bounded separately from readiness. Output and writer
            // progress wake the worker instead of waiting for the command timer.
            let mut wait = Duration::from_millis(100);
            if let Some(remaining) = terminal.sync_output_remaining() {
                wait = wait.min(remaining);
            }
            if let Some(deadline) = next_image_tick {
                wait = wait.min(Duration::from_millis(deadline.saturating_sub(now)));
            }
            let readable = if eof || writer.congested() || pending_output.is_some() {
                &no_output
            } else {
                &output
            };
            let command = crossbeam_channel::select_biased! {
                recv(resize_events) -> _ => {
                    reconcile_size(&*master, &mut terminal, &shared, &mut current_size)?;
                    route_effects(terminal.drain_effects()?, &writer, &shared)?;
                    publish(&mut terminal, &shared)?;
                    None
                },
                recv(commands) -> command => Some(command),
                recv(writer_events) -> _ => None,
                recv(readable) -> bytes => {
                    match bytes {
                        Ok(bytes) => pending_output = Some(bytes),
                        Err(_) => eof = true,
                    }
                    None
                },
                default(wait) => None,
            };
            match command {
                Some(Ok(command)) => {
                    if matches!(&command.mutation, Mutation::Terminate) {
                        *termination_reply = Some(command.result);
                        writer.stop();
                        child.terminate()?;
                        let status = child
                            .try_wait()?
                            .context("terminated child has no exit status")?;
                        terminal.program_exited();
                        publish(&mut terminal, &shared)?;
                        let mut published = shared.published.lock().unwrap();
                        published.exit_code = Some(status.exit_code());
                        published.lifecycle = "terminating".into();
                        drop(published);
                        shared.changes.notify();
                        break;
                    }
                    // Observe execution exit before accepting native input, not only
                    // at the end of the turn after it could have reached a dead PTY.
                    let status = match child.try_wait().and_then(|status| {
                        if status.is_some() {
                            child.terminate()?;
                        }
                        Ok(status)
                    }) {
                        Ok(status) => status,
                        Err(error) => {
                            let _ = command.result.send(Err(format!("{error:#}")));
                            return Err(error);
                        }
                    };
                    if let Some(status) = status {
                        terminal.program_exited();
                        let mut published = shared.published.lock().unwrap();
                        published.exit_code = Some(status.exit_code());
                        published.status = terminal.program_status();
                        let complete = eof && terminal.sync_output_remaining().is_none();
                        published.lifecycle = if complete { "exited" } else { "draining" }.into();
                        let outcome = completed_command(&published, &command.mutation);
                        drop(published);
                        shared.changes.notify();
                        let _ = command.result.send(outcome);
                        if complete {
                            break;
                        }
                        continue;
                    }
                    let needs_publish = matches!(
                        &command.mutation,
                        Mutation::Colors { .. } | Mutation::Refresh
                    );
                    let outcome = (|| -> Result<Option<u64>> {
                        let mut write_id = None;
                        match command.mutation {
                            Mutation::Colors {
                                foreground,
                                background,
                                cursor,
                                palette,
                            } => {
                                terminal.set_default_colors(foreground, background, cursor)?;
                                if let Some(palette) = palette {
                                    terminal.set_palette(&palette)?;
                                }
                            }
                            Mutation::Raw(data) => write_id = Some(writer.enqueue(data, false)?),
                            Mutation::Paste(data) => {
                                let bytes = terminal.encode_paste(&data)?;
                                if !bytes.is_empty() {
                                    write_id = Some(writer.enqueue(bytes, false)?);
                                }
                            }
                            Mutation::PasteContents(view_id, contents, location, allow_unsafe) => {
                                let id = {
                                    let mut grants = shared.paste.lock().unwrap();
                                    if grants.origins.len()
                                        >= bkpr_terminal_engine::PASTE_OFFER_MAX_COUNT
                                    {
                                        bail!("pending paste grant limit");
                                    }
                                    grants.next = grants
                                        .next
                                        .checked_add(1)
                                        .context("paste grant ID exhausted")?;
                                    let id = grants.next;
                                    grants.origins.insert(
                                        id,
                                        PasteOrigin {
                                            view_id,
                                            expires: Instant::now()
                                                + bkpr_terminal_engine::PASTE_OFFER_TIMEOUT,
                                        },
                                    );
                                    id
                                };
                                let encoded =
                                    terminal.paste_contents(id, contents, location, allow_unsafe);
                                let accepted =
                                    encoded.map_err(anyhow::Error::from).and_then(|bytes| {
                                        if bytes.is_empty() {
                                            Ok(None)
                                        } else {
                                            writer.enqueue(bytes, false).map(Some)
                                        }
                                    });
                                if accepted.is_err() || !terminal.has_paste_offer(id) {
                                    shared.paste.lock().unwrap().origins.remove(&id);
                                    terminal.cancel_paste_offer(id);
                                }
                                write_id = accepted?;
                            }
                            Mutation::Key(event) => {
                                let bytes = terminal.encode_key(&event)?;
                                if !bytes.is_empty() {
                                    write_id = Some(writer.enqueue(bytes, false)?);
                                }
                            }
                            Mutation::Mouse(event) => {
                                let bytes = terminal.encode_mouse(&event)?;
                                if !bytes.is_empty() {
                                    write_id = Some(writer.enqueue(bytes, false)?);
                                }
                            }
                            Mutation::Focus(focused) => {
                                let bytes = terminal.encode_focus(focused)?;
                                if !bytes.is_empty() {
                                    write_id = Some(writer.enqueue(bytes, false)?);
                                }
                            }
                            Mutation::Refresh => {}
                            Mutation::Terminate => {}
                        }
                        route_effects(terminal.drain_effects()?, &writer, &shared)?;
                        if needs_publish {
                            publish(&mut terminal, &shared)?;
                        }
                        Ok(write_id)
                    })();
                    let _ = command.result.send(
                        outcome
                            .map(CommandOutcome::Applied)
                            .map_err(|error| format!("{error:#}")),
                    );
                }
                Some(Err(_)) => {
                    child.terminate()?;
                    break;
                }
                None => {}
            }
            if let Some(status) = child.try_wait()? {
                child.terminate()?;
                terminal.program_exited();
                let complete = eof && terminal.sync_output_remaining().is_none();
                let mut p = shared.published.lock().unwrap();
                let lifecycle = if complete { "exited" } else { "draining" };
                let status_changed =
                    p.exit_code != Some(status.exit_code()) || p.lifecycle != lifecycle;
                p.status = terminal.program_status();
                p.exit_code = Some(status.exit_code());
                p.lifecycle = lifecycle.into();
                drop(p);
                if status_changed {
                    shared.changes.notify();
                }
                if complete {
                    break;
                }
            }
        }
        Ok(())
    })();
    // Always tear down the owned session even on an engine/PTY error.
    let cleanup = child.terminate();
    writer.stop();
    reader_stop.0.store(true, Ordering::Release);
    drop(master);
    drop(output);
    if let Some(handle) = writer_thread.take() {
        let _ = handle.join();
    }
    let _ = reader_thread.join();
    result.and(cleanup)
}
struct ReaderStop(Arc<AtomicBool>);
impl Drop for ReaderStop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct OwnedChild {
    inner: Box<dyn portable_pty::Child + Send + Sync>,
    pid: Option<u32>,
    cleaned: bool,
    last_observed: Instant,
    #[cfg(target_os = "linux")]
    tree: Option<process_tree::Tree>,
    #[cfg(target_os = "linux")]
    exit_observed: bool,
}
impl OwnedChild {
    fn try_wait(&mut self) -> Result<Option<portable_pty::ExitStatus>> {
        #[cfg(target_os = "linux")]
        if !self.exit_observed
            && let Some(pid) = self.pid
        {
            // Keep the session leader's birth-bound /proc identity alive until the
            // final session observation. waitpid/Child::try_wait would reap it first,
            // losing same-session jobs already adopted by this multi-pane subreaper.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                return Err(error).context("observe owned child exit without reaping");
            }
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            if let Some(tree) = self.tree.as_mut() {
                tree.observe();
            }
            self.exit_observed = true;
        }
        Ok(self.inner.try_wait()?)
    }
    fn observe(&mut self) {
        if self.last_observed.elapsed() < Duration::from_millis(100) {
            return;
        }
        self.last_observed = Instant::now();
        #[cfg(target_os = "linux")]
        if let Some(tree) = self.tree.as_mut() {
            tree.observe();
        }
    }
    fn terminate(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if let Some(tree) = self.tree.as_mut() {
            tree.signal(libc::SIGHUP);
        }
        #[cfg(not(target_os = "linux"))]
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGHUP);
            }
        }
        let grace = Instant::now() + Duration::from_millis(250);
        while Instant::now() < grace {
            if self.try_wait()?.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(4));
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            #[cfg(target_os = "linux")]
            if let Some(tree) = self.tree.as_mut() {
                tree.signal(libc::SIGKILL);
            }
            #[cfg(not(target_os = "linux"))]
            if let Some(pid) = self.pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let exited = self.try_wait()?.is_some();
            #[cfg(target_os = "linux")]
            let remaining = if let Some(tree) = self.tree.as_mut() {
                tree.reap();
                tree.remaining()
            } else {
                false
            };
            #[cfg(not(target_os = "linux"))]
            let remaining = false;
            if exited && !remaining {
                self.cleaned = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("owned process cleanup deadline exceeded; termination is incomplete");
            }
            thread::sleep(Duration::from_millis(4));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(test)]
mod lifecycle_tests;
#[cfg(all(test, target_os = "linux"))]
mod process_exit_tests;

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use bkpr_protocol::{Cell, ColorSource, Cursor, CursorShape, Row, Style};

    fn frame() -> Frame {
        let cell = Cell {
            text: "a".into(),
            width: 1,
            style: Style {
                foreground: [255; 3],
                background: [0; 3],
                underline_color: [255; 3],
                foreground_source: ColorSource::Default,
                background_source: ColorSource::Default,
                underline_source: ColorSource::Default,
                bold: false,
                italic: false,
                faint: false,
                blink: false,
                inverse: false,
                invisible: false,
                strikethrough: false,
                overline: false,
                underline: 0,
            },
            hyperlink: None,
        };
        Frame {
            size: Size {
                cols: 2,
                rows: 2,
                cell_width: 8,
                cell_height: 16,
            },
            rows: vec![
                Row {
                    cells: vec![cell.clone(), cell.clone()],
                },
                Row {
                    cells: vec![cell.clone(), cell],
                },
            ],
            cursor: Cursor {
                x: 0,
                y: 0,
                visible: true,
                blinking: false,
                shape: CursorShape::Block,
                color: None,
            },
            foreground: [255; 3],
            background: [0; 3],
            palette: Vec::new(),
            images: Vec::new(),
            placements: Vec::new(),
            alternate_screen: false,
            scrollback_total: 2,
            scrollback_offset: 0,
        }
    }

    fn pane() -> Pane {
        let (commands, _) = crossbeam_channel::bounded(1);
        Pane {
            shared: Arc::new(Shared {
                authority: Mutex::new(Authority::default()),
                published: Mutex::new(Published {
                    frame: Some(Arc::new(frame())),
                    revision: 1,
                    lifecycle: "exited".into(),
                    ..Published::default()
                }),
                clipboard: Mutex::new(None),
                requests: Mutex::new(()),
                command_gate: Mutex::new(()),
                view_policy: Mutex::new(()),
                pane_id: "test-pane".into(),
                writer: Mutex::new(None),
                paste: Mutex::new(PasteGrants::default()),
                changes: Arc::new(SnapshotChanges::default()),
                delta_frames: Mutex::new(BTreeMap::new()),
                finished: AtomicBool::new(true),
                forgotten: AtomicBool::new(false),
                on_finish: Mutex::new(None),
                resize_wake: crossbeam_channel::bounded(1).0,
                resize_barrier: Mutex::new(None),
            }),
            commands,
        }
    }

    fn request(pane: &Pane, request: Value) -> Value {
        pane.request(serde_json::from_value(request).unwrap())
            .unwrap()
    }

    #[test]
    fn default_poll_is_one_shot_and_delta_baselines_follow_views() {
        let pane = pane();
        let attached = request(
            &pane,
            json!({
                "op": "attach", "size": frame().size,
                "visible": false, "interactive": false, "input": false, "clipboard": false,
            }),
        );
        let id = attached["view_id"].as_str().unwrap();
        let first = request(&pane, json!({"op":"poll", "view_id":id, "row_deltas":true}));
        assert_eq!(first["frame"]["rows"].as_array().unwrap().len(), 2);
        assert!(first.get("row_delta").is_none());
        let token = first["snapshot_revision"].as_u64().unwrap();
        let unchanged = request(
            &pane,
            json!({
                "op":"poll", "view_id":id, "snapshot_after":token, "frame_after":1,
            }),
        );
        assert_eq!(unchanged["snapshot_revision"], token);
        assert!(unchanged["frame"].is_null());
        assert!(unchanged.get("row_delta").is_none());

        let mut changed = frame();
        changed.rows[1].cells[0].text = "界".into();
        changed.rows[1].cells[0].width = 2;
        changed.rows[1].cells[1].text.clear();
        changed.rows[1].cells[1].width = 0;
        {
            let mut published = pane.shared.published.lock().unwrap();
            published.frame = Some(Arc::new(changed));
            published.revision = 2;
        }
        pane.shared.changes.notify();
        let delta = request(
            &pane,
            json!({
                "op":"poll", "view_id":id, "frame_after":1, "row_deltas":true,
            }),
        );
        assert_eq!(delta["frame"]["rows"], json!([]));
        assert_eq!(delta["row_delta"]["base_revision"], 1);
        assert_eq!(delta["row_delta"]["rows"][0][0], 1);
        assert_eq!(delta["row_delta"]["rows"][0][1]["cells"][0]["text"], "界");
        let full = request(&pane, json!({"op":"poll", "view_id":id, "frame_after":1}));
        assert!(full.get("row_delta").is_none());
        assert_eq!(full["frame"]["rows"].as_array().unwrap().len(), 2);
        request(&pane, json!({"op":"detach", "view_id":id}));
        assert!(pane.shared.delta_frames.lock().unwrap().is_empty());
        assert!(
            pane.request(
                serde_json::from_value(json!({
                    "op":"poll", "view_id":id, "wait_ms":1000,
                }))
                .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn deltas_reconstruct_exact_frames_and_fall_back_without_matching_baseline() {
        let base = Arc::new(frame());
        let mut current = (*base).clone();
        current.rows[1].cells[0].text = "e\u{301}".into();
        current.cursor.y = 1;
        let baseline = (4, base.clone());
        let (mut metadata, delta) = frame_payload(Some(&current), Some(&baseline), 4, &[]);
        let delta = delta.unwrap();
        metadata["rows"] = json!(base.rows);
        for change in delta["rows"].as_array().unwrap() {
            metadata["rows"][change[0].as_u64().unwrap() as usize] = change[1].clone();
        }
        assert_eq!(metadata, json!(current.wire(&[])));
        let (_, cursor_only) = frame_payload(
            Some(&current),
            Some(&(5, Arc::new(current.clone()))),
            5,
            &[],
        );
        assert_eq!(cursor_only.unwrap()["rows"], json!([]));
        assert!(
            frame_payload(Some(&current), Some(&baseline), 3, &[])
                .1
                .is_none()
        );
        current.size.cols += 1;
        assert!(
            frame_payload(Some(&current), Some(&baseline), 4, &[])
                .1
                .is_none()
        );
        current.size = base.size;
        current.rows.pop();
        assert!(
            frame_payload(Some(&current), Some(&baseline), 4, &[])
                .1
                .is_none()
        );
        assert_eq!(
            frame_payload(None, Some(&baseline), 4, &[]),
            (Value::Null, None)
        );
    }

    #[test]
    fn compact_scene_round_trips_exact_cells_styles_sources_and_resources() {
        use bkpr_protocol::{Image, ImageLayer, PixelFormat, Placement};
        let mut expected = frame();
        expected.size.cols = 6;
        let mut cells = vec![expected.rows[0].cells[0].clone(); 6];
        for (cell, (text, width)) in cells.iter_mut().zip([
            ("e\u{301}", 1),
            ("👩\u{200d}💻", 2),
            ("", 0),
            ("ab", 1),
            ("", 1),
            ("\u{301}", 0),
        ]) {
            cell.text = text.into();
            cell.width = width;
        }
        cells[0].style.foreground_source = ColorSource::Palette(7);
        cells[1].style.background_source = ColorSource::Rgb([9, 8, 7]);
        cells[2].style.underline_source = ColorSource::Palette(255);
        cells[3].style.bold = true;
        cells[3].style.italic = true;
        cells[3].style.faint = true;
        cells[3].style.blink = true;
        cells[3].style.inverse = true;
        cells[3].style.invisible = true;
        cells[3].style.strikethrough = true;
        cells[3].style.overline = true;
        cells[3].style.underline = 5;
        cells[3].style.underline_color = [1, 2, 3];
        cells[4].hyperlink = Some("https://example.invalid/界".into());
        cells[5].hyperlink = cells[4].hyperlink.clone();
        expected.rows = vec![
            Row {
                cells: cells.clone(),
            },
            Row { cells },
        ];
        expected.palette = vec![[1, 2, 3]; 256];
        expected.images = vec![Arc::new(Image {
            id: 9,
            generation: 42,
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
            pixels: vec![1, 2, 3, 4],
        })];
        expected.placements = vec![Placement {
            image_id: 9,
            generation: 42,
            placement_id: 5,
            col: -1,
            row: 1,
            offset_x: 2,
            offset_y: 3,
            width: 7,
            height: 8,
            source_x: 0,
            source_y: 0,
            source_width: 1,
            source_height: 1,
            layer: ImageLayer::BelowText,
            z: -2,
        }];
        expected.alternate_screen = true;
        expected.scrollback_total = 500;
        expected.scrollback_offset = 42;
        let wire = scene_wire::encode(&expected, 4, None, 0, &[]).unwrap();
        assert_eq!(wire["hyperlinks"].as_array().unwrap().len(), 1);
        assert_eq!(wire["styles"].as_array().unwrap().len(), 5);
        let mut actual = None;
        let mut revision = 0;
        scene_wire::decode(wire)
            .unwrap()
            .apply(&mut revision, &mut actual)
            .unwrap();
        assert_eq!(revision, 4);
        assert_eq!(actual.as_ref(), Some(&expected));

        let mut changed = expected.clone();
        changed.rows[1].cells[0].text = "changed".into();
        changed.cursor.y = 1;
        let baseline = (4, Arc::new(expected));
        let wire =
            scene_wire::encode(&changed, 5, Some(&baseline), 4, &changed.image_keys()).unwrap();
        assert_eq!(wire["base_revision"], 4);
        assert_eq!(wire["rows"].as_array().unwrap().len(), 1);
        assert_eq!(wire["frame"]["images"], json!([]));
        scene_wire::decode(wire)
            .unwrap()
            .apply(&mut revision, &mut actual)
            .unwrap();
        // Supplied image bytes intentionally exclude acknowledged resources.
        changed.images.clear();
        assert_eq!(actual.as_ref(), Some(&changed));
        let wire = scene_wire::encode(&changed, 6, Some(&baseline), 3, &[]).unwrap();
        assert!(wire["base_revision"].is_null());
        assert_eq!(wire["rows"].as_array().unwrap().len(), 2);
        changed.size.cell_width += 1;
        assert!(
            scene_wire::encode(&changed, 6, Some(&baseline), 4, &[]).unwrap()["base_revision"]
                .is_null()
        );
    }

    #[test]
    fn compact_scene_rejects_corrupt_layouts_before_consumer_mutation() {
        let original = frame();
        let full = scene_wire::encode(&original, 4, None, 0, &[]).unwrap();
        let mut corruptions = Vec::new();
        let mut value = full.clone();
        value["rows"][0][1][0][0] = json!(999);
        corruptions.push(value);
        let mut value = full.clone();
        value["rows"][0][1][0][1] = json!(0);
        corruptions.push(value);
        let mut value = full.clone();
        value["rows"][0][1][0][3] = json!([[0, 1, u32::MAX]]);
        corruptions.push(value);
        let mut value = full.clone();
        value["rows"][0][1][0][2] = json!("界");
        value["rows"][0][1][0][3] = json!([[1, 1, 2]]);
        corruptions.push(value);
        let mut value = full.clone();
        value["rows"][1][0] = json!(0);
        corruptions.push(value);
        let mut value = full.clone();
        value["rows"].as_array_mut().unwrap().pop();
        corruptions.push(value);
        let mut value = full.clone();
        value["frame"]["rows"] = json!(original.rows);
        corruptions.push(value);
        let mut value = full.clone();
        value["frame"]["size"]["cols"] = json!(u16::MAX);
        corruptions.push(value);
        let mut value = full.clone();
        value["version"] = json!(99);
        corruptions.push(value);
        let mut value = full;
        value["base_revision"] = json!(4);
        corruptions.push(value);
        for value in corruptions {
            assert!(scene_wire::decode(value).is_err());
        }
        let baseline = (3, Arc::new(original.clone()));
        let delta = scene_wire::encode(&original, 4, Some(&baseline), 3, &[]).unwrap();
        let mut actual = Some(original.clone());
        let mut revision = 2;
        assert!(
            scene_wire::decode(delta.clone())
                .unwrap()
                .apply(&mut revision, &mut actual)
                .is_err()
        );
        assert_eq!(revision, 2);
        assert_eq!(actual.as_ref(), Some(&original));
        revision = 3;
        actual.as_mut().unwrap().size.cols += 1;
        let before = actual.clone();
        assert!(
            scene_wire::decode(delta)
                .unwrap()
                .apply(&mut revision, &mut actual)
                .is_err()
        );
        assert_eq!(actual, before);
    }

    #[test]
    fn compact_poll_negotiates_and_falls_back_to_full_after_lost_baseline() {
        let pane = pane();
        let attached = request(
            &pane,
            json!({
                "op":"attach", "size":frame().size,
                "visible":false, "interactive":false, "input":false, "clipboard":false,
            }),
        );
        let id = attached["view_id"].as_str().unwrap();
        let first = request(
            &pane,
            json!({"op":"poll", "view_id":id, "scene_encoding":"compact_v1"}),
        );
        assert!(first["frame"].is_null());
        assert!(first.get("row_delta").is_none());
        let mut actual = None;
        let mut revision = 0;
        scene_wire::decode(first["scene"].clone())
            .unwrap()
            .apply(&mut revision, &mut actual)
            .unwrap();
        assert_eq!(actual.as_ref(), Some(&frame()));
        let unchanged = request(
            &pane,
            json!({
                "op":"poll", "view_id":id, "frame_after":1, "scene_encoding":"compact_v1",
            }),
        );
        assert!(unchanged.get("scene").is_none());
        {
            let mut published = pane.shared.published.lock().unwrap();
            let mut changed = frame();
            changed.cursor.x = 1;
            published.frame = Some(Arc::new(changed));
            published.revision = 2;
        }
        let cursor_only = request(
            &pane,
            json!({
                "op":"poll", "view_id":id, "frame_after":1, "scene_encoding":"compact_v1",
            }),
        );
        assert_eq!(cursor_only["scene"]["rows"], json!([]));
        scene_wire::decode(cursor_only["scene"].clone())
            .unwrap()
            .apply(&mut revision, &mut actual)
            .unwrap();
        assert_eq!(actual.unwrap().cursor.x, 1);
        let fallback = request(
            &pane,
            json!({
                "op":"poll", "view_id":id, "frame_after":1, "scene_encoding":"compact_v1",
            }),
        );
        assert!(fallback["scene"]["base_revision"].is_null());
        assert_eq!(fallback["scene"]["rows"].as_array().unwrap().len(), 2);
        assert!(request(&pane, json!({"op":"inspect"}))["frame"]["rows"].is_array());
        assert!(
            pane.request(
                serde_json::from_value(json!({
                    "op":"poll", "view_id":id, "scene_encoding":"unknown",
                }))
                .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn compact_scene_represents_large_empty_and_ascii_grids_with_bounded_runs() {
        let mut grid = frame();
        grid.size.cols = 240;
        grid.size.rows = 79;
        let mut cell = grid.rows[0].cells[0].clone();
        cell.text.clear();
        grid.rows = vec![
            Row {
                cells: vec![cell; 240]
            };
            79
        ];
        let wire = scene_wire::encode(&grid, 1, None, 0, &[]).unwrap();
        assert_eq!(wire["styles"].as_array().unwrap().len(), 1);
        assert_eq!(wire["rows"][0][1][0][3], json!([[0, 1, 240]]));
        assert!(serde_json::to_vec(&wire).unwrap().len() < 10_000);
        let mut decoded = None;
        scene_wire::decode(wire)
            .unwrap()
            .apply(&mut 0, &mut decoded)
            .unwrap();
        assert_eq!(decoded.as_ref(), Some(&grid));
        for row in &mut grid.rows {
            for cell in &mut row.cells {
                cell.text = "x".into();
            }
        }
        let wire = scene_wire::encode(&grid, 2, None, 0, &[]).unwrap();
        assert_eq!(wire["rows"][0][1][0][3], json!([]));
        scene_wire::decode(wire)
            .unwrap()
            .apply(&mut 0, &mut decoded)
            .unwrap();
        assert_eq!(decoded.as_ref(), Some(&grid));
    }

    #[test]
    fn change_wait_handles_notification_before_or_after_wait_registration() {
        let changes = Arc::new(SnapshotChanges::default());
        let after = changes.revision();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let waiter = {
            let changes = changes.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                changes.wait(after, u64::MAX);
                changes.revision()
            })
        };
        barrier.wait();
        changes.notify();
        assert_ne!(waiter.join().unwrap(), after);
        changes.wait(after, 1000);
        changes.wait(changes.revision(), 1);
    }

    #[test]
    fn clipboard_clear_and_shutdown_publish_control_changes_without_a_new_frame() {
        let pane = pane();
        let (response, result) = mpsc::sync_channel(1);
        let info = ClipboardPending {
            request_id: "request".into(),
            view_id: "view".into(),
            request: bkpr_protocol::ClipboardRequest {
                operation: bkpr_protocol::ClipboardOperation::Read {
                    mimes: Vec::new(),
                    list: false,
                },
                location: ClipboardLocation::Standard,
                name: String::new(),
                granted: false,
                can_remember: false,
                paste_offer_id: None,
            },
        };
        *pane.shared.clipboard.lock().unwrap() = Some(ClipboardWait { info, response });
        let before = pane.shared.changes.revision();
        pane.cancel_clipboard(None);
        assert_ne!(pane.shared.changes.revision(), before);
        assert!(pane.shared.clipboard.lock().unwrap().is_none());
        assert!(result.try_recv().is_ok());
        let before = pane.shared.changes.revision();
        request(&pane, json!({"op":"shutdown"}));
        assert_ne!(pane.shared.changes.revision(), before);
        assert_eq!(pane.shared.published.lock().unwrap().revision, 1);
    }
}
