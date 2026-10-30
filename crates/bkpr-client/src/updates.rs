use crate::transport;
use anyhow::{Context, Result, bail};
use bkpr_protocol::*;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

pub struct Snapshot {
    pub metadata: PaneSnapshot,
    pub scene: Option<scene_wire::DecodedScene>,
}
/// Bounded single-flight receiver: output cannot grow an unbounded queue or
/// block the UI's keyboard reader. All worker state is owned, never UI-borrowed.
pub struct Pump<T> {
    receiver: Option<mpsc::Receiver<Result<T, String>>>,
    pub wake: UnixStream,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl<T: Send + 'static> Pump<T> {
    fn start(mut next: impl FnMut() -> Result<T> + Send + 'static) -> Result<Self> {
        let (wake, mut notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let value = next().map_err(|e| format!("{e:#}"));
                let failed = value.is_err();
                if sender.send(value).is_err() {
                    break;
                }
                let _ = notify.write_all(&[1]);
                if failed {
                    break;
                }
            }
        });
        Ok(Self {
            receiver: Some(receiver),
            wake,
            stop,
            worker: Some(worker),
        })
    }
    #[cfg(test)]
    pub(crate) fn capture() -> Self {
        let (_, receiver) = mpsc::sync_channel(1);
        let (wake, _) = UnixStream::pair().unwrap();
        Self {
            receiver: Some(receiver),
            wake,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }
    pub fn initial(&self) -> Result<T> {
        self.receiver
            .as_ref()
            .unwrap()
            .recv()
            .context("snapshot worker disconnected")?
            .map_err(anyhow::Error::msg)
    }
    pub fn latest(&mut self) -> Result<Option<T>> {
        let mut bytes = [0u8; 128];
        while matches!(self.wake.read(&mut bytes),Ok(n)if n>0) {}
        match self.receiver.as_ref().unwrap().try_recv() {
            Ok(value) => value.map(Some).map_err(anyhow::Error::msg),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => bail!("snapshot worker disconnected"),
        }
    }
}
impl<T> Drop for Pump<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver.take();
        // Pane owners retain this pump until Detach has revoked the view and
        // awakened its poll. Never join a thread on the input/render path:
        // transport completion/decode can still be finishing after that ACK.
        drop(self.worker.take());
    }
}
pub fn pane(socket: &Path, pane_id: &str, view: &str) -> Result<Pump<Snapshot>> {
    let socket = socket.to_owned();
    let pane_id = pane_id.to_owned();
    let view_id = view.to_owned();
    let (mut frame_after, mut snapshot_after, mut resources) = (0, 0, Vec::new());
    Pump::start(move || {
        let mut metadata: PaneSnapshot = serde_json::from_value(transport::checked(
            &socket,
            Operation::Pane {
                pane_id: pane_id.clone(),
                request: PaneRequest::Poll {
                    view_id: view_id.clone(),
                    after: 0,
                    frame_after,
                    resources: resources.clone(),
                    snapshot_after,
                    wait_ms: 1000,
                    row_deltas: true,
                    scene_encoding: Some("compact_v1".into()),
                },
            },
        )?)?;
        let scene = metadata.scene.take().map(scene_wire::decode).transpose()?;
        snapshot_after = metadata.snapshot_revision;
        frame_after = metadata.revision;
        if scene.is_some() {
            resources = metadata.image_refs.clone();
        }
        Ok(Snapshot { metadata, scene })
    })
}
pub fn registry(socket: &Path, after: u64) -> Result<Pump<RegistrySnapshot>> {
    let socket = socket.to_owned();
    let mut after = after;
    Pump::start(move || {
        let value: RegistryResult = serde_json::from_value(transport::checked(
            &socket,
            Operation::Wait {
                after,
                wait_ms: 1000,
            },
        )?)?;
        after = value.registry.revision;
        Ok(value.registry)
    })
}

#[path = "input.rs"]
mod input;
#[cfg(test)]
pub(crate) use input::Capture;
pub(crate) use input::{
    DeviceReservation, InputFailure, InputLane, InputPump, InputRejected, Receipt, ReservedInput,
    cleanup,
};
