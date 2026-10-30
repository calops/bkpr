use std::collections::BTreeMap;
#[derive(Clone, Copy)]
struct Process {
    pid: i32,
    parent: i32,
    session: i32,
    birth: u64,
}
fn identity(pid: i32) -> Option<Process> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat[stat.rfind(')')? + 1..].split_whitespace();
    fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    fields.next()?;
    let session = fields.next()?.parse().ok()?;
    let birth = fields.nth(15)?.parse().ok()?;
    Some(Process {
        pid,
        parent,
        session,
        birth,
    })
}
fn snapshot() -> Vec<Process> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .parse::<i32>()
                .ok()
                .and_then(identity)
        })
        .collect()
}
/// `isolated_owner` is ONLY the dedicated one-pane daemon process. It is never
/// enabled by in-process library tests or a general multi-pane host. Its exact
/// birth-bound children include orphaned double forks adopted by the subreaper.
pub struct Tree {
    root: i32,
    known: BTreeMap<i32, u64>,
    isolated_owner: Option<(i32, u64)>,
}
impl Tree {
    pub fn new(root: u32, isolated: bool) -> Self {
        let owner = if isolated {
            identity(std::process::id() as i32).map(|p| (p.pid, p.birth))
        } else {
            None
        };
        let mut tree = Self {
            root: root as i32,
            known: BTreeMap::new(),
            isolated_owner: owner,
        };
        if let Some(process) = identity(root as i32) {
            tree.known.insert(process.pid, process.birth);
        }
        tree.observe();
        tree
    }
    pub fn observe(&mut self) {
        let processes = snapshot();
        let live = processes
            .iter()
            .map(|p| (p.pid, p.birth))
            .collect::<BTreeMap<_, _>>();
        let session_owned = processes
            .iter()
            .any(|p| p.session == self.root && self.known.get(&p.pid) == Some(&p.birth));
        for process in &processes {
            let adopted = self.isolated_owner.is_some_and(|(owner, birth)| {
                process.parent == owner && live.get(&owner) == Some(&birth)
            });
            if adopted || session_owned && process.session == self.root {
                self.known.insert(process.pid, process.birth);
            }
        }
        loop {
            let mut changed = false;
            for process in &processes {
                if self
                    .known
                    .get(&process.parent)
                    .is_some_and(|birth| live.get(&process.parent) == Some(birth))
                    && self.known.get(&process.pid) != Some(&process.birth)
                {
                    self.known.insert(process.pid, process.birth);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    pub fn signal(&mut self, signal: i32) {
        self.observe();
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
        self.observe();
        self.known
            .iter()
            .any(|(&pid, &birth)| identity(pid).is_some_and(|p| p.birth == birth))
    }
}
