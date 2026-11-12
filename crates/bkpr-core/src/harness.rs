//! Launch-scoped authority. Tokens never enter registry records, Debug or persistence.
use crate::harness_process::{self as proc, Process};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use uuid::Uuid;

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
    pub(crate) control: crate::ControlLease,
}
impl Launch {
    pub(crate) fn control_lease(&self) -> Result<crate::ControlLease> {
        self.control.bind(&self.root, &self.pane_root)?;
        Ok(self.control.clone())
    }
}
#[derive(Default)]
pub(crate) struct Captures {
    pub launches: BTreeMap<String, Launch>,
    pub version: u64,
    pub native_status: BTreeMap<String, NativeStatusChannel>,
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

pub(crate) struct NativeStatusChannel {
    pub binding: NativeConversation,
    pub peer: ProcessIdentity,
    pub origin: crate::ControlLease,
    pub launch: crate::ControlLease,
    pub observation: Option<NativeStatusObservation>,
}
pub(crate) struct NativeStatusObservation {
    pub status: ProgramStatus,
    pub changed_at_ms: u64,
}
impl NativeStatusChannel {
    pub fn ensure_reporter(&self, peer: &Peer, binding: &NativeConversation) -> Result<()> {
        ensure!(
            peer.uid == unsafe { libc::geteuid() }
                && peer.pid as u32 == self.peer.pid
                && peer.birth == self.peer.birth,
            "native status originating process does not match enrollment"
        );
        ensure!(
            binding == &self.binding,
            "native status binding generation changed"
        );
        self.origin.ensure_active()?;
        self.launch.ensure_active()
    }
}

pub(crate) struct NativeStatusStream {
    pub grant: CaptureGrant,
    pub binding: NativeConversation,
    pub origin: crate::ControlLease,
    pub launch: crate::ControlLease,
}
impl NativeStatusStream {
    pub fn ensure_active(&self) -> Result<()> {
        self.origin.ensure_active()?;
        self.launch.ensure_active()
    }
}
impl Drop for NativeStatusStream {
    fn drop(&mut self) {
        self.origin.revoke();
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
                    first_prompt_submitted_at_ms: None,
                },
                grant: grant.clone(),
                control: crate::ControlLease::default(),
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
                if let Some(channel) = self.native_status.remove(launch_id) {
                    channel.origin.revoke();
                }
                launch.managed.conversation = Some(NativeConversation {
                    id: conversation_id,
                    source,
                    generation,
                });
                launch.managed.error = None;
            }
            CaptureEvent::PromptSubmitted {
                conversation_id,
                source,
            } => {
                ensure!(
                    source.harness() == launch.grant.kind
                        && launch
                            .managed
                            .conversation
                            .as_ref()
                            .is_some_and(|conversation| conversation.id == conversation_id
                                && conversation.source == source),
                    "prompt submission does not match the bound native conversation"
                );
                let managed = &mut self.launches.get_mut(launch_id).unwrap().managed;
                if managed.first_prompt_submitted_at_ms.is_some() {
                    return Ok(());
                }
                managed.first_prompt_submitted_at_ms = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                );
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
            CaptureEvent::StatusEnroll { .. } | CaptureEvent::StatusReport { .. } => {
                anyhow::bail!("native status requires authenticated enrollment")
            }
            CaptureEvent::End => {
                launch.control.revoke();
                self.launches.remove(launch_id);
                self.clear_native_status(launch_id);
            }
        }
        self.version = self.version.wrapping_add(1);
        Ok(())
    }
    pub fn retire_pane(&mut self, pane_id: &str) {
        self.launches.retain(|_, launch| {
            if launch.grant.pane_id == pane_id {
                launch.control.revoke();
                false
            } else {
                true
            }
        });
        self.native_status.retain(|id, channel| {
            if self.launches.contains_key(id) {
                true
            } else {
                channel.origin.revoke();
                false
            }
        });
        self.version = self.version.wrapping_add(1);
    }
    pub fn clear_native_status(&mut self, launch_id: &str) {
        if let Some(channel) = self.native_status.remove(launch_id) {
            channel.origin.revoke();
        }
    }
}
pub(crate) fn constant_equal(a: &[u8], b: &[u8]) -> bool {
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
    fn prompt_submission_is_launch_bound_and_recorded_once() {
        let (mut state, grant) = captures();
        let submit = |id: &str, source| CaptureEvent::PromptSubmitted {
            conversation_id: id.into(),
            source,
        };
        assert_eq!(
            state
                .lookup(&grant.launch_id, &grant.token)
                .unwrap()
                .managed
                .first_prompt_submitted_at_ms,
            None
        );
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    submit("native", ConversationSource::ClaudeHook)
                )
                .is_err()
        );
        state
            .event(
                &grant.launch_id,
                &grant.token,
                CaptureEvent::Bind {
                    conversation_id: "native".into(),
                    source: ConversationSource::ClaudeHook,
                },
            )
            .unwrap();
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    submit("other", ConversationSource::ClaudeHook)
                )
                .is_err()
        );
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    submit("native", ConversationSource::OmpExtension)
                )
                .is_err()
        );
        state
            .event(
                &grant.launch_id,
                &grant.token,
                submit("native", ConversationSource::ClaudeHook),
            )
            .unwrap();
        let first = state
            .lookup(&grant.launch_id, &grant.token)
            .unwrap()
            .managed
            .first_prompt_submitted_at_ms
            .unwrap();
        let version = state.version;
        state
            .event(
                &grant.launch_id,
                &grant.token,
                submit("native", ConversationSource::ClaudeHook),
            )
            .unwrap();
        assert_eq!(
            state
                .lookup(&grant.launch_id, &grant.token)
                .unwrap()
                .managed
                .first_prompt_submitted_at_ms,
            Some(first)
        );
        assert_eq!(state.version, version);
        state.retire_pane(&grant.pane_id);
        let fresh = state
            .insert(process(3), process(1), "pane".into(), HarnessKind::Claude)
            .unwrap();
        assert_eq!(
            state
                .lookup(&fresh.launch_id, &fresh.token)
                .unwrap()
                .managed
                .first_prompt_submitted_at_ms,
            None
        );
    }
    #[test]
    fn many_launches_keep_exact_authority_and_retire_only_their_pane() {
        let mut state = Captures::default();
        let grants = (2..162)
            .map(|pid| {
                state
                    .insert(
                        process(pid),
                        process(1),
                        format!("pane-{pid}"),
                        HarnessKind::Claude,
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            state
                .insert(
                    process(2),
                    process(1),
                    "duplicate".into(),
                    HarnessKind::Claude
                )
                .is_err()
        );
        let first = state
            .lookup(&grants[0].launch_id, &grants[0].token)
            .unwrap();
        let second = state
            .lookup(&grants[1].launch_id, &grants[1].token)
            .unwrap();
        state.retire_pane(&grants[0].pane_id);
        assert!(!first.control.is_valid());
        assert!(
            state
                .lookup(&grants[0].launch_id, &grants[0].token)
                .is_err()
        );
        assert!(second.control.is_valid());
        state
            .event(&grants[1].launch_id, &grants[1].token, CaptureEvent::End)
            .unwrap();
        assert!(!second.control.is_valid());
        assert!(
            state
                .lookup(&grants[1].launch_id, &grants[1].token)
                .is_err()
        );
        for grant in &grants[2..] {
            assert!(
                state
                    .lookup(&grant.launch_id, &grant.token)
                    .unwrap()
                    .control
                    .is_valid()
            );
        }
        assert_eq!(state.launches.len(), grants.len() - 2);
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
