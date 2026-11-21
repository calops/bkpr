//! Bounded FIFO PTY writes and retained progress, separate from application consumption.
use anyhow::{Result, bail};
use bkpr_protocol::{WriteReceipt, WriterSnapshot};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};

/// Byte budget for unfinished user-input packets, including the active packet.
pub const USER_BYTES: usize = 2 * 1024 * 1024;
const TOTAL_BYTES: usize = 4 * 1024 * 1024;
struct Packet {
    id: u64,
    bytes: Vec<u8>,
}
struct State {
    queue: VecDeque<Packet>,
    receipts: VecDeque<WriteReceipt<&'static str>>,
    next: u64,
    bytes: usize,
    active: usize,
    stopping: bool,
    error: Option<String>,
}
/// Pane-local input queue; one worker preserves byte order.
pub struct Queue {
    state: Mutex<State>,
    wake: Condvar,
    changed: Box<dyn Fn() + Send + Sync>,
}
/// A checked FIFO admission that remains invisible until committed.
pub(super) struct Reservation<'a> {
    queue: &'a Queue,
    state: std::sync::MutexGuard<'a, State>,
    bytes: Vec<u8>,
}
impl Reservation<'_> {
    /// Publish the reserved packet; dropping the reservation aborts it instead.
    pub(super) fn commit(self) -> u64 {
        let Self {
            queue,
            mut state,
            bytes,
        } = self;
        if state.receipts.len() >= 128 {
            state.receipts.pop_front();
        }
        state.next += 1;
        let id = state.next;
        let total = bytes.len();
        state.bytes += total;
        state.active += 1;
        state.receipts.push_back(WriteReceipt {
            id,
            state: "queued",
            total,
            written: 0,
        });
        state.queue.push_back(Packet { id, bytes });
        drop(state);
        queue.wake.notify_one();
        (queue.changed)();
        id
    }
}
/// Owning writer-thread lifetime; drop requests stop and joins the thread.
pub struct Worker {
    queue: Arc<Queue>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    /// Stop admission and wait for the writer to finish or cancel queued packets.
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
    /// Start a writer for an already-open output handle; fails if thread creation fails.
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
    /// Admit bytes once or reject without enqueueing; internal replies have a reserved budget.
    pub fn enqueue(&self, bytes: Vec<u8>, internal: bool) -> Result<u64> {
        Ok(self.reserve(bytes, internal)?.commit())
    }
    /// Reserve ordered capacity for an internal report without exposing it to the writer.
    pub(super) fn reserve_internal(&self, bytes: Vec<u8>) -> Result<Reservation<'_>> {
        self.reserve(bytes, true)
    }
    fn reserve(&self, bytes: Vec<u8>, internal: bool) -> Result<Reservation<'_>> {
        let state = self.state.lock().unwrap();
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
        if state.receipts.len() >= 128
            && state
                .receipts
                .front()
                .is_some_and(|r| matches!(r.state, "queued" | "writing"))
        {
            bail!("PTY receipt capacity exhausted; input was not accepted");
        }
        Ok(Reservation {
            queue: self,
            state,
            bytes,
        })
    }
    /// Whether unfinished bytes have reached the user-input budget.
    pub fn congested(&self) -> bool {
        self.state.lock().unwrap().bytes >= USER_BYTES
    }
    /// Stop new admission and wake the worker to cancel unfinished packets.
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
    /// Copy bounded progress metadata without allocating state-label strings.
    pub fn snapshot(&self) -> WriterSnapshot<&'static str> {
        let state = self.state.lock().unwrap();
        WriterSnapshot {
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
    fn saturated_scheme_report_leaves_native_colors_and_scheme_retryable() {
        use crate::{AppliedColors, ColorUpdate, Terminal};
        use bkpr_protocol::{Effect, Size};

        struct GatedOutput {
            gate: Option<mpsc::Receiver<()>>,
            writes: mpsc::Sender<Vec<u8>>,
        }
        impl Write for GatedOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(gate) = self.gate.take() {
                    let _ = gate.recv();
                }
                self.writes.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (release, gate) = mpsc::channel();
        let (writes, written) = mpsc::channel();
        let (changed, changes) = mpsc::channel();
        let (queue, worker) = Queue::start(
            Box::new(GatedOutput {
                gate: Some(gate),
                writes,
            }),
            Box::new(move || {
                let _ = changed.send(());
            }),
        )
        .unwrap();
        // Release the output gate before the writer joins, including on unwind.
        let release_output = release;
        let mut terminal = Terminal::new(Size {
            cols: 6,
            rows: 2,
            cell_width: 8,
            cell_height: 16,
        })
        .unwrap();
        let mut colors = AppliedColors::default();
        let theme = |foreground, background, ansi| ColorUpdate {
            view_id: "owner".into(),
            foreground: Some(foreground),
            background: Some(background),
            cursor: None,
            palette: None,
            ansi,
            observed_ansi: None,
        };
        let dark = theme([200; 3], [0; 3], None);
        let (_, report) = colors.apply(&mut terminal, &queue, dark, None).unwrap();
        assert!(report.is_none());
        terminal.feed(b"\x1b[?2031h").unwrap();
        for _ in 0..128 {
            queue.enqueue(b"x".to_vec(), true).unwrap();
        }

        let light = || theme([10; 3], [255; 3], Some([[33; 3]; 16]));
        let target = colors.target_palette(&light());
        let Err(rejection) = colors.apply(&mut terminal, &queue, light(), target.as_ref()) else {
            panic!("saturated writer accepted the theme");
        };
        release_output.send(()).unwrap();
        assert!(rejection.to_string().contains("queue full"));
        let frame = terminal.snapshot().unwrap();
        assert_eq!(frame.foreground, [200; 3]);
        assert_eq!(frame.background, [0; 3]);
        assert_eq!(frame.palette.as_slice(), Terminal::factory_palette());
        assert!(matches!(
            terminal.feed(b"\x1b[?996n").unwrap().as_slice(),
            [Effect::PtyWrite(bytes)] if bytes.as_slice() == b"\x1b[?997;1n"
        ));

        for _ in 0..128 {
            assert_eq!(
                written
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .as_slice(),
                b"x"
            );
        }
        while queue.snapshot().active != 0 {
            changes.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        queue.enqueue(b"earlier".to_vec(), false).unwrap();
        let target = colors.target_palette(&light());
        let (changed, report) = colors
            .apply(&mut terminal, &queue, light(), target.as_ref())
            .unwrap();
        assert!(changed);
        report.unwrap().commit();
        queue.enqueue(b"later".to_vec(), false).unwrap();
        for expected in [b"earlier".as_slice(), b"\x1b[?997;2n", b"later"] {
            assert_eq!(
                written
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .as_slice(),
                expected
            );
        }
        assert!(matches!(
            terminal.feed(b"\x1b[?996n").unwrap().as_slice(),
            [Effect::PtyWrite(bytes)] if bytes.as_slice() == b"\x1b[?997;2n"
        ));
        let current = terminal.snapshot().unwrap();
        assert_eq!(current.foreground, [10; 3]);
        assert_eq!(current.background, [255; 3]);
        assert_eq!(current.palette[..16], [[33; 3]; 16]);
        worker.join().unwrap();
    }

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
