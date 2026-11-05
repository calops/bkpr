use super::*;
use crate::{
    harness::{self, Peer},
    harness_process::{self as proc, Process},
};
use std::sync::atomic::Ordering;

impl Registry {
    pub(super) fn pane_context(&self, id: &str) -> Result<Value> {
        let pane = {
            let state = self.inner.state.lock().unwrap();
            context_pane(&state, id)?.0.clone()
        };
        let root = pane
            .shared
            .root
            .lock()
            .unwrap()
            .clone()
            .context("pane has no native execution root")?;
        current_root(&root)?;
        self.publish_context(id, &pane, &root)
    }

    pub(super) fn context_for_peer(&self, peer: &Peer) -> Result<Value> {
        let mut caller = peer.current()?;
        ensure!(
            !caller.stopped && caller.tty != 0,
            "context peer has no live controlling PTY"
        );
        let mut selected = self.select_context(&caller)?;
        if selected.is_none() {
            // A root can connect before spawn publishes its native handle. Waiting
            // only admits another lookup; OS provenance is checked again afterward.
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, SessionClosing("daemon is shutting down"));
            #[cfg(test)]
            if state.spawning > 0
                && let Some(entered) = self.inner.capture_admission_wait.lock().unwrap().take()
            {
                let _ = entered.try_send(());
            }
            let (state, deadline) = self
                .inner
                .changed
                .wait_timeout_while(state, Duration::from_secs(8), |state| {
                    state.spawning > 0 && !state.closing && state.durability_error.is_none()
                })
                .unwrap();
            healthy(&state)?;
            ensure!(!state.closing, SessionClosing("daemon is shutting down"));
            ensure!(
                !deadline.timed_out() || state.spawning == 0,
                "context admission timed out while pane spawn is pending"
            );
            drop(state);
            caller = peer.current()?;
            ensure!(
                !caller.stopped && caller.tty != 0,
                "context peer has no live controlling PTY"
            );
            selected = self.select_context(&caller)?;
        }
        let (id, pane, root) =
            selected.context("context peer is not a live native pane descendant on its PTY")?;
        // Sampling is off-lock, then the final registry read checks the exact
        // execution and reads current labels rather than the selection snapshot.
        let caller = peer.current()?;
        context_lineage(&caller, &root)?;
        self.publish_context(&id, &pane, &root)
    }

    fn select_context(&self, caller: &Process) -> Result<Option<(String, Pane, Process)>> {
        let panes = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, SessionClosing("daemon is shutting down"));
            state
                .live
                .iter()
                .filter(|(_, pane)| !pane.shared.finished.load(Ordering::Acquire))
                .map(|(id, pane)| (id.clone(), pane.clone()))
                .collect::<Vec<_>>()
        };
        let mut selected = None;
        for (id, pane) in panes {
            let Some(root) = pane.shared.root.lock().unwrap().clone() else {
                continue;
            };
            if caller.tty != root.tty || context_lineage(caller, &root).is_err() {
                continue;
            }
            ensure!(selected.is_none(), "ambiguous native pane context");
            selected = Some((id, pane, root));
        }
        Ok(selected)
    }

    fn publish_context(&self, id: &str, pane: &Pane, root: &Process) -> Result<Value> {
        let state = self.inner.state.lock().unwrap();
        let (current, info) = context_pane(&state, id)?;
        ensure!(
            Arc::ptr_eq(&current.shared, &pane.shared)
                && current.shared.root.lock().unwrap().as_ref() == Some(root),
            "pane execution changed during context lookup"
        );
        let workspace = info
            .workspace_id
            .as_ref()
            .map(|id| {
                state
                    .record
                    .registry
                    .workspaces
                    .iter()
                    .find(|workspace| &workspace.id == id)
                    .context("pane execution workspace is unavailable")
            })
            .transpose()?;
        let window = info
            .window_id
            .as_ref()
            .map(|id| {
                workspace
                    .and_then(|workspace| workspace.windows.iter().find(|window| &window.id == id))
                    .context("pane execution window is unavailable")
            })
            .transpose()?;
        let mut pane_title = info.title.clone();
        super::project_title(&state, id, &mut pane_title);
        let context = PaneContext {
            session: self.inner.session.clone(),
            pane_id: info.id.clone(),
            pane_title,
            workspace_id: workspace.map(|workspace| workspace.id.clone()),
            workspace_name: workspace.map(|workspace| workspace.name.clone()),
            window_id: window.map(|window| window.id.clone()),
            window_name: window.map(|window| window.name.clone()),
            revision: state.record.registry.revision,
        };
        drop(state);
        Ok(serde_json::to_value(context)?)
    }
}

fn context_pane<'a>(state: &'a State, id: &str) -> Result<(&'a Pane, &'a PaneInfo)> {
    healthy(state)?;
    ensure!(!state.closing, SessionClosing("daemon is shutting down"));
    ensure!(!state.closing_panes.contains(id), "pane is closing");
    ensure!(
        !state.closing_document_panes.contains(id),
        "pane document is closing"
    );
    let info = state
        .record
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == id)
        .context("unknown pane")?;
    ensure!(info.status == PaneStatus::Running, "pane is not running");
    ensure!(
        info.workspace_id
            .as_ref()
            .is_none_or(|id| !state.closing_workspaces.contains(id)),
        "pane workspace is closing"
    );
    let pane = state
        .live
        .get(id)
        .context("pane execution is unavailable")?;
    ensure!(
        !pane.shared.finished.load(Ordering::Acquire),
        "pane execution ended"
    );
    Ok((pane, info))
}

fn current_root(root: &Process) -> Result<Process> {
    let current = proc::process(root.pid)?.context("pane native execution ended")?;
    ensure!(
        current.identity == root.identity && current.tty != 0 && current.tty == root.tty,
        "pane native execution changed"
    );
    ensure!(!current.stopped, "pane native execution is stopped");
    Ok(current)
}

fn context_lineage(caller: &Process, root: &Process) -> Result<()> {
    ensure!(
        !caller.stopped && caller.tty != 0 && caller.tty == root.tty,
        "context peer is outside pane PTY"
    );
    let root = current_root(root)?;
    let chain = harness::ancestry(caller, &root)?;
    ensure!(
        chain
            .iter()
            .all(|process| !process.stopped && process.tty == root.tty),
        "context peer crossed a detached or nested PTY"
    );
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "context_tests.rs"]
mod tests;
