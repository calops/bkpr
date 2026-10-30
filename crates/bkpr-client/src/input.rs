use crate::transport;
use anyhow::{Context, Result, bail};
use bkpr_protocol::{MAX_INPUT_BYTES, Operation, PaneRequest};
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

const MAX_QUEUED_INPUTS: usize = 2048;
const MAX_QUEUED_PAYLOAD_BYTES: usize = 8 * MAX_INPUT_BYTES;
const MAX_DEVICE_RESPONSES: usize = 2;
const MAX_DEVICE_PAYLOAD_BYTES: usize = 2 * MAX_INPUT_BYTES;
const MAX_POLICY_REQUESTS: usize = 8;
const MAX_POLICY_PAYLOAD_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputLane {
    Commands,
    Clipboard,
    Policy,
}
impl InputLane {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Commands => 0,
            Self::Clipboard => 1,
            Self::Policy => 2,
        }
    }
    pub(crate) fn request(request: &PaneRequest) -> Self {
        match request {
            PaneRequest::Clipboard { .. } => Self::Clipboard,
            PaneRequest::Update { .. } | PaneRequest::Pin { .. } => Self::Policy,
            _ => Self::Commands,
        }
    }
}
pub(crate) fn cleanup(request: &PaneRequest) -> bool {
    matches!(
        request,
        PaneRequest::Detach { .. } | PaneRequest::Focus { focused: false, .. }
    ) || matches!(request, PaneRequest::Update { spec, .. }
            if !spec.visible && !spec.interactive && !spec.clipboard)
        || matches!(request, PaneRequest::Mouse { event, .. }
            if event.action == bkpr_protocol::MouseAction::Release)
}
pub(crate) type Receipt = mpsc::Receiver<Result<(), String>>;

#[derive(Debug, Default)]
struct BudgetState {
    requests: usize,
    bytes: usize,
}
#[derive(Debug)]
struct InputBudget {
    state: Mutex<BudgetState>,
    requests: usize,
    bytes: usize,
}
impl InputBudget {
    fn new(requests: usize, bytes: usize) -> Self {
        Self {
            state: Mutex::new(BudgetState::default()),
            requests,
            bytes,
        }
    }
    fn acquire(self: &Arc<Self>, requests: usize, bytes: usize) -> Result<InputPermit> {
        let mut state = self.state.lock().unwrap();
        if requests > self.requests.saturating_sub(state.requests)
            || bytes > self.bytes.saturating_sub(state.bytes)
        {
            bail!("aggregate input queue request/payload limit reached; input was not admitted");
        }
        state.requests += requests;
        state.bytes += bytes;
        Ok(InputPermit {
            budget: self.clone(),
            requests,
            bytes,
            pending: None,
        })
    }
}
#[derive(Debug)]
struct InputPermit {
    budget: Arc<InputBudget>,
    requests: usize,
    bytes: usize,
    pending: Option<Arc<AtomicUsize>>,
}
impl InputPermit {
    fn attach(&mut self, pending: &Arc<AtomicUsize>) {
        pending.fetch_add(1, Ordering::Relaxed);
        self.pending = Some(pending.clone());
    }
}
impl Drop for InputPermit {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.fetch_sub(1, Ordering::Release);
        }
        let mut state = self.budget.state.lock().unwrap();
        state.requests -= self.requests;
        state.bytes -= self.bytes;
    }
}

fn payload_bytes(pane: &str, request: &PaneRequest) -> usize {
    let view = match request {
        PaneRequest::Update { view_id, .. }
        | PaneRequest::Detach { view_id }
        | PaneRequest::Poll { view_id, .. }
        | PaneRequest::Pin { view_id, .. }
        | PaneRequest::Input { view_id, .. }
        | PaneRequest::Paste { view_id, .. }
        | PaneRequest::PasteContents { view_id, .. }
        | PaneRequest::Key { view_id, .. }
        | PaneRequest::Mouse { view_id, .. }
        | PaneRequest::Focus { view_id, .. }
        | PaneRequest::Scroll { view_id, .. }
        | PaneRequest::Clipboard { view_id, .. }
        | PaneRequest::Colors { view_id, .. }
        | PaneRequest::Terminate { view_id } => view_id.len(),
        _ => 0,
    };
    let contents = |contents: &[bkpr_protocol::ClipboardContent]| {
        contents
            .iter()
            .map(|content| content.mime.len() + content.data.len())
            .sum::<usize>()
    };
    let payload = match request {
        PaneRequest::Input { data, .. } | PaneRequest::Paste { data, .. } => data.len(),
        PaneRequest::PasteContents { contents: data, .. } => contents(data),
        PaneRequest::Key { event, .. } => event.key.len() + event.text.len(),
        PaneRequest::Clipboard {
            request_id,
            response,
            ..
        } => {
            request_id.len()
                + contents(&response.contents)
                + response.available.iter().map(String::len).sum::<usize>()
        }
        PaneRequest::Colors { palette, .. } => {
            palette.as_ref().map_or(0, |colors| colors.len() * 3)
        }
        PaneRequest::Poll {
            resources,
            scene_encoding,
            ..
        } => {
            resources.len() * std::mem::size_of::<bkpr_protocol::ImageKey>()
                + scene_encoding.as_ref().map_or(0, String::len)
        }
        _ => 0,
    };
    pane.len() + view + payload
}

#[derive(Clone, Debug)]
pub(crate) struct InputFailure {
    pub pane_id: String,
    pub view_id: Option<String>,
    pub operation: &'static str,
    pub lane: InputLane,
    pub stopped: bool,
    pub unknown_outcome: bool,
    pub not_dispatched: usize,
    pub error: String,
}
impl InputFailure {
    fn context(operation: &Operation) -> Self {
        let Operation::Pane { pane_id, request } = operation else {
            unreachable!();
        };
        let (name, view) = match request {
            PaneRequest::Inspect => ("inspect", None),
            PaneRequest::Attach { .. } => ("attach", None),
            PaneRequest::Update { view_id, .. } => ("update", Some(view_id)),
            PaneRequest::Detach { view_id } => ("detach", Some(view_id)),
            PaneRequest::Poll { view_id, .. } => ("poll", Some(view_id)),
            PaneRequest::Pin { view_id, .. } => ("pin", Some(view_id)),
            PaneRequest::Input { view_id, .. } => ("input", Some(view_id)),
            PaneRequest::Paste { view_id, .. } => ("paste", Some(view_id)),
            PaneRequest::PasteContents { view_id, .. } => ("paste_contents", Some(view_id)),
            PaneRequest::Key { view_id, .. } => ("key", Some(view_id)),
            PaneRequest::Mouse { view_id, .. } => ("mouse", Some(view_id)),
            PaneRequest::Focus { view_id, .. } => ("focus", Some(view_id)),
            PaneRequest::Scroll { view_id, .. } => ("scroll", Some(view_id)),
            PaneRequest::Clipboard { view_id, .. } => ("clipboard", Some(view_id)),
            PaneRequest::Colors { view_id, .. } => ("colors", Some(view_id)),
            PaneRequest::Terminate { view_id } => ("terminate", Some(view_id)),
            PaneRequest::Shutdown => ("shutdown", None),
        };
        Self {
            pane_id: pane_id.clone(),
            view_id: view.cloned(),
            operation: name,
            lane: InputLane::request(request),
            stopped: false,
            unknown_outcome: false,
            not_dispatched: 0,
            error: String::new(),
        }
    }
    pub(crate) fn admission(operation: &Operation, error: impl std::fmt::Display) -> InputRejected {
        let mut failure = Self::context(operation);
        failure.error = error.to_string();
        InputRejected(failure)
    }
}
impl std::fmt::Display for InputFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "pane {} view {} {} {}: {}",
            self.pane_id,
            self.view_id.as_deref().unwrap_or("-"),
            self.operation,
            if self.stopped {
                "lane stopped"
            } else {
                "not admitted"
            },
            self.error
        )?;
        if self.unknown_outcome {
            write!(out, "; outcome unknown, do not replay")?;
        }
        if self.not_dispatched > 0 {
            write!(
                out,
                "; {} accepted operations not dispatched",
                self.not_dispatched
            )?;
        }
        Ok(())
    }
}
#[derive(Debug)]
pub(crate) struct InputRejected(pub InputFailure);
impl std::fmt::Display for InputRejected {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{}; new input was not admitted", self.0)
    }
}
impl std::error::Error for InputRejected {}
struct LaneFailure {
    failure: InputFailure,
    reported: bool,
}

#[derive(Debug)]
pub(crate) struct ReservedInput {
    operation: Operation,
    before: Option<Operation>,
    permit: InputPermit,
}
impl ReservedInput {
    pub(crate) fn mouse(&self) -> Option<(&str, &str, bkpr_protocol::MouseEvent)> {
        match &self.operation {
            Operation::Pane {
                pane_id,
                request: PaneRequest::Mouse { view_id, event },
            } => Some((pane_id, view_id, *event)),
            _ => None,
        }
    }
}
pub(crate) struct DeviceReservation(ReservedInput);
#[derive(Debug)]
struct QueuedInput {
    reserved: ReservedInput,
    done: Option<mpsc::SyncSender<Result<(), String>>>,
}
struct Lane {
    sender: mpsc::Sender<QueuedInput>,
    pending: Arc<AtomicUsize>,
    retiring: bool,
    failure: Arc<Mutex<Option<LaneFailure>>>,
    worker: thread::JoinHandle<()>,
}
#[derive(Default)]
struct PaneLanes {
    commands: Option<Lane>,
    device: Option<Lane>,
    policy: Option<Lane>,
}
impl PaneLanes {
    fn slot(&mut self, kind: InputLane) -> &mut Option<Lane> {
        match kind {
            InputLane::Commands => &mut self.commands,
            InputLane::Clipboard => &mut self.device,
            InputLane::Policy => &mut self.policy,
        }
    }
    fn lane(&self, kind: InputLane) -> Option<&Lane> {
        match kind {
            InputLane::Commands => self.commands.as_ref(),
            InputLane::Clipboard => self.device.as_ref(),
            InputLane::Policy => self.policy.as_ref(),
        }
    }
}

/// FIFO is pane-local. Clipboard replies have a distinct lane because a native
/// callback can be waiting for that reply while an earlier pane command is busy.
/// Command lanes share one admission budget, including requests in flight.
/// A separate bounded device reserve cannot be exhausted by blocked input.
pub struct InputPump {
    socket: PathBuf,
    lanes: Mutex<HashMap<String, PaneLanes>>,
    tombstones: Mutex<HashMap<String, [Option<InputFailure>; 3]>>,
    budget: Arc<InputBudget>,
    device_budget: Arc<InputBudget>,
    policy_budget: Arc<InputBudget>,
    pub wake: UnixStream,
    notify: Arc<UnixStream>,
    #[cfg(test)]
    capture: Option<(mpsc::Sender<QueuedInput>, Arc<AtomicUsize>)>,
}
fn notify(wake: &UnixStream) {
    let mut wake = wake;
    let _ = wake.write_all(&[1]);
}
impl InputPump {
    pub fn new(socket: &Path) -> Result<Self> {
        let (wake, notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        Ok(Self {
            socket: socket.to_owned(),
            lanes: Mutex::new(HashMap::new()),
            tombstones: Mutex::new(HashMap::new()),
            budget: Arc::new(InputBudget::new(
                MAX_QUEUED_INPUTS,
                MAX_QUEUED_PAYLOAD_BYTES,
            )),
            device_budget: Arc::new(InputBudget::new(
                MAX_DEVICE_RESPONSES,
                MAX_DEVICE_PAYLOAD_BYTES,
            )),
            policy_budget: Arc::new(InputBudget::new(
                MAX_POLICY_REQUESTS,
                MAX_POLICY_PAYLOAD_BYTES,
            )),
            wake,
            notify: Arc::new(notify),
            #[cfg(test)]
            capture: None,
        })
    }
    fn lane(&self, initial_failure: Option<InputFailure>) -> Result<Lane> {
        let (sender, receiver) = mpsc::channel::<QueuedInput>();
        let pending = Arc::new(AtomicUsize::new(0));
        let socket = self.socket.clone();
        let failure = Arc::new(Mutex::new(initial_failure.map(|failure| LaneFailure {
            failure,
            reported: true,
        })));
        let failed = failure.clone();
        let wake = self.notify.clone();
        let worker = thread::Builder::new().name("bkpr-pane-input".into()).spawn(move || {
            while let Ok(job) = receiver.recv() {
                let QueuedInput { reserved, done } = job;
                let ReservedInput { operation, before, permit } = reserved;
                let mut completed = Ok(());
                for operation in before.into_iter().chain(std::iter::once(operation)) {
                    let cleanup = matches!(&operation, Operation::Pane { request, .. } if cleanup(request));
                    let blocked = {
                        let mut state = failed.lock().unwrap();
                        if let Some(state) = state.as_mut().filter(|_| !cleanup) {
                            state.failure.not_dispatched += 1;
                            state.reported = false;
                            Some(state.failure.to_string())
                        } else { None }
                    };
                    if let Some(error) = blocked {
                        completed = Err(error);
                        notify(&wake);
                        continue;
                    }
                    let colors = matches!(&operation, Operation::Pane { request: PaneRequest::Colors { .. }, .. });
                    let mut context = InputFailure::context(&operation);
                    let result = transport::checked(&socket, operation).map(|_| ());
                    if let Err(error) = &result {
                        let expected_owner_rejection = colors && !transport::outcome_unknown(error)
                            && error.root_cause().to_string() == "only current size owner may seed terminal colors";
                        if !expected_owner_rejection {
                            context.stopped = true;
                            context.error = format!("{error:#}");
                            context.unknown_outcome = transport::outcome_unknown(error);
                            let mut state = failed.lock().unwrap();
                            if state.is_none() { *state = Some(LaneFailure { failure: context, reported: false }); }
                            else if cleanup {
                                let previous = state.as_mut().unwrap();
                                previous.failure.error.push_str(&format!("; cleanup also failed: {context}"));
                                previous.failure.unknown_outcome |= context.unknown_outcome;
                                previous.reported = false;
                            }
                            notify(&wake);
                        }
                    }
                    completed = result.map_err(|error| format!("{error:#}"));
                }
                drop(permit);
                if let Some(done) = done { let _ = done.send(completed); }
                // Local capacity deferrals must wake on every settled job, not
                // wait for an unrelated frame or the UI's long poll timeout.
                notify(&wake);
            }
        }).context("starting independent pane input worker")?;
        Ok(Lane {
            sender,
            pending,
            retiring: false,
            failure,
            worker,
        })
    }
    fn stopped_in(
        &self,
        lanes: &HashMap<String, PaneLanes>,
        pane: &str,
        kind: InputLane,
    ) -> Option<InputFailure> {
        let index = kind.index();
        if let Some(failure) = self
            .tombstones
            .lock()
            .unwrap()
            .get(pane)
            .and_then(|failures| failures[index].clone())
        {
            return Some(failure);
        }
        let lanes = lanes.get(pane)?;
        let lane = lanes.lane(kind)?;
        lane.failure
            .lock()
            .unwrap()
            .as_ref()
            .map(|state| state.failure.clone())
    }
    fn admit(
        &self,
        operation: &Operation,
        before: Option<&Operation>,
        future_bytes: usize,
    ) -> Result<InputPermit> {
        let Operation::Pane { pane_id, request } = operation else {
            bail!("input dispatcher requires a pane-scoped operation");
        };
        let kind = InputLane::request(request);
        if !cleanup(request)
            && let Some(failure) = self.stopped_in(&self.lanes.lock().unwrap(), pane_id, kind)
        {
            return Err(InputRejected(failure).into());
        }
        let mut bytes = payload_bytes(pane_id, request) + future_bytes;
        if let Some(Operation::Pane { pane_id, request }) = before {
            bytes += payload_bytes(pane_id, request);
        }
        let requests = 1 + usize::from(before.is_some());
        let budget = match kind {
            InputLane::Commands => &self.budget,
            InputLane::Clipboard => &self.device_budget,
            InputLane::Policy => &self.policy_budget,
        };
        budget
            .acquire(requests, bytes)
            .map_err(|error| InputFailure::admission(operation, error).into())
    }
    fn reserve_before(
        &self,
        operation: Operation,
        before: Option<Operation>,
    ) -> Result<ReservedInput> {
        let permit = self.admit(&operation, before.as_ref(), 0)?;
        Ok(ReservedInput {
            operation,
            before,
            permit,
        })
    }
    pub fn reserve(&self, operation: Operation) -> Result<ReservedInput> {
        self.reserve_before(operation, None)
    }
    fn dispatch(
        &self,
        mut reserved: ReservedInput,
        done: Option<mpsc::SyncSender<Result<(), String>>>,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some((sender, pending)) = &self.capture {
            reserved.permit.attach(pending);
            sender
                .send(QueuedInput { reserved, done })
                .context("captured input queue disconnected")?;
            return Ok(());
        }
        let Operation::Pane { pane_id, request } = &reserved.operation else {
            unreachable!();
        };
        let kind = InputLane::request(request);
        let cleanup = cleanup(request);
        let mut lanes = self.lanes.lock().unwrap();
        let stopped = self.stopped_in(&lanes, pane_id, kind);
        if !cleanup && let Some(failure) = &stopped {
            return Err(InputRejected(failure.clone()).into());
        }
        if !lanes.contains_key(pane_id) {
            lanes.insert(pane_id.clone(), PaneLanes::default());
        }
        let pane = lanes.get_mut(pane_id).unwrap();
        let slot = pane.slot(kind);
        if slot.is_none() {
            *slot = Some(
                self.lane(stopped)
                    .map_err(|error| InputFailure::admission(&reserved.operation, error))?,
            );
        }
        let lane = slot.as_mut().unwrap();
        reserved.permit.attach(&lane.pending);
        lane.retiring = kind == InputLane::Clipboard || cleanup;
        let context = InputFailure::context(&reserved.operation);
        lane.sender
            .send(QueuedInput { reserved, done })
            .map_err(|_| {
                let mut failure = context;
                failure.error = "pane input worker disconnected".into();
                InputRejected(failure)
            })?;
        Ok(())
    }
    fn submit(
        &self,
        operation: Operation,
        done: Option<mpsc::SyncSender<Result<(), String>>>,
    ) -> Result<()> {
        self.dispatch(self.reserve(operation)?, done)
    }
    pub fn send(&self, operation: Operation) -> Result<()> {
        self.submit(operation, None)
    }
    pub fn send_reserved(&self, reserved: ReservedInput) -> Result<()> {
        self.dispatch(reserved, None)
    }
    /// Keep a physical device response owned by the UI when local admission
    /// fails. Never query the device again merely because its reply queue is full.
    fn reserve_owned(&self, operation: &mut Option<Operation>) -> Result<ReservedInput> {
        let request = operation
            .as_ref()
            .context("owned input operation is missing")?;
        let permit = self.admit(request, None, 0)?;
        Ok(ReservedInput {
            operation: operation.take().unwrap(),
            before: None,
            permit,
        })
    }
    pub fn send_owned_operation(&self, operation: &mut Option<Operation>) -> Result<()> {
        self.dispatch(self.reserve_owned(operation)?, None)
    }
    pub fn send_owned(&self, operation: &mut Option<Operation>) -> Result<Receipt> {
        let reserved = self.reserve_owned(operation)?;
        let (done, receipt) = mpsc::sync_channel(1);
        self.dispatch(reserved, Some(done))?;
        Ok(receipt)
    }
    pub fn reserve_response(
        &self,
        pane: &str,
        view: &str,
        request: &str,
    ) -> Result<DeviceReservation> {
        let operation = Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Clipboard {
                view_id: view.into(),
                request_id: request.into(),
                response: bkpr_protocol::ClipboardResponse::denied(),
            },
        };
        // The device parser bounds aggregate bytes to 1 MiB and sixteen MIME
        // names of at most 256 bytes. Charge before starting the physical query.
        let permit = self.admit(&operation, None, MAX_INPUT_BYTES + 16 * 256)?;
        Ok(DeviceReservation(ReservedInput {
            operation,
            before: None,
            permit,
        }))
    }
    pub fn send_response(
        &self,
        reservation: DeviceReservation,
        response: bkpr_protocol::ClipboardResponse,
    ) -> Result<Receipt> {
        let mut reserved = reservation.0;
        let Operation::Pane {
            request: PaneRequest::Clipboard {
                response: stored, ..
            },
            ..
        } = &mut reserved.operation
        else {
            unreachable!();
        };
        *stored = response;
        let Operation::Pane { pane_id, request } = &reserved.operation else {
            unreachable!();
        };
        let actual = payload_bytes(pane_id, request);
        if actual > reserved.permit.bytes {
            return Err(InputFailure::admission(
                &reserved.operation,
                "clipboard response exceeded its reserved payload bound",
            )
            .into());
        }
        let unused = reserved.permit.bytes - actual;
        reserved.permit.budget.state.lock().unwrap().bytes -= unused;
        reserved.permit.bytes = actual;
        let (done, receipt) = mpsc::sync_channel(1);
        self.dispatch(reserved, Some(done))?;
        Ok(receipt)
    }
    pub fn policy(&self, pane: &str, view: &str, spec: bkpr_protocol::ViewSpec) -> Result<Receipt> {
        let (done, receipt) = mpsc::sync_channel(1);
        self.submit(
            Operation::Pane {
                pane_id: pane.into(),
                request: PaneRequest::Update {
                    view_id: view.into(),
                    spec,
                },
            },
            Some(done),
        )?;
        Ok(receipt)
    }
    pub fn retire(&self, pane: &str, view: &str, focus_out: bool) -> Result<Receipt> {
        let operation = Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Detach {
                view_id: view.into(),
            },
        };
        let before = focus_out.then(|| Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Focus {
                view_id: view.into(),
                focused: false,
            },
        });
        let reserved = self.reserve_before(operation, before)?;
        let (done, received) = mpsc::sync_channel(1);
        self.dispatch(reserved, Some(done))?;
        Ok(received)
    }
    fn reap(&self) -> Result<()> {
        let mut finished = Vec::new();
        {
            let mut lanes = self.lanes.lock().unwrap();
            for (pane_id, pane) in lanes.iter_mut() {
                for kind in [InputLane::Commands, InputLane::Clipboard, InputLane::Policy] {
                    let slot = pane.slot(kind);
                    if slot.as_ref().is_some_and(|lane| {
                        lane.retiring
                            && lane.pending.load(Ordering::Acquire) == 0
                            && lane
                                .failure
                                .lock()
                                .unwrap()
                                .as_ref()
                                .is_none_or(|state| state.reported)
                    }) {
                        if let Some(state) = slot.as_ref().unwrap().failure.lock().unwrap().as_ref()
                        {
                            self.tombstones
                                .lock()
                                .unwrap()
                                .entry(pane_id.clone())
                                .or_default()[kind.index()] = Some(state.failure.clone());
                        }
                        finished.push(slot.take().unwrap());
                    }
                }
            }
            lanes.retain(|_, pane| {
                pane.commands.is_some() || pane.device.is_some() || pane.policy.is_some()
            });
        }
        for lane in finished {
            drop(lane.sender);
            lane.worker
                .join()
                .map_err(|_| anyhow::anyhow!("pane input worker failed"))?;
        }
        Ok(())
    }
    pub fn check(&self) -> Result<Vec<InputFailure>> {
        let mut wake = &self.wake;
        let mut bytes = [0u8; 128];
        while matches!(wake.read(&mut bytes), Ok(n) if n > 0) {}
        let mut failures = Vec::new();
        {
            let lanes = self.lanes.lock().unwrap();
            for pane in lanes.values() {
                for lane in [&pane.commands, &pane.device, &pane.policy]
                    .into_iter()
                    .flatten()
                {
                    if let Some(state) = lane.failure.lock().unwrap().as_mut()
                        && !state.reported
                    {
                        failures.push(state.failure.clone());
                        state.reported = true;
                    }
                }
            }
        }
        self.reap()?;
        Ok(failures)
    }
    pub fn finish(mut self) -> Result<()> {
        #[cfg(test)]
        self.capture.take();
        let lanes = std::mem::take(self.lanes.get_mut().unwrap());
        let mut workers = Vec::new();
        for pane in lanes.into_values() {
            for lane in [pane.commands, pane.device, pane.policy]
                .into_iter()
                .flatten()
            {
                drop(lane.sender);
                workers.push((lane.worker, lane.failure));
            }
        }
        // All workers are already draining concurrently; never gate one pane's
        // dispatch on joining another pane's blocked native operation.
        let mut failure = None;
        for (worker, state) in workers {
            if worker.join().is_err() && failure.is_none() {
                failure = Some(anyhow::anyhow!("pane input worker failed"));
            }
            if let Some(state) = state.lock().unwrap().as_ref()
                && !state.reported
                && failure.is_none()
            {
                failure = Some(anyhow::anyhow!(state.failure.to_string()));
            }
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn capture() -> (Self, Capture) {
        let mut input = Self::new(Path::new("/unused-input-capture")).unwrap();
        let (sender, receiver) = mpsc::channel();
        input.capture = Some((sender, Arc::new(AtomicUsize::new(0))));
        (
            input,
            Capture {
                receiver,
                pending: std::cell::RefCell::new(None),
            },
        )
    }
}
#[cfg(test)]
pub(crate) struct Capture {
    receiver: mpsc::Receiver<QueuedInput>,
    pending: std::cell::RefCell<Option<QueuedInput>>,
}
#[cfg(test)]
impl Capture {
    pub fn try_recv(&self) -> Result<Operation, mpsc::TryRecvError> {
        let mut job = match self.pending.borrow_mut().take() {
            Some(job) => job,
            None => self.receiver.try_recv()?,
        };
        if let Some(before) = job.reserved.before.take() {
            self.pending.replace(Some(job));
            return Ok(before);
        }
        let QueuedInput { reserved, done } = job;
        let ReservedInput {
            operation, permit, ..
        } = reserved;
        drop(permit);
        if let Some(done) = done {
            let _ = done.send(Ok(()));
        }
        Ok(operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{
        ClipboardContent, ClipboardResponse, ClipboardResult, KeyAction, KeyEvent,
        PROTOCOL_VERSION, Request, Response,
    };
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        time::Duration,
    };

    fn key(pane: &str, name: &str) -> Operation {
        Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Key {
                view_id: format!("{pane}-view"),
                event: KeyEvent {
                    key: name.into(),
                    modifiers: 0,
                    text: String::new(),
                    action: KeyAction::Press,
                    unshifted_codepoint: 0,
                },
            },
        }
    }
    #[test]
    fn blocked_pane_preserves_its_fifo_without_blocking_other_pane_or_clipboard_reply() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "bkpr-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        struct SocketPath(PathBuf);
        impl Drop for SocketPath {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = SocketPath(path.clone());
        let listener = UnixListener::bind(&path).unwrap();
        let (observed, received) = mpsc::channel();
        let (release, blocked) = mpsc::sync_channel(1);
        let blocked = Arc::new(Mutex::new(blocked));
        let server = thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                let observed = observed.clone();
                let release = release.clone();
                let blocked = blocked.clone();
                workers.push(thread::spawn(move || {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    BufReader::new(stream.try_clone().unwrap())
                        .read_until(b'\n', &mut bytes)
                        .unwrap();
                    let request: Request = serde_json::from_slice(&bytes).unwrap();
                    observed.send(request.operation.clone()).unwrap();
                    match request.operation {
                        Operation::Pane {
                            pane_id,
                            request: PaneRequest::Focus { .. },
                        } if pane_id == "alpha" => {
                            blocked
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                        }
                        Operation::Pane {
                            pane_id,
                            request:
                                PaneRequest::Clipboard {
                                    view_id,
                                    request_id,
                                    response,
                                },
                        } => {
                            assert_eq!(pane_id, "alpha");
                            assert_eq!(view_id, "original-device");
                            assert_eq!(request_id, "native-request");
                            assert_eq!(response.contents[0].mime, "application/octet-stream");
                            assert_eq!(response.contents[0].data, b"exact\0clipboard\xff");
                            release.send(()).unwrap();
                        }
                        _ => {}
                    }
                    let response = Response {
                        version: PROTOCOL_VERSION,
                        ok: true,
                        result: Some(serde_json::json!({"accepted":true})),
                        error: None,
                    };
                    let mut bytes = serde_json::to_vec(&response).unwrap();
                    bytes.push(b'\n');
                    stream.write_all(&bytes).unwrap();
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let input = InputPump::new(&path).unwrap();
        input
            .send(Operation::Pane {
                pane_id: "alpha".into(),
                request: PaneRequest::Focus {
                    view_id: "alpha-view".into(),
                    focused: false,
                },
            })
            .unwrap();
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, request: PaneRequest::Focus { .. } } if pane_id == "alpha")
        );
        let policy = input
            .policy(
                "alpha",
                "alpha-view",
                bkpr_protocol::ViewSpec {
                    size: bkpr_protocol::Size {
                        cols: 80,
                        rows: 24,
                        cell_width: 8,
                        cell_height: 16,
                    },
                    visible: false,
                    interactive: false,
                    clipboard: false,
                    input: true,
                },
            )
            .unwrap();
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, request: PaneRequest::Update { view_id, spec } }
                if pane_id == "alpha" && view_id == "alpha-view"
                    && spec.input && !spec.visible && !spec.interactive && !spec.clipboard)
        );
        policy
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let retired = input.retire("alpha", "alpha-view", false).unwrap();
        input.send(key("beta", "KeyA")).unwrap();
        let (beta_done, beta_completed) = mpsc::sync_channel(1);
        input.submit(key("beta", "Enter"), Some(beta_done)).unwrap();
        for name in ["KeyA", "Enter"] {
            assert!(
                matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
                Operation::Pane { pane_id, request: PaneRequest::Key { event, .. } }
                    if pane_id == "beta" && event.key == name)
            );
        }
        beta_completed
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(matches!(retired.try_recv(), Err(mpsc::TryRecvError::Empty)));
        input
            .send(Operation::Pane {
                pane_id: "alpha".into(),
                request: PaneRequest::Clipboard {
                    view_id: "original-device".into(),
                    request_id: "native-request".into(),
                    response: ClipboardResponse {
                        result: ClipboardResult::Success,
                        contents: vec![ClipboardContent {
                            mime: "application/octet-stream".into(),
                            data: b"exact\0clipboard\xff".to_vec(),
                        }],
                        available: Vec::new(),
                        remember: false,
                    },
                },
            })
            .unwrap();
        assert!(matches!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane {
                request: PaneRequest::Clipboard { .. },
                ..
            }
        ));
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, request: PaneRequest::Detach { view_id } }
                if pane_id == "alpha" && view_id == "alpha-view")
        );
        retired
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(input.check().unwrap().is_empty());
        assert_eq!(input.budget.state.lock().unwrap().requests, 0);
        input.finish().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn aggregate_message_limit_is_shared_and_device_reserve_still_unblocks_input() {
        let (input, captured) = InputPump::capture();
        for index in 0..MAX_QUEUED_INPUTS {
            input.send(key(&format!("pane-{index}"), "KeyA")).unwrap();
        }
        assert!(input.send(key("another-pane", "Enter")).is_err());
        for pane in ["alpha", "beta"] {
            input
                .send(Operation::Pane {
                    pane_id: pane.into(),
                    request: PaneRequest::Clipboard {
                        view_id: format!("{pane}-device"),
                        request_id: "request".into(),
                        response: ClipboardResponse::denied(),
                    },
                })
                .unwrap();
        }
        assert!(
            input
                .send(Operation::Pane {
                    pane_id: "gamma".into(),
                    request: PaneRequest::Clipboard {
                        view_id: "gamma-device".into(),
                        request_id: "request".into(),
                        response: ClipboardResponse::denied(),
                    }
                })
                .is_err()
        );
        assert_eq!(
            input.budget.state.lock().unwrap().requests,
            MAX_QUEUED_INPUTS
        );
        assert_eq!(
            input.device_budget.state.lock().unwrap().requests,
            MAX_DEVICE_RESPONSES
        );
        drop(captured);
        assert_eq!(input.budget.state.lock().unwrap().requests, 0);
        assert_eq!(input.device_budget.state.lock().unwrap().requests, 0);
        input.finish().unwrap();
    }
    #[test]
    fn aggregate_payload_limit_covers_queued_and_inflight_requests_across_panes() {
        let (input, captured) = InputPump::capture();
        let mut admitted = 0;
        for index in 0..16 {
            let result = input.send(Operation::Pane {
                pane_id: format!("pane-{index}"),
                request: PaneRequest::Paste {
                    view_id: format!("view-{index}"),
                    data: vec![b'x'; MAX_INPUT_BYTES],
                },
            });
            if result.is_err() {
                break;
            }
            admitted += 1;
        }
        assert!(admitted > 0 && admitted < 16);
        assert!(input.budget.state.lock().unwrap().bytes <= MAX_QUEUED_PAYLOAD_BYTES);
        input
            .send(Operation::Pane {
                pane_id: "blocked-pane".into(),
                request: PaneRequest::Clipboard {
                    view_id: "device".into(),
                    request_id: "request".into(),
                    response: ClipboardResponse::denied(),
                },
            })
            .unwrap();
        drop(captured);
        assert_eq!(input.budget.state.lock().unwrap().bytes, 0);
        assert_eq!(input.device_budget.state.lock().unwrap().bytes, 0);
        input.finish().unwrap();
    }
    #[test]
    fn unknown_failure_stops_only_its_pane_and_survives_lane_reaping() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (observed, received) = mpsc::channel();
        let (release, gate) = mpsc::sync_channel(1);
        let gate = Arc::new(Mutex::new(gate));
        let server = thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let observed = observed.clone();
                let gate = gate.clone();
                workers.push(thread::spawn(move || {
                    let mut bytes = Vec::new();
                    BufReader::new(stream.try_clone().unwrap())
                        .read_until(b'\n', &mut bytes)
                        .unwrap();
                    let request: Request = serde_json::from_slice(&bytes).unwrap();
                    let failing = matches!(&request.operation, Operation::Pane {
                        pane_id, request: PaneRequest::Key { event, .. },
                    } if pane_id == "alpha" && event.key == "KeyA");
                    observed.send(request.operation).unwrap();
                    if failing {
                        gate.lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                    }
                    let response = Response {
                        version: PROTOCOL_VERSION,
                        ok: !failing,
                        result: (!failing).then(|| serde_json::json!({"accepted":true})),
                        error: failing.then(|| "native command timed out; outcome unknown".into()),
                    };
                    let mut bytes = serde_json::to_vec(&response).unwrap();
                    bytes.push(b'\n');
                    stream.write_all(&bytes).unwrap();
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let input = InputPump::new(&path).unwrap();
        input.send(key("alpha", "KeyA")).unwrap();
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, .. } if pane_id == "alpha")
        );
        input.send(key("alpha", "KeyB")).unwrap();
        let retired = input.retire("alpha", "alpha-view", false).unwrap();
        let (done, completed) = mpsc::sync_channel(1);
        input.submit(key("beta", "Enter"), Some(done)).unwrap();
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, .. } if pane_id == "beta")
        );
        completed
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        release.send(()).unwrap();
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, request: PaneRequest::Detach { .. } } if pane_id == "alpha")
        );
        retired
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let failures = input.check().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].pane_id, "alpha");
        assert_eq!(failures[0].operation, "key");
        assert!(failures[0].stopped && failures[0].unknown_outcome);
        assert_eq!(failures[0].not_dispatched, 1);
        assert!(!input.lanes.lock().unwrap().contains_key("alpha"));
        let error = input.send(key("alpha", "KeyC")).unwrap_err();
        assert!(
            error
                .downcast_ref::<InputRejected>()
                .unwrap()
                .0
                .unknown_outcome
        );
        let (done, completed) = mpsc::sync_channel(1);
        input.submit(key("beta", "KeyB"), Some(done)).unwrap();
        assert!(
            matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Operation::Pane { pane_id, .. } if pane_id == "beta")
        );
        completed
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        input.finish().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn deferred_reservations_share_normal_admission_and_do_not_charge_twice() {
        let (input, captured) = InputPump::capture();
        let mut deferred = (0..MAX_QUEUED_INPUTS)
            .map(|_| input.reserve(key("alpha", "KeyA")).unwrap())
            .collect::<Vec<_>>();
        assert!(input.send(key("beta", "Enter")).is_err());
        input.send_reserved(deferred.pop().unwrap()).unwrap();
        assert_eq!(
            input.budget.state.lock().unwrap().requests,
            MAX_QUEUED_INPUTS
        );
        captured.try_recv().unwrap();
        input.send(key("beta", "Enter")).unwrap();
        drop(deferred);
        assert_eq!(input.budget.state.lock().unwrap().requests, 1);
        drop(captured);
        assert_eq!(input.budget.state.lock().unwrap().requests, 0);
        input.finish().unwrap();
    }
    #[test]
    fn future_device_reply_and_policy_reserves_survive_full_normal_budget() {
        let (input, captured) = InputPump::capture();
        for _ in 0..MAX_QUEUED_INPUTS {
            input.send(key("busy", "KeyA")).unwrap();
        }
        let reservation = input
            .reserve_response("origin", "original-device", "native-request")
            .unwrap();
        assert!(input.device_budget.state.lock().unwrap().bytes >= MAX_INPUT_BYTES);
        let mut denied = Some(Operation::Pane {
            pane_id: "other".into(),
            request: PaneRequest::Clipboard {
                view_id: "other-device".into(),
                request_id: "other-request".into(),
                response: ClipboardResponse::denied(),
            },
        });
        let denied_done = input.send_owned(&mut denied).unwrap();
        let mut waiting = Some(Operation::Pane {
            pane_id: "waiting".into(),
            request: PaneRequest::Clipboard {
                view_id: "waiting-device".into(),
                request_id: "waiting-request".into(),
                response: ClipboardResponse::denied(),
            },
        });
        assert!(input.send_owned(&mut waiting).is_err());
        assert!(
            waiting.is_some(),
            "local non-admission dropped the owned reply"
        );
        let spec = bkpr_protocol::ViewSpec {
            size: bkpr_protocol::Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            visible: false,
            interactive: false,
            clipboard: false,
            input: true,
        };
        let policies = (0..MAX_POLICY_REQUESTS)
            .map(|index| {
                input
                    .policy(&format!("policy-{index}"), "view", spec.clone())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(input.policy("overflow", "view", spec).is_err());
        let answered = input
            .send_response(
                reservation,
                ClipboardResponse {
                    result: ClipboardResult::Success,
                    contents: vec![ClipboardContent {
                        mime: "application/octet-stream".into(),
                        data: b"exact\0bytes\xff".to_vec(),
                    }],
                    available: Vec::new(),
                    remember: false,
                },
            )
            .unwrap();
        assert_eq!(input.device_budget.state.lock().unwrap().requests, 2);
        assert!(input.device_budget.state.lock().unwrap().bytes < MAX_INPUT_BYTES);
        let mut original = false;
        while let Ok(operation) = captured.try_recv() {
            if let Operation::Pane {
                pane_id,
                request:
                    PaneRequest::Clipboard {
                        view_id,
                        request_id,
                        response,
                    },
            } = operation
                && pane_id == "origin"
            {
                assert_eq!(view_id, "original-device");
                assert_eq!(request_id, "native-request");
                assert_eq!(response.contents[0].data, b"exact\0bytes\xff");
                assert_eq!(response.contents[0].mime, "application/octet-stream");
                original = true;
            }
        }
        assert!(original);
        answered.recv().unwrap().unwrap();
        denied_done.recv().unwrap().unwrap();
        for policy in policies {
            policy.recv().unwrap().unwrap();
        }
        assert_eq!(input.budget.state.lock().unwrap().requests, 0);
        assert_eq!(input.device_budget.state.lock().unwrap().bytes, 0);
        assert_eq!(input.policy_budget.state.lock().unwrap().requests, 0);
        input.finish().unwrap();
    }
}
