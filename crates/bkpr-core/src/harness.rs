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
    pub(crate) binding_generation: u64,
    /// Request-local only; never retained in the capture registry or durable store.
    pub(crate) binding_guard: Option<AgentBindingGuard>,
    /// Private, in-process admission witness. Never decoded from the wire.
    pub(crate) binding_context_admitted: bool,
}
impl Launch {
    pub(crate) fn control_lease(&self) -> Result<crate::ControlLease> {
        self.control.bind(&self.root, &self.pane_root)?;
        Ok(self.control.clone())
    }
    pub(crate) fn binding_snapshot(&self) -> NativeBindingSnapshot {
        NativeBindingSnapshot {
            revision: self.binding_generation,
            conversation: self.managed.conversation.clone(),
        }
    }
    pub(crate) fn accepts_binding_event(&self, event: &CaptureEvent) -> Result<bool> {
        Ok(match event {
            CaptureEvent::Bind {
                expected_revision, ..
            } => {
                ensure!(
                    self.grant.kind != HarnessKind::Opencode || expected_revision.is_some(),
                    "OpenCode conversation bind requires a binding revision"
                );
                expected_revision.is_none_or(|revision| revision == self.binding_generation)
            }
            CaptureEvent::Unbind {
                conversation,
                expected_revision,
            } => {
                *expected_revision == self.binding_generation
                    && *conversation == self.managed.conversation
            }
            _ => true,
        })
    }
}
#[derive(Default)]
pub(crate) struct Captures {
    pub launches: BTreeMap<String, Launch>,
    pub version: u64,
    pub native_status: BTreeMap<String, NativeStatusChannel>,
    native_contexts: BTreeMap<String, NativeContexts>,
}

const NATIVE_CONTEXT_CACHE_BYTES: usize = 64 * 1024;
const NATIVE_CONTEXT_CACHE_ENTRIES: usize = 256;

#[derive(Default)]
struct NativeContexts {
    entries: BTreeMap<String, u64>,
    bytes: usize,
    clock: u64,
}
impl NativeContexts {
    fn contains(&self, context_id: &str) -> bool {
        self.entries.contains_key(context_id)
    }
    fn attest(&mut self, context_id: String) {
        self.clock = self.clock.checked_add(1).unwrap_or_else(|| {
            // Cache turnover cannot revoke an admitted request's epoch witness.
            self.entries.clear();
            self.bytes = 0;
            1
        });
        if let Some(refreshed) = self.entries.get_mut(&context_id) {
            *refreshed = self.clock;
            return;
        }
        while self.entries.len() >= NATIVE_CONTEXT_CACHE_ENTRIES
            || self.bytes + context_id.len() > NATIVE_CONTEXT_CACHE_BYTES
        {
            let oldest = *self.entries.values().min().expect("nonempty context cache");
            let bytes = &mut self.bytes;
            self.entries.retain(|id, age| {
                if *age == oldest {
                    *bytes -= id.len();
                    false
                } else {
                    true
                }
            });
        }
        self.bytes += context_id.len();
        self.entries.insert(context_id, self.clock);
    }
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
    for (index, p) in chain.iter().enumerate() {
        if let Some(found) = proc::kind(p.pid)? {
            if found == HarnessKind::Opencode
                && let Some(owner) = chain.get(index + 1)
                && proc::owned_opencode_worker(p, owner)?
            {
                // The direct native standalone worker is one TUI's role, not
                // a second harness. Its verified owner remains a boundary.
                continue;
            }
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
                binding_generation: 0,
                binding_guard: None,
                binding_context_admitted: false,
            },
        );
        self.version = self.version.wrapping_add(1);
        Ok(grant)
    }
    pub fn lookup(&self, launch_id: &str, token: &str) -> Result<Launch> {
        Ok(self.lookup_ref(launch_id, token)?.clone())
    }
    fn lookup_ref(&self, launch_id: &str, token: &str) -> Result<&Launch> {
        let launch = self
            .launches
            .get(launch_id)
            .context("unknown or ended launch")?;
        // Fixed-size capabilities; no prefix/partial match and never include them in diagnostics.
        ensure!(
            token.len() == 64 && constant_equal(token.as_bytes(), launch.grant.token.as_bytes()),
            "invalid capture capability"
        );
        Ok(launch)
    }
    pub(crate) fn authorize(&self, request: &Launch) -> Result<()> {
        let current = self
            .launches
            .get(&request.grant.launch_id)
            .context("unknown or ended launch")?;
        ensure!(
            constant_equal(
                request.grant.token.as_bytes(),
                current.grant.token.as_bytes()
            ),
            "invalid capture capability"
        );
        if let Some(guard) = &request.binding_guard {
            ensure!(
                guard.conversation.generation != 0
                    && current.managed.conversation.as_ref() == Some(&guard.conversation),
                "native conversation binding guard changed"
            );
            ensure!(
                request.control.same_authority(&current.control) && request.control.is_valid(),
                "native conversation control binding changed"
            );
            ensure!(
                request.binding_context_admitted
                    || guard.context_id == guard.conversation.id
                    || self
                        .native_contexts
                        .get(&request.grant.launch_id)
                        .is_some_and(|contexts| contexts.contains(&guard.context_id)),
                "native context is not authorized for this binding generation"
            );
        }
        Ok(())
    }
    pub(crate) fn admit(&self, request: &mut Launch) -> Result<()> {
        request.binding_context_admitted = false;
        self.authorize(request)?;
        // Membership is needed only at facade admission. Every later boundary
        // still checks the immutable native root and the revocable control epoch.
        request.binding_context_admitted = request.binding_guard.is_some();
        Ok(())
    }
    fn validate_native_id(&self, id: &str) -> Result<()> {
        ensure!(
            !id.trim().is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control),
            "invalid native conversation ID"
        );
        ensure!(
            !self
                .launches
                .values()
                .any(|launch| id.contains(&launch.grant.token)),
            "native conversation ID contains private capability"
        );
        Ok(())
    }
    pub fn event(&mut self, launch_id: &str, token: &str, event: CaptureEvent) -> Result<()> {
        let event = match event {
            CaptureEvent::NativeContext {
                conversation,
                context_id,
            } => {
                let launch = self.lookup_ref(launch_id, token)?;
                ensure!(
                    launch.managed.conversation.as_ref() == Some(&conversation),
                    "native context binding generation changed"
                );
                self.validate_native_id(&context_id)?;
                if context_id == conversation.id {
                    return Ok(());
                }
                if let Some(contexts) = self.native_contexts.get_mut(launch_id) {
                    contexts.attest(context_id);
                } else {
                    let mut contexts = NativeContexts::default();
                    contexts.attest(context_id);
                    self.native_contexts.insert(launch_id.to_owned(), contexts);
                }
                // No launch/store clone or pane/status publication per attestation.
                return Ok(());
            }
            event => event,
        };
        let launch = self.lookup(launch_id, token)?;
        ensure!(
            launch.accepts_binding_event(&event)?,
            "native conversation binding revision changed"
        );
        match event {
            CaptureEvent::Bind {
                conversation_id,
                source,
                expected_revision,
            } => {
                ensure!(
                    source.harness() == launch.grant.kind,
                    "conversation source does not match harness"
                );
                self.validate_native_id(&conversation_id)?;
                if launch.grant.kind == HarnessKind::Agy {
                    let id = Uuid::parse_str(&conversation_id)
                        .context("AGY conversation must be a canonical UUID")?;
                    ensure!(
                        id.to_string() == conversation_id,
                        "AGY conversation must be a canonical UUID"
                    );
                }
                let launch = self.launches.get_mut(launch_id).unwrap();
                if expected_revision.is_none()
                    && launch
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
                    .binding_generation
                    .checked_add(1)
                    .context("conversation generation exhausted")?;
                launch.binding_generation = generation;
                launch.control.revoke();
                launch.control = crate::ControlLease::default();
                self.native_contexts.remove(launch_id);
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
            CaptureEvent::Unbind { .. } => {
                let generation = launch
                    .binding_generation
                    .checked_add(1)
                    .context("conversation generation exhausted")?;
                let launch = self.launches.get_mut(launch_id).unwrap();
                launch.binding_generation = generation;
                launch.managed.conversation = None;
                launch.managed.error = None;
                launch.control.revoke();
                launch.control = crate::ControlLease::default();
                self.native_contexts.remove(launch_id);
                self.clear_native_status(launch_id);
            }
            CaptureEvent::NativeContext { .. } => unreachable!("attestation handled above"),
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
                self.retire_launch(launch_id);
            }
        }
        self.version = self.version.wrapping_add(1);
        Ok(())
    }
    /// Revoke a launch and release its volatile capture resources.
    pub fn retire_launch(&mut self, launch_id: &str) {
        if let Some(launch) = self.launches.remove(launch_id) {
            launch.control.revoke();
        }
        self.native_contexts.remove(launch_id);
        self.clear_native_status(launch_id);
    }
    #[cfg(test)]
    /// Return the number of retained per-launch native context caches.
    pub(crate) fn native_context_cache_count(&self) -> usize {
        self.native_contexts.len()
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
        self.native_contexts
            .retain(|id, _| self.launches.contains_key(id));
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
        "opencode2",
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
                    expected_revision: None,
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
            expected_revision: None,
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
    fn opencode_binding_requires_cas_and_unbound_enrollment_fences_old_producers() {
        let mut state = Captures::default();
        let grant = state
            .insert(process(2), process(1), "pane".into(), HarnessKind::Opencode)
            .unwrap();
        let bind = |expected_revision| CaptureEvent::Bind {
            conversation_id: "root-A".into(),
            source: ConversationSource::OpencodePlugin,
            expected_revision,
        };
        let initial = state.lookup(&grant.launch_id, &grant.token).unwrap();
        let version = state.version;
        assert!(
            state
                .event(&grant.launch_id, &grant.token, bind(None))
                .is_err()
        );
        assert_eq!(state.version, version);
        assert_eq!(
            state
                .lookup(&grant.launch_id, &grant.token)
                .unwrap()
                .binding_snapshot(),
            NativeBindingSnapshot {
                revision: 0,
                conversation: None
            },
        );
        assert!(initial.control.is_valid());
        state
            .event(
                &grant.launch_id,
                &grant.token,
                CaptureEvent::Unbind {
                    conversation: None,
                    expected_revision: 0,
                },
            )
            .unwrap();
        assert!(!initial.control.is_valid());
        let reserved = state.lookup(&grant.launch_id, &grant.token).unwrap();
        assert_eq!(
            reserved.binding_snapshot(),
            NativeBindingSnapshot {
                revision: 1,
                conversation: None
            }
        );
        let version = state.version;
        assert!(
            state
                .event(&grant.launch_id, &grant.token, bind(Some(0)))
                .is_err()
        );
        assert_eq!(state.version, version);
        assert!(reserved.control.is_valid());
        state
            .event(&grant.launch_id, &grant.token, bind(Some(1)))
            .unwrap();
        let first = state.lookup(&grant.launch_id, &grant.token).unwrap();
        assert_eq!(first.managed.conversation.as_ref().unwrap().generation, 2);
        state
            .event(&grant.launch_id, &grant.token, bind(Some(2)))
            .unwrap();
        assert!(!first.control.is_valid());
        let replacement = state.lookup(&grant.launch_id, &grant.token).unwrap();
        assert_eq!(replacement.binding_generation, 3);
        assert_eq!(
            replacement
                .managed
                .conversation
                .as_ref()
                .unwrap()
                .generation,
            3
        );
        let version = state.version;
        assert!(
            state
                .event(
                    &grant.launch_id,
                    &grant.token,
                    CaptureEvent::Unbind {
                        conversation: first.managed.conversation,
                        expected_revision: first.binding_generation,
                    },
                )
                .is_err()
        );
        assert_eq!(state.version, version);
        assert!(replacement.control.is_valid());
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
                        source: ConversationSource::OmpExtension,
                        expected_revision: None,
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
                        source: ConversationSource::ClaudeHook,
                        expected_revision: None,
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
                            source: ConversationSource::AgyMcp,
                            expected_revision: None,
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
                    expected_revision: None,
                },
            )
            .unwrap();
    }
    #[test]
    fn native_context_cache_refreshes_and_turns_over_with_bounded_storage() {
        let mut contexts = NativeContexts::default();
        for index in 0..NATIVE_CONTEXT_CACHE_ENTRIES {
            contexts.attest(format!("child-{index}"));
        }
        contexts.attest("child-0".into());
        contexts.attest("newest-child".into());
        assert!(contexts.contains("child-0"));
        assert!(!contexts.contains("child-1"));
        assert!(contexts.contains("newest-child"));
        assert_eq!(contexts.entries.len(), NATIVE_CONTEXT_CACHE_ENTRIES);
        let padding = "x".repeat(900);
        for index in 0..1024 {
            let id = format!("long-child-{index}-{padding}");
            contexts.attest(id.clone());
            assert!(contexts.contains(&id));
            assert!(contexts.bytes <= NATIVE_CONTEXT_CACHE_BYTES);
            assert!(contexts.entries.len() <= NATIVE_CONTEXT_CACHE_ENTRIES);
            assert_eq!(
                contexts.bytes,
                contexts.entries.keys().map(String::len).sum::<usize>()
            );
        }
        assert!(!contexts.contains("child-0"));
        contexts.clock = u64::MAX;
        contexts.attest("clock-rollover".into());
        assert_eq!(contexts.clock, 1);
        assert_eq!(contexts.entries.len(), 1);
        assert!(contexts.contains("clock-rollover"));
    }
}
