//! Durable directory intent is separate from initial CWD and volatile foreground facts.
use super::*;
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Directory {
    #[serde(with = "bkpr_protocol::path_wire")]
    pub cwd: PathBuf,
}
pub(super) struct Pending {
    pane: Pane,
    cwd: PathBuf,
    native: Option<(HarnessKind, String)>,
}
pub(super) fn validate(cwd: &Path) -> Result<()> {
    let bytes = cwd.as_os_str().as_bytes();
    ensure!(
        cwd.is_absolute() && bytes.len() <= 16 * 1024 && !bytes.contains(&0),
        "invalid durable working directory"
    );
    Ok(())
}
pub(super) fn end_native(state: &mut State, id: &str, kind: HarnessKind, conversation: &str) {
    if let Some(pending) = state.pending_directories.get_mut(id)
        && pending
            .native
            .as_ref()
            .is_some_and(|(saved_kind, saved_id)| *saved_kind == kind && saved_id == conversation)
    {
        pending.native = None;
    }
}

pub(super) fn queue(
    state: &mut State,
    id: &str,
    pane: &Pane,
    cwd: &Path,
    native: Option<(HarnessKind, &str)>,
) {
    if validate(cwd).is_err()
        || state.durability_error.is_some()
        || state.resuming.contains_key(id)
        || state
            .live
            .get(id)
            .is_none_or(|current| !Arc::ptr_eq(&current.shared, &pane.shared))
    {
        return;
    }
    let persisted = state
        .record
        .directories
        .get(id)
        .map(|directory| directory.cwd.as_path())
        .or_else(|| {
            state
                .record
                .registry
                .panes
                .iter()
                .find(|info| info.id == id)
                .map(|info| info.cwd.as_path())
        });
    let desired = native.as_ref().and_then(|(kind, conversation)| {
        state.record.recoveries.get(id).filter(|descriptor| {
            descriptor.kind == *kind && descriptor.conversation_id == *conversation
        })
    });
    let role_changed = match (state.record.automatic.get(id), desired) {
        (Some(saved), Some(desired)) => {
            saved.kind != desired.kind
                || saved.conversation_id != desired.conversation_id
                || saved.cwd != cwd
        }
        (None, None) => false,
        _ => true,
    };
    if persisted == Some(cwd) && !role_changed {
        state.pending_directories.remove(id);
        return;
    }
    if state.pending_directories.get(id).is_some_and(|pending| {
        Arc::ptr_eq(&pending.pane.shared, &pane.shared)
            && pending.cwd == cwd
            && pending
                .native
                .as_ref()
                .map(|(kind, conversation)| (*kind, conversation.as_str()))
                == native
    }) {
        return;
    }
    state.pending_directories.insert(
        id.to_owned(),
        Pending {
            pane: pane.clone(),
            cwd: cwd.to_owned(),
            native: native.map(|(kind, conversation)| (kind, conversation.to_owned())),
        },
    );
}

impl Registry {
    /// Worker-only fsync batching all changed panes after one observation cycle.
    /// Guard publication by the exact execution allocation; late observations from
    /// an old process cannot rewrite a replacement's durable directory intent.
    pub(super) fn flush_directories(&self) -> Result<()> {
        let _transaction = self.inner.transaction.lock().unwrap();
        let (mut next, accepted) = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            let accepted = state
                .pending_directories
                .iter()
                .filter(|(id, pending)| {
                    !state.resuming.contains_key(*id)
                        && !state.closing_panes.contains(*id)
                        && !pending
                            .pane
                            .shared
                            .finished
                            .load(std::sync::atomic::Ordering::Acquire)
                        && state.live.get(*id).is_some_and(|current| {
                            Arc::ptr_eq(&current.shared, &pending.pane.shared)
                        })
                        && state
                            .record
                            .registry
                            .panes
                            .iter()
                            .any(|info| &info.id == *id && info.status == PaneStatus::Running)
                })
                .map(|(id, pending)| (id.clone(), pending.cwd.clone(), pending.native.clone()))
                .collect::<Vec<_>>();
            if accepted.is_empty() {
                return Ok(());
            }
            (state.record.clone(), accepted)
        };
        for (id, cwd, native) in &accepted {
            next.directories
                .insert(id.clone(), Directory { cwd: cwd.clone() });
            next.automatic.remove(id);
            if let Some((kind, conversation)) = native
                && let Some(descriptor) = next.recoveries.get_mut(id)
                && descriptor.kind == *kind
                && descriptor.conversation_id == *conversation
            {
                descriptor.cwd = cwd.clone();
                next.automatic.insert(id.clone(), descriptor.clone());
            }
        }
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit(&self.inner, next, |state| {
            for (id, _, _) in accepted {
                state.pending_directories.remove(&id);
            }
        })?;
        Ok(())
    }
}
