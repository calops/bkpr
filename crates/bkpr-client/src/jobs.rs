use anyhow::{Context, Result, anyhow, bail};
use std::{
    any::Any,
    collections::HashSet,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

pub(crate) struct Completion<T> {
    pub key: String,
    pub result: Result<T>,
}

struct Job<T> {
    key: String,
    run: Box<dyn FnOnce() -> Result<T> + Send + 'static>,
}

/// Fixed workers and bounded queues keep both running work and completed,
/// unconsumed results within the admission limit. Only shutdown joins workers.
pub(crate) struct Jobs<T: Send + 'static> {
    limit: usize,
    pending: HashSet<String>,
    sender: Option<mpsc::SyncSender<Job<T>>>,
    receiver: Option<mpsc::Receiver<Completion<T>>>,
    workers: Vec<JoinHandle<()>>,
    pub wake: UnixStream,
}

impl<T: Send + 'static> Jobs<T> {
    pub fn new(limit: usize) -> Result<Self> {
        if limit == 0 {
            bail!("background job limit must be positive");
        }
        let (wake, notify) = UnixStream::pair().context("creating background job wake channel")?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let (sender, jobs) = mpsc::sync_channel::<Job<T>>(limit);
        let (completed, receiver) = mpsc::sync_channel(limit);
        let jobs = Arc::new(Mutex::new(jobs));
        let mut dispatcher = Self {
            limit,
            pending: HashSet::new(),
            sender: Some(sender),
            receiver: Some(receiver),
            workers: Vec::new(),
            wake,
        };
        for _ in 0..limit {
            let jobs = Arc::clone(&jobs);
            let completed = completed.clone();
            let mut notify = notify
                .try_clone()
                .context("cloning background job wake channel")?;
            let worker = thread::Builder::new()
                .spawn(move || {
                    loop {
                        // The receiver lock protects only queue access, never a job.
                        let job = {
                            let receiver = jobs.lock().unwrap_or_else(|error| error.into_inner());
                            receiver.recv()
                        };
                        let Ok(Job { key, run }) = job else {
                            return;
                        };
                        let result = match catch_unwind(AssertUnwindSafe(run)) {
                            Ok(result) => result,
                            Err(panic) => {
                                Err(crate::transport::mark_outcome_unknown(panic_error(panic)))
                            }
                        };
                        if completed.send(Completion { key, result }).is_err() {
                            // Drop closes the result receiver before joining. Still
                            // run accepted queued jobs, without blocking on results.
                            continue;
                        }
                        if signal(&mut notify).is_err() {
                            return;
                        }
                    }
                })
                .context("starting background job worker")?;
            dispatcher.workers.push(worker);
        }
        Ok(dispatcher)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.pending.contains(key)
    }

    pub fn is_full(&self) -> bool {
        self.pending.len() >= self.limit
    }

    pub fn submit(
        &mut self,
        key: String,
        job: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<()> {
        if self.contains(&key) {
            bail!("background job already pending: {key}");
        }
        if self.is_full() {
            bail!("background job limit reached");
        }
        self.check_workers()?;
        let sender = self
            .sender
            .as_ref()
            .context("background jobs are shut down")?;
        let queued = Job {
            key: key.clone(),
            run: Box::new(job),
        };
        match sender.try_send(queued) {
            Ok(()) => {
                self.pending.insert(key);
                Ok(())
            }
            Err(mpsc::TrySendError::Full(_)) => bail!("background job queue is full"),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                bail!("background job workers disconnected")
            }
        }
    }

    /// Drain notifications before results: a concurrent publication either gets
    /// consumed here or leaves a readable notification for the next UI turn.
    pub fn latest(&mut self) -> Result<Vec<Completion<T>>> {
        let mut bytes = [0u8; 256];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => bail!("background job wake channel closed"),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error).context("reading background job wake channel"),
            }
        }
        let mut completions = Vec::new();
        loop {
            let received = self
                .receiver
                .as_ref()
                .context("background jobs are shut down")?
                .try_recv();
            match received {
                Ok(completion) => {
                    if !self.pending.remove(&completion.key) {
                        bail!(
                            "completion for an unknown background job: {}",
                            completion.key
                        );
                    }
                    completions.push(completion);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    bail!("background job workers disconnected")
                }
            }
        }
        self.check_workers()?;
        Ok(completions)
    }

    /// Shutdown is the only blocking path: close admission, collect every
    /// outstanding accepted result, then join the fixed worker pool.
    pub fn finish(mut self) -> Result<Vec<Completion<T>>> {
        self.sender.take();
        let mut completions = Vec::with_capacity(self.pending.len());
        let mut failure = None;
        while !self.pending.is_empty() {
            let received = self.receiver.as_ref().unwrap().recv();
            match received {
                Ok(completion) => {
                    if !self.pending.remove(&completion.key) {
                        failure = Some(anyhow!(
                            "completion for an unknown background job: {}",
                            completion.key
                        ));
                        break;
                    }
                    completions.push(completion);
                }
                Err(_) => {
                    failure = Some(anyhow!(
                        "background workers exited without completing accepted jobs"
                    ));
                    break;
                }
            }
        }
        // Even an internal failure must not leave a sender blocked on shutdown.
        self.receiver.take();
        let joined = self.join_workers();
        if let Some(error) = failure {
            return Err(error);
        }
        joined?;
        Ok(completions)
    }

    fn check_workers(&self) -> Result<()> {
        if self.workers.iter().any(JoinHandle::is_finished) {
            bail!("background job worker exited unexpectedly");
        }
        Ok(())
    }

    fn join_workers(&mut self) -> Result<()> {
        let mut failure = None;
        for worker in self.workers.drain(..) {
            if let Err(panic) = worker.join() {
                failure.get_or_insert_with(|| panic_error(panic));
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<T: Send + 'static> Drop for Jobs<T> {
    fn drop(&mut self) {
        self.sender.take();
        self.receiver.take();
        let _ = self.join_workers();
    }
}

fn panic_error(panic: Box<dyn Any + Send>) -> anyhow::Error {
    if let Some(message) = panic.downcast_ref::<String>() {
        anyhow!("background job panicked: {message}")
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        anyhow!("background job panicked: {message}")
    } else {
        anyhow!("background job panicked with a non-string payload")
    }
}

fn signal(notify: &mut UnixStream) -> io::Result<()> {
    loop {
        match notify.write(&[1]) {
            Ok(1) => return Ok(()),
            Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            // A full nonblocking socket is already readable. The result was
            // queued first, so draining that wake cannot lose its completion.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    fn wait_for_wake<T: Send + 'static>(jobs: &mut Jobs<T>) {
        jobs.wake.set_nonblocking(false).unwrap();
        jobs.wake
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        jobs.wake.read_exact(&mut [0]).unwrap();
        jobs.wake.set_nonblocking(true).unwrap();
    }

    #[test]
    fn distinct_keys_progress_while_another_job_is_blocked() {
        let mut jobs = Jobs::new(2).unwrap();
        let (release, blocked) = mpsc::channel();
        let (started, running) = mpsc::channel();
        jobs.submit("blocked".into(), move || {
            started.send(())?;
            blocked.recv()?;
            Ok(1)
        })
        .unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        jobs.submit("ready".into(), || Ok(2)).unwrap();
        wait_for_wake(&mut jobs);
        let mut completed = jobs.latest().unwrap();
        assert_eq!(completed.len(), 1);
        let completion = completed.pop().unwrap();
        assert_eq!(completion.key, "ready");
        assert_eq!(completion.result.unwrap(), 2);
        assert!(!jobs.contains("ready"));
        assert!(jobs.contains("blocked"));
        release.send(()).unwrap();
        let mut completed = jobs.finish().unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed.pop().unwrap().result.unwrap(), 1);
    }

    #[test]
    fn duplicate_and_limit_rejection_include_unconsumed_results() {
        let mut jobs = Jobs::new(2).unwrap();
        let (release_one, blocked_one) = mpsc::channel();
        let (release_two, blocked_two) = mpsc::channel();
        jobs.submit("one".into(), move || {
            blocked_one.recv()?;
            Ok(1)
        })
        .unwrap();
        jobs.submit("two".into(), move || {
            blocked_two.recv()?;
            Ok(2)
        })
        .unwrap();
        let rejected_ran = Arc::new(AtomicBool::new(false));
        for key in ["one", "three"] {
            let ran = Arc::clone(&rejected_ran);
            assert!(
                jobs.submit(key.into(), move || {
                    ran.store(true, Ordering::Relaxed);
                    Ok(99)
                })
                .is_err()
            );
        }
        release_one.send(()).unwrap();
        wait_for_wake(&mut jobs);
        assert!(jobs.contains("one"));
        assert!(jobs.is_full());
        assert!(jobs.submit("three".into(), || Ok(3)).is_err());
        let mut completed = jobs.latest().unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed.pop().unwrap().result.unwrap(), 1);
        assert!(!jobs.contains("one"));
        assert!(!jobs.is_full());
        // Reusing a consumed key never depends on a worker exiting.
        jobs.submit("one".into(), || Ok(3)).unwrap();
        release_two.send(()).unwrap();
        let mut values: Vec<_> = jobs
            .finish()
            .unwrap()
            .into_iter()
            .map(|completion| completion.result.unwrap())
            .collect();
        values.sort_unstable();
        assert_eq!(values, [2, 3]);
        assert!(!rejected_ran.load(Ordering::Relaxed));
    }

    #[test]
    fn finish_collects_all_accepted_work() {
        let mut jobs = Jobs::new(2).unwrap();
        let (release_one, blocked_one) = mpsc::channel();
        let (release_two, blocked_two) = mpsc::channel();
        jobs.submit("one".into(), move || {
            blocked_one.recv()?;
            Ok(1)
        })
        .unwrap();
        jobs.submit("two".into(), move || {
            blocked_two.recv()?;
            Ok(2)
        })
        .unwrap();
        release_two.send(()).unwrap();
        release_one.send(()).unwrap();
        let mut completed = jobs.finish().unwrap();
        completed.sort_by(|left, right| left.key.cmp(&right.key));
        assert_eq!(completed.len(), 2);
        assert_eq!(completed.remove(0).result.unwrap(), 1);
        assert_eq!(completed.remove(0).result.unwrap(), 2);
    }

    #[test]
    fn unknown_outcome_marker_survives_worker_completion() {
        let mut jobs = Jobs::new(1).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            BufReader::new(&mut stream)
                .read_until(b'\n', &mut request)
                .unwrap();
            // Accept the request but lose its reply: replay is not safe.
        });
        jobs.submit("unknown".into(), move || {
            crate::transport::checked(&socket, bkpr_protocol::Operation::Shutdown)
                .map(|_| ())
                .context("dispatching a background mutation")
        })
        .unwrap();
        let mut completed = jobs.finish().unwrap();
        server.join().unwrap();
        let error = completed.pop().unwrap().result.unwrap_err();
        assert!(crate::transport::outcome_unknown(&error), "{error:#}");
    }

    #[test]
    fn panic_is_an_explicit_failed_completion() {
        let mut jobs = Jobs::<()>::new(1).unwrap();
        jobs.submit("panic".into(), || panic!("controlled worker panic"))
            .unwrap();
        let mut completed = jobs.finish().unwrap();
        let completion = completed.pop().unwrap();
        assert_eq!(completion.key, "panic");
        let error = completion.result.unwrap_err();
        assert!(format!("{error:#}").contains("controlled worker panic"));
        assert!(crate::transport::outcome_unknown(&error), "{error:#}");
    }
}
