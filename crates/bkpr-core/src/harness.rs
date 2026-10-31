//! Launch-scoped authority. Tokens never enter registry records, Debug or persistence.
use crate::harness_process::{self as proc, Process};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_LAUNCHES: usize = MAX_LIVE_PANES * 2;
#[derive(Clone)]
pub(crate) struct SpawnEnvironment {
    pub socket: PathBuf,
    pub wrappers: PathBuf,
    pub executable: PathBuf,
}
#[derive(Clone)]
pub(crate) struct Launch {
    pub root: Process,
    pub pane_root: Process,
    pub grant: CaptureGrant,
    pub managed: ManagedHarness,
}
#[derive(Default)]
pub(crate) struct Captures {
    pub launches: BTreeMap<String, Launch>,
    pub version: u64,
}
/// Constructed exclusively from Unix socket credentials by the transport.
pub(crate) struct Peer {
    pub pid: i32,
    pub uid: u32,
    pub birth: String,
}
impl Peer {
    pub(crate) fn current(&self) -> Result<Process> {
        ensure!(self.uid == unsafe { libc::geteuid() }, "foreign peer uid");
        let p = proc::process(self.pid)?.context("capture peer exited")?;
        ensure!(p.identity == self.birth, "capture peer birth changed");
        Ok(p)
    }
}
/// Read a bounded live ancestry on the control worker, never on input/UI/global locks.
pub(crate) fn ancestry(peer: &Process, root: &Process) -> Result<Vec<Process>> {
    let mut chain = Vec::new();
    let mut current = peer.clone();
    for _ in 0..128 {
        chain.push(current.clone());
        if current.pid == root.pid {
            ensure!(
                current.identity == root.identity && current.tty == root.tty,
                "stale process root"
            );
            ensure!(proc::live(&chain), "process lineage changed during capture");
            return Ok(chain);
        }
        current =
            proc::process(current.parent)?.context("capture caller is outside launch lineage")?;
    }
    anyhow::bail!("capture ancestry exceeds bound")
}
pub(crate) fn reject_nested(chain: &[Process], kind: HarnessKind, begin: bool) -> Result<()> {
    let mut count = 0;
    for p in chain {
        if let Some(found) = proc::kind(p.pid)? {
            ensure!(
                !begin && found == kind,
                "nested or unmanaged harness context"
            );
            count += 1;
            ensure!(count <= 1, "nested harness context");
        }
    }
    Ok(())
}
impl Captures {
    pub fn insert(
        &mut self,
        peer: Process,
        pane_root: Process,
        pane_id: String,
        kind: HarnessKind,
    ) -> Result<CaptureGrant> {
        ensure!(self.launches.len() < MAX_LAUNCHES, "managed launch limit");
        ensure!(
            !self
                .launches
                .values()
                .any(|l| l.root.pid == peer.pid && l.root.identity == peer.identity),
            "launch already registered"
        );
        let grant = CaptureGrant {
            launch_id: Uuid::new_v4().to_string(),
            token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            pane_id,
            kind,
        };
        self.launches.insert(
            grant.launch_id.clone(),
            Launch {
                root: peer,
                pane_root,
                managed: ManagedHarness {
                    launch_id: grant.launch_id.clone(),
                    conversation: None,
                    error: None,
                },
                grant: grant.clone(),
            },
        );
        self.version = self.version.wrapping_add(1);
        Ok(grant)
    }
    pub fn lookup(&self, launch_id: &str, token: &str) -> Result<Launch> {
        let launch = self
            .launches
            .get(launch_id)
            .context("unknown or ended launch")?;
        // Fixed-size capabilities; no prefix/partial match and never include them in diagnostics.
        ensure!(
            token.len() == 64 && constant_equal(token.as_bytes(), launch.grant.token.as_bytes()),
            "invalid capture capability"
        );
        Ok(launch.clone())
    }
    pub fn event(&mut self, launch_id: &str, token: &str, event: CaptureEvent) -> Result<()> {
        let launch = self.lookup(launch_id, token)?;
        match event {
            CaptureEvent::Bind {
                conversation_id,
                source,
            } => {
                ensure!(
                    source.harness() == launch.grant.kind,
                    "conversation source does not match harness"
                );
                ensure!(
                    !conversation_id.trim().is_empty()
                        && conversation_id.len() <= 1024
                        && !conversation_id.chars().any(char::is_control),
                    "invalid native conversation ID"
                );
                ensure!(
                    !self
                        .launches
                        .values()
                        .any(|l| conversation_id.contains(&l.grant.token)),
                    "conversation ID contains private capability"
                );
                if launch.grant.kind == HarnessKind::Agy {
                    let id = Uuid::parse_str(&conversation_id)
                        .context("AGY conversation must be a canonical UUID")?;
                    ensure!(
                        id.to_string() == conversation_id,
                        "AGY conversation must be a canonical UUID"
                    );
                }
                let launch = self.launches.get_mut(launch_id).unwrap();
                if launch
                    .managed
                    .conversation
                    .as_ref()
                    .is_some_and(|c| c.id == conversation_id && c.source == source)
                {
                    launch.managed.error = None;
                    self.version = self.version.wrapping_add(1);
                    return Ok(());
                }
                let generation = launch
                    .managed
                    .conversation
                    .as_ref()
                    .map_or(Some(1), |c| c.generation.checked_add(1))
                    .context("conversation generation exhausted")?;
                launch.managed.conversation = Some(NativeConversation {
                    id: conversation_id,
                    source,
                    generation,
                });
                launch.managed.error = None;
            }
            CaptureEvent::Error { message } => {
                ensure!(
                    !message.trim().is_empty()
                        && message.len() <= 2048
                        && !message.chars().any(char::is_control),
                    "invalid capture error"
                );
                ensure!(
                    !self
                        .launches
                        .values()
                        .any(|l| message.contains(&l.grant.token)),
                    "capture error contains private capability"
                );
                self.launches.get_mut(launch_id).unwrap().managed.error = Some(message);
            }
            CaptureEvent::End => {
                self.launches.remove(launch_id);
            }
        }
        self.version = self.version.wrapping_add(1);
        Ok(())
    }
    pub fn retire_pane(&mut self, pane_id: &str) {
        self.launches.retain(|_, l| l.grant.pane_id != pane_id);
        self.version = self.version.wrapping_add(1);
    }
}
fn constant_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
pub(crate) fn validate_callback(peer: &Peer, launch: &Launch) -> Result<Vec<Process>> {
    let peer = peer.current()?;
    let root = proc::process(launch.root.pid)?.context("launch root exited")?;
    ensure!(
        root.identity == launch.root.identity
            && root.parent == launch.root.parent
            && root.tty == launch.root.tty,
        "launch provenance changed"
    );
    ensure!(
        root.tty != 0 && root.tty == launch.pane_root.tty,
        "launch PTY provenance changed"
    );
    let root_chain = ancestry(&root, &launch.pane_root)?;
    // Only inspect ancestors above the wrapper for nested managed/ordinary harnesses.
    reject_nested(&root_chain[1..], launch.grant.kind, true)?;
    let chain = ancestry(&peer, &root)?;
    reject_nested(&chain, launch.grant.kind, false)?;
    Ok(chain)
}

pub(crate) fn provision(root: &Path, socket: &Path, executable: &Path) -> Result<SpawnEnvironment> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    ensure!(
        socket.is_absolute() && executable.is_absolute(),
        "capture paths must be absolute"
    );
    let meta = std::fs::metadata(executable).context("selected bkpr executable")?;
    ensure!(
        meta.is_file()
            && meta.permissions().mode() & 0o111 != 0
            && meta.permissions().mode() & 0o022 == 0,
        "bkpr must be an executable not writable by other users"
    );
    let wrappers = root.join("harness-bin");
    if !wrappers.exists() {
        std::fs::create_dir(&wrappers)?;
        std::fs::set_permissions(&wrappers, std::fs::Permissions::from_mode(0o700))?;
    }
    let meta = std::fs::symlink_metadata(&wrappers)?;
    ensure!(
        meta.is_dir()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0,
        "wrapper directory must be private and user-owned"
    );
    for name in [
        "bkpr",
        "claude",
        "claude-code",
        "codex",
        "omp",
        "agy",
        "antigravity",
        "opencode",
        "cursor-agent",
        "copilot",
        "copilot-cli",
        "github-copilot",
        "aider",
        "cline",
        "goose",
    ] {
        let path = wrappers.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(m) => {
                ensure!(
                    m.file_type().is_symlink() && m.uid() == unsafe { libc::geteuid() },
                    "refusing foreign wrapper entry"
                );
                if std::fs::read_link(&path)? == executable {
                    continue;
                }
                std::fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        symlink(executable, path)?;
    }
    Ok(SpawnEnvironment {
        socket: socket.to_owned(),
        wrappers,
        executable: executable.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn process(pid: i32) -> Process {
        Process {
            pid,
            parent: 1,
            group: pid,
            foreground: pid,
            tty: 7,
            stopped: false,
            identity: format!("birth-{pid}"),
            #[cfg(target_os = "linux")]
            session: 1,
            #[cfg(target_os = "linux")]
            birth_ticks: pid as u64,
        }
    }
    fn captures() -> (Captures, CaptureGrant) {
        let mut captures = Captures::default();
        let grant = captures
            .insert(process(2), process(1), "pane".into(), HarnessKind::Claude)
            .unwrap();
        (captures, grant)
    }
    #[test]
    fn bind_is_exact_idempotent_and_replaces_generation() {
        let (mut state, grant) = captures();
        let bind = |id: &str| CaptureEvent::Bind {
            conversation_id: id.into(),
            source: ConversationSource::ClaudeHook,
        };
        state
            .event(&grant.launch_id, &grant.token, bind("one"))
            .unwrap();
        state
            .event(&grant.launch_id, &grant.token, bind("one"))
            .unwrap();
        assert_eq!(
            state
                .lookup(&grant.launch_id, &grant.token)
                .unwrap()
                .managed
                .conversation
                .unwrap()
                .generation,
            1
        );
        state
            .event(&grant.launch_id, &grant.token, bind("two"))
            .unwrap();
        let c = state
            .lookup(&grant.launch_id, &grant.token)
            .unwrap()
            .managed
            .conversation
            .unwrap();
        assert_eq!((c.id.as_str(), c.generation), ("two", 2));
        state
            .event(&grant.launch_id, &grant.token, CaptureEvent::End)
            .unwrap();
        assert!(
            state
                .event(&grant.launch_id, &grant.token, bind("late"))
                .is_err()
        );
    }
    #[test]
    fn capabilities_sources_and_bounds_fail_closed() {
        let (mut state, grant) = captures();
        assert!(state.lookup(&grant.launch_id, &"0".repeat(64)).is_err());
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    CaptureEvent::Bind {
                        conversation_id: "native".into(),
                        source: ConversationSource::OmpExtension
                    }
                )
                .is_err()
        );
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    CaptureEvent::Error {
                        message: grant.token.clone()
                    }
                )
                .is_err()
        );
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    CaptureEvent::Bind {
                        conversation_id: " ".into(),
                        source: ConversationSource::ClaudeHook
                    }
                )
                .is_err()
        );
        assert!(!format!("{grant:?}").contains(&grant.token));
        state.retire_pane("pane");
        assert!(state.lookup(&grant.launch_id, &grant.token).is_err());
    }
    #[test]
    fn agy_only_accepts_canonical_native_uuid() {
        let mut state = Captures::default();
        let grant = state
            .insert(process(2), process(1), "pane".into(), HarnessKind::Agy)
            .unwrap();
        for id in [
            "guessed",
            "ABCDEF12-1234-1234-1234-123456789012",
            "{12345678-1234-1234-1234-123456789012}",
        ] {
            assert!(
                state
                    .event(
                        &grant.launch_id,
                        &grant.token,
                        CaptureEvent::Bind {
                            conversation_id: id.into(),
                            source: ConversationSource::AgyMcp
                        }
                    )
                    .is_err()
            );
        }
        state
            .event(
                &grant.launch_id,
                &grant.token,
                CaptureEvent::Bind {
                    conversation_id: "12345678-1234-1234-1234-123456789012".into(),
                    source: ConversationSource::AgyMcp,
                },
            )
            .unwrap();
    }
}
