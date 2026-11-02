use super::{Authority, Shared, Terminal};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::SelectionSnapshot;
use std::{collections::BTreeMap, time::Instant};

/// Inspection authority is deliberately independent of input, size, device
/// clipboard ownership, and whether the attachment is an interactive frontend.
/// The visibility epoch binds a queued operation to its inspection lifetime;
/// hide-then-show cannot resurrect it, while sizing/focus activity remains independent.
pub(super) fn visible_record(
    authority: &Authority,
    id: &str,
    inspection_epoch: Option<u64>,
) -> Result<u64> {
    let view = authority.views.get(id).context("stale selection view")?;
    ensure!(
        view.live() && view.spec.visible,
        "selection requires a live visible view"
    );
    ensure!(
        inspection_epoch.is_none_or(|epoch| epoch == view.inspection_epoch),
        "selection view inspection lifetime changed"
    );
    Ok(view.inspection_epoch)
}
pub(super) fn visible(shared: &Shared, id: &str, inspection_epoch: Option<u64>) -> Result<u64> {
    let mut authority = shared.authority.lock().unwrap();
    ensure!(
        shared.published.lock().unwrap().lifecycle == "running",
        "selection requires a live pane"
    );
    let inspection_epoch = visible_record(&authority, id, inspection_epoch)?;
    authority
        .views
        .get_mut(id)
        .expect("validated view")
        .last_seen = Instant::now();
    Ok(inspection_epoch)
}
pub(super) fn retire(terminal: &mut Terminal, shared: &Shared, authority: &Authority) {
    let revoked = std::mem::take(&mut *shared.selection_revoked.lock().unwrap());
    terminal.retain_selection_views(|id| {
        !revoked.contains(id)
            && authority
                .views
                .get(id)
                .is_some_and(|view| view.live() && view.spec.visible)
    });
}
pub(super) fn snapshots(
    terminal: &mut Terminal,
    shared: &Shared,
) -> Result<BTreeMap<String, (u64, SelectionSnapshot)>> {
    let views = {
        let authority = shared.authority.lock().unwrap();
        retire(terminal, shared, &authority);
        if !terminal.has_view_selections() {
            return Ok(BTreeMap::new());
        }
        authority
            .views
            .values()
            .filter(|view| view.live() && view.spec.visible)
            .map(|view| (view.id.clone(), view.inspection_epoch))
            .collect::<Vec<_>>()
    };
    let mut selections = BTreeMap::new();
    for (id, inspection_epoch) in views {
        if let Some(snapshot) = terminal.view_selection(&id)? {
            selections.insert(id, (inspection_epoch, snapshot));
        }
    }
    Ok(selections)
}
pub(super) fn publish(terminal: &mut Terminal, shared: &Shared, id: &str) -> Result<()> {
    let inspection_epoch = {
        let authority = shared.authority.lock().unwrap();
        retire(terminal, shared, &authority);
        visible_record(&authority, id, None)?
    };
    let snapshot = terminal.view_selection(id)?;
    // Match ordinary publication's authority -> published lock order.
    let authority = shared.authority.lock().unwrap();
    let mut published = shared.published.lock().unwrap();
    if visible_record(&authority, id, Some(inspection_epoch)).is_ok() {
        if let Some(snapshot) = snapshot {
            published
                .selections
                .insert(id.to_owned(), (inspection_epoch, snapshot));
        } else {
            published.selections.remove(id);
        }
    } else {
        published.selections.remove(id);
    }
    drop(published);
    drop(authority);
    // Frame revision deliberately remains untouched: the client reuses its
    // existing immutable scene/resources and receives only view annotation.
    shared.changes.notify();
    Ok(())
}

#[cfg(test)]
mod tests;
