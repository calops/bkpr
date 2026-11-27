use super::*;
use crate::channels::{AttentionWaker, deliver_channel_attention};
use crate::notifications::{NotificationObservation, NotificationService};
use bkpr_protocol::PaneNotificationSource;
use bkpr_terminal_engine::ProgramStatusView;
use crossbeam_channel::{Sender, TryRecvError, bounded, select_biased};
use std::thread::{self, JoinHandle};

#[derive(Default)]
pub(crate) struct Capture {
    last: Option<NotificationObservation>,
    pending_withdrawal: bool,
}
impl Capture {
    pub(super) fn invalidate(&mut self) {
        self.pending_withdrawal = true;
    }
    pub(super) fn flush(&mut self, service: &NotificationService) {
        if self.pending_withdrawal {
            self.withdraw(service);
        }
    }
    pub(super) fn withdraw_launch(&mut self, service: &NotificationService, launch_id: &str) {
        if self
            .last
            .as_ref()
            .is_some_and(|last| last.source.launch_id.as_deref() == Some(launch_id))
        {
            self.withdraw(service);
        }
    }
    pub(super) fn withdraw(&mut self, service: &NotificationService) {
        self.pending_withdrawal = false;
        if let Some(mut previous) = self.last.take() {
            previous.state = None;
            previous.kind = None;
            previous.observed_at_ms = now_ms();
            service.observe(previous);
        }
    }
    pub(super) fn publish(
        &mut self,
        service: &NotificationService,
        observation: NotificationObservation,
    ) {
        self.flush(service);
        if let Some(previous) = &self.last {
            if same_authority(&previous.source, &observation.source)
                && previous.state == observation.state
                && previous.kind == observation.kind
            {
                return;
            }
            if !same_authority(&previous.source, &observation.source) {
                self.withdraw(service);
            }
        }
        self.last = Some(observation.clone());
        service.observe(observation);
    }
}

fn same_authority(left: &PaneNotificationSource, right: &PaneNotificationSource) -> bool {
    left.session.session_id == right.session.session_id
        && left.session.session_instance_id == right.session.session_instance_id
        && left.session.daemon_id == right.session.daemon_id
        && left.pane_id == right.pane_id
        && left.harness == right.harness
        && left.process == right.process
        && left.launch_id == right.launch_id
        && left.binding_generation == right.binding_generation
}

pub(super) fn managed_authority(
    managed: Option<&ManagedHarness>,
) -> Option<(&str, Option<&NativeConversation>)> {
    managed.map(|managed| (managed.launch_id.as_str(), managed.conversation.as_ref()))
}

pub(super) struct Flush<'a> {
    registry: &'a Registry,
    pane: Pane,
}
impl<'a> Flush<'a> {
    pub(super) fn new(registry: &'a Registry, pane: Pane) -> Self {
        Self { registry, pane }
    }
}
impl Drop for Flush<'_> {
    fn drop(&mut self) {
        if let Some(service) = self.registry.inner.notifications.get() {
            self.pane
                .shared
                .notification_capture
                .lock()
                .unwrap()
                .flush(service);
        }
    }
}

/// One scheduler per host, not one worker (or retained Registry) per session.
/// The channel databases are the durable work list. Scans stream session roots
/// one at a time; an empty pass sleeps until another committed attention post.
pub(crate) struct ChannelAttentionWorker {
    wake: AttentionWaker,
    running: Mutex<Option<(Sender<()>, JoinHandle<()>)>>,
}

enum AttentionScope {
    Host(PathBuf),
    Single(PathBuf),
}

impl ChannelAttentionWorker {
    pub(crate) fn host(sessions_root: PathBuf, service: NotificationService) -> Result<Self> {
        Self::start(AttentionScope::Host(sessions_root), service)
    }

    fn start(scope: AttentionScope, service: NotificationService) -> Result<Self> {
        let (roots, woken) = bounded::<Arc<Path>>(64);
        let (overflow, rescanning) = bounded(1);
        let (stop, stopped) = bounded(0);
        let worker = thread::Builder::new()
            .name("bkpr-channel-attention".into())
            .spawn(move || {
                let cancelled = || stopped.try_recv() == Err(TryRecvError::Disconnected);
                let mut host_retry_at = 0;
                let mut next = scope.scan(&service, &mut host_retry_at, &cancelled);
                loop {
                    let deadline = next.map_or_else(crossbeam_channel::never, |at| {
                        crossbeam_channel::after(Duration::from_millis(at.saturating_sub(now_ms())))
                    });
                    select_biased! {
                        recv(stopped) -> _ => break,
                        recv(deadline) -> _ => {
                            next = scope.scan(&service, &mut host_retry_at, &cancelled);
                        },
                        recv(rescanning) -> _ => {
                            next = scope.scan(&service, &mut host_retry_at, &cancelled);
                        },
                        recv(woken) -> root => match root {
                            Ok(root) => {
                                let at = scope.scan_woken(
                                    &root,
                                    &service,
                                    &mut host_retry_at,
                                    &cancelled,
                                );
                                next = next.into_iter().chain(at).min();
                            }
                            Err(_) => break,
                        },
                    }
                }
            })?;
        Ok(Self {
            wake: AttentionWaker { roots, overflow },
            running: Mutex::new(Some((stop, worker))),
        })
    }

    pub(crate) fn stop(&self) {
        let mut running = self.running.lock().unwrap();
        let Some((stop, worker)) = running.take() else {
            return;
        };
        drop(stop);
        let _ = worker.join();
    }
}

impl Drop for ChannelAttentionWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn scan_attention_root(
    root: &Path,
    key: Option<&str>,
    service: &NotificationService,
    host_retry_at: &mut u64,
    cancelled: &impl Fn() -> bool,
) -> Option<u64> {
    match deliver_channel_attention(
        root,
        service,
        || sessions::legacy_attention_session(root, key),
        host_retry_at,
        cancelled,
    ) {
        Ok(at) => at,
        Err(error) if !cancelled() => {
            crate::diagnostics::record("channels.attention_store", None, "deferred");
            let transient = error
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| {
                    matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy
                                | rusqlite::ErrorCode::DatabaseLocked
                                | rusqlite::ErrorCode::SystemIoFailure
                                | rusqlite::ErrorCode::DiskFull
                                | rusqlite::ErrorCode::CannotOpen
                        )
                    )
                });
            transient.then(|| now_ms().saturating_add(1_000))
        }
        Err(_) => None,
    }
}

impl AttentionScope {
    fn scan_woken(
        &self,
        root: &Path,
        service: &NotificationService,
        host_retry_at: &mut u64,
        cancelled: &impl Fn() -> bool,
    ) -> Option<u64> {
        let key = match self {
            Self::Host(sessions) => {
                if root.parent() != Some(sessions.as_path()) {
                    return None;
                }
                Some(root.file_name()?.to_str()?)
            }
            Self::Single(session) => {
                if root != session.as_path() {
                    return None;
                }
                None
            }
        };
        scan_attention_root(root, key, service, host_retry_at, cancelled)
    }

    fn scan(
        &self,
        service: &NotificationService,
        host_retry_at: &mut u64,
        cancelled: &impl Fn() -> bool,
    ) -> Option<u64> {
        match self {
            Self::Single(root) => {
                scan_attention_root(root, None, service, host_retry_at, cancelled)
            }
            Self::Host(sessions) => {
                let mut next = None;
                let entries = (|| -> Result<_> {
                    let meta = fs::symlink_metadata(sessions)?;
                    ensure!(
                        meta.is_dir()
                            && meta.uid() == unsafe { libc::geteuid() }
                            && meta.mode() & 0o077 == 0,
                        "unsafe host sessions root"
                    );
                    Ok(fs::read_dir(sessions)?)
                })();
                let Ok(entries) = entries else {
                    crate::diagnostics::record("channels.attention_scan", None, "unavailable");
                    return Some(now_ms().saturating_add(1_000));
                };
                for entry in entries {
                    if cancelled() {
                        break;
                    }
                    let Ok(entry) = entry else { continue };
                    let name = entry.file_name();
                    let Some(key) = name.to_str().filter(|key| {
                        key.len() == 64
                            && key
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    }) else {
                        continue;
                    };
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        let at = scan_attention_root(
                            &entry.path(),
                            Some(key),
                            service,
                            host_retry_at,
                            cancelled,
                        );
                        next = next.into_iter().chain(at).min();
                    }
                }
                next
            }
        }
    }
}

impl Registry {
    /// Attach the notification service before starting session panes.
    /// Rejects a second attachment or attachment after bootstrap has started.
    pub fn configure_notifications(&self, service: NotificationService) -> Result<()> {
        self.attach_notifications(service, None)
    }

    pub(crate) fn configure_host_notifications(
        &self,
        service: NotificationService,
        attention: &ChannelAttentionWorker,
    ) -> Result<()> {
        self.attach_notifications(service, Some(attention))
    }

    fn attach_notifications(
        &self,
        service: NotificationService,
        attention: Option<&ChannelAttentionWorker>,
    ) -> Result<()> {
        let state = self.inner.state.lock().unwrap();
        ensure!(
            !state.closing
                && !state.bootstrap_started
                && state.live.is_empty()
                && state.spawning == 0,
            "notifications must be configured before pane startup"
        );
        ensure!(
            self.inner.notifications.get().is_none(),
            "notifications already configured"
        );
        let attention = match attention {
            Some(attention) => attention,
            None => {
                let worker = ChannelAttentionWorker::start(
                    AttentionScope::Single(self.inner.root.clone()),
                    service.clone(),
                )?;
                assert!(self.inner.standalone_attention.set(worker).is_ok());
                self.inner.standalone_attention.get().unwrap()
            }
        };
        assert!(self.inner.notifications.set(service).is_ok());
        self.inner
            .channels
            .set_attention_waker(attention.wake.clone());
        Ok(())
    }

    /// Inspect or subscribe within this registry's exact persistent session scope.
    pub fn request_notifications(
        &self,
        request: NotificationRequest,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Payload> {
        self.inner
            .notifications
            .get()
            .context("notifications are not configured")?
            .request(request, Some(&self.inner.session.session_id), is_connected)
    }

    pub(super) fn install_notification_capture(&self, pane: &Pane) {
        if self.inner.notifications.get().is_none() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let shared = Arc::downgrade(&pane.shared);
        let _ = pane
            .shared
            .status_observer
            .set(Box::new(move |records, explicit| {
                let (Some(inner), Some(shared)) = (weak.upgrade(), shared.upgrade()) else {
                    return;
                };
                let Some(service) = inner.notifications.get() else {
                    return;
                };
                let mut capture = shared.notification_capture.lock().unwrap();
                let observation = {
                    let state = inner.state.lock().unwrap();
                    let captures = inner.captures.lock().unwrap();
                    let published = shared.published.lock().unwrap();
                    if state.closing
                        || shared.finished.load(std::sync::atomic::Ordering::Acquire)
                        || !state
                            .live
                            .get(&shared.pane_id)
                            .is_some_and(|pane| Arc::ptr_eq(&pane.shared, &shared))
                        || shared
                            .status_epoch
                            .load(std::sync::atomic::Ordering::Acquire)
                            != shared
                                .status_feed_epoch
                                .load(std::sync::atomic::Ordering::Acquire)
                    {
                        return;
                    } else {
                        state
                            .record
                            .registry
                            .panes
                            .iter()
                            .find(|info| {
                                info.id == shared.pane_id && info.status == PaneStatus::Running
                            })
                            .and_then(|info| {
                                osc_observation(
                                    &inner.session,
                                    info,
                                    &published,
                                    &captures,
                                    records,
                                    explicit,
                                )
                            })
                    }
                };
                if let Some(observation) = observation {
                    capture.publish(service, observation);
                }
            }));
    }

    pub(super) fn withdraw_notifications(&self, pane: &Pane) {
        if let Some(service) = self.inner.notifications.get() {
            pane.shared
                .notification_capture
                .lock()
                .unwrap()
                .withdraw(service);
        }
    }
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(super) fn source(
    session: &SessionInfo,
    info: &PaneInfo,
    published: &crate::Published,
    captures: &crate::harness::Captures,
) -> Option<PaneNotificationSource> {
    let harness = info.harness.as_ref()?;
    let process = published.program_identity.as_ref()?;
    if published.lifecycle != "running"
        || process.pid != harness.pid
        || process.birth != harness.birth
    {
        return None;
    }
    let (launch_id, binding_generation) = if let Some(managed) = &harness.managed {
        let launch = captures.launches.get(&managed.launch_id)?;
        if launch.grant.kind != harness.kind
            || launch.grant.pane_id != info.id
            || launch.managed.conversation != managed.conversation
            || !launch.control.is_valid()
        {
            return None;
        }
        (
            Some(managed.launch_id.clone()),
            Some(launch.binding_generation),
        )
    } else {
        (None, None)
    };
    Some(PaneNotificationSource {
        session: session.clone(),
        workspace_id: info.workspace_id.clone(),
        window_id: info.window_id.clone(),
        pane_id: info.id.clone(),
        harness: harness.kind,
        process: process.clone(),
        launch_id,
        binding_generation,
    })
}

pub(super) fn observation(
    source: PaneNotificationSource,
    status: Option<&ProgramStatus>,
    observed_at_ms: u64,
) -> NotificationObservation {
    NotificationObservation {
        source,
        state: status.map(|status| status.state.clone()),
        kind: status
            .and_then(|status| status.kind.as_deref())
            .filter(|kind| matches!(*kind, "permission" | "question" | "auth"))
            .map(str::to_owned),
        observed_at_ms,
    }
}

fn osc_observation(
    session: &SessionInfo,
    info: &PaneInfo,
    published: &crate::Published,
    captures: &crate::harness::Captures,
    records: ProgramStatusView<'_>,
    explicit: bool,
) -> Option<NotificationObservation> {
    let source = source(session, info, published, captures)?;
    if let Some(launch_id) = &source.launch_id
        && let Some(channel) = captures.native_status.get(launch_id)
        && info
            .harness
            .as_ref()?
            .managed
            .as_ref()?
            .conversation
            .as_ref()
            == Some(&channel.binding)
        && channel.origin.is_valid()
        && channel.launch.is_valid()
        && channel.observation.is_some()
    {
        return Some(observation(
            source,
            channel.observation.as_ref().map(|report| &report.status),
            now_ms(),
        ));
    }
    let status = explicit
        .then(|| super::capture::status_summary(source.harness, records.iter()))
        .flatten();
    Some(observation(source, status, now_ms()))
}
