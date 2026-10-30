//! Isolated, Zellij-independent engine proof for Bkpr's pane-first replacement.
pub mod authority;
#[cfg(target_os = "linux")]
mod process_tree;
pub mod render;
pub mod transport;
mod writer;

use anyhow::{Context, Result, bail};
use authority::{Authority, ViewSpec, valid_size};
use bkpr_terminal::{
    ClipboardContent, ClipboardLocation, ClipboardRequest, ClipboardResponse, Effect, Frame,
    ImageKey, KeyEvent, MouseEvent, ProgramStatus, Size, Terminal,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INPUT: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Inspect,
    Attach {
        #[serde(flatten)]
        spec: ViewSpec,
    },
    Update {
        view_id: String,
        #[serde(flatten)]
        spec: ViewSpec,
    },
    Detach {
        view_id: String,
    },
    Poll {
        view_id: String,
        #[serde(default)]
        after: u64,
        #[serde(default)]
        frame_after: u64,
        #[serde(default)]
        resources: Vec<ImageKey>,
    },
    Pin {
        view_id: String,
        size: Option<Size>,
    },
    Input {
        view_id: String,
        data: Vec<u8>,
    },
    Paste {
        view_id: String,
        data: Vec<u8>,
    },
    PasteContents {
        view_id: String,
        contents: Vec<ClipboardContent>,
        location: ClipboardLocation,
        #[serde(default)]
        allow_unsafe: bool,
    },
    Key {
        view_id: String,
        event: KeyEvent,
    },
    Mouse {
        view_id: String,
        event: MouseEvent,
    },
    Focus {
        view_id: String,
        focused: bool,
    },
    Scroll {
        view_id: String,
        delta: i64,
    },
    Clipboard {
        view_id: String,
        request_id: String,
        response: ClipboardResponse,
    },
    Colors {
        view_id: String,
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: Option<[u8; 3]>,
        palette: Option<Vec<[u8; 3]>>,
    },
    Terminate {
        view_id: String,
    },
    Shutdown,
}
#[derive(Clone, Serialize)]
pub struct ClipboardPending {
    pub request_id: String,
    pub view_id: String,
    pub request: ClipboardRequest,
}
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
#[derive(Clone, Serialize)]
pub struct Event {
    pub sequence: u64,
    pub effect: Effect,
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
    Resize(Size),
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
struct Command {
    mutation: Mutation,
    result: mpsc::SyncSender<std::result::Result<Option<u64>, String>>,
}
struct Shared {
    authority: Mutex<Authority>,
    published: Mutex<Published>,
    clipboard: Mutex<Option<ClipboardWait>>,
    // Keep authority validation and command insertion in the same order across clients.
    requests: Mutex<()>,
    pane_id: String,
    writer: Mutex<Option<Arc<writer::Queue>>>,
    paste: Mutex<PasteGrants>,
}
#[derive(Clone)]
pub struct Pane {
    shared: Arc<Shared>,
    commands: mpsc::SyncSender<Command>,
}
pub struct SpawnSpec {
    pub argv: Vec<String>,
    pub home: PathBuf,
    pub size: Size,
    pub isolated_process: bool,
}
impl Pane {
    pub fn spawn(spec: SpawnSpec) -> Result<Self> {
        valid_size(spec.size)?;
        if spec.argv.is_empty() {
            bail!("empty shell argv");
        }
        prepare_home(&spec.home)?;
        #[cfg(target_os = "linux")]
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let shared = Arc::new(Shared {
            authority: Mutex::new(Authority::default()),
            published: Mutex::new(Published::default()),
            clipboard: Mutex::new(None),
            requests: Mutex::new(()),
            pane_id: Uuid::new_v4().to_string(),
            writer: Mutex::new(None),
            paste: Mutex::new(PasteGrants::default()),
        });
        let (commands, rx) = mpsc::sync_channel(64);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let state = shared.clone();
        thread::Builder::new()
            .name("proof-pane".into())
            .spawn(move || {
                let result = run_pane(spec, state.clone(), rx, ready_tx);
                if let Err(error) = result {
                    let mut p = state.published.lock().unwrap();
                    p.lifecycle = "fault".into();
                    p.error = Some(format!("{error:#}"));
                }
            })?;
        ready_rx
            .recv()
            .context("pane worker stopped during spawn")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self { shared, commands })
    }
    fn command(&self, mutation: Mutation) -> Result<Option<u64>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command {
                mutation,
                result: tx,
            })
            .map_err(|_| anyhow::anyhow!("pane command queue full or closed"))?;
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
        }
    }
    pub fn request(&self, request: Request) -> Result<Value> {
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
            Request::Inspect => return self.snapshot(None, 0, 0, &[]),
            Request::Poll {
                view_id,
                after,
                frame_after,
                resources,
            } => return self.snapshot(Some(view_id), *after, *frame_after, resources),
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
                > MAX_INPUT
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
            wait.response
                .try_send(response)
                .context("clipboard request expired")?;
            return Ok(json!({"accepted":true}));
        }
        let _serial = self.shared.requests.lock().unwrap();
        match request {
            Request::Inspect => self.snapshot(None, 0, 0, &[]),
            Request::Poll {
                view_id,
                after,
                frame_after,
                resources,
            } => self.snapshot(Some(&view_id), after, frame_after, &resources),
            Request::Attach { spec } => {
                let (id, size) = {
                    let mut a = self.shared.authority.lock().unwrap();
                    let id = a.attach(spec)?;
                    (id, a.owner().map(|(_, size)| size))
                };
                if let Some(size) = size {
                    self.command(Mutation::Resize(size))?;
                }
                Ok(json!({"view_id":id,"pane_id":self.shared.pane_id}))
            }
            Request::Update { view_id, spec } => {
                if !spec.clipboard || !spec.visible || !spec.interactive {
                    self.cancel_clipboard(Some(&view_id));
                }
                let size = {
                    let mut a = self.shared.authority.lock().unwrap();
                    a.update(&view_id, spec)?;
                    a.owner().map(|(_, size)| size)
                };
                if let Some(size) = size {
                    self.command(Mutation::Resize(size))?;
                }
                Ok(json!({"updated":true}))
            }
            Request::Detach { view_id } => {
                self.cancel_clipboard(Some(&view_id));
                let size = {
                    let mut a = self.shared.authority.lock().unwrap();
                    a.detach(&view_id)?;
                    a.owner().map(|(_, size)| size)
                };
                if let Some(size) = size {
                    self.command(Mutation::Resize(size))?;
                }
                Ok(json!({"detached":true}))
            }
            Request::Pin { view_id, size } => {
                let size = {
                    let mut a = self.shared.authority.lock().unwrap();
                    a.pin(&view_id, size)?;
                    a.owner().map(|(_, size)| size)
                };
                if let Some(size) = size {
                    self.command(Mutation::Resize(size))?;
                }
                Ok(json!({"pinned":size}))
            }
            Request::Input { view_id, data } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                if data.len() > MAX_INPUT {
                    bail!("input payload limit");
                }
                let id = self.command(Mutation::Raw(data))?;
                Ok(json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"}))
            }
            Request::Paste { view_id, data } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                if data.len() > MAX_INPUT {
                    bail!("input payload limit");
                }
                let id = self.command(Mutation::Paste(data))?;
                Ok(json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"}))
            }
            Request::PasteContents {
                view_id,
                contents,
                location,
                allow_unsafe,
            } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                {
                    let a = self.shared.authority.lock().unwrap();
                    let view = a.views.get(&view_id).context("stale view")?;
                    if !view.spec.clipboard || !view.spec.visible || !view.spec.interactive {
                        bail!("enhanced MIME paste requires an eligible clipboard grant");
                    }
                }
                if contents.len() > 16
                    || contents.iter().map(|c| c.data.len()).sum::<usize>() > MAX_INPUT
                {
                    bail!("paste payload limits");
                }
                let id = self.command(Mutation::PasteContents(
                    view_id,
                    contents,
                    location,
                    allow_unsafe,
                ))?;
                Ok(json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"}))
            }
            Request::Key { view_id, event } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                let id = self.command(Mutation::Key(event))?;
                Ok(json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"}))
            }
            Request::Mouse { view_id, event } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                let id = self.command(Mutation::Mouse(event))?;
                Ok(json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"}))
            }
            Request::Focus { view_id, focused } => {
                self.shared.authority.lock().unwrap().input(&view_id)?;
                let id = self.command(Mutation::Focus(focused))?;
                Ok(json!({"accepted":true,"write_id":id,"semantics":"queued_once_no_replay"}))
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
                self.command(Mutation::Terminate)?;
                Ok(json!({"terminated":true}))
            }
            Request::Shutdown => {
                self.cancel_clipboard(None);
                if matches!(
                    self.shared.published.lock().unwrap().lifecycle.as_str(),
                    "running" | "draining"
                ) {
                    self.command(Mutation::Terminate)?;
                }
                Ok(json!({"shutdown":true}))
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
    ) -> Result<Value> {
        if resources.len() > 1024 {
            bail!("image receipt limit");
        }
        let mut a = self.shared.authority.lock().unwrap();
        if let Some(id) = view {
            a.touch(id)?;
        }
        let p = self.shared.published.lock().unwrap();
        let clipboard = self.shared.clipboard.lock().unwrap();
        let frame = view
            .and_then(|id| p.scroll_frames.get(id))
            .or(p.frame.as_ref());
        let image_refs = frame.map(|frame| frame.image_keys()).unwrap_or_default();
        let frame = frame
            .filter(|_| frame_after != p.revision)
            .map(|frame| frame.wire(resources));
        Ok(
            json!({"pane_id":self.shared.pane_id,"pid":p.pid,"revision":p.revision,"frame":frame,"image_refs":image_refs,"resource_protocol":1,"writer":self.shared.writer.lock().unwrap().as_ref().map(|w|w.snapshot()),"lifecycle":p.lifecycle,"exit_code":p.exit_code,"error":p.error,"program_status":p.status,"events":p.events.iter().filter(|e| e.sequence > after).collect::<Vec<_>>(),"event_sequence":p.event_sequence,"size_owner":a.owner().map(|(id,size)|json!({"view_id":id,"size":size})),"lease_generation":a.generation,"views":a.views.values().collect::<Vec<_>>(),"clipboard":clipboard.as_ref().filter(|w|view == Some(w.info.view_id.as_str())).map(|w|&w.info),"clipboard_pending":clipboard.is_some(),"capabilities":{"sixel":false,"normalized_frames":true,"clipboard_timeout_ms":CLIPBOARD_TIMEOUT.as_millis(),"view_timeout_ms":10000}}),
        )
    }
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
        && !home.join(".bkpr-mux-proof-home").is_file()
        && std::fs::read_dir(home)?.next().is_some()
    {
        bail!("refusing nonempty HOME without proof isolation marker");
    }
    std::fs::create_dir_all(home)?;
    std::fs::write(
        home.join(".bkpr-mux-proof-home"),
        b"isolated engine proof\n",
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
    Ok(())
}
fn route_effects(effects: Vec<Effect>, writer: &writer::Queue, shared: &Shared) -> Result<()> {
    let mut replies = Vec::new();
    for effect in effects {
        match effect {
            Effect::PtyWrite(data) => {
                if replies.len().saturating_add(data.len()) > 2 * MAX_INPUT {
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
fn run_pane(
    spec: SpawnSpec,
    shared: Arc<Shared>,
    commands: mpsc::Receiver<Command>,
    ready: mpsc::SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    let setup = (|| -> Result<_> {
        let pair = native_pty_system().openpty(pty_size(spec.size))?;
        let mut cmd = CommandBuilder::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..]);
        cmd.env_clear();
        cmd.env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        );
        cmd.env("HOME", &spec.home);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("SHELL", &spec.argv[0]);
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
        cmd.cwd(&spec.home);
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
        .context("proof requires a native PTY descriptor")?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let (writer, writer_thread) = writer::Queue::start(master.take_writer()?)?;
    let mut writer_thread = Some(writer_thread);
    *shared.writer.lock().unwrap() = Some(writer.clone());
    let mut reader = master.try_clone_reader()?;
    let (output_tx, output) = mpsc::sync_channel::<Vec<u8>>(64);
    let reader_stop = ReaderStop(Arc::new(AtomicBool::new(false)));
    let reader_cancel = reader_stop.0.clone();
    let reader_thread = thread::Builder::new()
        .name("proof-pty-reader".into())
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
                        thread::sleep(Duration::from_millis(4))
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
                    result: bkpr_terminal::ClipboardResult::Success,
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
        let response = rx
            .recv_timeout(CLIPBOARD_TIMEOUT)
            .unwrap_or_else(|_| ClipboardResponse::denied());
        let mut pending = clipboard_state.clipboard.lock().unwrap();
        if pending
            .as_ref()
            .is_some_and(|p| p.info.request_id == request_id)
        {
            *pending = None;
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
    let _ = ready.send(Ok(()));
    let mut termination_reply = None;
    let result = (|| -> Result<()> {
        let mut eof = false;
        let clock = Instant::now();
        let mut next_image_tick = None;
        let mut current_size = spec.size;
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
                match output.try_recv() {
                    Ok(bytes) => {
                        route_effects(terminal.feed(&bytes)?, &writer, &shared)?;
                        dirty = true;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        eof = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
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
            match commands.recv_timeout(Duration::from_millis(8)) {
                Ok(command) => {
                    let terminate = matches!(&command.mutation, Mutation::Terminate);
                    let needs_publish = matches!(
                        &command.mutation,
                        Mutation::Resize(_) | Mutation::Colors { .. } | Mutation::Refresh
                    );
                    let outcome = (|| -> Result<Option<u64>> {
                        let mut write_id = None;
                        match command.mutation {
                            Mutation::Resize(size) => {
                                if size != current_size {
                                    master.resize(pty_size(size))?;
                                    terminal.resize(size)?;
                                    current_size = size;
                                }
                            }
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
                                    if grants.origins.len() >= bkpr_terminal::PASTE_OFFER_MAX_COUNT
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
                                                + bkpr_terminal::PASTE_OFFER_TIMEOUT,
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
                    if terminate {
                        writer.stop();
                        child.terminate()?;
                        terminal.program_exited();
                        publish(&mut terminal, &shared)?;
                        shared.published.lock().unwrap().lifecycle = "terminating".into();
                        termination_reply =
                            Some((command.result, outcome.map_err(|e| format!("{e:#}"))));
                        break;
                    }
                    let _ = command.result.send(outcome.map_err(|e| format!("{e:#}")));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    child.terminate()?;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(status) = child.inner.try_wait()? {
                child.terminate()?;
                terminal.program_exited();
                let complete = eof && terminal.sync_output_remaining().is_none();
                let mut p = shared.published.lock().unwrap();
                p.status = terminal.program_status();
                p.exit_code = Some(status.exit_code());
                p.lifecycle = if complete { "exited" } else { "draining" }.into();
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
    if let Some((reply, outcome)) = termination_reply {
        shared.published.lock().unwrap().lifecycle = "terminated".into();
        let _ = reply.send(outcome);
    }
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
}
impl OwnedChild {
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
            if self.inner.try_wait()?.is_some() {
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
            let exited = self.inner.try_wait()?.is_some();
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
