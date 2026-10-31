use crate::harness_process::{self as process, ObservationLease};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
#[derive(Clone, Copy)]
struct Process {
    pid: i32,
    birth: u64,
}
fn identity(pid: i32) -> Option<Process> {
    // Lifecycle ownership includes zombies until waitpid reaps them; harness liveness does not.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(") ")?;
    let birth = tail.split_whitespace().nth(19)?.parse().ok()?;
    Some(Process { pid, birth })
}
/// `isolated_owner` is ONLY the dedicated one-pane daemon process. It is never
/// enabled by in-process library tests or a general multi-pane host. Its exact
/// birth-bound children include orphaned double forks adopted by the subreaper.
pub struct Tree {
    root: i32,
    known: BTreeMap<i32, u64>,
    isolated_owner: Option<(i32, u64)>,
    _observation: ObservationLease,
}
impl Tree {
    pub fn new(root: u32, isolated: bool) -> Result<Self> {
        let observation = ObservationLease::acquire()?;
        let owner = if isolated {
            identity(std::process::id() as i32).map(|p| (p.pid, p.birth))
        } else {
            None
        };
        let mut tree = Self {
            root: root as i32,
            known: BTreeMap::new(),
            isolated_owner: owner,
            _observation: observation,
        };
        if let Some(process) = identity(root as i32) {
            tree.known.insert(process.pid, process.birth);
        }
        // Initial lifecycle admission is a barrier on the shared worker, not a new per-pane scan.
        tree.observe_fresh();
        Ok(tree)
    }
    /// Cache-only periodic observation: pane input actors never scan procfs.
    pub fn observe(&mut self) {
        if let Ok(table) = process::snapshot() {
            self.observe_table(&table);
        }
    }
    /// Observe after WNOWAIT reports exit, before any wait operation can reap the leader.
    /// Cached absence cannot recover jobs reparented out of a just-exited process tree.
    pub fn observe_exit(&mut self) -> Result<()> {
        let table = process::fresh_snapshot()?;
        self.observe_table(&table);
        Ok(())
    }
    fn observe_fresh(&mut self) -> bool {
        match process::fresh_snapshot() {
            Ok(table) => {
                self.observe_table(&table);
                true
            }
            Err(_) => false,
        }
    }
    fn observe_table(&mut self, processes: &HashMap<i32, process::Process>) {
        let session_owned = processes
            .values()
            .any(|p| p.session == self.root && self.known.get(&p.pid) == Some(&p.birth_ticks));
        self.known
            .retain(|pid, birth| processes.get(pid).is_some_and(|p| p.birth_ticks == *birth));
        for p in processes.values() {
            let adopted = self.isolated_owner.is_some_and(|(owner, birth)| {
                p.parent == owner
                    && processes
                        .get(&owner)
                        .is_some_and(|p| p.birth_ticks == birth)
            });
            if adopted || session_owned && p.session == self.root {
                self.known.insert(p.pid, p.birth_ticks);
            }
        }
        loop {
            let mut changed = false;
            for p in processes.values() {
                if self.known.get(&p.parent).is_some_and(|birth| {
                    processes
                        .get(&p.parent)
                        .is_some_and(|parent| parent.birth_ticks == *birth)
                }) && self.known.get(&p.pid) != Some(&p.birth_ticks)
                {
                    self.known.insert(p.pid, p.birth_ticks);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    pub fn signal(&mut self, signal: i32) {
        self.observe_fresh();
        for (&pid, &birth) in &self.known {
            if identity(pid).is_some_and(|p| p.birth == birth) {
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        }
    }
    pub fn reap(&self) {
        for (&pid, &birth) in &self.known {
            if pid != self.root && identity(pid).is_none_or(|p| p.birth == birth) {
                unsafe {
                    libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
        }
    }
    pub fn remaining(&mut self) -> bool {
        if !self.observe_fresh() {
            return true;
        }
        self.known
            .iter()
            .any(|(&pid, &birth)| identity(pid).is_some_and(|p| p.birth == birth))
    }
}
