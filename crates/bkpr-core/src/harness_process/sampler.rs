//! One bounded OS process-table reader for all panes and lifecycle observers.
use super::*;
use std::{
    sync::{Arc, Condvar, LazyLock, Mutex},
    thread,
    time::Duration,
};
type Table = HashMap<i32, Process>;
#[derive(Default)]
struct State {
    active: usize,
    requested: u64,
    completed: u64,
    table: Option<Arc<Table>>,
    error: Option<String>,
}
struct Sampler {
    state: Mutex<State>,
    wake: Condvar,
}
static SAMPLER: LazyLock<Mutex<Option<Arc<Sampler>>>> = LazyLock::new(|| Mutex::new(None));
pub(crate) struct ObservationLease(Arc<Sampler>);
impl ObservationLease {
    pub(crate) fn acquire() -> Result<Self> {
        let mut slot = SAMPLER.lock().unwrap();
        if slot.is_none() {
            let sampler = Arc::new(Sampler {
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
            });
            let worker = sampler.clone();
            thread::Builder::new()
                .name("bkpr-process-table".into())
                .spawn(move || sample(worker))?;
            *slot = Some(sampler);
        }
        let sampler = slot.as_ref().unwrap().clone();
        {
            let mut state = sampler.state.lock().unwrap();
            state.active += 1;
            state.requested = state.requested.wrapping_add(1);
            sampler.wake.notify_all();
        }
        Ok(Self(sampler))
    }
}
impl Drop for ObservationLease {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.active -= 1;
        self.0.wake.notify_all();
    }
}
fn shared() -> Result<Arc<Sampler>> {
    SAMPLER
        .lock()
        .unwrap()
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("process sampler is not running"))
}
fn published(state: &State) -> Result<Arc<Table>> {
    if let Some(error) = &state.error {
        bail!("process observation unavailable: {error}");
    }
    state
        .table
        .clone()
        .ok_or_else(|| anyhow::anyhow!("initial process observation pending"))
}
/// Cache-only, constant-time read. It never performs OS I/O or waits for a scan.
#[cfg(target_os = "linux")]
pub(crate) fn snapshot() -> Result<Arc<Table>> {
    let sampler = shared()?;
    let state = sampler.state.lock().unwrap();
    published(&state)
}
/// Explicit lifecycle barriers coalesce into the same worker scan. Not an input path API.
pub(crate) fn fresh_snapshot() -> Result<Arc<Table>> {
    let sampler = shared()?;
    let mut state = sampler.state.lock().unwrap();
    ensure_active(&state)?;
    state.requested = state
        .requested
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("process sample revision exhausted"))?;
    let target = state.requested;
    sampler.wake.notify_all();
    let (state, result) = sampler
        .wake
        .wait_timeout_while(state, Duration::from_secs(5), |s| {
            s.completed < target && s.active > 0
        })
        .unwrap();
    if result.timed_out() || state.completed < target {
        bail!("process sampler did not complete lifecycle barrier");
    }
    published(&state)
}
fn ensure_active(state: &State) -> Result<()> {
    if state.active == 0 {
        bail!("process sampler has no active executions");
    }
    Ok(())
}
fn sample(sampler: Arc<Sampler>) {
    loop {
        let mut state = sampler.state.lock().unwrap();
        while state.active == 0 {
            state = sampler.wake.wait(state).unwrap();
        }
        let requested = state.requested;
        drop(state);
        // All procfs/libproc work happens outside the publication mutex.
        let result = super::platform::snapshot();
        let mut state = sampler.state.lock().unwrap();
        match result {
            Ok(table) => {
                state.table = Some(Arc::new(table));
                state.error = None;
            }
            Err(error) => {
                state.table = None;
                state.error = Some(format!("{error:#}"));
            }
        }
        state.completed = requested;
        sampler.wake.notify_all();
        let _ = sampler
            .wake
            .wait_timeout_while(state, Duration::from_millis(100), |s| {
                s.active > 0 && s.requested == requested
            })
            .unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{process::Command, time::Instant};
    #[test]
    fn lifecycle_samples_keep_zombies_while_direct_capture_liveness_rejects_them() {
        let _lease = ObservationLease::acquire().unwrap();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while super::super::process(pid).unwrap().is_some() {
            assert!(Instant::now() < deadline, "owned fixture did not exit");
            thread::sleep(Duration::from_millis(5));
        }
        let sample = fresh_snapshot().unwrap();
        let zombie = sample
            .get(&pid)
            .expect("a not-yet-reaped owned child must remain in lifecycle sampling");
        assert!(zombie.stopped);
        assert!(super::super::process(pid).unwrap().is_none());
        child.wait().unwrap();
        assert!(fresh_snapshot().unwrap().get(&pid).is_none());
    }
}
