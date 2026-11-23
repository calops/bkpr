use crate::tree::Owner;
use anyhow::Result;
use bkpr_protocol::{
    Notification, NotificationKind, NotificationSource, PaneInfo, PaneNotificationSource,
    PaneStatus, RegistrySnapshot, SessionInfo,
};
use bkpr_tui_framework::{TerminalGuard, notification};
use std::{
    collections::VecDeque,
    io::Write,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REPORT_WINDOW: Duration = Duration::from_millis(350);
const CHANNEL_ALERT_TTL: Duration = Duration::from_secs(300);

pub(crate) struct ChannelTarget {
    pub workspace: String,
    pub channel: ChannelId,
    pub attention: ChannelAttention,
}

pub(crate) enum Source {
    Pane(NotificationSource),
    Channel(ChannelTarget),
}

pub(crate) struct QueuedChannel {
    pub owner: Owner,
    pub target: ChannelTarget,
    expires: Instant,
    ready: Instant,
    count: u32,
}

/// Display labels resolved through the notification's original session and launch.
#[derive(Default)]
pub(crate) struct Context<'a> {
    /// Whether the original process and binding are still current.
    pub current: bool,
    workspace: Option<&'a str>,
    title: Option<&'a str>,
}

impl<'a> Context<'a> {
    /// Resolve labels without borrowing a replacement launch's title.
    pub fn resolve(
        source: &PaneNotificationSource,
        session: &SessionInfo,
        registry: &'a RegistrySnapshot,
    ) -> Self {
        if !source_session_is_current(source, session) {
            return Self::default();
        }
        let pane = registry
            .panes
            .iter()
            .find(|pane| pane.id == source.pane_id && source_is_current(source, session, pane));
        Self {
            current: pane.is_some(),
            workspace: registry
                .workspaces
                .iter()
                .find(|workspace| Some(&workspace.id) == source.workspace_id.as_ref())
                .map(|workspace| workspace.name.as_str()),
            title: pane.map(|pane| crate::metadata::pane_title(pane, None)),
        }
    }

    fn description(&self, source: &PaneNotificationSource) -> String {
        let title = self
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(source.harness.display_name());
        format!(
            "{} · {} · {}",
            source.session.name,
            self.workspace.unwrap_or("Unknown workspace"),
            title
        )
    }
}

/// An attached source retained for a bounded physical notification lifetime.
pub(crate) struct Target {
    /// Exact attachment slot and admission generation.
    pub owner: Owner,
    /// Historical source revalidated before navigation.
    pub source: PaneNotificationSource,
    expires: Instant,
    id: notification::Id,
}

/// Independent best-effort desktop policy for one physical TUI.
#[derive(Default)]
pub(crate) struct Desktop {
    targets: VecDeque<Target>,
    channels: VecDeque<QueuedChannel>,
}

impl Desktop {
    /// Attempt pane presentation without retry or an acceptance/display acknowledgement.
    pub fn show<'a>(
        &mut self,
        owner: Owner,
        record: Notification,
        resolve: impl FnOnce(&PaneNotificationSource) -> Option<Context<'a>>,
        terminal: &mut TerminalGuard,
        out: &mut impl Write,
    ) -> Result<()> {
        let NotificationSource::Pane(source) = record.source else {
            return Ok(());
        };
        let Some(context) = resolve(&source) else {
            return Ok(());
        };
        let now = Instant::now();
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let remaining = u128::from(record.expires_at_ms).saturating_sub(epoch);
        if remaining == 0 {
            return Ok(());
        }
        let expires = now
            .checked_add(Duration::from_millis(
                remaining.min(u128::from(u64::MAX)) as u64
            ))
            .unwrap_or(now);
        let state = match (record.kind, record.attention_kind.as_deref()) {
            (NotificationKind::Attention, Some("permission")) => "Permission needed",
            (NotificationKind::Attention, Some("question")) => "Question waiting",
            (NotificationKind::Attention, Some("auth")) => "Authentication needed",
            (NotificationKind::Attention, _) => "Attention needed",
            (NotificationKind::Error, _) => "Error reported",
            (NotificationKind::Idle, _) => "Became idle",
            (NotificationKind::Done, _) => "Completion reported",
        };
        let title = format!("Bkpr · {state}");
        let context = context.description(&source);
        let compact;
        let (title, body) = if capabilities.body {
            (title.as_str(), context)
        } else {
            compact = format!("{title} · {context}");
            (compact.as_str(), "")
        };
        let report = capabilities.activation && capabilities.close;
        let activation = match (report, capabilities.focus) {
            (true, true) => notification::Activation::ReportAndFocus,
            (true, false) => notification::Activation::Report,
            (false, true) => notification::Activation::Focus,
            (false, false) => notification::Activation::None,
        };
        let id = target.id;
        let evicted = terminal.show_notification(
            out,
            &notification::Notification {
                id,
                app_name: "Bkpr",
                title,
                body,
                icon: Some(include_bytes!("../assets/bkpr-notification.png")),
                activation,
                report_close: false,
                urgency: notification::Urgency::Normal,
            },
        )?;
        if let Some(evicted) = evicted {
            self.targets.retain(|target| target.id != evicted);
        }
        if self.targets.len() == notification::MAX_ACTIVE
            && let Some(oldest) = self.targets.pop_front()
        {
            terminal.close_notification(out, oldest.id)?;
        }
        self.targets.push_back(Target {
            owner,
            source,
            expires: now
                .checked_add(Duration::from_millis(
                    remaining.min(u128::from(u64::MAX)) as u64
                ))
                .unwrap_or(now),
            id,
        });
        Ok(())
    }

    /// Consume a callback once; duplicates and evicted IDs have no target.
    pub fn take(&mut self, id: notification::Id) -> Option<Target> {
        let index = self.targets.iter().position(|target| target.id == id)?;
        self.targets.remove(index)
    }

    /// Release expired or retired attachment resources through the output owner.
    pub fn expire(
        &mut self,
        mut current: impl FnMut(Owner) -> bool,
        terminal: &mut TerminalGuard,
        out: &mut impl Write,
    ) -> Result<()> {
        let now = Instant::now();
        let mut index = 0;
        while index < self.targets.len() {
            if self.targets[index].expires <= now || !current(self.targets[index].owner) {
                let target = self
                    .targets
                    .remove(index)
                    .expect("bounded target index exists");
                terminal.close_notification(out, target.id)?;
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    /// Earliest retained resource expiry for the existing event-loop deadline.
    pub fn deadline(&self) -> Option<Instant> {
        self.targets
            .iter()
            .map(|target| target.expires)
            .chain(self.channels.iter().map(|alert| alert.ready))
            .min()
    }
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Match owner, host incarnation and runtime, independently of display names.
pub(crate) fn source_session_is_current(
    source: &PaneNotificationSource,
    session: &SessionInfo,
) -> bool {
    source.session.session_id == session.session_id
        && source.session.daemon_id == session.daemon_id
        && source.session.session_instance_id == session.session_instance_id
}

/// Reject retired execution or replacement launch/binding before navigation.
pub(crate) fn source_is_current(
    source: &PaneNotificationSource,
    session: &SessionInfo,
    pane: &PaneInfo,
) -> bool {
    if !source_session_is_current(source, session)
        || source.pane_id != pane.id
        || source.workspace_id != pane.workspace_id
        || source.window_id != pane.window_id
        || pane.status != PaneStatus::Running
    {
        return false;
    }
    let Some(harness) = &pane.harness else {
        return false;
    };
    let launch = harness.managed.as_ref();
    source.harness == harness.kind
        && source.process.pid == harness.pid
        && source.process.birth == harness.birth
        && source.launch_id.as_deref() == launch.map(|managed| managed.launch_id.as_str())
        && source.binding_generation
            == launch
                .and_then(|managed| managed.conversation.as_ref())
                .map(|conversation| conversation.generation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{
        ConversationSource, HarnessInfo, HarnessKind, ManagedHarness, NativeConversation,
        ProcessIdentity,
    };

    fn fixture() -> (SessionInfo, PaneNotificationSource, PaneInfo) {
        let session = SessionInfo {
            name: "Main".into(),
            session_id: "session".into(),
            daemon_id: "host".into(),
            session_instance_id: "instance".into(),
        };
        let source = PaneNotificationSource {
            session: session.clone(),
            workspace_id: Some("workspace".into()),
            window_id: Some("window".into()),
            pane_id: "pane".into(),
            harness: HarnessKind::Omp,
            process: ProcessIdentity {
                pid: 42,
                birth: "birth".into(),
            },
            launch_id: Some("launch".into()),
            binding_generation: Some(1),
        };
        let pane = PaneInfo {
            id: "pane".into(),
            workspace_id: source.workspace_id.clone(),
            window_id: source.window_id.clone(),
            title: "Agent".into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: "/".into(),
            foreground_cwd: None,
            harness: Some(HarnessInfo {
                kind: HarnessKind::Omp,
                pid: 42,
                birth: "birth".into(),
                cwd: "/".into(),
                managed: Some(ManagedHarness {
                    launch_id: "launch".into(),
                    conversation: Some(NativeConversation {
                        id: "conversation".into(),
                        source: ConversationSource::OmpExtension,
                        generation: 1,
                    }),
                    error: None,
                    first_prompt_submitted_at_ms: None,
                }),
            }),
            harness_status: None,
            harness_status_changed_at_ms: None,
            project: None,
            home: "/".into(),
            isolated_environment: true,
        };
        (session, source, pane)
    }

    #[test]
    fn context_uses_source_workspace_and_cleaned_title_without_replacement_labels() {
        let (session, source, mut pane) = fixture();
        pane.title = "π > Frontend 日本語".into();
        let mut registry = RegistrySnapshot {
            revision: 1,
            workspaces: vec![
                bkpr_protocol::Workspace {
                    id: "elsewhere".into(),
                    name: "Unrelated focus".into(),
                    revision: 1,
                    windows: vec![],
                },
                bkpr_protocol::Workspace {
                    id: "workspace".into(),
                    name: "Development".into(),
                    revision: 1,
                    windows: vec![],
                },
            ],
            panes: vec![pane],
        };
        let context = Context::resolve(&source, &session, &registry);
        assert!(context.current);
        assert_eq!(
            context.description(&source),
            "Main · Development · Frontend 日本語"
        );
        registry.panes[0]
            .harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .launch_id = "new-launch".into();
        registry.panes[0].title = "Unrelated replacement".into();
        let context = Context::resolve(&source, &session, &registry);
        assert!(!context.current);
        assert_eq!(context.title, None);
        assert!(
            !context
                .description(&source)
                .contains("Unrelated replacement")
        );
        registry.workspaces.clear();
        assert!(
            Context::resolve(&source, &session, &registry)
                .description(&source)
                .contains("Unknown workspace")
        );
        let mut foreign = session;
        foreign.session_instance_id = "new-session".into();
        let context = Context::resolve(&source, &foreign, &registry);
        assert!(context.workspace.is_none() && context.title.is_none());
    }

    #[test]
    fn activation_rejects_replaced_session_process_launch_and_binding() {
        let (session, source, pane) = fixture();
        assert!(source_is_current(&source, &session, &pane));
        let mut replaced = session.clone();
        replaced.session_instance_id = "reopened".into();
        assert!(!source_is_current(&source, &replaced, &pane));
        let mut changed = pane.clone();
        changed.harness.as_mut().unwrap().birth = "reused-pid".into();
        assert!(!source_is_current(&source, &session, &changed));
        let mut changed = pane.clone();
        changed
            .harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .launch_id = "replacement".into();
        assert!(!source_is_current(&source, &session, &changed));
        let mut changed = pane.clone();
        changed
            .harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .conversation
            .as_mut()
            .unwrap()
            .generation = 2;
        assert!(!source_is_current(&source, &session, &changed));
        let mut changed = pane;
        changed.status = PaneStatus::Exited;
        assert!(!source_is_current(&source, &session, &changed));
    }
}
