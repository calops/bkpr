use crate::harness_process::{self as process, ObservationLease};
use anyhow::{Context, Result};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock, Mutex, Weak},
};
#[derive(Clone, Copy)]
struct Process {
    pid: i32,
    birth: u64,
    session: i32,
}
fn identity(pid: i32) -> Option<Process> {
    // Lifecycle ownership includes zombies until waitpid reaps them; harness liveness does not.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(") ")?;
    let fields = tail.split_whitespace();
    let session = fields.clone().nth(3)?.parse().ok()?;
    let birth = fields.clone().nth(19)?.parse().ok()?;
    Some(Process {
        pid,
        birth,
        session,
    })
}
static TREES: LazyLock<Mutex<Vec<Weak<Mutex<Tracked>>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
struct Tracked {
    root: i32,
    // The OwnedChild wait handle, not a sampled table's presence, owns this root.
    root_birth: u64,
    known: BTreeMap<i32, u64>,
    isolated_owner: Option<(i32, u64)>,
}
/// A birth-bound pane tree. Only a dedicated one-pane subprocess may claim all
/// its adopted children; a shared daemon must prove ancestry or session ownership.
pub struct Tree {
    tracked: Arc<Mutex<Tracked>>,
    _observation: ObservationLease,
}
/// Called only by the existing off-path shared sampler, never an input actor.
/// Keep pane roots reserved for their owner's fresh exit barrier and output drain.
pub(crate) fn observe_sample(processes: &HashMap<i32, process::Process>) {
    let trees = {
        let mut registrations = TREES.lock().unwrap();
        let trees = registrations
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        registrations.retain(|tree| tree.strong_count() != 0);
        trees
    };
    let roots = trees
        .iter()
        .map(|tree| tree.lock().unwrap().root)
        .collect::<std::collections::BTreeSet<_>>();
    for tree in trees {
        let mut tree = tree.lock().unwrap();
        tree.observe_table(processes);
        tree.reap_adopted(processes, &roots);
    }
}
impl Tree {
    pub fn new(root: u32, isolated: bool) -> Result<Self> {
        let observation = ObservationLease::acquire()?;
        let root_identity =
            identity(root as i32).context("owned pane root identity unavailable")?;
        let owner = if isolated {
            identity(std::process::id() as i32).map(|p| (p.pid, p.birth))
        } else {
            None
        };
        let mut tracked = Tracked {
            root: root as i32,
            root_birth: root_identity.birth,
            known: BTreeMap::new(),
            isolated_owner: owner,
        };
        tracked.known.insert(root_identity.pid, root_identity.birth);
        let tracked = Arc::new(Mutex::new(tracked));
        TREES.lock().unwrap().push(Arc::downgrade(&tracked));
        let mut tree = Self {
            tracked,
            _observation: observation,
        };
        // Admission shares the worker's OS scan; no per-pane scanner.
        tree.observe_fresh();
        Ok(tree)
    }
    /// Observe after WNOWAIT reports exit, before any wait operation can reap the leader.
    pub fn observe_exit(&mut self) -> Result<()> {
        let table = process::fresh_snapshot()?;
        self.tracked.lock().unwrap().observe_table(&table);
        Ok(())
    }
    fn observe_fresh(&mut self) -> bool {
        match process::fresh_snapshot() {
            Ok(table) => {
                self.tracked.lock().unwrap().observe_table(&table);
                true
            }
            Err(_) => false,
        }
    }
    pub fn signal(&mut self, signal: i32) {
        self.observe_fresh();
        let tracked = self.tracked.lock().unwrap();
        if identity(tracked.root).is_some_and(|root| root.birth == tracked.root_birth) {
            unsafe {
                libc::kill(tracked.root, signal);
            }
        }
        for (&pid, &birth) in &tracked.known {
            if pid != tracked.root && identity(pid).is_some_and(|p| p.birth == birth) {
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        }
    }
    pub fn remaining(&mut self) -> Result<usize> {
        let table = process::fresh_snapshot()?;
        let mut tracked = self.tracked.lock().unwrap();
        tracked.observe_table(&table);
        let root = usize::from(
            identity(tracked.root).is_some_and(|root| root.birth == tracked.root_birth),
        );
        Ok(root
            + tracked
                .known
                .iter()
                .filter(|&(&pid, &birth)| {
                    pid != tracked.root && identity(pid).is_some_and(|p| p.birth == birth)
                })
                .count())
    }
}
impl Tracked {
    fn observe_table(&mut self, processes: &HashMap<i32, process::Process>) {
        // A shared sample can omit a newly admitted or concurrently changing
        // process. Absence is not proof that its owned wait identity disappeared.
        let root = processes
            .get(&self.root)
            .filter(|root| root.birth_ticks == self.root_birth)
            .map(|root| (root.birth_ticks, root.session))
            .or_else(|| identity(self.root).map(|root| (root.birth, root.session)));
        let root_owned = root.is_some_and(|(birth, _)| birth == self.root_birth);
        if root_owned {
            self.known.insert(self.root, self.root_birth);
        }
        let session_owned = root
            .is_some_and(|(birth, session)| birth == self.root_birth && session == self.root)
            || processes
                .values()
                .any(|p| p.session == self.root && self.known.get(&p.pid) == Some(&p.birth_ticks));
        self.known.retain(|pid, birth| {
            if *pid == self.root {
                return root_owned;
            }
            processes.get(pid).map_or_else(
                || identity(*pid).is_some_and(|process| process.birth == *birth),
                |process| process.birth_ticks == *birth,
            )
        });
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
    fn reap_adopted(
        &mut self,
        processes: &HashMap<i32, process::Process>,
        roots: &std::collections::BTreeSet<i32>,
    ) {
        let owner = std::process::id() as i32;
        self.known.retain(|&pid, &mut birth| {
            if pid == self.root
                || roots.contains(&pid)
                || !processes
                    .get(&pid)
                    .is_some_and(|p| p.parent == owner && p.stopped && p.birth_ticks == birth)
                || identity(pid).is_none_or(|p| p.birth != birth)
            {
                return true;
            }
            // Exact PID only: ECHILD leaves another parent's wait ownership alone.
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) != pid }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        os::unix::process::CommandExt,
        process::{Child, Command, Stdio},
    };

    struct WaitOwner(Child);
    impl Drop for WaitOwner {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn native_child() -> Child {
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", "read payload; exit 37"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().unwrap()
    }

    #[test]
    #[ignore = "isolated ownership fixture exercised by absent_sample_cannot_erase_owned_native_root"]
    fn absent_sample_cleanup_fixture() {
        let raw = native_child();
        let pid = raw.id();
        let mut root = crate::OwnedChild {
            inner: Box::new(raw),
            pid: Some(pid),
            cleaned: false,
            tree: Some(Tree::new(pid, false).unwrap()),
            exit_observed: false,
        };
        let mut independent = WaitOwner(native_child());
        let independent_pid = independent.0.id();
        let independent_birth = identity(independent_pid as i32).unwrap().birth;
        let stale = Tree::new(independent_pid, false).unwrap();
        {
            let mut tracked = stale.tracked.lock().unwrap();
            tracked.root_birth = tracked.root_birth.wrapping_add(1);
            tracked.observe_table(&HashMap::new());
        }
        let mut stale = stale;
        stale.signal(libc::SIGKILL);
        assert!(
            independent.0.try_wait().unwrap().is_none(),
            "a stale root birth must not signal the live replacement identity"
        );
        // Inject the sampling omission itself, then exercise the real native
        // cleanup/wait API rather than asserting retained internal map entries.
        root.tree
            .as_mut()
            .unwrap()
            .tracked
            .lock()
            .unwrap()
            .observe_table(&HashMap::new());
        root.terminate().unwrap();
        assert!(root.try_wait().unwrap().is_some());
        assert!(identity(pid as i32).is_none(), "owned root was not reaped");
        assert_eq!(
            identity(independent_pid as i32).unwrap().birth,
            independent_birth
        );
        independent
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"finish\n")
            .unwrap();
        assert_eq!(
            independent.0.wait().unwrap().code(),
            Some(37),
            "owned cleanup stole the independent child's wait ownership"
        );
    }

    #[test]
    fn absent_sample_cannot_erase_owned_native_root() {
        let result = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .args([
                "--ignored",
                "--exact",
                "process_tree::tests::absent_sample_cleanup_fixture",
                "--test-threads=1",
            ])
            .status()
            .unwrap();
        assert!(
            result.success(),
            "isolated native root ownership fixture failed"
        );
    }
}
