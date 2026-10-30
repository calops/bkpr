use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    collections::VecDeque,
    io::Write,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};

pub const USER_BYTES: usize = 2 * 1024 * 1024;
const TOTAL_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Serialize)]
pub struct Receipt {
    pub id: u64,
    pub state: &'static str,
    pub total: usize,
    pub written: usize,
}
struct Packet {
    id: u64,
    bytes: Vec<u8>,
}
struct State {
    queue: VecDeque<Packet>,
    receipts: VecDeque<Receipt>,
    next: u64,
    bytes: usize,
    active: usize,
    stopping: bool,
    error: Option<String>,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub queued_bytes: usize,
    pub active: usize,
    pub receipts: Vec<Receipt>,
    pub stopped: bool,
    pub error: Option<String>,
}
pub struct Queue {
    state: Mutex<State>,
    wake: Condvar,
    changed: Box<dyn Fn() + Send + Sync>,
}
pub struct Worker {
    queue: Arc<Queue>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    pub fn join(mut self) -> thread::Result<()> {
        self.queue.stop();
        self.thread.take().unwrap().join()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.queue.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Queue {
    pub fn start(
        mut output: Box<dyn Write + Send>,
        changed: Box<dyn Fn() + Send + Sync>,
    ) -> Result<(Arc<Self>, Worker)> {
        let queue = Arc::new(Self {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                receipts: VecDeque::new(),
                next: 0,
                bytes: 0,
                active: 0,
                stopping: false,
                error: None,
            }),
            wake: Condvar::new(),
            changed,
        });
        let worker = queue.clone();
        let thread = thread::Builder::new()
            .name("bkpr-pty-writer".into())
            .spawn(move || {
                loop {
                    let packet = {
                        let mut state = worker.state.lock().unwrap();
                        while state.queue.is_empty() && !state.stopping {
                            state = worker.wake.wait(state).unwrap();
                        }
                        if state.stopping {
                            break;
                        }
                        state.queue.pop_front().unwrap()
                    };
                    let mut written = 0;
                    let mut outcome = "written";
                    while written < packet.bytes.len() {
                        if worker.state.lock().unwrap().stopping {
                            outcome = "cancelled";
                            break;
                        }
                        match output.write(&packet.bytes[written..]) {
                            Ok(0) => {
                                outcome = "failed";
                                worker.fail("PTY writer accepted zero bytes".into());
                                break;
                            }
                            Ok(n) => {
                                written += n;
                                let mut state = worker.state.lock().unwrap();
                                if let Some(receipt) =
                                    state.receipts.iter_mut().find(|r| r.id == packet.id)
                                {
                                    receipt.state = "writing";
                                    receipt.written = written;
                                }
                                drop(state);
                                (worker.changed)();
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                let state = worker.state.lock().unwrap();
                                if !state.stopping {
                                    let _ = worker
                                        .wake
                                        .wait_timeout(state, Duration::from_millis(4))
                                        .unwrap();
                                }
                            }
                            Err(error) => {
                                outcome = "failed";
                                worker.fail(format!("PTY writer I/O failure: {error}"));
                                break;
                            }
                        }
                    }
                    let mut state = worker.state.lock().unwrap();
                    state.bytes -= packet.bytes.len();
                    state.active -= 1;
                    if let Some(receipt) = state.receipts.iter_mut().find(|r| r.id == packet.id) {
                        receipt.state = outcome;
                        receipt.written = written;
                    }
                    let stopping = state.stopping;
                    drop(state);
                    (worker.changed)();
                    if stopping {
                        break;
                    }
                }
                let mut state = worker.state.lock().unwrap();
                while let Some(packet) = state.queue.pop_front() {
                    state.bytes -= packet.bytes.len();
                    state.active -= 1;
                    if let Some(receipt) = state.receipts.iter_mut().find(|r| r.id == packet.id) {
                        receipt.state = "cancelled";
                    }
                }
                drop(state);
                (worker.changed)();
            })?;
        Ok((
            queue.clone(),
            Worker {
                queue,
                thread: Some(thread),
            },
        ))
    }
    fn fail(&self, error: String) {
        let mut state = self.state.lock().unwrap();
        state.error = Some(error);
        state.stopping = true;
        drop(state);
        (self.changed)();
        self.wake.notify_all();
    }
    pub fn enqueue(&self, bytes: Vec<u8>, internal: bool) -> Result<u64> {
        let mut state = self.state.lock().unwrap();
        if state.stopping {
            bail!("PTY writer is stopped; input was not accepted");
        }
        let (byte_limit, packet_limit) = if internal {
            (TOTAL_BYTES, 128)
        } else {
            (USER_BYTES, 64)
        };
        if state.bytes.saturating_add(bytes.len()) > byte_limit || state.active >= packet_limit {
            bail!(
                "PTY input queue full; input was not accepted or dispatched; retry only this rejected request"
            );
        }
        if state.receipts.len() >= 128 {
            if state
                .receipts
                .front()
                .is_some_and(|r| matches!(r.state, "queued" | "writing"))
            {
                bail!("PTY receipt capacity exhausted; input was not accepted");
            }
            state.receipts.pop_front();
        }
        state.next += 1;
        let id = state.next;
        let total = bytes.len();
        state.bytes += total;
        state.active += 1;
        state.receipts.push_back(Receipt {
            id,
            state: "queued",
            total,
            written: 0,
        });
        state.queue.push_back(Packet { id, bytes });
        self.wake.notify_one();
        drop(state);
        (self.changed)();
        Ok(id)
    }
    pub fn congested(&self) -> bool {
        self.state.lock().unwrap().bytes >= USER_BYTES
    }
    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        if state.stopping {
            return;
        }
        state.stopping = true;
        drop(state);
        (self.changed)();
        self.wake.notify_all();
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap();
        Snapshot {
            queued_bytes: state.bytes,
            active: state.active,
            receipts: state.receipts.iter().cloned().collect(),
            stopped: state.stopping,
            error: state.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn receipt_completion_and_stop_notify_without_holding_writer_state() {
        let (changed, changes) = mpsc::channel();
        let (queue, worker) = Queue::start(
            Box::new(std::io::sink()),
            Box::new(move || {
                let _ = changed.send(());
            }),
        )
        .unwrap();
        let id = queue.enqueue(b"input".to_vec(), false).unwrap();
        loop {
            let snapshot = queue.snapshot();
            if snapshot
                .receipts
                .iter()
                .any(|receipt| receipt.id == id && receipt.state == "written")
            {
                assert_eq!(snapshot.queued_bytes, 0);
                assert_eq!(snapshot.active, 0);
                break;
            }
            changes.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        worker.join().unwrap();
        assert!(queue.snapshot().stopped);
        assert!(changes.try_recv().is_ok());
    }

    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn asynchronous_writer_fault_notifies_snapshot_consumers() {
        let (changed, changes) = mpsc::channel();
        let (queue, worker) = Queue::start(
            Box::new(BrokenWriter),
            Box::new(move || {
                let _ = changed.send(());
            }),
        )
        .unwrap();
        queue.enqueue(b"input".to_vec(), false).unwrap();
        while queue.snapshot().error.is_none() {
            changes.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        worker.join().unwrap();
        let snapshot = queue.snapshot();
        assert!(snapshot.stopped);
        assert_eq!(snapshot.receipts[0].state, "failed");
    }
}
