use crate::{
    controller::*,
    transport,
    updates::{self, InputPump, Pump, Snapshot},
};
use anyhow::{Context, Result, bail};
use bkpr_protocol::*;
use bkpr_tui_framework::*;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use termwiz::input::{InputEvent, InputParser, Modifiers, MouseButtons};

#[path = "jobs.rs"]
mod jobs;
#[path = "management.rs"]
mod management;

enum Background {
    Management(management::Completion),
    Attached {
        pane: String,
        attached: Box<Attached>,
    },
}
#[derive(Clone, Copy, Debug)]
enum AttachOwnership {
    NotAdmitted,
    Detached,
    Unconfirmed,
}
impl std::fmt::Display for AttachOwnership {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::NotAdmitted => "attachment was not admitted",
            Self::Detached => "admitted attachment was confirmed detached",
            Self::Unconfirmed => "attachment ownership remains unconfirmed; its slot is reserved",
        })
    }
}
impl std::error::Error for AttachOwnership {}
struct ManagementIntent {
    epoch: u64,
    selection: Option<management::Selection>,
}
enum RetirementPolicy {
    Visible,
    Waiting(updates::Receipt),
    Revoked,
    Failed,
}
struct RetiringCandidate {
    pane: String,
    attached: Attached,
}
struct PointerOrigin {
    pane: String,
    view: String,
}
#[derive(Clone)]
enum PointerCapture {
    Ui,
    Pane(Arc<PointerOrigin>),
}
struct PressedPointer {
    origin: Arc<PointerOrigin>,
    event: MouseEvent,
    admitted: bool,
    release: Option<Operation>,
}
impl PressedPointer {
    fn release_operation(&self) -> Operation {
        let mut event = self.event;
        event.action = MouseAction::Release;
        Operation::Pane {
            pane_id: self.origin.pane.clone(),
            request: PaneRequest::Mouse {
                view_id: self.origin.view.clone(),
                event,
            },
        }
    }
}

pub struct Options {
    pub read_only: bool,
    pub clipboard_policy: String,
    pub shell: Vec<String>,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub isolated_environment: bool,
}
impl Options {
    pub fn inherited() -> Result<Self> {
        Ok(Self {
            read_only: false,
            clipboard_policy: "ask".into(),
            shell: vec![
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                "-i".into(),
            ],
            cwd: std::env::current_dir()?,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is required")?,
            isolated_environment: false,
        })
    }
}
struct Attached {
    view: String,
    spec: ViewSpec,
    pump: Pump<Snapshot>,
    metadata: PaneSnapshot,
    clipboard_id: Option<String>,
    frame: Option<Frame>,
    revision: u64,
    images: HashMap<ImageKey, Arc<Image>>,
    seeded: u64,
    retirement_requested: bool,
    retirement_policy: RetirementPolicy,
    retirement_focus_out: bool,
    retirement_error: Option<String>,
}
impl Attached {
    fn retire(
        &mut self,
        input: &InputPump,
        pane: &str,
        focus_out: bool,
        wait_policy: bool,
        drain_ready: bool,
    ) -> Result<Option<updates::Receipt>> {
        self.retirement_requested = true;
        self.retirement_focus_out |= focus_out;
        if matches!(self.retirement_policy, RetirementPolicy::Visible) {
            let mut hidden = self.spec.clone();
            hidden.visible = false;
            hidden.interactive = false;
            hidden.clipboard = false;
            // Previously accepted input still drains under its original grant.
            self.retirement_policy =
                RetirementPolicy::Waiting(input.policy(pane, &self.view, hidden)?);
        }
        if let RetirementPolicy::Waiting(receipt) = &self.retirement_policy {
            match receipt.try_recv() {
                Ok(Ok(())) => self.retirement_policy = RetirementPolicy::Revoked,
                Ok(Err(error)) => {
                    self.retirement_error = Some(error);
                    self.retirement_policy = RetirementPolicy::Failed;
                }
                Err(mpsc::TryRecvError::Empty) if wait_policy => return Ok(None),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.retirement_error = Some(format!(
                        "pane {pane} view {} policy completion disconnected; outcome unknown; not replayed",
                        self.view
                    ));
                    self.retirement_policy = RetirementPolicy::Failed;
                }
            }
        }
        if !drain_ready {
            return Ok(None);
        }
        input
            .retire(pane, &self.view, self.retirement_focus_out)
            .map(Some)
    }
    fn scene(&mut self, update: scene_wire::DecodedScene) -> Result<()> {
        update.apply(&mut self.revision, &mut self.frame)?;
        let frame = self.frame.as_mut().context("decoded scene missing")?;
        for image in frame.images.drain(..) {
            self.images.insert(image.key(), image);
        }
        let resources = &self.metadata.image_refs;
        let retained = resources.iter().copied().collect::<HashSet<_>>();
        self.images.retain(|key, _| retained.contains(key));
        frame.images = resources
            .iter()
            .map(|key| {
                self.images
                    .get(key)
                    .cloned()
                    .context("missing normalized image generation")
            })
            .collect::<Result<_>>()?;
        Ok(())
    }
    fn apply(&mut self, snapshot: Snapshot) -> Result<()> {
        self.metadata = snapshot.metadata;
        self.clipboard_id = self
            .metadata
            .clipboard
            .as_ref()
            .map(|p| p.request_id.clone());
        if let Some(scene) = snapshot.scene {
            self.scene(scene)?;
        }
        Ok(())
    }
}
struct DeviceConsent {
    pane: String,
    view: String,
    state: Consent,
    reply: Option<Operation>,
    reservation: Option<updates::DeviceReservation>,
}
struct RetiringView {
    attached: Attached,
    receipt: updates::Receipt,
    poll_error: Option<String>,
}
struct DeviceResponse {
    pane: String,
    view: String,
    receipt: updates::Receipt,
}
struct App<'a> {
    socket: &'a Path,
    options: Options,
    registry: RegistrySnapshot,
    state: Controller,
    views: HashMap<String, Attached>,
    retiring: Vec<RetiringView>,
    retiring_candidates: Vec<RetiringCandidate>,
    jobs: jobs::Jobs<Background>,
    management: HashMap<String, ManagementIntent>,
    selection_epoch: u64,
    attach_slots: usize,
    retirement_budget: Option<usize>,
    responses: Vec<DeviceResponse>,
    unconfirmed_views: usize,
    held: HashMap<String, (String, String)>,
    input: InputPump,
    input_failures: HashMap<String, [Option<updates::InputFailure>; 3]>,
    view_failures: HashMap<String, String>,
    consent: Option<DeviceConsent>,
    answered: HashMap<String, String>,
    deferred: RefCell<VecDeque<updates::ReservedInput>>,
    paste_offer: Option<PasteOffer>,
    paste_target: Option<(String, String)>,
    enhanced_supported: Option<bool>,
    colors: ColorProbe,
    last_button: MouseButton,
    pointer_capture: Option<PointerCapture>,
    pressed_pointers: RefCell<Vec<PressedPointer>>,
    notified_focus: Option<(String, String)>,
    focus_out_pending: HashSet<(String, String)>,
    lease_pending: HashSet<String>,
    host_focused: bool,
}
impl App<'_> {
    fn focus(&mut self, pane: String) {
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.state.focus(pane);
        self.select_current_lease();
    }
    fn switch(&mut self, workspace: String) {
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.state.switch(workspace, &self.registry);
        self.select_current_lease();
    }
    fn select_current_lease(&mut self) {
        if self.host_focused
            && let Some(pane) = &self.state.focused
        {
            self.lease_pending.insert(pane.clone());
        }
    }
    fn manage(
        &mut self,
        command: management::Command,
        selection: Option<management::Selection>,
    ) -> Result<()> {
        let changes_selection = selection.is_some()
            || matches!(
                &command,
                management::Command::Spawn { .. } | management::Command::Create { .. }
            );
        let scope = match &command {
            management::Command::Spawn { workspace, .. } => format!("workspace:{}", workspace.id),
            management::Command::Create { .. } => "create".into(),
            management::Command::Single(operation) => match operation {
                Operation::WorkspaceRename { workspace_id, .. }
                | Operation::WorkspaceRemove { workspace_id, .. }
                | Operation::LayoutUpdate { workspace_id, .. } => {
                    format!("workspace:{workspace_id}")
                }
                Operation::PaneRename { pane_id, .. } | Operation::PaneTerminate { pane_id } => {
                    format!("pane:{pane_id}")
                }
                _ => bail!("unsupported interactive management operation"),
            },
        };
        let key = format!("manage:{scope}");
        let socket = self.socket.to_owned();
        self.jobs.submit(key.clone(), move || {
            management::execute(&socket, command).map(Background::Management)
        })?;
        if changes_selection {
            self.selection_epoch = self.selection_epoch.wrapping_add(1);
        }
        self.management.insert(
            key.clone(),
            ManagementIntent {
                epoch: self.selection_epoch,
                selection,
            },
        );
        self.state.status = format!("{key} pending; other panes remain interactive");
        Ok(())
    }
    fn operation(&mut self, operation: Operation) -> Result<()> {
        self.manage(management::Command::Single(operation), None)
    }
    fn complete_background(&mut self) -> Result<()> {
        for completion in self.jobs.latest()? {
            let intent = self.management.remove(&completion.key);
            match completion.result {
                Ok(Background::Management(result)) => {
                    if result.registry.revision >= self.registry.revision {
                        self.registry = result.registry;
                        self.state.reconcile(&self.registry);
                    }
                    if let Some(intent) = intent
                        && intent.epoch == self.selection_epoch
                        && self.state.form.is_none()
                        && let Some(selection) = intent.selection.or(result.selection)
                        && self
                            .registry
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.id == selection.workspace)
                        && self
                            .registry
                            .panes
                            .iter()
                            .any(|pane| pane.id == selection.pane)
                    {
                        self.state.switch(selection.workspace, &self.registry);
                        self.state.focus(selection.pane);
                        self.state.reconcile(&self.registry);
                    }
                    self.state.status = format!("{} completed", completion.key);
                }
                Ok(Background::Attached { pane, attached }) => {
                    self.attach_slots -= 1;
                    if self.views.contains_key(&pane) || attached.retirement_requested {
                        self.retiring_candidates.push(RetiringCandidate {
                            pane,
                            attached: *attached,
                        });
                    } else {
                        self.views.insert(pane, *attached);
                    }
                }
                Err(error) => {
                    let unknown = if transport::outcome_unknown(&error) {
                        "; outcome unknown; not replayed"
                    } else {
                        "; not replayed"
                    };
                    let message = format!("{} failed{unknown}: {error:#}", completion.key);
                    if let Some(pane) = completion.key.strip_prefix("attach:") {
                        let held = error
                            .downcast_ref::<AttachOwnership>()
                            .map(|ownership| matches!(ownership, AttachOwnership::Unconfirmed))
                            .unwrap_or_else(|| transport::outcome_unknown(&error));
                        if !held {
                            self.attach_slots -= 1;
                        }
                        self.view_failures.insert(pane.to_owned(), message.clone());
                    }
                    self.state.status = message;
                }
            }
        }
        Ok(())
    }
    fn record_input_failure(&mut self, failure: updates::InputFailure) {
        self.state.status = failure.to_string();
        let slot = &mut self
            .input_failures
            .entry(failure.pane_id.clone())
            .or_default()[failure.lane.index()];
        if slot.as_ref().is_none_or(|previous| {
            !previous.stopped
                || (failure.stopped
                    && previous.operation == failure.operation
                    && previous.view_id == failure.view_id)
        }) {
            *slot = Some(failure);
        }
    }
    fn scope_input_result(&mut self, result: Result<()>) -> Result<()> {
        match result {
            Err(error) => {
                if let Some(rejected) = error.downcast_ref::<updates::InputRejected>() {
                    self.record_input_failure(rejected.0.clone());
                    Ok(())
                } else {
                    Err(error)
                }
            }
            Ok(()) => Ok(()),
        }
    }
    fn queue_control(&mut self, pane: &str, request: PaneRequest) -> Result<bool> {
        match self.enqueue(pane, request) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.scope_input_result(Err(error))?;
                Ok(false)
            }
        }
    }
    fn input_enabled(&self, pane: &str) -> bool {
        !self.view_failures.contains_key(pane)
            && self
                .views
                .get(pane)
                .is_none_or(|view| !view.retirement_requested && view.spec.visible)
            && self.input_failures.get(pane).is_none_or(|failures| {
                [0, 2].into_iter().all(|index| {
                    failures[index]
                        .as_ref()
                        .is_none_or(|failure| !failure.stopped)
                })
            })
    }
    fn poll_views(&mut self) {
        let mut failed = Vec::new();
        for (pane, view) in &mut self.views {
            if self.view_failures.contains_key(pane) {
                continue;
            }
            let result = view.pump.latest().and_then(|snapshot| {
                if let Some(snapshot) = snapshot {
                    view.apply(snapshot)?;
                }
                Ok(())
            });
            if let Err(error) = result {
                failed.push((pane.clone(), view.view.clone(), format!("{error:#}")));
                view.frame = None;
                view.images.clear();
            }
        }
        for (pane, view, error) in failed {
            self.state.status = format!("pane {pane} view {view} poll failed: {error}");
            self.view_failures.insert(pane, error);
        }
    }
    fn barrier_target(&self) -> Option<(&str, &str)> {
        self.consent
            .as_ref()
            .filter(|consent| consent.state.paste && (consent.state.sent || consent.state.allowed))
            .map(|consent| (consent.pane.as_str(), consent.view.as_str()))
            .or_else(|| {
                self.paste_offer
                    .as_ref()
                    .and(self.paste_target.as_ref())
                    .map(|(pane, view)| (pane.as_str(), view.as_str()))
            })
    }
    fn pointer_origin(
        &self,
        pane: &str,
        view: &str,
        event: MouseEvent,
    ) -> Option<Arc<PointerOrigin>> {
        if !matches!(
            event.button,
            MouseButton::Left | MouseButton::Middle | MouseButton::Right
        ) {
            return None;
        }
        self.pressed_pointers
            .borrow()
            .iter()
            .find(|pressed| {
                pressed.origin.pane == pane
                    && pressed.origin.view == view
                    && pressed.event.button == event.button
            })
            .map(|pressed| pressed.origin.clone())
            .or_else(|| {
                (event.action == MouseAction::Press).then(|| {
                    Arc::new(PointerOrigin {
                        pane: pane.into(),
                        view: view.into(),
                    })
                })
            })
    }
    fn record_pointer(&self, origin: Arc<PointerOrigin>, event: MouseEvent) {
        let mut pointers = self.pressed_pointers.borrow_mut();
        let found = pointers.iter().position(|pressed| {
            pressed.origin.pane == origin.pane
                && pressed.origin.view == origin.view
                && pressed.event.button == event.button
        });
        if event.action == MouseAction::Release {
            if let Some(index) = found {
                pointers.swap_remove(index);
            }
        } else if let Some(index) = found {
            pointers[index].event = event;
            pointers[index].admitted = true;
        } else if event.action == MouseAction::Press {
            pointers.push(PressedPointer {
                origin,
                event,
                admitted: true,
                release: None,
            });
        }
    }
    fn escrow_pointer_release(&self, pane: &str, view: &str, event: MouseEvent) {
        let mut pointers = self.pressed_pointers.borrow_mut();
        let release = Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Mouse {
                view_id: view.into(),
                event,
            },
        };
        if let Some(pressed) = pointers.iter_mut().find(|pressed| {
            pressed.origin.pane == pane
                && pressed.origin.view == view
                && pressed.event.button == event.button
        }) {
            pressed.release = Some(release);
        } else {
            // The press was accepted into the enhanced-paste suffix but has
            // not reached its native lane. Wait for record_pointer(Press).
            pointers.push(PressedPointer {
                origin: Arc::new(PointerOrigin {
                    pane: pane.into(),
                    view: view.into(),
                }),
                event,
                admitted: false,
                release: Some(release),
            });
        }
    }
    fn dispatch_reserved(&self, reserved: updates::ReservedInput) -> Result<()> {
        let pointer = reserved.mouse().and_then(|(pane, view, event)| {
            self.pointer_origin(pane, view, event)
                .map(|origin| (origin, event))
        });
        self.input.send_reserved(reserved)?;
        if let Some((origin, event)) = pointer {
            self.record_pointer(origin, event);
        }
        Ok(())
    }
    fn cancel_pointer(&mut self, pane: &str, view: &str) {
        if matches!(&self.pointer_capture, Some(PointerCapture::Pane(origin))
            if origin.pane == pane && origin.view == view)
        {
            self.pointer_capture = Some(PointerCapture::Ui);
        }
        for pressed in self
            .pressed_pointers
            .borrow_mut()
            .iter_mut()
            .filter(|pressed| pressed.origin.pane == pane && pressed.origin.view == view)
        {
            if pressed.release.is_none() {
                pressed.release = Some(pressed.release_operation());
            }
        }
    }
    fn flush_pointer_releases(&self) -> Result<()> {
        let mut result = Ok(());
        let barrier = self.barrier_target();
        let mut pointers = self.pressed_pointers.borrow_mut();
        let mut index = 0;
        while index < pointers.len() {
            if self
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == pointers[index].origin.pane)
                .is_none_or(|pane| pane.status != PaneStatus::Running)
            {
                pointers.swap_remove(index);
                continue;
            }
            if !pointers[index].admitted
                || pointers[index].release.is_none()
                || barrier.is_some_and(|(pane, view)| {
                    pointers[index].origin.pane == pane && pointers[index].origin.view == view
                })
            {
                index += 1;
                continue;
            }
            match self
                .input
                .send_owned_operation(&mut pointers[index].release)
            {
                Ok(()) => {
                    pointers.swap_remove(index);
                }
                Err(error) => {
                    // An immediate dispatcher error is local non-admission;
                    // asynchronous native errors are reported by InputPump.
                    if pointers[index].release.is_none() {
                        pointers[index].release = Some(pointers[index].release_operation());
                    }
                    accumulate_cleanup(&mut result, Err(error));
                    index += 1;
                }
            }
        }
        result
    }
    fn enqueue(&self, id: &str, request: PaneRequest) -> Result<()> {
        let lane = updates::InputLane::request(&request);
        if !updates::cleanup(&request)
            && let Some(failure) = self
                .input_failures
                .get(id)
                .and_then(|failures| failures[lane.index()].as_ref())
                .filter(|failure| failure.stopped)
        {
            return Err(updates::InputRejected(failure.clone()).into());
        }
        let target = self.barrier_target();
        let ordered = match &request {
            PaneRequest::Key { view_id, .. }
            | PaneRequest::Paste { view_id, .. }
            | PaneRequest::Mouse { view_id, .. }
            | PaneRequest::Focus { view_id, .. } => {
                target.is_some_and(|(pane, view)| pane == id && view_id == view)
            }
            _ => false,
        };
        let operation = Operation::Pane {
            pane_id: id.into(),
            request,
        };
        if ordered {
            let mut deferred = self.deferred.borrow_mut();
            if deferred.len() >= 64 {
                return Err(updates::InputFailure::admission(
                    &operation,
                    "pane-local enhanced paste input barrier full",
                )
                .into());
            }
            deferred.push_back(self.input.reserve(operation)?);
            Ok(())
        } else {
            self.input.send(operation)
        }
    }
    fn execution_completed(&self, pane: &str) -> bool {
        self.registry
            .panes
            .iter()
            .any(|info| info.id == pane && info.status == PaneStatus::Exited)
            || self.views.get(pane).is_some_and(|attached| {
                attached.metadata.error.is_none()
                    && matches!(
                        attached.metadata.lifecycle.as_str(),
                        "exited" | "terminated"
                    )
            })
    }
    fn active(&self) -> Option<(String, String)> {
        let id = self.state.focused.as_ref()?;
        if !self.input_enabled(id) || self.execution_completed(id) {
            return None;
        }
        if !self
            .registry
            .panes
            .iter()
            .any(|p| p.id == *id && p.status == PaneStatus::Running)
        {
            return None;
        }
        let attached = self.views.get(id)?;
        Some((id.clone(), attached.view.clone()))
    }
    fn spec(&self, size: Size) -> ViewSpec {
        ViewSpec {
            size,
            interactive: true,
            visible: true,
            input: !self.options.read_only,
            clipboard: !self.options.read_only && self.options.clipboard_policy != "deny",
        }
    }
    fn rects(&self, size: Size) -> Vec<(String, Rect)> {
        let (_, content) = screen_geometry(size);
        let mut rects = Vec::new();
        if let Some(workspace) = self.state.current(&self.registry) {
            layout_rects(workspace.layout.as_ref(), content, &mut rects);
        }
        rects
    }
    fn owned_views(&self) -> usize {
        self.views.len()
            + self.retiring.len()
            + self.retiring_candidates.len()
            + self.attach_slots
            + self.unconfirmed_views
    }
    fn retiring_views(&self) -> bool {
        !self.retiring.is_empty()
            || !self.retiring_candidates.is_empty()
            || self.views.values().any(|view| view.retirement_requested)
    }
    fn projection_limit(&self) -> usize {
        self.retirement_budget.unwrap_or(MAX_ADMITTED_VIEWS)
    }
    fn placeholder(&self, pane: &str) -> String {
        if let Some(error) = self.view_failures.get(pane) {
            return error.clone();
        }
        if let Some(failure) = self
            .input_failures
            .get(pane)
            .and_then(|failures| failures.iter().flatten().find(|failure| failure.stopped))
        {
            return failure.to_string();
        }
        if self
            .views
            .get(pane)
            .is_some_and(|view| view.retirement_requested)
        {
            return "Previous view retiring; accepted operations are still draining.".into();
        }
        if self.owned_views() >= self.projection_limit() {
            return if self.retiring_views() {
                "Waiting for retiring view capacity; this pane is currently unprojected.".into()
            } else {
                format!(
                    "Shared view capacity ({MAX_ADMITTED_VIEWS}) reached; this pane is unprojected. Remove displayed leaves or detach another client."
                )
            };
        }
        if self
            .registry
            .panes
            .iter()
            .find(|info| info.id == pane)
            .is_none_or(|info| info.status == PaneStatus::Stopped)
        {
            return "Execution unavailable. Ctrl-b n creates a new pane.".into();
        }
        "Attaching pane; waiting for its initial scene or a background worker slot.".into()
    }
    fn badge(&self, pane: &str) -> Option<&'static str> {
        if self.view_failures.contains_key(pane) {
            return Some("VIEW ERROR");
        }
        if let Some(failures) = self.input_failures.get(pane) {
            if [0, 2].into_iter().any(|index| {
                failures[index]
                    .as_ref()
                    .is_some_and(|failure| failure.stopped)
            }) {
                return Some("INPUT ERROR");
            }
            if failures[1].as_ref().is_some_and(|failure| failure.stopped) {
                return Some("CLIPBOARD ERROR");
            }
        }
        match self.views.get(pane) {
            Some(view) if view.retirement_requested => Some("RETIRING"),
            Some(view) if view.frame.is_some() => None,
            _ => Some("UNPROJECTED"),
        }
    }
    fn device_busy(&self, pane: &str, view: &str) -> bool {
        self.responses
            .iter()
            .any(|response| response.pane == pane && response.view == view)
            || self.consent.as_ref().is_some_and(|consent| {
                consent.pane == pane && consent.view == view && consent.reply.is_some()
            })
    }
    fn reap_responses(&mut self) {
        let mut index = 0;
        while index < self.responses.len() {
            match self.responses[index].receipt.try_recv() {
                Ok(result) => {
                    let response = self.responses.swap_remove(index);
                    if let Err(error) = result {
                        self.state.status = format!(
                            "pane {} view {} clipboard response failed: {error}",
                            response.pane, response.view
                        );
                    }
                }
                Err(mpsc::TryRecvError::Empty) => index += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let response = self.responses.swap_remove(index);
                    self.state.status = format!(
                        "pane {} view {} clipboard completion disconnected; outcome unknown; not replayed",
                        response.pane, response.view
                    );
                }
            }
        }
    }
    fn retire_view(&mut self, pane: &str, wait_policy: bool) -> Result<()> {
        let Some(attached) = self.views.get(pane) else {
            return Ok(());
        };
        let target = (pane.to_owned(), attached.view.clone());
        self.cancel_pointer(pane, &target.1);
        let cancellation = self.flush_pointer_releases();
        let completed = self.execution_completed(pane);
        if completed {
            // No execution remains to receive a previously deferred focus-out.
            self.views.get_mut(pane).unwrap().retirement_focus_out = false;
        }
        let focus_out = !completed
            && !self.options.read_only
            && (self.notified_focus.as_ref() == Some(&target)
                || self.focus_out_pending.contains(&target));
        if wait_policy && self.device_busy(pane, &target.1) {
            let attached = self.views.get_mut(pane).unwrap();
            attached.retirement_requested = true;
            attached.retirement_focus_out |= focus_out;
            return cancellation;
        }
        let ready = !self
            .pressed_pointers
            .borrow()
            .iter()
            .any(|pressed| pressed.origin.pane == pane && pressed.origin.view == target.1);
        let receipt = match self.views.get_mut(pane).unwrap().retire(
            &self.input,
            pane,
            focus_out,
            wait_policy,
            ready,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                let mut result = cancellation;
                accumulate_cleanup(&mut result, Err(error));
                return result;
            }
        };
        if let Some(error) = &self.views[pane].retirement_error {
            self.state.status = error.clone();
        }
        let Some(receipt) = receipt else {
            return cancellation;
        };
        self.focus_out_pending.remove(&target);
        self.lease_pending.remove(pane);
        if self.notified_focus.as_ref() == Some(&target) {
            self.notified_focus = None;
        }
        let attached = self.views.remove(pane).unwrap();
        self.held.retain(|_, (origin, _)| origin != pane);
        self.retiring.push(RetiringView {
            attached,
            receipt,
            poll_error: None,
        });
        cancellation
    }
    fn reap_retiring(&mut self) -> Result<()> {
        let mut index = 0;
        while index < self.retiring.len() {
            match self.retiring[index].receipt.try_recv() {
                Ok(result) => {
                    let retired = self.retiring.swap_remove(index);
                    if let Err(error) = result {
                        self.unconfirmed_views += 1;
                        self.state.status = format!(
                            "pane {} view {} detach failed: {error}; retirement not confirmed",
                            retired.attached.metadata.pane_id, retired.attached.view
                        );
                    }
                    drop(retired);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    let retired = &mut self.retiring[index];
                    if retired.poll_error.is_none()
                        && let Err(error) = retired.attached.pump.latest()
                    {
                        // Detach may already have committed while its ACK is in
                        // flight. Keep this evidence without reporting expected
                        // poll cancellation as a helper fault after success.
                        retired.poll_error = Some(format!("{error:#}"));
                    }
                    index += 1;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let retired = self.retiring.swap_remove(index);
                    self.unconfirmed_views += 1;
                    self.state.status = format!(
                        "pane {} view {} retirement completion unknown",
                        retired.attached.metadata.pane_id, retired.attached.view
                    );
                    drop(retired);
                }
            }
        }
        Ok(())
    }
    fn sync_views(&mut self, size: Size, rects: &[(String, Rect)]) -> Result<()> {
        let visible = rects
            .iter()
            .filter(|(_, r)| content(*r).height > 0)
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        let removed = self
            .views
            .keys()
            .filter(|id| {
                !visible.contains(*id)
                    || self.views[*id].retirement_requested
                    || self.view_failures.contains_key(*id)
                    || self
                        .registry
                        .panes
                        .iter()
                        .find(|p| p.id == **id)
                        .is_none_or(|p| p.status == PaneStatus::Stopped)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !removed.is_empty() && self.retirement_budget.is_none() {
            self.retirement_budget =
                Some(self.owned_views().clamp(MAX_LIVE_PANES, MAX_ADMITTED_VIEWS));
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|c| removed.contains(&c.pane))
        {
            let result = self.finish_consent(ClipboardResponse::denied());
            self.scope_input_result(result)?;
        }
        if self
            .paste_target
            .as_ref()
            .is_some_and(|(pane, _)| removed.contains(pane))
        {
            let result = self.abort_listing("paste origin left the visible layout");
            self.scope_input_result(result)?;
        }
        for id in removed {
            let result = self.retire_view(&id, true);
            self.scope_input_result(result)?;
        }
        let mut index = 0;
        while index < self.retiring_candidates.len() {
            let candidate = &mut self.retiring_candidates[index];
            let result = candidate
                .attached
                .retire(&self.input, &candidate.pane, false, true, true);
            match result {
                Ok(Some(receipt)) => {
                    let candidate = self.retiring_candidates.swap_remove(index);
                    self.retiring.push(RetiringView {
                        attached: candidate.attached,
                        receipt,
                        poll_error: None,
                    });
                }
                Ok(None) => index += 1,
                Err(error) => {
                    self.scope_input_result(Err(error))?;
                    index += 1;
                }
            }
        }
        if !self.retiring_views() {
            self.retirement_budget = None;
        }
        let focused = rects
            .iter()
            .position(|(id, _)| Some(id) == self.state.focused.as_ref());
        for index in focused
            .into_iter()
            .chain((0..rects.len()).filter(|index| Some(*index) != focused))
        {
            let (id, rect) = &rects[index];
            let rect = content(*rect);
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            let local = rect.size(size);
            let mut spec = self.spec(local);
            if self
                .input_failures
                .get(id)
                .and_then(|failures| failures[1].as_ref())
                .is_some_and(|failure| failure.stopped)
            {
                spec.clipboard = false;
            }
            if !self
                .registry
                .panes
                .iter()
                .any(|p| p.id == *id && p.status == PaneStatus::Running)
            {
                spec.input = false;
                spec.interactive = false;
                spec.clipboard = false;
            }
            if let Some(attached) = self.views.get(id) {
                if attached.retirement_requested {
                    continue;
                }
                if attached.spec.clipboard
                    && !spec.clipboard
                    && self.device_busy(id, &attached.view)
                {
                    continue;
                }
                if attached.spec.size != local
                    || !attached.spec.visible
                    || attached.spec.interactive != spec.interactive
                    || attached.spec.input != spec.input
                    || attached.spec.clipboard != spec.clipboard
                    || self.lease_pending.contains(id)
                {
                    let view = attached.view.clone();
                    if self.queue_control(
                        id,
                        PaneRequest::Update {
                            view_id: view,
                            spec: spec.clone(),
                        },
                    )? {
                        self.views.get_mut(id).unwrap().spec = spec;
                        self.lease_pending.remove(id);
                    }
                }
            } else if !self.view_failures.contains_key(id)
                && self
                    .registry
                    .panes
                    .iter()
                    .any(|p| p.id == *id && p.status != PaneStatus::Stopped)
            {
                let key = format!("attach:{id}");
                if !self.jobs.contains(&key)
                    && !self.jobs.is_full()
                    && self.owned_views() < self.projection_limit()
                {
                    let socket = self.socket.to_owned();
                    let pane = id.clone();
                    if let Err(error) = self.jobs.submit(key, move || {
                        attach(&socket, &pane, spec).map(|attached| Background::Attached {
                            pane,
                            attached: Box::new(attached),
                        })
                    }) {
                        let message = format!("pane {id} attach not admitted: {error:#}");
                        self.view_failures.insert(id.clone(), message.clone());
                        self.state.status = message;
                    } else {
                        self.attach_slots += 1;
                    }
                }
            }
        }
        self.notify_focus()
    }
    fn notify_focus(&mut self) -> Result<()> {
        let focused = if self.host_focused
            && self.state.form.is_none()
            && !self
                .consent
                .as_ref()
                .is_some_and(|c| !c.state.allowed && !c.state.sent)
        {
            self.active()
        } else {
            None
        };
        if focused != self.notified_focus
            && let Some(previous) = self.notified_focus.take()
        {
            self.focus_out_pending.insert(previous);
        }
        let pending = self.focus_out_pending.iter().cloned().collect::<Vec<_>>();
        for (pane, view) in pending {
            if !self.views.contains_key(&pane)
                || self.options.read_only
                || self.execution_completed(&pane)
                || self
                    .views
                    .get(&pane)
                    .is_some_and(|attached| attached.retirement_requested)
                || self.queue_control(
                    &pane,
                    PaneRequest::Focus {
                        view_id: view.clone(),
                        focused: false,
                    },
                )?
            {
                self.focus_out_pending.remove(&(pane, view));
            }
        }
        if focused != self.notified_focus
            && let Some((pane, view)) = &focused
            && (self.options.read_only
                || self.queue_control(
                    pane,
                    PaneRequest::Focus {
                        view_id: view.clone(),
                        focused: true,
                    },
                )?)
        {
            self.notified_focus = focused;
        }
        Ok(())
    }
    fn spawn(&mut self, size: Size, axis: Axis) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot create panes");
        }
        let workspace = self
            .state
            .current(&self.registry)
            .cloned()
            .context("create a workspace first (Ctrl-b c)")?;
        let parent = self
            .state
            .focused
            .as_ref()
            .and_then(|id| self.registry.panes.iter().find(|p| p.id == *id));
        let spec = PaneSpawnSpec {
            argv: self.options.shell.clone(),
            cwd: parent
                .map(|p| p.cwd.clone())
                .unwrap_or_else(|| self.options.cwd.clone()),
            home: parent
                .map(|p| p.home.clone())
                .unwrap_or_else(|| self.options.home.clone()),
            size,
            title: None,
            isolated_environment: parent
                .map(|p| p.isolated_environment)
                .unwrap_or(self.options.isolated_environment),
        };
        let target = self.state.focused.clone().unwrap_or_default();
        self.manage(
            management::Command::Spawn {
                workspace,
                target,
                axis,
                spec,
            },
            None,
        )
    }
    fn show_existing(&mut self, pane: String) -> Result<()> {
        let workspace = self
            .state
            .current(&self.registry)
            .cloned()
            .context("create a workspace first")?;
        let mut visible = Vec::new();
        leaves(workspace.layout.as_ref(), &mut visible);
        if !visible.contains(&pane) {
            if self.options.read_only {
                bail!("read-only client cannot change layout");
            }
            let target = self.state.focused.as_deref().unwrap_or("");
            let layout = split_leaf(workspace.layout, target, &pane, Axis::Horizontal);
            return self.manage(
                management::Command::Single(Operation::LayoutUpdate {
                    workspace_id: workspace.id.clone(),
                    expected_revision: workspace.revision,
                    layout,
                }),
                Some(management::Selection {
                    workspace: workspace.id,
                    pane,
                }),
            );
        }
        self.focus(pane);
        Ok(())
    }
    fn fail_paste(
        &mut self,
        pane: &str,
        view: &str,
        error: anyhow::Error,
        extra: usize,
    ) -> anyhow::Error {
        self.cancel_pointer(pane, view);
        let mut deferred = self.deferred.borrow_mut();
        let not_dispatched = deferred.len() + extra;
        deferred.clear();
        self.pressed_pointers.borrow_mut().retain(|pressed| {
            pressed.admitted || pressed.origin.pane != pane || pressed.origin.view != view
        });
        let prior = error.downcast_ref::<updates::InputRejected>();
        let unknown_outcome = prior.is_some_and(|rejected| rejected.0.unknown_outcome);
        let stopped = not_dispatched > 0 || prior.is_some_and(|rejected| rejected.0.stopped);
        updates::InputRejected(updates::InputFailure {
            pane_id: pane.into(),
            view_id: Some(view.into()),
            operation: "paste_contents",
            lane: updates::InputLane::Commands,
            stopped,
            unknown_outcome,
            not_dispatched,
            error: format!("{error:#}; following pane input was not dispatched"),
        })
        .into()
    }
    fn abort_listing(&mut self, reason: &str) -> Result<()> {
        let listing = self.paste_offer.take().is_some();
        let target = self.paste_target.take();
        if listing || !self.deferred.borrow().is_empty() {
            let (pane, view) = target.context("enhanced paste barrier lost its origin")?;
            return Err(self.fail_paste(&pane, &view, anyhow::anyhow!("{reason}"), 0));
        }
        Ok(())
    }
    fn finish_consent(&mut self, response: ClipboardResponse) -> Result<()> {
        let Some(mut consent) = self.consent.take() else {
            return Ok(());
        };
        if consent.state.paste {
            if response.result != ClipboardResult::Success && !self.deferred.borrow().is_empty() {
                return Err(self.fail_paste(
                    &consent.pane,
                    &consent.view,
                    anyhow::anyhow!("enhanced paste failed: {:?}", response.result),
                    0,
                ));
            }
            if response.result == ClipboardResult::Success {
                let result = self.enqueue(
                    &consent.pane,
                    PaneRequest::PasteContents {
                        view_id: consent.view.clone(),
                        contents: response.contents,
                        location: consent.state.request.location,
                        allow_unsafe: false,
                    },
                );
                if let Err(error) = result {
                    return Err(self.fail_paste(&consent.pane, &consent.view, error, 0));
                }
            }
            loop {
                let operation = self.deferred.borrow_mut().pop_front();
                let Some(operation) = operation else {
                    break;
                };
                if let Err(error) = self.dispatch_reserved(operation) {
                    return Err(self.fail_paste(&consent.pane, &consent.view, error, 1));
                }
            }
        } else {
            let result = if let Some(reservation) = consent.reservation.take() {
                self.input.send_response(reservation, response)
            } else {
                if consent.reply.is_none() {
                    consent.reply = Some(Operation::Pane {
                        pane_id: consent.pane.clone(),
                        request: PaneRequest::Clipboard {
                            view_id: consent.view.clone(),
                            request_id: consent.state.id.clone(),
                            response,
                        },
                    });
                }
                self.input.send_owned(&mut consent.reply)
            };
            match result {
                Ok(receipt) => {
                    self.responses.push(DeviceResponse {
                        pane: consent.pane.clone(),
                        view: consent.view.clone(),
                        receipt,
                    });
                    self.answered.insert(consent.pane, consent.state.id);
                }
                Err(error) => {
                    let retained =
                        consent.reply.is_some()
                            && error.downcast_ref::<updates::InputRejected>().is_some_and(
                                |rejected| !rejected.0.stopped && !rejected.0.unknown_outcome,
                            );
                    if retained {
                        self.consent = Some(consent);
                    } else {
                        self.answered.insert(consent.pane, consent.state.id);
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn begin_consent(&mut self, out: &mut impl Write) -> Result<()> {
        let Some(consent) = self.consent.as_mut() else {
            return Ok(());
        };
        if !consent.state.paste && consent.reservation.is_none() {
            match self
                .input
                .reserve_response(&consent.pane, &consent.view, &consent.state.id)
            {
                Ok(reservation) => consent.reservation = Some(reservation),
                Err(error) => {
                    if error
                        .downcast_ref::<updates::InputRejected>()
                        .is_some_and(|rejected| rejected.0.stopped || rejected.0.unknown_outcome)
                    {
                        let consent = self.consent.take().unwrap();
                        self.answered.insert(consent.pane, consent.state.id);
                    }
                    return Err(error);
                }
            }
        }
        let answer = begin_clipboard(out, &mut consent.state)?;
        if let Some(answer) = answer {
            self.finish_consent(answer)?;
        }
        Ok(())
    }
    fn drain_responses(&mut self) -> Result<()> {
        let mut result = Ok(());
        for response in self.responses.drain(..) {
            accumulate_cleanup(
                &mut result,
                response
                    .receipt
                    .recv()
                    .context("device reply worker stopped before completion")
                    .and_then(|result| result.map_err(anyhow::Error::msg)),
            );
        }
        result
    }
    fn clipboard(&mut self, out: &mut impl Write) -> Result<()> {
        self.answered.retain(|pane, id| {
            self.views
                .get(pane)
                .is_some_and(|view| view.clipboard_id.as_ref() == Some(&*id))
        });
        if self.consent.as_ref().is_some_and(|consent| {
            !consent.state.paste
                && self.views.get(&consent.pane).is_none_or(|view| {
                    self.view_failures.contains_key(&consent.pane)
                        || view.clipboard_id.as_ref() != Some(&consent.state.id)
                })
        }) {
            let consent = self.consent.take().unwrap();
            self.state.status = format!(
                "pane {} view {} clipboard grant expired or was revoked",
                consent.pane, consent.view
            );
            if let Some(reply) = consent.reply {
                return Err(updates::InputFailure::admission(
                    &reply,
                    "clipboard grant expired before reply admission; response was not replayed",
                )
                .into());
            }
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| consent.reply.is_some())
        {
            // Retry only local non-admission, preserving the original bytes.
            self.finish_consent(ClipboardResponse::denied())?;
        }
        let pending = self
            .views
            .iter()
            .filter(|(id, view)| {
                !view.retirement_requested
                    && view.spec.clipboard
                    && !self.view_failures.contains_key(*id)
                    && view.metadata.clipboard.is_some()
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for pane in pending {
            let attached = self.views.get_mut(&pane).unwrap();
            let view = attached.view.clone();
            let pending = attached.metadata.clipboard.take().unwrap();
            if pending.view_id != view {
                let error =
                    format!("pane {pane} clipboard grant does not belong to attached view {view}");
                self.view_failures.insert(pane, error.clone());
                self.state.status = error;
                continue;
            }
            let id = &pending.request_id;
            if self.answered.get(&pane) == Some(id) {
                continue;
            }
            if self
                .consent
                .as_ref()
                .is_some_and(|c| c.pane == pane && c.state.id == *id)
            {
                continue;
            }
            if self.consent.is_some() || self.paste_offer.is_some() || self.state.form.is_some() {
                let mut response = Some(Operation::Pane {
                    pane_id: pane.clone(),
                    request: PaneRequest::Clipboard {
                        view_id: view.clone(),
                        request_id: id.clone(),
                        response: ClipboardResponse::denied(),
                    },
                });
                match self.input.send_owned(&mut response) {
                    Ok(receipt) => {
                        self.responses.push(DeviceResponse {
                            pane: pane.clone(),
                            view,
                            receipt,
                        });
                        self.answered.insert(pane, id.clone());
                    }
                    Err(error) => {
                        if response.is_some()
                            && error.downcast_ref::<updates::InputRejected>().is_some_and(
                                |rejected| !rejected.0.stopped && !rejected.0.unknown_outcome,
                            )
                        {
                            self.views.get_mut(&pane).unwrap().metadata.clipboard = Some(pending);
                        } else {
                            self.answered.insert(pane, id.clone());
                        }
                        return Err(error);
                    }
                }
                continue;
            }
            self.consent = Some(DeviceConsent {
                pane,
                view,
                reply: None,
                reservation: None,
                state: Consent {
                    id: pending.request_id,
                    request: pending.request,
                    started: Instant::now(),
                    sent: false,
                    allowed: self.options.clipboard_policy == "allow",
                    contents: Vec::new(),
                    available: Vec::new(),
                    enhanced_supported: self.enhanced_supported,
                    paste: false,
                    password: None,
                },
            });
        }
        if let Some(consent) = self.consent.as_mut() {
            consent.state.enhanced_supported = self.enhanced_supported;
            if self.options.clipboard_policy == "allow" {
                consent.state.allowed = true;
            }
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| consent.state.allowed && !consent.state.sent)
        {
            self.begin_consent(out)?;
        }
        Ok(())
    }
    fn paste_reply(&mut self, response: &[u8]) -> Result<()> {
        let listing = self.paste_offer.is_some();
        match paste_offer_packet(&mut self.paste_offer, response) {
            Ok(Some(consent)) => {
                let (pane, view) = self
                    .paste_target
                    .take()
                    .context("enhanced paste listing lost its origin")?;
                self.consent = Some(DeviceConsent {
                    pane,
                    view,
                    state: consent,
                    reply: None,
                    reservation: None,
                });
            }
            Ok(None) if !listing && self.paste_offer.is_some() => {
                self.paste_target = self.active();
                if self.paste_target.is_none() {
                    self.paste_offer = None;
                    self.state.status =
                        "Enhanced paste has no focused pane; no input was dispatched".into();
                }
            }
            Ok(None) if listing && self.paste_offer.is_none() => {
                let (pane, view) = self
                    .paste_target
                    .take()
                    .context("completed enhanced paste listing lost its origin")?;
                // Empty clipboard is a successful no-op, not a broken input
                // stream. Release the already accepted suffix in original order.
                loop {
                    let operation = self.deferred.borrow_mut().pop_front();
                    let Some(operation) = operation else {
                        break;
                    };
                    if let Err(error) = self.dispatch_reserved(operation) {
                        return Err(self.fail_paste(&pane, &view, error, 1));
                    }
                }
                self.state.status =
                    "Clipboard has no paste content; following input released".into();
            }
            Ok(None) => {}
            Err(error) => {
                self.paste_offer = None;
                if let Some((pane, view)) = self.paste_target.take().or_else(|| self.active()) {
                    return Err(self.fail_paste(&pane, &view, error, 0));
                }
                self.state.status = format!("Enhanced paste offer rejected: {error:#}");
            }
        }
        Ok(())
    }
    fn apply_form(&mut self, size: Size) -> Result<()> {
        let Some(form) = self.state.form.take() else {
            return Ok(());
        };
        self.state.focused = form.restore;
        match form.kind {
            FormKind::ChooseWorkspace => {
                if let Some(workspace) = self.registry.workspaces.get(form.selection) {
                    self.switch(workspace.id.clone());
                }
            }
            FormKind::ChoosePane => {
                if let Some(pane) = self.registry.panes.get(form.selection) {
                    self.show_existing(pane.id.clone())?;
                }
            }
            FormKind::CreateWorkspace => {
                if self.options.read_only {
                    bail!("read-only client cannot create workspaces");
                }
                self.manage(
                    management::Command::Create {
                        name: form.value,
                        spec: PaneSpawnSpec {
                            argv: self.options.shell.clone(),
                            cwd: self.options.cwd.clone(),
                            home: self.options.home.clone(),
                            size,
                            title: None,
                            isolated_environment: self.options.isolated_environment,
                        },
                    },
                    None,
                )?;
            }
            FormKind::RenameWorkspace => {
                if self.options.read_only {
                    bail!("read-only client cannot rename workspaces");
                }
                let workspace = self
                    .state
                    .current(&self.registry)
                    .context("no workspace")?
                    .clone();
                self.operation(Operation::WorkspaceRename {
                    workspace_id: workspace.id,
                    expected_revision: workspace.revision,
                    name: form.value,
                })?;
            }
            FormKind::RenamePane => {
                if self.options.read_only {
                    bail!("read-only client cannot rename panes");
                }
                self.operation(Operation::PaneRename {
                    pane_id: self.state.focused.clone().context("no focused pane")?,
                    title: form.value,
                })?;
            }
            FormKind::TerminatePane => {
                if form.value == "yes" {
                    if self.options.read_only {
                        bail!("read-only client cannot terminate panes");
                    }
                    self.operation(Operation::PaneTerminate {
                        pane_id: self.state.focused.clone().context("no focused pane")?,
                    })?;
                }
            }
            FormKind::RemoveWorkspace => {
                if form.value == "yes" {
                    if self.options.read_only {
                        bail!("read-only client cannot remove workspaces");
                    }
                    let workspace = self
                        .state
                        .current(&self.registry)
                        .context("no workspace")?
                        .clone();
                    self.operation(Operation::WorkspaceRemove {
                        workspace_id: workspace.id,
                        expected_revision: workspace.revision,
                    })?;
                }
            }
        }
        Ok(())
    }
    fn directional(&mut self, key: &str, rects: &[(String, Rect)]) {
        let Some((_, current)) = rects
            .iter()
            .find(|(id, _)| Some(id) == self.state.focused.as_ref())
        else {
            return;
        };
        let cx = i32::from(current.x) * 2 + i32::from(current.width);
        let cy = i32::from(current.y) * 2 + i32::from(current.height);
        let target = rects
            .iter()
            .filter(|(id, _)| Some(id) != self.state.focused.as_ref())
            .filter_map(|(id, r)| {
                let dx = i32::from(r.x) * 2 + i32::from(r.width) - cx;
                let dy = i32::from(r.y) * 2 + i32::from(r.height) - cy;
                let valid = match key {
                    "ArrowLeft" => dx < 0,
                    "ArrowRight" => dx > 0,
                    "ArrowUp" => dy < 0,
                    "ArrowDown" => dy > 0,
                    _ => false,
                };
                valid.then_some((id, dx * dx + dy * dy))
            })
            .min_by_key(|(_, distance)| *distance)
            .map(|(id, _)| id.clone());
        if let Some(id) = target {
            self.focus(id);
        }
    }
    fn dispatch_key(
        &mut self,
        event: KeyEvent,
        out: &mut impl Write,
        size: Size,
        rects: &[(String, Rect)],
    ) -> Result<bool> {
        let local = self.state.prefix || self.state.form.is_some();
        match self.key(event, out, size, rects) {
            Err(error) if error.downcast_ref::<updates::InputRejected>().is_some() => {
                self.scope_input_result(Err(error))?;
                Ok(false)
            }
            Err(error) if local => {
                self.state.status = format!("{error:#}");
                Ok(false)
            }
            result => result,
        }
    }
    fn key(
        &mut self,
        event: KeyEvent,
        out: &mut impl Write,
        size: Size,
        rects: &[(String, Rect)],
    ) -> Result<bool> {
        match event.action {
            KeyAction::Release if self.state.suppressed.remove(&event.key) => return Ok(false),
            KeyAction::Repeat if self.state.suppressed.contains(&event.key) => return Ok(false),
            KeyAction::Press => {
                self.state.suppressed.remove(&event.key);
            }
            _ => {}
        }
        if event.action == KeyAction::Release {
            if let Some((pane, view)) = self.held.remove(&event.key)
                && !self.execution_completed(&pane)
                && self
                    .views
                    .get(&pane)
                    .is_some_and(|attached| attached.view == view)
            {
                self.enqueue(
                    &pane,
                    PaneRequest::Key {
                        view_id: view,
                        event,
                    },
                )?;
            }
            return Ok(false);
        }
        if self.state.prefix {
            // Kitty reports modifier key transitions separately from the key
            // carrying the command's text (e.g. ShiftLeft before '%').
            // Neither those transitions nor repeats consume the prefix.
            if event.action != KeyAction::Press {
                return Ok(false);
            }
            if matches!(
                event.key.as_str(),
                "ShiftLeft"
                    | "ShiftRight"
                    | "ControlLeft"
                    | "ControlRight"
                    | "AltLeft"
                    | "AltRight"
                    | "MetaLeft"
                    | "MetaRight"
                    | "SuperLeft"
                    | "SuperRight"
                    | "HyperLeft"
                    | "HyperRight"
                    | "CapsLock"
                    | "NumLock"
                    | "ScrollLock"
            ) {
                if !self.held.contains_key(&event.key) {
                    self.state.suppressed.insert(event.key);
                }
                return Ok(false);
            }
        }
        if event.key == "BracketRight"
            && event.modifiers & 2 != 0
            && event.action != KeyAction::Release
        {
            return Ok(true);
        }
        if let Some(consent) = self
            .consent
            .as_mut()
            .filter(|c| !c.state.sent && !c.state.allowed)
        {
            if event.action != KeyAction::Release {
                self.state.suppressed.insert(event.key.clone());
                if event.modifiers & 6 == 0 {
                    if event.text.eq_ignore_ascii_case("y") {
                        consent.state.allowed = true;
                        self.begin_consent(out)?;
                    } else if event.text.eq_ignore_ascii_case("n") {
                        self.finish_consent(ClipboardResponse::denied())?;
                    }
                }
            }
            return Ok(false);
        }
        if self.state.form.is_some() {
            if event.action == KeyAction::Release {
                return Ok(false);
            }
            self.state.suppressed.insert(event.key.clone());
            match event.key.as_str() {
                "Escape" => {
                    self.state.cancel();
                    self.state.reconcile(&self.registry);
                }
                "Enter" => self.apply_form(size)?,
                "Backspace" => {
                    self.state.form.as_mut().unwrap().value.pop();
                }
                "KeyU" if event.modifiers & 2 != 0 => {
                    self.state.form.as_mut().unwrap().value.clear()
                }
                "ArrowDown" | "ArrowUp" => {
                    let form = self.state.form.as_mut().unwrap();
                    let n = match form.kind {
                        FormKind::ChooseWorkspace => self.registry.workspaces.len(),
                        FormKind::ChoosePane => self.registry.panes.len(),
                        _ => 0,
                    };
                    if n > 0 {
                        form.selection = if event.key == "ArrowDown" {
                            (form.selection + 1) % n
                        } else {
                            (form.selection + n - 1) % n
                        };
                    }
                }
                _ => {
                    if event.modifiers & 6 == 0 {
                        let form = self.state.form.as_mut().unwrap();
                        if !matches!(form.kind, FormKind::ChooseWorkspace | FormKind::ChoosePane)
                            && form.value.len() + event.text.len() <= 1024
                        {
                            form.value.push_str(&safe_text(&event.text));
                        }
                    }
                }
            }
            return Ok(false);
        }
        if event.action == KeyAction::Press && event.key == "KeyB" && event.modifiers & 2 != 0 {
            self.state.suppressed.insert(event.key);
            if self.state.prefix {
                self.state.prefix = false;
                if !self.options.read_only
                    && let Some((pane, view)) = self.active()
                {
                    for action in [KeyAction::Press, KeyAction::Release] {
                        self.enqueue(
                            &pane,
                            PaneRequest::Key {
                                view_id: view.clone(),
                                event: KeyEvent {
                                    key: "KeyB".into(),
                                    modifiers: 2,
                                    text: if action == KeyAction::Press {
                                        "b".into()
                                    } else {
                                        String::new()
                                    },
                                    action,
                                    unshifted_codepoint: 98,
                                },
                            },
                        )?;
                    }
                }
            } else {
                self.state.prefix = true;
            }
            return Ok(false);
        }
        if self.state.prefix {
            if event.action != KeyAction::Press {
                return Ok(false);
            }
            self.state.prefix = false;
            self.state.suppressed.insert(event.key.clone());
            self.state.status.clear();
            match (event.text.as_str(), event.key.as_str()) {
                ("d", _) => return Ok(true),
                ("c", _) => self.state.open(FormKind::CreateWorkspace, String::new()),
                ("r", _) => {
                    if let Some(workspace) = self.state.current(&self.registry) {
                        self.state
                            .open(FormKind::RenameWorkspace, workspace.name.clone());
                    }
                }
                ("p", _) => {
                    let title = self
                        .state
                        .focused
                        .as_ref()
                        .and_then(|id| self.registry.panes.iter().find(|p| p.id == *id))
                        .map(|p| p.title.clone())
                        .unwrap_or_default();
                    self.state.open(FormKind::RenamePane, title);
                }
                ("!", _) => self.state.open(FormKind::TerminatePane, String::new()),
                ("X", _) => self.state.open(FormKind::RemoveWorkspace, String::new()),
                ("w", _) => self.state.open(FormKind::ChooseWorkspace, String::new()),
                ("o", _) => self.state.open(FormKind::ChoosePane, String::new()),
                ("n", _) | ("%", _) => self.spawn(size, Axis::Horizontal)?,
                ("\"", _) => self.spawn(size, Axis::Vertical)?,
                ("x", _) => {
                    if self.options.read_only {
                        bail!("read-only client cannot change layouts");
                    }
                    let workspace = self
                        .state
                        .current(&self.registry)
                        .context("no workspace")?
                        .clone();
                    let target = self.state.focused.as_deref().context("no focused pane")?;
                    let layout = remove_leaf(workspace.layout, target);
                    self.operation(Operation::LayoutUpdate {
                        workspace_id: workspace.id,
                        expected_revision: workspace.revision,
                        layout,
                    })?;
                    self.state.status =
                        "Layout removal pending; process survives. Ctrl-b o or click ◌ to show it."
                            .into();
                }
                ("[", _) => {
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    self.state.cycle_workspace(&self.registry, -1);
                    self.select_current_lease();
                }
                ("]", _) => {
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    self.state.cycle_workspace(&self.registry, 1);
                    self.select_current_lease();
                }
                (_, "Tab") => {
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    self.state.cycle_pane(&self.registry);
                    self.select_current_lease();
                }
                (_, key @ ("ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown")) => {
                    self.directional(key, rects)
                }
                (_, "Escape") => {}
                _ => self.state.status = "Unknown Ctrl-b command; press Ctrl-b for bindings".into(),
            };
            return Ok(false);
        }
        if !self.options.read_only {
            self.notify_focus()?;
            let target = if event.action == KeyAction::Repeat {
                self.held.get(&event.key).cloned().or_else(|| self.active())
            } else {
                self.active()
            };
            if let Some((pane, view)) = target {
                let held = (event.action == KeyAction::Press)
                    .then(|| (event.key.clone(), (pane.clone(), view.clone())));
                self.enqueue(
                    &pane,
                    PaneRequest::Key {
                        view_id: view,
                        event,
                    },
                )?;
                if let Some((key, target)) = held {
                    self.held.insert(key, target);
                }
            } else if let Some(pane) = &self.state.focused {
                self.state.status = self
                    .view_failures
                    .get(pane)
                    .cloned()
                    .or_else(|| {
                        self.input_failures.get(pane).and_then(|failures| {
                            failures
                                .iter()
                                .flatten()
                                .find(|failure| failure.stopped)
                                .map(ToString::to_string)
                        })
                    })
                    .unwrap_or_else(|| {
                        format!("pane {pane} view is attaching or retiring; input not dispatched")
                    });
            }
        }
        Ok(false)
    }
    fn mouse(
        &mut self,
        mouse: termwiz::input::MouseEvent,
        size: Size,
        rects: &[(String, Rect)],
        workspace_hits: &[(SidebarTarget, u16)],
        hits: &HitMap,
    ) -> Result<()> {
        let button = if mouse.mouse_buttons.contains(MouseButtons::VERT_WHEEL) {
            if mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            }
        } else if mouse.mouse_buttons.contains(MouseButtons::HORZ_WHEEL) {
            if mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
                MouseButton::WheelLeft
            } else {
                MouseButton::WheelRight
            }
        } else if mouse.mouse_buttons.contains(MouseButtons::LEFT) {
            MouseButton::Left
        } else if mouse.mouse_buttons.contains(MouseButtons::MIDDLE) {
            MouseButton::Middle
        } else if mouse.mouse_buttons.contains(MouseButtons::RIGHT) {
            MouseButton::Right
        } else {
            MouseButton::None
        };
        let wheel = matches!(
            button,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        );
        let action = if wheel {
            MouseAction::Press
        } else if button == MouseButton::None && self.last_button != MouseButton::None {
            MouseAction::Release
        } else if button == self.last_button {
            MouseAction::Motion
        } else {
            MouseAction::Press
        };
        let event_button = if action == MouseAction::Release {
            self.last_button
        } else {
            button
        };
        let captured = self.pointer_capture.clone();
        if !wheel {
            self.last_button = button;
            if action == MouseAction::Release {
                self.pointer_capture = None;
            } else if action == MouseAction::Press {
                self.pointer_capture = Some(PointerCapture::Ui);
            }
        }
        let origin = if wheel {
            None
        } else {
            match &captured {
                Some(PointerCapture::Pane(origin)) => Some(origin.as_ref()),
                _ => None,
            }
        };
        if matches!(&captured, Some(PointerCapture::Ui)) && !wheel {
            return Ok(());
        }
        let modal = self.state.form.is_some()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent);
        // Complete an already accepted native gesture even if a modal acquired
        // focus afterward; never start a native gesture through a modal.
        if modal && !(origin.is_some() && action == MouseAction::Release) {
            return Ok(());
        }
        if origin.is_none()
            && hits
                .hit_test(mouse.x, mouse.y)
                .is_none_or(|id| id == "ui:modal")
        {
            return Ok(());
        }
        let (sidebar, _) = screen_geometry(size);
        if origin.is_none() && mouse.x < sidebar.width {
            if button == MouseButton::Left
                && action == MouseAction::Press
                && let Some((target, _)) = workspace_hits.iter().find(|(_, row)| *row == mouse.y)
            {
                match target {
                    SidebarTarget::Workspace(id) => self.switch(id.clone()),
                    SidebarTarget::Pane(id) => self.show_existing(id.clone())?,
                }
            }
            return Ok(());
        }
        let target = if let Some(origin) = origin {
            rects.iter().find(|(pane, _)| *pane == origin.pane)
        } else {
            rects
                .iter()
                .find(|(_, rect)| rect.contains(mouse.x, mouse.y))
        };
        let Some((pane, rect)) = target else {
            return Ok(());
        };
        if origin.is_none() && button == MouseButton::Left && action == MouseAction::Press {
            self.focus(pane.clone());
        }
        self.notify_focus()?;
        if !(self.input_enabled(pane) || origin.is_some() && action == MouseAction::Release) {
            self.state.status = self.placeholder(pane);
            return Ok(());
        }
        let rect = content(*rect);
        if rect.width == 0
            || rect.height == 0
            || self.options.read_only
            || (origin.is_none() && !rect.contains(mouse.x, mouse.y))
            || !self
                .registry
                .panes
                .iter()
                .any(|info| info.id == *pane && info.status == PaneStatus::Running)
        {
            return Ok(());
        }
        let Some(attached) = self.views.get(pane) else {
            return Ok(());
        };
        if origin.is_some_and(|origin| origin.view != attached.view) {
            return Ok(());
        }
        let Some(frame) = attached.frame.as_ref() else {
            return Ok(());
        };
        // A pinned or asynchronously resizing native pane can be smaller than
        // its client rectangle. Complete captured gestures at the last actual
        // native cell rather than losing their release in the letterbox.
        let cols = if origin.is_some() {
            rect.width.min(frame.size.cols)
        } else {
            rect.width
        };
        let rows = if origin.is_some() {
            rect.height.min(frame.size.rows)
        } else {
            rect.height
        };
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        let col = mouse.x.saturating_sub(rect.x).min(cols - 1);
        let row = mouse.y.saturating_sub(rect.y).min(rows - 1);
        let Some((x, y)) = mouse_position(col, row, rect.size(size), frame.size) else {
            return Ok(());
        };
        let view = &attached.view;
        let mut modifiers = 0;
        for (flag, bit) in [
            (Modifiers::SHIFT, 1),
            (Modifiers::CTRL, 2),
            (Modifiers::ALT, 4),
            (Modifiers::SUPER, 8),
        ] {
            if mouse.modifiers.contains(flag) {
                modifiers |= bit;
            }
        }
        let event = MouseEvent {
            x,
            y,
            button: event_button,
            action,
            modifiers,
        };
        let deferred = self
            .barrier_target()
            .is_some_and(|(target_pane, target_view)| target_pane == pane && target_view == view);
        if let Err(error) = self.enqueue(
            pane,
            PaneRequest::Mouse {
                view_id: view.clone(),
                event,
            },
        ) {
            if origin.is_some() && action == MouseAction::Release {
                self.escrow_pointer_release(pane, view, event);
            }
            return Err(error);
        }
        let pointer = if deferred {
            None
        } else {
            self.pointer_origin(pane, view, event)
        };
        if !wheel && action == MouseAction::Press {
            let origin = pointer.clone().unwrap_or_else(|| {
                Arc::new(PointerOrigin {
                    pane: pane.clone(),
                    view: view.clone(),
                })
            });
            self.pointer_capture = Some(PointerCapture::Pane(origin));
        }
        if let Some(origin) = pointer {
            self.record_pointer(origin, event);
        }
        Ok(())
    }
    fn reply(
        &mut self,
        response: &[u8],
        out: &mut impl Write,
        terminal: &mut TerminalGuard,
    ) -> Result<()> {
        if response.starts_with(b"G") {
            if let Some(status) = response.splitn(2, |b| *b == b';').nth(1)
                && status != b"OK"
            {
                bail!(
                    "physical graphics projection rejected: {}",
                    safe_text(&String::from_utf8_lossy(status))
                );
            }
            return Ok(());
        }
        if response.starts_with(b"input-error;") {
            bail!(
                "bounded physical input rejected: {}",
                String::from_utf8_lossy(response)
            );
        }
        if let Some(response) = response.strip_prefix(b"clipboard-cap;") {
            if let Some(status) = std::str::from_utf8(response)?
                .strip_prefix("?5522;")
                .and_then(|s| s.strip_suffix('$'))
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|n| *n <= 4)
            {
                let supported = (1..=4).contains(&status);
                self.enhanced_supported = Some(supported);
                if supported && terminal.clipboard_mode.is_none() {
                    terminal.clipboard_mode = Some(matches!(status, 1 | 3));
                    if !self.options.read_only && self.options.clipboard_policy != "deny" {
                        write!(out, "\x1b[?5522h")?;
                        out.flush()?;
                    }
                }
            }
            return Ok(());
        }
        if response.starts_with(b"keyboard;") {
            if !terminal.kitty_keyboard {
                write!(out, "\x1b[>31u")?;
                out.flush()?;
                terminal.kitty_keyboard = true;
            }
            return Ok(());
        }
        if response.starts_with(b"focus;") {
            self.host_focused = response == b"focus;in";
            if self.host_focused
                && let Some((pane, _)) = self.active()
            {
                self.lease_pending.insert(pane);
            }
            self.notify_focus()?;
            return Ok(());
        }
        self.colors.feed(response);
        if self.consent.is_none()
            && self.state.form.is_none()
            && !self.options.read_only
            && self.options.clipboard_policy != "deny"
        {
            self.paste_reply(response)?;
        }
        if let Some(consent) = self.consent.as_mut().filter(|c| c.state.sent) {
            match clipboard_packet(&mut consent.state, response) {
                Ok(Some(answer)) => self.finish_consent(answer)?,
                Ok(None) => {}
                Err(error) => {
                    if consent.state.paste {
                        let consent = self.consent.take().unwrap();
                        return Err(self.fail_paste(&consent.pane, &consent.view, error, 0));
                    }
                    self.finish_consent(ClipboardResponse::denied())?;
                }
            }
        }
        Ok(())
    }
}

// Runs exclusively on the bounded background pool. Attach starts ineligible:
// a stale or slow initial poll must never acquire hidden size/device authority.
fn attach(socket: &Path, pane: &str, mut spec: ViewSpec) -> Result<Attached> {
    spec.visible = false;
    spec.interactive = false;
    spec.clipboard = false;
    let payload = transport::checked(
        socket,
        Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Attach { spec: spec.clone() },
        },
    )
    .map_err(|error| {
        let ownership = if transport::outcome_unknown(&error) {
            AttachOwnership::Unconfirmed
        } else {
            AttachOwnership::NotAdmitted
        };
        error.context(ownership)
    })?;
    let result: AttachResult = serde_json::from_value(payload).map_err(|error| {
        transport::mark_outcome_unknown(error.into()).context(AttachOwnership::Unconfirmed)
    })?;
    let view = result.view_id;
    let pump = match updates::pane(socket, pane, &view) {
        Ok(pump) => pump,
        Err(error) => return Err(detach_failed_attach(socket, pane, &view, error)),
    };
    let Snapshot { metadata, scene } = match pump.initial() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let error = detach_failed_attach(socket, pane, &view, error);
            drop(pump);
            return Err(error);
        }
    };
    let clipboard_id = metadata
        .clipboard
        .as_ref()
        .map(|pending| pending.request_id.clone());
    let mut attached = Attached {
        view,
        spec,
        pump,
        metadata,
        clipboard_id,
        frame: None,
        revision: 0,
        images: HashMap::new(),
        seeded: 0,
        retirement_requested: false,
        retirement_policy: RetirementPolicy::Visible,
        retirement_focus_out: false,
        retirement_error: None,
    };
    if let Some(scene) = scene
        && let Err(error) = attached.scene(scene)
    {
        let error = detach_failed_attach(socket, pane, &attached.view, error);
        drop(attached);
        return Err(error);
    }
    Ok(attached)
}
fn detach_failed_attach(
    socket: &Path,
    pane: &str,
    view: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match transport::checked(
        socket,
        Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Detach {
                view_id: view.into(),
            },
        },
    ) {
        Ok(_) => error
            .context(format!(
                "pane {pane} admitted view {view} was detached after initial snapshot failure"
            ))
            .context(AttachOwnership::Detached),
        Err(cleanup) => {
            let unknown = transport::outcome_unknown(&error);
            let cleanup = cleanup.context(format!("pane {pane} admitted view {view} initial snapshot failed: {error:#}; detach was not confirmed"));
            let cleanup = if unknown {
                transport::mark_outcome_unknown(cleanup)
            } else {
                cleanup
            };
            cleanup.context(AttachOwnership::Unconfirmed)
        }
    }
}

pub fn run(socket: &Path, options: Options) -> Result<()> {
    if !matches!(options.clipboard_policy.as_str(), "ask" | "allow" | "deny") {
        bail!("clipboard must be ask, allow or deny");
    }
    let registry: RegistryResult =
        serde_json::from_value(transport::checked(socket, Operation::Inspect)?)?;
    let state = Controller::new(&registry.registry);
    let input = InputPump::new(socket)?;
    let mut registry_pump = updates::registry(socket, registry.registry.revision)?;
    let mut app = App {
        socket,
        options,
        registry: registry.registry,
        state,
        views: HashMap::new(),
        retiring: Vec::new(),
        retiring_candidates: Vec::new(),
        jobs: jobs::Jobs::new(8)?,
        management: HashMap::new(),
        selection_epoch: 0,
        attach_slots: 0,
        retirement_budget: None,
        responses: Vec::new(),
        unconfirmed_views: 0,
        held: HashMap::new(),
        input,
        input_failures: HashMap::new(),
        view_failures: HashMap::new(),
        consent: None,
        answered: HashMap::new(),
        deferred: RefCell::new(VecDeque::new()),
        paste_offer: None,
        paste_target: None,
        enhanced_supported: None,
        colors: ColorProbe::default(),
        last_button: MouseButton::None,
        pointer_capture: None,
        pressed_pointers: RefCell::new(Vec::new()),
        notified_focus: None,
        focus_out_pending: HashSet::new(),
        lease_pending: HashSet::new(),
        host_focused: true,
    };
    let result = (|| -> Result<()> {
        let mut terminal = TerminalGuard::enter()?;
        let mut renderer = Renderer::default();
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::with_capacity(256 * 1024, stdout.lock());
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let mut last_input = Instant::now();
        let initial_size = dimensions()?;
        let mut chrome_surface = UiSurface::new(initial_size);
        let mut popup_surface = UiSurface::new(initial_size);
        let mut placeholders = HashMap::<String, Frame>::new();
        let (resize_read, resize_write) = std::os::unix::net::UnixStream::pair()?;
        resize_read.set_nonblocking(true)?;
        resize_write.set_nonblocking(true)?;
        struct ResizeSignal(signal_hook::SigId);
        impl Drop for ResizeSignal {
            fn drop(&mut self) {
                signal_hook::low_level::unregister(self.0);
            }
        }
        let _resize = ResizeSignal(signal_hook::low_level::pipe::register(
            signal_hook::consts::SIGWINCH,
            resize_write,
        )?);
        let mut resize_read = resize_read;
        loop {
            for failure in app.input.check()? {
                app.record_input_failure(failure);
            }
            app.reap_retiring()?;
            app.reap_responses();
            let pointer_release = app.flush_pointer_releases();
            app.scope_input_result(pointer_release)?;
            app.complete_background()?;
            if let Some(registry) = registry_pump.latest()?
                && registry.revision >= app.registry.revision
            {
                app.registry = registry;
                app.state.reconcile(&app.registry);
            }
            let size = dimensions()?;
            let rects = app.rects(size);
            app.sync_views(size, &rects)?;
            app.poll_views();
            let color_revision = app.colors.revision;
            if color_revision > 0
                && let (Some(foreground), Some(background)) =
                    (app.colors.foreground, app.colors.background)
            {
                let palette = if app.colors.palette.len() == 256 {
                    Some((0..256).map(|i| app.colors.palette[&i]).collect::<Vec<_>>())
                } else {
                    None
                };
                let colors = app
                    .views
                    .iter()
                    .filter(|(_, view)| {
                        view.seeded != color_revision
                            && view
                                .metadata
                                .size_owner
                                .as_ref()
                                .is_some_and(|owner| owner.view_id == view.view)
                    })
                    .map(|(pane, view)| (pane.clone(), view.view.clone()))
                    .collect::<Vec<_>>();
                for (pane, view) in colors {
                    if app.queue_control(
                        &pane,
                        PaneRequest::Colors {
                            view_id: view,
                            foreground,
                            background,
                            cursor: app.colors.cursor,
                            palette: palette.clone(),
                        },
                    )? {
                        app.views.get_mut(&pane).unwrap().seeded = color_revision;
                    }
                }
            }
            let clipboard = app.clipboard(&mut out);
            app.scope_input_result(clipboard)?;
            if app
                .paste_offer
                .as_ref()
                .is_some_and(|p| p.started.elapsed() > Duration::from_secs(4))
            {
                let result = app.abort_listing("enhanced paste listing timed out");
                app.scope_input_result(result)?;
            }
            if app
                .consent
                .as_ref()
                .is_some_and(|c| c.state.started.elapsed() > Duration::from_secs(4))
            {
                let result = app.finish_consent(ClipboardResponse::denied());
                app.scope_input_result(result)?;
            }
            let workspace_hits = chrome(
                &mut chrome_surface,
                size,
                &app.state,
                &app.registry,
                &rects,
                app.colors.truecolor,
                |pane| app.badge(pane),
            );
            native_colors(&mut chrome_surface.frame, &app.colors);
            placeholders.retain(|id, _| {
                rects.iter().any(|(pane, _)| pane == id)
                    && app.views.get(id).is_none_or(|view| view.frame.is_none())
            });
            for (id, rect) in &rects {
                let rect = content(*rect);
                if app.views.get(id).is_some_and(|view| view.frame.is_some())
                    || rect.height == 0
                    || rect.width == 0
                {
                    continue;
                }
                let local = rect.size(size);
                let frame = placeholders
                    .entry(id.clone())
                    .or_insert_with(|| blank(local));
                if frame.size != local {
                    *frame = blank(local);
                }
                native_colors(frame, &app.colors);
                text(frame, 0, 0, &app.placeholder(id), false);
            }
            let popup = if let Some(form) = &app.state.form {
                modal(&mut popup_surface, size, form, &app.registry);
                true
            } else if app
                .consent
                .as_ref()
                .is_some_and(|c| !c.state.allowed && !c.state.sent)
            {
                let x = 2.min(size.cols.saturating_sub(1));
                popup_surface.paint(
                    size,
                    vec![(
                        Rect {
                            x,
                            y: size.rows / 2,
                            width: size.cols - x,
                            height: 1,
                        },
                        "Clipboard request: y allow / n deny (pane-local)".into(),
                        true,
                    )],
                    None,
                );
                true
            } else {
                false
            };
            native_colors(&mut popup_surface.frame, &app.colors);
            let mut surfaces = vec![Surface {
                id: "ui:chrome",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: size.rows,
                },
                frame: &chrome_surface.frame,
                focused: false,
            }];
            for (id, rect) in &rects {
                let rect = content(*rect);
                if rect.height == 0 || rect.width == 0 {
                    continue;
                }
                if let Some(frame) = app
                    .views
                    .get(id)
                    .and_then(|v| v.frame.as_ref())
                    .or_else(|| placeholders.get(id))
                {
                    surfaces.push(Surface {
                        id,
                        rect,
                        frame,
                        focused: !popup
                            && app.state.focused.as_ref() == Some(id)
                            && app.input_enabled(id)
                            && app
                                .registry
                                .panes
                                .iter()
                                .any(|p| p.id == *id && p.status == PaneStatus::Running),
                    });
                }
            }
            if popup {
                surfaces.push(Surface {
                    id: "ui:modal",
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: size.cols,
                        height: size.rows,
                    },
                    frame: &popup_surface.frame,
                    focused: true,
                });
            }
            let scene = Scene { size, surfaces };
            let hits = scene.hit_map();
            renderer.render(&mut out, &scene, &app.colors, &mut terminal)?;
            let mut fds = vec![
                libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: registry_pump.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: resize_read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: app.input.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: app.jobs.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            for (pane, view) in &app.views {
                if view.retirement_requested || app.view_failures.contains_key(pane) {
                    continue;
                }
                fds.push(libc::pollfd {
                    fd: view.pump.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            let escape = Duration::from_millis(50).saturating_sub(last_input.elapsed());
            let timeout = if escape.is_zero() {
                1000
            } else {
                escape.as_millis().max(1) as i32
            };
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
            if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut resized = [0u8; 64];
            while matches!(resize_read.read(&mut resized),Ok(n)if n>0) {}
            let available = fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0;
            let mut bytes = [0u8; 8192];
            let parts = if available {
                let n = std::io::stdin().read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                last_input = Instant::now();
                demux.feed(&bytes[..n])
            } else if last_input.elapsed() > Duration::from_millis(50) {
                vec![InputPart::Keys(demux.flush_escape())]
            } else {
                Vec::new()
            };
            let mut incoming = VecDeque::from(decode_input(&mut parser, parts, available));
            while let Some(event) = incoming.pop_front() {
                match event {
                    ClientEvent::Reply(response) => {
                        if let Some(key) = response.strip_prefix(b"key;") {
                            if app.dispatch_key(kitty_key(key)?, &mut out, size, &rects)? {
                                return Ok(());
                            }
                        } else {
                            let result = app.reply(&response, &mut out, &mut terminal);
                            app.scope_input_result(result)?;
                        }
                    }
                    ClientEvent::Legacy(InputEvent::Key(key)) => {
                        if app.dispatch_key(key_event(key), &mut out, size, &rects)? {
                            return Ok(());
                        }
                    }
                    ClientEvent::Legacy(InputEvent::Paste(text)) => {
                        if let Some(form) = app.state.form.as_mut() {
                            if !matches!(
                                form.kind,
                                FormKind::ChooseWorkspace | FormKind::ChoosePane
                            ) && form.value.len() + text.len() <= 1024
                            {
                                form.value.push_str(&safe_text(&text));
                            }
                        } else if !app
                            .consent
                            .as_ref()
                            .is_some_and(|c| !c.state.allowed && !c.state.sent)
                            && !app.options.read_only
                            && let Some((pane, view)) = app.active()
                        {
                            let result = app.enqueue(
                                &pane,
                                PaneRequest::Paste {
                                    view_id: view,
                                    data: text.into_bytes(),
                                },
                            );
                            app.scope_input_result(result)?;
                        }
                    }
                    ClientEvent::Legacy(InputEvent::Mouse(mouse)) => {
                        if let Err(error) = app.mouse(mouse, size, &rects, &workspace_hits, &hits) {
                            if error.downcast_ref::<updates::InputRejected>().is_some() {
                                app.scope_input_result(Err(error))?;
                            } else {
                                app.state.status = format!("{error:#}");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    let mut cleanup = Ok(());
    while app.consent.is_some() {
        let finished = app.finish_consent(ClipboardResponse::denied());
        let waiting = app
            .consent
            .as_ref()
            .is_some_and(|consent| consent.reply.is_some())
            && finished
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<updates::InputRejected>())
                .is_some_and(|rejected| !rejected.0.stopped && !rejected.0.unknown_outcome);
        if waiting && !app.responses.is_empty() {
            accumulate_cleanup(&mut cleanup, app.drain_responses());
            continue;
        }
        accumulate_cleanup(&mut cleanup, finished);
        break;
    }
    accumulate_cleanup(&mut cleanup, app.drain_responses());
    if let Err(error) = app.abort_listing("client detached before enhanced paste listing completed")
        && cleanup.is_ok()
    {
        cleanup = Err(error);
    }
    let panes = app.views.keys().cloned().collect::<Vec<_>>();
    for pane in panes {
        let retirement = app.retire_view(&pane, false);
        accumulate_cleanup(&mut cleanup, retirement);
    }
    let fallback = std::mem::take(&mut app.views);
    // A full local policy reserve means this intent was never dispatched.
    // Shutdown may synchronously admit it before draining blocked native input.
    for (pane, attached) in &fallback {
        if matches!(attached.retirement_policy, RetirementPolicy::Visible) {
            let mut spec = attached.spec.clone();
            spec.visible = false;
            spec.interactive = false;
            spec.clipboard = false;
            let revoke = transport::checked(
                socket,
                Operation::Pane {
                    pane_id: pane.clone(),
                    request: PaneRequest::Update {
                        view_id: attached.view.clone(),
                        spec,
                    },
                },
            )
            .map(|_| ());
            accumulate_cleanup(&mut cleanup, revoke);
        }
    }
    // No UI work remains. Finish accepted metadata/attach jobs without losing
    // late admitted view IDs, including results made stale by detach/navigation.
    match app.jobs.finish() {
        Ok(completions) => {
            for completion in completions {
                match completion.result {
                    Ok(Background::Attached { pane, attached }) => {
                        accumulate_cleanup(
                            &mut cleanup,
                            detach_view(socket, &pane, &attached, false),
                        );
                    }
                    Ok(Background::Management(_)) => {}
                    Err(error) => {
                        accumulate_cleanup(&mut cleanup, Err(error.context(completion.key)))
                    }
                }
            }
        }
        Err(error) => accumulate_cleanup(&mut cleanup, Err(error)),
    }
    for candidate in std::mem::take(&mut app.retiring_candidates) {
        // Never displayed, so no user input was addressed to this view.
        accumulate_cleanup(
            &mut cleanup,
            detach_view(socket, &candidate.pane, &candidate.attached, false),
        );
    }
    let retiring = std::mem::take(&mut app.retiring);
    let pointers = std::mem::take(app.pressed_pointers.get_mut());
    let drained = app.input.finish();
    for mut pointer in pointers {
        if !pointer.admitted {
            continue;
        }
        let operation = pointer
            .release
            .take()
            .unwrap_or_else(|| pointer.release_operation());
        accumulate_cleanup(
            &mut cleanup,
            transport::checked(socket, operation).map(|_| ()),
        );
    }
    // Only views whose final retirement was not admitted reach this fallback.
    // All accepted lane operations have settled before their explicit detach.
    for (pane, attached) in fallback {
        accumulate_cleanup(
            &mut cleanup,
            detach_view(socket, &pane, &attached, attached.retirement_focus_out),
        );
    }
    let mut released = Ok(());
    for retired in retiring {
        let result = retired
            .receipt
            .recv()
            .context("pane input worker stopped before view retirement")
            .and_then(|result| result.map_err(anyhow::Error::msg));
        drop(retired.attached);
        accumulate_cleanup(&mut released, result);
    }
    result.and(cleanup).and(drained).and(released)
}

fn accumulate_cleanup(result: &mut Result<()>, next: Result<()>) {
    if let Err(error) = next {
        *result = match std::mem::replace(result, Ok(())) {
            Ok(()) => Err(error),
            Err(previous) => {
                Err(previous.context(format!("additional cleanup failure: {error:#}")))
            }
        };
    }
}
fn detach_view(socket: &Path, pane: &str, attached: &Attached, focus_out: bool) -> Result<()> {
    let mut result = Ok(());
    if focus_out {
        accumulate_cleanup(
            &mut result,
            transport::checked(
                socket,
                Operation::Pane {
                    pane_id: pane.into(),
                    request: PaneRequest::Focus {
                        view_id: attached.view.clone(),
                        focused: false,
                    },
                },
            )
            .map(|_| ()),
        );
    }
    accumulate_cleanup(
        &mut result,
        transport::checked(
            socket,
            Operation::Pane {
                pane_id: pane.into(),
                request: PaneRequest::Detach {
                    view_id: attached.view.clone(),
                },
            },
        )
        .map(|_| ()),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (App<'static>, updates::Capture) {
        let registry = RegistrySnapshot {
            revision: 0,
            workspaces: Vec::new(),
            panes: Vec::new(),
        };
        let state = Controller::new(&registry);
        let (input, received) = InputPump::capture();
        (
            App {
                socket: Path::new("/unused-ui-test-endpoint"),
                options: Options {
                    read_only: false,
                    clipboard_policy: "ask".into(),
                    shell: vec!["/bin/sh".into(), "-i".into()],
                    cwd: PathBuf::from("/fixture"),
                    home: PathBuf::from("/fixture/home"),
                    isolated_environment: true,
                },
                registry,
                state,
                views: HashMap::new(),
                retiring: Vec::new(),
                retiring_candidates: Vec::new(),
                jobs: jobs::Jobs::new(8).unwrap(),
                management: HashMap::new(),
                selection_epoch: 0,
                attach_slots: 0,
                retirement_budget: None,
                responses: Vec::new(),
                unconfirmed_views: 0,
                held: HashMap::new(),
                input,
                input_failures: HashMap::new(),
                view_failures: HashMap::new(),
                consent: None,
                answered: HashMap::new(),
                deferred: RefCell::new(VecDeque::new()),
                paste_offer: None,
                paste_target: None,
                enhanced_supported: None,
                colors: ColorProbe::default(),
                last_button: MouseButton::None,
                pointer_capture: None,
                pressed_pointers: RefCell::new(Vec::new()),
                notified_focus: None,
                focus_out_pending: HashSet::new(),
                lease_pending: HashSet::new(),
                host_focused: true,
            },
            received,
        )
    }
    fn consent(paste: bool) -> DeviceConsent {
        DeviceConsent {
            pane: "origin-pane".into(),
            view: "origin-view".into(),
            reply: None,
            reservation: None,
            state: Consent {
                id: "request".into(),
                request: ClipboardRequest {
                    operation: ClipboardOperation::Read {
                        mimes: vec!["text/plain".into()],
                        list: false,
                    },
                    location: ClipboardLocation::Standard,
                    name: String::new(),
                    granted: false,
                    can_remember: false,
                    paste_offer_id: None,
                },
                started: Instant::now(),
                sent: true,
                allowed: true,
                contents: Vec::new(),
                available: Vec::new(),
                enhanced_supported: Some(true),
                paste,
                password: None,
            },
        }
    }
    fn key(name: &str) -> KeyEvent {
        KeyEvent {
            key: name.into(),
            text: String::new(),
            modifiers: 0,
            action: KeyAction::Press,
            unshifted_codepoint: 0,
        }
    }
    fn size() -> Size {
        Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }
    #[test]
    fn slow_paste_blocks_only_origin_and_preserves_fifo_after_focus_changes() {
        let (mut app, received) = fixture();
        app.consent = Some(consent(true));
        app.state.focused = Some("other-pane".into());
        app.enqueue(
            "origin-pane",
            PaneRequest::Key {
                view_id: "origin-view".into(),
                event: key("Enter"),
            },
        )
        .unwrap();
        assert!(received.try_recv().is_err());
        app.enqueue(
            "other-pane",
            PaneRequest::Key {
                view_id: "other-view".into(),
                event: key("KeyA"),
            },
        )
        .unwrap();
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::Key{..}}if pane_id=="other-pane")
        );
        app.finish_consent(ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "text/plain".into(),
                data: b"paste-sentinel".to_vec(),
            }],
            available: Vec::new(),
            remember: false,
        })
        .unwrap();
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::PasteContents{view_id,contents,..}}if pane_id=="origin-pane"&&view_id=="origin-view"&&contents[0].data==b"paste-sentinel")
        );
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::Key{view_id,event}}if pane_id=="origin-pane"&&view_id=="origin-view"&&event.key=="Enter")
        );
    }
    #[test]
    fn failed_paste_never_releases_following_input() {
        let (mut app, received) = fixture();
        app.consent = Some(consent(true));
        app.enqueue(
            "origin-pane",
            PaneRequest::Key {
                view_id: "origin-view".into(),
                event: key("Enter"),
            },
        )
        .unwrap();
        assert!(app.finish_consent(ClipboardResponse::denied()).is_err());
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn clipboard_answer_stays_bound_to_origin_after_focus_changes() {
        let (mut app, received) = fixture();
        app.consent = Some(consent(false));
        app.state.focused = Some("other-pane".into());
        app.finish_consent(ClipboardResponse::denied()).unwrap();
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::Clipboard{view_id,request_id,..}}if pane_id=="origin-pane"&&view_id=="origin-view"&&request_id=="request")
        );
    }
    #[test]
    fn pending_capability_consumes_consent_key_release_and_repeat() {
        let (mut app, received) = fixture();
        let mut request = consent(false);
        request.state.sent = false;
        request.state.allowed = false;
        request.state.enhanced_supported = None;
        app.consent = Some(request);
        let mut press = key("KeyY");
        press.text = "y".into();
        let mut out = Vec::new();
        app.key(press.clone(), &mut out, size(), &[]).unwrap();
        assert!(app.consent.as_ref().unwrap().state.allowed);
        assert!(!app.consent.as_ref().unwrap().state.sent);
        app.consent = None;
        for action in [KeyAction::Repeat, KeyAction::Release] {
            let mut event = press.clone();
            event.action = action;
            app.key(event, &mut out, size(), &[]).unwrap();
        }
        assert!(received.try_recv().is_err());
        assert!(!app.state.suppressed.contains("KeyY"));
    }
    #[test]
    fn read_only_split_is_rejected_without_sending_mutation() {
        let (mut app, received) = fixture();
        app.options.read_only = true;
        app.state.prefix = true;
        let mut event = key("KeyN");
        event.text = "n".into();
        assert!(app.key(event, &mut Vec::new(), size(), &[]).is_err());
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn detach_never_sends_a_termination() {
        let (mut app, received) = fixture();
        let mut event = key("BracketRight");
        event.modifiers = 2;
        assert!(app.key(event, &mut Vec::new(), size(), &[]).unwrap());
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn read_only_is_independent_of_visible_interactive_size_eligibility() {
        let (mut app, _received) = fixture();
        app.options.read_only = true;
        app.options.clipboard_policy = "deny".into();
        let spec = app.spec(size());
        assert!(spec.visible && spec.interactive);
        assert!(!spec.input && !spec.clipboard);
    }
    fn attach_fixture(app: &mut App<'_>) {
        attach_named_fixture(app, "focused-pane", "focused-view");
    }
    fn attach_named_fixture(app: &mut App<'_>, pane: &str, view: &str) {
        app.registry.panes.push(PaneInfo {
            id: pane.into(),
            workspace_id: None,
            title: "test shell".into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: app.options.cwd.clone(),
            home: app.options.home.clone(),
            isolated_environment: true,
        });
        let metadata = PaneSnapshot {
            pane_id: pane.into(),
            pid: None,
            revision: 1,
            snapshot_revision: 1,
            frame: None,
            image_refs: Vec::new(),
            resource_protocol: 1,
            writer: None,
            lifecycle: "running".into(),
            exit_code: None,
            error: None,
            program_status: Vec::new(),
            events: Vec::new(),
            event_sequence: 0,
            size_owner: None,
            lease_generation: 0,
            views: Vec::new(),
            clipboard: None,
            clipboard_pending: false,
            capabilities: PaneCapabilities {
                sixel: false,
                normalized_frames: true,
                clipboard_timeout_ms: 5000,
                view_timeout_ms: 5000,
                poll_wait_max_ms: 1000,
                row_deltas: true,
                scene_encodings: vec!["compact_v1".into()],
            },
            row_delta: None,
            scene: None,
        };
        app.views.insert(
            pane.into(),
            Attached {
                view: view.into(),
                spec: app.spec(size()),
                pump: Pump::capture(),
                metadata,
                clipboard_id: None,
                frame: Some(blank(size())),
                revision: 1,
                images: HashMap::new(),
                seeded: 0,
                retirement_requested: false,
                retirement_policy: RetirementPolicy::Visible,
                retirement_focus_out: false,
                retirement_error: None,
            },
        );
        app.state.focused = Some(pane.into());
        app.notified_focus = Some((pane.into(), view.into()));
    }
    fn physical_keys(app: &mut App<'_>, bytes: &[u8]) -> Result<()> {
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        for event in decode_input(&mut parser, demux.feed(bytes), false) {
            let key = match event {
                ClientEvent::Reply(packet) if packet.starts_with(b"key;") => {
                    kitty_key(&packet[4..])?
                }
                ClientEvent::Legacy(InputEvent::Key(key)) => key_event(key),
                _ => panic!("expected physical keyboard event"),
            };
            assert!(!app.key(key, &mut Vec::new(), size(), &[])?);
        }
        Ok(())
    }
    #[test]
    fn kitty_modifier_transitions_and_repeats_preserve_prefix_until_percent_press() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        app.options.read_only = true;
        physical_keys(&mut app, b"\x1b[57442;5:1u\x1b[98;5:1u").unwrap();
        assert!(app.state.prefix);
        physical_keys(&mut app, b"\x1b[98;5:2u\x1b[98;5:3u\x1b[57442;1:3u").unwrap();
        assert!(app.state.prefix);
        physical_keys(
            &mut app,
            b"\x1b[57441;2:1u\x1b[57441;2:2u\x1b[97;2:2u\x1b[97;2:3u",
        )
        .unwrap();
        assert!(app.state.prefix);
        assert!(app.state.status.is_empty());
        let error = physical_keys(&mut app, b"\x1b[53:37;2:1;37u").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("read-only client cannot create panes")
        );
        assert!(!app.state.prefix);
        assert!(app.state.suppressed.contains("Digit5"));
        assert!(received.try_recv().is_err());
        app.options.read_only = false;
        physical_keys(
            &mut app,
            b"\x1b[53:37;2:2;37u\x1b[53:37;2:3u\x1b[57441;1:3u",
        )
        .unwrap();
        assert!(
            received.try_recv().is_err(),
            "UI command repeat/release leaked to pane"
        );
        physical_keys(&mut app, b"\x1b[97;1:1;97u").unwrap();
        assert!(matches!(received.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. }
        } if pane_id == "focused-pane" && event.key == "KeyA" && event.text == "a"));
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn kitty_double_prefix_queues_exactly_one_literal_control_b_lifecycle() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        physical_keys(
            &mut app,
            b"\x1b[98;5:1u\x1b[98;5:2u\x1b[98;5:3u\x1b[57442;1:3u",
        )
        .unwrap();
        assert!(app.state.prefix);
        assert!(received.try_recv().is_err());
        physical_keys(&mut app, b"\x1b[98;5:1u\x1b[98;5:2u\x1b[98;5:3u").unwrap();
        assert!(!app.state.prefix);
        for expected in [KeyAction::Press, KeyAction::Release] {
            assert!(matches!(received.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { view_id, event }
            } if pane_id == "focused-pane" && view_id == "focused-view"
                && event.key == "KeyB" && event.modifiers == 2 && event.action == expected));
        }
        assert!(
            received.try_recv().is_err(),
            "physical prefix repeat/release was forwarded twice"
        );
    }
    #[test]
    fn physical_sgr_sidebar_click_selects_the_rendered_workspace_row() {
        let (mut app, received) = fixture();
        app.registry.workspaces = vec![
            Workspace {
                id: "main".into(),
                name: "Main".into(),
                revision: 1,
                layout: Some(LayoutNode::Split {
                    axis: Axis::Horizontal,
                    ratio: 500,
                    first: Box::new(LayoutNode::Leaf {
                        pane_id: "alpha".into(),
                    }),
                    second: Box::new(LayoutNode::Leaf {
                        pane_id: "beta".into(),
                    }),
                }),
            },
            Workspace {
                id: "remote".into(),
                name: "Remote-style projection".into(),
                revision: 1,
                layout: None,
            },
        ];
        app.registry.panes = ["alpha", "beta"]
            .into_iter()
            .map(|id| PaneInfo {
                id: id.into(),
                workspace_id: Some("main".into()),
                title: id.into(),
                status: PaneStatus::Running,
                exit_code: None,
                cwd: app.options.cwd.clone(),
                home: app.options.home.clone(),
                isolated_environment: true,
            })
            .collect();
        app.state = Controller::new(&app.registry);
        let size = Size {
            cols: 100,
            rows: 36,
            cell_width: 9,
            cell_height: 18,
        };
        let rects = app.rects(size);
        let mut surface = UiSurface::new(size);
        let workspace_hits = chrome(
            &mut surface,
            size,
            &app.state,
            &app.registry,
            &rects,
            true,
            |_| None,
        );
        assert!(workspace_hits.iter().any(|(target, row)| *row == 5
            && matches!(target, SidebarTarget::Workspace(id) if id == "remote")));
        let row = surface.frame.rows[5]
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(row.contains("Remote-style"));
        let scene = Scene {
            size,
            surfaces: vec![Surface {
                id: "ui:chrome",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: size.rows,
                },
                frame: &surface.frame,
                focused: false,
            }],
        };
        let hits = scene.hit_map();
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        for event in decode_input(&mut parser, demux.feed(b"\x1b[<0;6;6M\x1b[<0;6;6m"), false) {
            let ClientEvent::Legacy(InputEvent::Mouse(mouse)) = event else {
                panic!("expected physical SGR mouse event");
            };
            app.mouse(mouse, size, &rects, &workspace_hits, &hits)
                .unwrap();
        }
        assert_eq!(app.state.workspace.as_deref(), Some("remote"));
        assert!(
            received.try_recv().is_err(),
            "sidebar input leaked to a pane"
        );
    }
    struct PointerFixture {
        size: Size,
        rects: Vec<(String, Rect)>,
        hits: HitMap,
    }
    impl PointerFixture {
        fn new(app: &mut App<'_>) -> Self {
            attach_fixture(app);
            attach_named_fixture(app, "beta", "beta-view");
            app.state.focused = Some("focused-pane".into());
            app.notified_focus = Some(("focused-pane".into(), "focused-view".into()));
            let size = Size {
                cols: 120,
                rows: 32,
                ..size()
            };
            let (_, body) = screen_geometry(size);
            let first = Rect {
                width: body.width / 2,
                ..body
            };
            let second = Rect {
                x: body.x + first.width,
                width: body.width - first.width,
                ..body
            };
            let rects: Vec<(String, Rect)> =
                vec![("focused-pane".into(), first), ("beta".into(), second)];
            let frame = blank(size);
            let mut surfaces = vec![Surface {
                id: "ui:chrome",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: size.rows,
                },
                frame: &frame,
                focused: false,
            }];
            for (pane, rect) in &rects {
                surfaces.push(Surface {
                    id: pane,
                    rect: content(*rect),
                    frame: &frame,
                    focused: false,
                });
            }
            let hits = Scene { size, surfaces }.hit_map();
            Self { size, rects, hits }
        }
        fn send(&self, app: &mut App<'_>, button: u8, x: u16, y: u16, release: bool) {
            self.try_send(app, button, x, y, release).unwrap();
        }
        fn try_send(
            &self,
            app: &mut App<'_>,
            button: u8,
            x: u16,
            y: u16,
            release: bool,
        ) -> Result<()> {
            let packet = format!(
                "\x1b[<{button};{};{}{}",
                x + 1,
                y + 1,
                if release { 'm' } else { 'M' }
            );
            let mut parser = InputParser::new();
            let mut demux = ReplyDemux::default();
            for event in decode_input(&mut parser, demux.feed(packet.as_bytes()), false) {
                let ClientEvent::Legacy(InputEvent::Mouse(mouse)) = event else {
                    panic!("expected decoded SGR mouse event");
                };
                app.mouse(mouse, self.size, &self.rects, &[], &self.hits)?;
            }
            Ok(())
        }
    }
    fn captured_mouse(capture: &updates::Capture) -> Vec<(String, String, MouseEvent)> {
        let mut events = Vec::new();
        while let Ok(operation) = capture.try_recv() {
            if let Operation::Pane {
                pane_id,
                request: PaneRequest::Mouse { view_id, event },
            } = operation
            {
                events.push((pane_id, view_id, event));
            }
        }
        events
    }
    #[test]
    fn physical_drag_and_release_stay_in_original_pane_and_clip_to_its_edge() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 4, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 36, beta.x + 2, beta.y + 2, false);
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|(pane, view, _)| pane == "focused-pane" && view == "focused-view")
        );
        assert_eq!(events[0].2.action, MouseAction::Press);
        assert_eq!(events[1].2.action, MouseAction::Motion);
        let edge = mouse_position(alpha.width - 1, 2, alpha.size(pointer.size), size()).unwrap();
        assert_eq!(events[1].2.x.to_bits(), edge.0.to_bits());
        assert_eq!(events[1].2.y.to_bits(), edge.1.to_bits());
        assert_eq!(app.state.focused.as_deref(), Some("focused-pane"));
        pointer.send(&mut app, 4, beta.x + 2, beta.y + 2, true);
        let released = captured_mouse(&capture);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, "focused-pane");
        assert_eq!(released[0].2.action, MouseAction::Release);
        assert_eq!(released[0].2.modifiers, 1);
        assert_eq!(released[0].2.x.to_bits(), edge.0.to_bits());
        assert!(app.pressed_pointers.borrow().is_empty());
        assert!(app.pointer_capture.is_none());
    }
    #[test]
    fn captured_gesture_clips_to_actual_frame_when_owner_size_is_smaller_than_projection() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        let actual = Size {
            cols: 10,
            rows: 6,
            ..size()
        };
        app.views.get_mut("focused-pane").unwrap().frame = Some(blank(actual));
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 32, beta.x + 2, beta.y + 2, false);
        pointer.send(&mut app, 0, beta.x + 2, beta.y + 2, true);
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 3);
        let edge = mouse_position(actual.cols - 1, 2, alpha.size(pointer.size), actual).unwrap();
        assert!(
            events
                .iter()
                .all(|(pane, view, _)| pane == "focused-pane" && view == "focused-view")
        );
        assert_eq!(events[1].2.action, MouseAction::Motion);
        assert_eq!(events[2].2.action, MouseAction::Release);
        assert_eq!(events[1].2.x.to_bits(), edge.0.to_bits());
        assert_eq!(events[2].2.x.to_bits(), edge.0.to_bits());
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn full_normal_budget_retains_physical_release_until_local_admission() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        let mut held = (0..2048)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            pointer
                .try_send(&mut app, 0, beta.x + 2, beta.y + 2, true)
                .is_err()
        );
        assert!(app.pointer_capture.is_none());
        assert!(app.pressed_pointers.borrow()[0].release.is_some());
        assert!(app.flush_pointer_releases().is_err());
        assert!(capture.try_recv().is_err());
        held.pop();
        app.flush_pointer_releases().unwrap();
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "focused-pane");
        assert_eq!(events[0].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn failed_release_admission_waits_for_accepted_deferred_press_before_completing_it() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        let held = (0..2047)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            pointer
                .try_send(&mut app, 0, alpha.x + 3, alpha.y + 2, true)
                .is_err()
        );
        drop(held);
        app.flush_pointer_releases().unwrap();
        assert!(
            capture.try_recv().is_err(),
            "release preceded its held press"
        );
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        assert_eq!(captured_mouse(&capture)[0].2.action, MouseAction::Press);
        app.flush_pointer_releases().unwrap();
        let released = captured_mouse(&capture);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn native_prefix_release_never_overtakes_accepted_paste_barrier_suffix() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        for _ in 0..64 {
            pointer.send(&mut app, 32, alpha.x + 3, alpha.y + 2, false);
        }
        assert!(
            pointer
                .try_send(&mut app, 0, alpha.x + 3, alpha.y + 2, true)
                .is_err()
        );
        app.flush_pointer_releases().unwrap();
        assert!(
            capture.try_recv().is_err(),
            "release overtook accepted suffix"
        );
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        app.flush_pointer_releases().unwrap();
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 65);
        assert!(
            events[..64]
                .iter()
                .all(|(_, _, event)| event.action == MouseAction::Motion)
        );
        assert_eq!(events[64].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn completed_shell_key_release_is_not_forwarded_to_dead_or_new_execution() {
        for snapshot_first in [false, true] {
            let (mut app, capture) = fixture();
            let _pointer = PointerFixture::new(&mut app);
            let mut exit = key("KeyD");
            exit.modifiers = 2;
            app.key(exit.clone(), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { event, .. },
            } if pane_id == "focused-pane" && event.action == KeyAction::Press));
            if snapshot_first {
                app.views
                    .get_mut("focused-pane")
                    .unwrap()
                    .metadata
                    .lifecycle = "exited".into();
            } else {
                app.registry.panes[0].status = PaneStatus::Exited;
            }
            app.state.focused = Some("beta".into());
            exit.action = KeyAction::Release;
            app.key(exit, &mut Vec::new(), size(), &[]).unwrap();
            assert!(capture.try_recv().is_err());
            assert!(!app.held.contains_key("KeyD"));
            assert!(app.state.status.is_empty());
            assert!(app.input_failures.is_empty());
            app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Focus { focused: true, .. },
            } if pane_id == "beta"));
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { event, .. },
            } if pane_id == "beta" && event.action == KeyAction::Press));
            assert!(capture.try_recv().is_err());
        }
    }
    #[test]
    fn completed_shell_retirement_discards_pending_focus_but_detaches_and_keeps_peer_input() {
        for snapshot_first in [false, true] {
            let (mut app, capture) = fixture();
            let _pointer = PointerFixture::new(&mut app);
            // Completion can arrive after retirement captured a running focus.
            app.retire_view("focused-pane", true).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                request: PaneRequest::Update { spec, .. }, ..
            } if !spec.visible && !spec.interactive && !spec.clipboard));
            if snapshot_first {
                app.views
                    .get_mut("focused-pane")
                    .unwrap()
                    .metadata
                    .lifecycle = "exited".into();
            } else {
                app.registry.panes[0].status = PaneStatus::Exited;
            }
            app.focus_out_pending
                .insert(("focused-pane".into(), "focused-view".into()));
            app.notify_focus().unwrap();
            assert!(capture.try_recv().is_err());
            app.retire_view("focused-pane", true).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Detach { view_id },
            } if pane_id == "focused-pane" && view_id == "focused-view"));
            app.reap_retiring().unwrap();
            assert!(app.retiring.is_empty());
            assert_eq!(app.unconfirmed_views, 0);
            assert!(app.state.status.is_empty());
            assert!(app.input_failures.is_empty());
            assert!(app.view_failures.is_empty());
            app.state.focused = Some("beta".into());
            app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Focus { focused: true, .. },
            } if pane_id == "beta"));
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { .. },
            } if pane_id == "beta"));
            assert!(capture.try_recv().is_err());
        }
    }
    #[test]
    fn retiring_pressed_view_queues_original_release_before_focus_and_detach() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 4, alpha.x + 2, alpha.y + 2, false);
        let press = captured_mouse(&capture).pop().unwrap().2;
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Mouse { view_id, event },
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == MouseAction::Release && event.button == press.button
            && event.modifiers == press.modifiers && event.x.to_bits() == press.x.to_bits()
            && event.y.to_bits() == press.y.to_bits()));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Update { spec, .. }, ..
        } if spec.input && !spec.visible && !spec.interactive && !spec.clipboard));
        app.notify_focus().unwrap();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Focus { focused: false, .. },
                ..
            }
        ));
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Detach { .. },
                ..
            }
        ));
        app.reap_retiring().unwrap();
        pointer.send(&mut app, 4, beta.x + 2, beta.y + 2, true);
        assert!(
            capture.try_recv().is_err(),
            "physical release leaked to newly visible pane"
        );
        assert!(app.retiring.is_empty());
        assert_eq!(app.registry.panes[0].status, PaneStatus::Running);
    }
    #[test]
    fn empty_paste_listing_releases_suffix_and_unrelated_replies_keep_origin() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        app.enqueue(
            "focused-pane",
            PaneRequest::Key {
                view_id: "focused-view".into(),
                event: key("KeyA"),
            },
        )
        .unwrap();
        app.paste_reply(b"10;rgb:ffff/ffff/ffff").unwrap();
        assert_eq!(app.paste_target.as_ref().unwrap().0, "focused-pane");
        assert!(capture.try_recv().is_err());
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        assert!(app.paste_offer.is_none() && app.paste_target.is_none());
        assert!(app.deferred.borrow().is_empty());
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.key == "KeyA"));
    }
    #[test]
    fn malformed_listing_drops_only_origin_suffix_and_never_invents_mouse_release() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, true);
        assert_eq!(app.deferred.borrow().len(), 2);
        assert!(app.pressed_pointers.borrow().is_empty());
        let error = app
            .paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<updates::InputRejected>()
                .unwrap()
                .0
                .not_dispatched,
            2
        );
        app.scope_input_result(Err(error)).unwrap();
        app.flush_pointer_releases().unwrap();
        assert!(capture.try_recv().is_err());
        assert!(
            app.enqueue(
                "focused-pane",
                PaneRequest::Key {
                    view_id: "focused-view".into(),
                    event: key("KeyB"),
                }
            )
            .is_err()
        );
        app.enqueue(
            "beta",
            PaneRequest::Key {
                view_id: "beta-view".into(),
                event: key("KeyB"),
            },
        )
        .unwrap();
        assert!(
            matches!(capture.try_recv().unwrap(), Operation::Pane { pane_id, .. } if pane_id == "beta")
        );
    }
    #[test]
    fn paste_failure_completes_queued_prefix_press_but_discards_deferred_release() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        pointer.send(&mut app, 0, alpha.x + 3, alpha.y + 2, true);
        let failed = app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;");
        app.scope_input_result(failed).unwrap();
        app.flush_pointer_releases().unwrap();
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "focused-pane");
        assert_eq!(events[0].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn accepted_device_response_settles_before_eligibility_revoke() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        let mut pending = consent(false);
        pending.pane = "focused-pane".into();
        pending.view = "focused-view".into();
        pending.reservation = Some(
            app.input
                .reserve_response(&pending.pane, &pending.view, &pending.state.id)
                .unwrap(),
        );
        app.consent = Some(pending);
        app.finish_consent(ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "application/octet-stream".into(),
                data: b"\0origin\xff".to_vec(),
            }],
            available: Vec::new(),
            remember: false,
        })
        .unwrap();
        app.retire_view("focused-pane", true).unwrap();
        assert!(app.views.contains_key("focused-pane"));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Clipboard { view_id, response, .. },
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && response.contents[0].data == b"\0origin\xff"));
        assert!(
            capture.try_recv().is_err(),
            "revocation overtook an accepted device response"
        );
        app.reap_responses();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Update { .. },
                ..
            }
        ));
        app.retire_view("focused-pane", true).unwrap();
        capture.try_recv().unwrap();
        capture.try_recv().unwrap();
        app.reap_retiring().unwrap();
        assert!(app.retiring.is_empty());
    }
    #[test]
    fn retirement_compound_rejects_without_partial_focus_and_retries_only_local_intent() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        let mut held = (0..2047)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Update { .. },
                ..
            }
        ));
        assert!(app.retire_view("focused-pane", true).is_err());
        assert!(app.views.contains_key("focused-pane"));
        assert!(
            capture.try_recv().is_err(),
            "partial focus escaped failed compound admission"
        );
        held.pop();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Focus { focused: false, .. },
                ..
            }
        ));
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Detach { .. },
                ..
            }
        ));
        app.reap_retiring().unwrap();
        assert!(app.retiring.is_empty());
    }
    fn wait_background(app: &mut App<'_>, key: &str) {
        while app.jobs.contains(key) {
            let mut fd = libc::pollfd {
                fd: app.jobs.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert!(
                unsafe { libc::poll(&mut fd, 1, 5000) } > 0,
                "background completion did not wake UI"
            );
            app.complete_background().unwrap();
        }
    }
    #[test]
    fn newer_selection_intent_and_registry_snapshot_win_out_of_order_completions() {
        let (mut app, _) = fixture();
        attach_fixture(&mut app);
        let old = Workspace {
            id: "old".into(),
            name: "Old".into(),
            revision: 1,
            layout: Some(LayoutNode::Leaf {
                pane_id: "focused-pane".into(),
            }),
        };
        let new = Workspace {
            id: "new".into(),
            name: "New".into(),
            ..old.clone()
        };
        let older = RegistrySnapshot {
            revision: 3,
            workspaces: vec![old.clone()],
            panes: app.registry.panes.clone(),
        };
        let newer = RegistrySnapshot {
            revision: 7,
            workspaces: vec![old, new],
            panes: app.registry.panes.clone(),
        };
        app.registry = older.clone();
        app.state = Controller::new(&app.registry);
        app.selection_epoch = 2;
        app.management.insert(
            "manage:old".into(),
            ManagementIntent {
                epoch: 1,
                selection: None,
            },
        );
        app.management.insert(
            "manage:new".into(),
            ManagementIntent {
                epoch: 2,
                selection: None,
            },
        );
        let (release, waiting) = mpsc::sync_channel(1);
        app.jobs
            .submit("manage:old".into(), move || {
                waiting.recv().unwrap();
                Ok(Background::Management(management::Completion {
                    registry: older,
                    selection: Some(management::Selection {
                        workspace: "old".into(),
                        pane: "focused-pane".into(),
                    }),
                }))
            })
            .unwrap();
        app.jobs
            .submit("manage:new".into(), move || {
                Ok(Background::Management(management::Completion {
                    registry: newer,
                    selection: Some(management::Selection {
                        workspace: "new".into(),
                        pane: "focused-pane".into(),
                    }),
                }))
            })
            .unwrap();
        wait_background(&mut app, "manage:new");
        assert_eq!(app.state.workspace.as_deref(), Some("new"));
        release.send(()).unwrap();
        wait_background(&mut app, "manage:old");
        assert_eq!(app.state.workspace.as_deref(), Some("new"));
        assert_eq!(app.registry.revision, 7);
        assert_eq!(app.registry.workspaces.len(), 2);
    }
    #[test]
    fn selection_generation_advances_when_new_selection_job_is_admitted() {
        let (mut app, _) = fixture();
        app.manage(
            management::Command::Single(Operation::LayoutUpdate {
                workspace_id: "old".into(),
                expected_revision: 1,
                layout: None,
            }),
            Some(management::Selection {
                workspace: "old".into(),
                pane: "pane".into(),
            }),
        )
        .unwrap();
        assert_eq!(app.selection_epoch, 1);
        app.manage(
            management::Command::Create {
                name: "new".into(),
                spec: PaneSpawnSpec {
                    argv: vec!["/bin/sh".into()],
                    cwd: app.options.cwd.clone(),
                    home: app.options.home.clone(),
                    size: size(),
                    title: None,
                    isolated_environment: true,
                },
            },
            None,
        )
        .unwrap();
        assert_eq!(app.selection_epoch, 2);
    }
    #[test]
    fn attach_slot_ownership_is_independent_of_nested_poll_unknown_marker() {
        use std::io::BufRead;
        for detached in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("cleanup.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                std::io::BufReader::new(stream.try_clone().unwrap())
                    .read_until(b'\n', &mut request)
                    .unwrap();
                let request: Request = serde_json::from_slice(&request).unwrap();
                assert!(matches!(
                    request.operation,
                    Operation::Pane {
                        request: PaneRequest::Detach { .. },
                        ..
                    }
                ));
                let response = Response {
                    version: PROTOCOL_VERSION,
                    ok: detached,
                    result: detached.then(|| serde_json::json!({"detached":true})),
                    error: (!detached).then(|| "cleanup rejected".into()),
                };
                let mut response = serde_json::to_vec(&response).unwrap();
                response.push(b'\n');
                stream.write_all(&response).unwrap();
            });
            let initial = if detached {
                transport::mark_outcome_unknown(anyhow::anyhow!("initial poll EOF"))
            } else {
                anyhow::anyhow!("bad initial scene")
            };
            let error = detach_failed_attach(&socket, "new-pane", "admitted-view", initial);
            assert_eq!(transport::outcome_unknown(&error), detached);
            let (mut app, _) = fixture();
            app.attach_slots = 1;
            app.jobs
                .submit("attach:new-pane".into(), move || Err(error))
                .unwrap();
            wait_background(&mut app, "attach:new-pane");
            assert_eq!(app.attach_slots, usize::from(!detached));
            server.join().unwrap();
        }
    }
    #[test]
    fn permanent_view_capacity_is_explicitly_unprojected_not_loading() {
        let (mut app, _) = fixture();
        app.attach_slots = MAX_ADMITTED_VIEWS;
        let status = app.placeholder("retained-pane");
        assert!(status.contains("capacity") && status.contains("unprojected"));
        assert!(!status.contains("Attaching"));
        assert_eq!(app.badge("retained-pane"), Some("UNPROJECTED"));
    }
}
