//! Best-effort, body-free daemon diagnostics; never part of a durable receipt.
//!
//! Dependency decision: crossbeam-channel (already used, MIT/Apache-2.0) supplies
//! bounded nonblocking admission and an indefinitely sleeping receiver. Evaluated
//! tracing-appender 0.2.5 (MIT, released 2026-04-17): its worker is idle-quiet, but
//! its time-based rolling appender does not enforce a retained byte bound or
//! no-follow opens. file-rotate 0.8.0 (MIT, released 2025-02-27) supplies byte/count
//! rotation, but opens paths itself and documents discarding filesystem errors.
//! Neither supplies our private descriptor-relative file checks. Keep that small
//! policy here, reusing libc, serde_json and crossbeam rather than a global logger.
//! Sources: docs.rs/tracing-appender/0.2.5/src/tracing_appender/worker.rs.html;
//! docs.rs/file-rotate/0.8.0/file_rotate/; crates.io API version metadata.
use anyhow::{Context, Result, ensure};
use crossbeam_channel::{Receiver, Sender, bounded, select_biased};
use serde::Serialize;
use std::{
    ffi::CStr,
    fs::{File, OpenOptions},
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// Fixed-size events bound queue memory as well as queue work. No request bodies
// or arbitrary strings can be retained. At most one event is being written.
const QUEUE_EVENTS: usize = 256;
const FILE_BYTES: u64 = 1024 * 1024;
const FILES: [&CStr; 4] = [
    c"events.jsonl",
    c"events.jsonl.1",
    c"events.jsonl.2",
    c"events.jsonl.3",
];
const SHUTDOWN_WAIT: Duration = Duration::from_millis(100);
static ACTIVE: OnceLock<Recorder> = OnceLock::new();

#[derive(Clone, Copy)]
pub(crate) struct OpaqueId([u8; 36]);
impl OpaqueId {
    pub(crate) fn new(value: &str) -> Option<Self> {
        // IDs are UUIDs, not titles, paths, capabilities or arbitrary caller text.
        if value.len() != 36
            || !value.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
        {
            return None;
        }
        let mut bytes = [0; 36];
        bytes.copy_from_slice(value.as_bytes());
        Some(Self(bytes))
    }
    pub(crate) fn as_str(&self) -> &str {
        // Construction accepts only ASCII.
        std::str::from_utf8(&self.0).expect("validated opaque ID")
    }
}
#[derive(Clone, Copy)]
struct Event {
    time_ms: u64,
    operation: &'static str,
    target: Option<OpaqueId>,
    outcome: &'static str,
}
fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
#[derive(Default)]
struct Counts {
    accepting: AtomicBool,
    admission: RwLock<()>,
    written: AtomicU64,
    dropped: AtomicU64,
    io_failures: AtomicU64,
    pending: AtomicU64,
    stopped: AtomicBool,
}
/// Counters describe diagnostic loss only, never request or message outcomes.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub written: u64,
    pub dropped: u64,
    pub io_failures: u64,
    pub pending: u64,
    pub stopped: bool,
}
impl Counts {
    fn report(&self) -> Report {
        Report {
            written: self.written.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            io_failures: self.io_failures.load(Ordering::Relaxed),
            pending: self.pending.load(Ordering::Relaxed),
            stopped: self.stopped.load(Ordering::Acquire),
        }
    }
}
#[derive(Clone)]
struct Recorder {
    queue: Sender<Event>,
    counts: Arc<Counts>,
}
impl Recorder {
    fn record(&self, operation: &'static str, target: Option<&str>, outcome: &'static str) {
        let Ok(_admission) = self.counts.admission.try_read() else {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let parsed_target = target.and_then(OpaqueId::new);
        if !self.counts.accepting.load(Ordering::Acquire)
            || !label(operation)
            || !label(outcome)
            || (target.is_some() && parsed_target.is_none())
        {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let event = Event {
            time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            operation,
            target: parsed_target,
            outcome,
        };
        self.counts.pending.fetch_add(1, Ordering::Relaxed);
        if self.queue.try_send(event).is_err() {
            self.counts.pending.fetch_sub(1, Ordering::Relaxed);
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
/// Nonblocking, allocation-free admission; no filesystem or registry access.
/// Disabled in library consumers unless their binary explicitly calls `install`.
pub fn record(operation: &'static str, target_id: Option<&str>, outcome: &'static str) {
    if let Some(recorder) = ACTIVE.get() {
        recorder.record(operation, target_id, outcome);
    }
}
/// Daemon-owned lifetime. Dropping never joins a possibly blocked filesystem worker.
#[must_use]
pub struct Guard {
    recorder: Recorder,
    stop: Sender<()>,
    done: Receiver<()>,
}
impl Guard {
    /// Give the worker at most 100ms to drain its fixed queue. No fsync/durability
    /// promise: a stalled filesystem or process exit may lose pending diagnostics.
    pub fn finish(self) -> Report {
        self.signal_stop();
        let _ = self.done.recv_timeout(SHUTDOWN_WAIT);
        self.recorder.counts.report()
    }
    fn signal_stop(&self) {
        self.recorder
            .counts
            .accepting
            .store(false, Ordering::Release);
        let _ = self.stop.try_send(());
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.signal_stop();
    }
}
/// Initialize once in bkprd, after state-root ownership is acquired, before panes
/// start. Failure disables diagnostics only; callers must not fail committed work.
pub fn install(root: &Path, session_id: &str) -> Result<Guard> {
    ensure!(
        ACTIVE.get().is_none(),
        "daemon diagnostics already installed"
    );
    let (recorder, guard) = start(root, session_id, FILE_BYTES)?;
    ensure!(
        ACTIVE.set(recorder).is_ok(),
        "daemon diagnostics already installed"
    );
    Ok(guard)
}
fn start(root: &Path, session_id: &str, file_bytes: u64) -> Result<(Recorder, Guard)> {
    let session =
        OpaqueId::new(session_id).context("diagnostics requires opaque session identity")?;
    let writer = PrivateFiles::open(root, file_bytes)?;
    let (queue, receiver) = bounded(QUEUE_EVENTS);
    let (stop, stopping) = bounded(1);
    let (finished, done) = bounded(1);
    let counts = Arc::new(Counts {
        accepting: AtomicBool::new(true),
        ..Counts::default()
    });
    let worker_counts = counts.clone();
    thread::Builder::new()
        .name("bkpr-diagnostics".into())
        .spawn(move || {
            run(writer, session, receiver, stopping, &worker_counts);
            worker_counts.stopped.store(true, Ordering::Release);
            let _ = finished.try_send(());
        })
        .context("starting diagnostic writer")?;
    let recorder = Recorder { queue, counts };
    let guard = Guard {
        recorder: recorder.clone(),
        stop,
        done,
    };
    Ok((recorder, guard))
}
fn run(
    mut writer: PrivateFiles,
    session: OpaqueId,
    queue: Receiver<Event>,
    stop: Receiver<()>,
    counts: &Counts,
) {
    loop {
        // No timer, empty-queue flush or periodic wake. Shutdown has priority over
        // traffic, then drains at most the bounded number already admitted.
        select_biased! {
            recv(stop) -> _ => break,
            recv(queue) -> event => match event {
                Ok(event) => if !write_event(&mut writer, &session, event, counts) { break; },
                Err(_) => break,
            }
        }
    }
    counts.accepting.store(false, Ordering::Release);
    // Fence any producer that passed the accepting check before shutdown. This
    // lock is worker-only blocking; producers only try_read and never wait.
    let admission = counts.admission.write().unwrap_or_else(|e| e.into_inner());
    drop(admission);
    for event in queue.try_iter().take(QUEUE_EVENTS) {
        if counts.io_failures.load(Ordering::Relaxed) != 0 {
            counts.dropped.fetch_add(1, Ordering::Relaxed);
            counts.pending.fetch_sub(1, Ordering::Relaxed);
        } else {
            write_event(&mut writer, &session, event, counts);
        }
    }
    if counts.io_failures.load(Ordering::Relaxed) == 0 {
        let summary = Event {
            time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            operation: "diagnostics.summary",
            target: None,
            outcome: "best_effort",
        };
        if writer.event(&session, summary, counts).is_err() {
            counts.io_failures.fetch_add(1, Ordering::Relaxed);
        }
    }
}
fn write_event(
    writer: &mut PrivateFiles,
    session: &OpaqueId,
    event: Event,
    counts: &Counts,
) -> bool {
    let written = writer.event(session, event, counts).is_ok();
    if written {
        counts.written.fetch_add(1, Ordering::Relaxed);
    } else {
        counts.io_failures.fetch_add(1, Ordering::Relaxed);
        counts.dropped.fetch_add(1, Ordering::Relaxed);
        counts.accepting.store(false, Ordering::Release);
    }
    counts.pending.fetch_sub(1, Ordering::Relaxed);
    written
}

// Descriptor-relative opens and renames pin the checked directory even if a
// pathname above it changes. Only four exact diagnostic filenames are managed;
// no traversal, directory scan, unrelated cleanup or recovery retention policy.
struct PrivateFiles {
    directory: File,
    current: File,
    bytes: u64,
    limit: u64,
}
fn private(file: &File, directory: bool) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file() && metadata.nlink() == 1
            },
        "diagnostic storage must be private, current-user-owned and not linked"
    );
    Ok(())
}
fn open_at(directory: &File, name: &CStr, flags: i32) -> std::io::Result<File> {
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}
impl PrivateFiles {
    fn open(root: &Path, limit: u64) -> Result<Self> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)?;
        private(&root, true)?;
        let created = unsafe { libc::mkdirat(root.as_raw_fd(), c"diagnostics".as_ptr(), 0o700) };
        if created != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        let directory = open_at(&root, c"diagnostics", libc::O_RDONLY | libc::O_DIRECTORY)?;
        private(&directory, true)?;
        // Check every managed file before modifying any. Do not follow a symlink
        // into user configuration, nor write through a hard link to another file.
        for name in FILES {
            Self::existing(&directory, name)?;
        }
        for name in FILES {
            if let Some(file) = Self::existing(&directory, name)?
                && file.metadata()?.len() > limit
            {
                Self::remove(&directory, name)?;
            }
        }
        let current = open_at(
            &directory,
            FILES[0],
            libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT,
        )?;
        private(&current, false)?;
        let bytes = current.metadata()?.len();
        Ok(Self {
            directory,
            current,
            bytes,
            limit,
        })
    }
    fn existing(directory: &File, name: &CStr) -> Result<Option<File>> {
        match open_at(directory, name, libc::O_RDONLY) {
            Ok(file) => {
                private(&file, false)?;
                Ok(Some(file))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn remove(directory: &File, name: &CStr) -> Result<()> {
        if Self::existing(directory, name)?.is_some()
            && unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    fn rotate(&mut self) -> Result<()> {
        // Preflight prevents a pre-existing unsafe backup from being overwritten.
        for name in FILES {
            Self::existing(&self.directory, name)?;
        }
        Self::remove(&self.directory, FILES[FILES.len() - 1])?;
        for index in (0..FILES.len() - 1).rev() {
            if Self::existing(&self.directory, FILES[index])?.is_some()
                && unsafe {
                    libc::renameat(
                        self.directory.as_raw_fd(),
                        FILES[index].as_ptr(),
                        self.directory.as_raw_fd(),
                        FILES[index + 1].as_ptr(),
                    )
                } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        self.current = open_at(
            &self.directory,
            FILES[0],
            libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_EXCL,
        )?;
        private(&self.current, false)?;
        self.bytes = 0;
        Ok(())
    }
    fn event(&mut self, session: &OpaqueId, event: Event, counts: &Counts) -> Result<()> {
        #[derive(Serialize)]
        struct Line<'a> {
            time_ms: u64,
            session_id: &'a str,
            operation: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            target_id: Option<&'a str>,
            outcome: &'static str,
            dropped_total: u64,
            io_failures_total: u64,
        }
        // Fixed stack storage also bounds serialization work; no per-event heap
        // buffer, user-controlled escaping or arbitrary-length formatted values.
        let mut bytes = [0_u8; 512];
        let mut line = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer(
            &mut line,
            &Line {
                time_ms: event.time_ms,
                session_id: session.as_str(),
                operation: event.operation,
                target_id: event.target.as_ref().map(OpaqueId::as_str),
                outcome: event.outcome,
                dropped_total: counts.dropped.load(Ordering::Relaxed),
                io_failures_total: counts.io_failures.load(Ordering::Relaxed),
            },
        )?;
        line.write_all(b"\n")?;
        let length = line.position();
        ensure!(length <= self.limit, "diagnostic event exceeds file limit");
        if self.bytes + length > self.limit {
            self.rotate()?;
        }
        self.current.write_all(&bytes[..length as usize])?;
        self.bytes += length;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
