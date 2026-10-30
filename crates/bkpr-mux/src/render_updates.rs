use super::*;

/// One in-flight request and one queued result; IPC never blocks keyboard reads.
pub(super) struct SnapshotPump {
    receiver: Option<std::sync::mpsc::Receiver<std::result::Result<Value, String>>>,
    pub wake: std::os::unix::net::UnixStream,
    stop: Arc<std::sync::atomic::AtomicBool>,
    resize_signal: signal_hook::SigId,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl SnapshotPump {
    pub fn new(socket: &Path, view: &str) -> Result<Self> {
        let (wake, mut notify) = std::os::unix::net::UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let resize_signal = signal_hook::low_level::pipe::register(
            signal_hook::consts::SIGWINCH,
            notify.try_clone()?,
        )?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let socket = socket.to_owned();
        let mut request = json!({"op":"poll","view_id":view,"frame_after":0,
            "resources":[],"snapshot_after":0,"wait_ms":1000,"row_deltas":true});
        let worker = std::thread::spawn(move || {
            while !stopped.load(std::sync::atomic::Ordering::Acquire) {
                let result =
                    transport::checked(&socket, &request).map_err(|error| format!("{error:#}"));
                if let Ok(snapshot) = &result {
                    request["snapshot_after"] = snapshot["snapshot_revision"].clone();
                    request["frame_after"] = snapshot["revision"].clone();
                    if !snapshot["frame"].is_null() {
                        request["resources"] = snapshot["image_refs"].clone();
                    }
                }
                let failed = result.is_err();
                if sender.send(result).is_err() {
                    break;
                }
                // A full notification socket already guarantees readability.
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
            resize_signal,
        })
    }
    pub fn initial(&self) -> Result<Value> {
        self.receiver
            .as_ref()
            .unwrap()
            .recv()
            .context("snapshot worker disconnected")?
            .map_err(anyhow::Error::msg)
    }
    pub fn update(&self, snapshot: &mut Value) -> Result<()> {
        match self.receiver.as_ref().unwrap().try_recv() {
            Ok(result) => *snapshot = result.map_err(anyhow::Error::msg)?,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bail!("snapshot worker disconnected")
            }
        }
        Ok(())
    }
    pub fn drain_wakeup(&mut self) {
        let mut bytes = [0u8; 128];
        while matches!(self.wake.read(&mut bytes), Ok(n) if n > 0) {}
    }
}
impl Drop for SnapshotPump {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        signal_hook::low_level::unregister(self.resize_signal);
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
