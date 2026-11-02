use super::{Shared, Terminal, selection};
use anyhow::Result;
use bkpr_protocol::SearchSnapshot;
use std::collections::BTreeMap;

fn epochs(shared: &Shared) -> BTreeMap<String, u64> {
    let authority = shared.authority.lock().unwrap();
    if shared.published.lock().unwrap().lifecycle != "running" {
        return BTreeMap::new();
    }
    authority
        .views
        .values()
        .filter(|v| v.spec.visible)
        .map(|v| (v.id.clone(), v.inspection_epoch))
        .collect()
}
pub(super) fn retire(terminal: &mut Terminal, shared: &Shared) {
    if !terminal.has_view_searches() {
        return;
    }
    let epochs = epochs(shared);
    // No native feed/tick runs under authority or published locks. A hide/show
    // changes inspection_epoch even when both transitions precede this turn.
    terminal.retain_search_views(|id| epochs.get(id).copied());
}
pub(super) fn snapshots(
    terminal: &mut Terminal,
    shared: &Shared,
) -> Result<BTreeMap<String, (u64, SearchSnapshot)>> {
    if !terminal.has_view_searches() {
        return Ok(BTreeMap::new());
    }
    let epochs = epochs(shared);
    terminal.retain_search_views(|id| epochs.get(id).copied());
    let mut snapshots = BTreeMap::new();
    for (id, epoch) in epochs {
        if let Some(snapshot) = terminal.view_search(&id)? {
            snapshots.insert(id, (epoch, snapshot));
        }
    }
    Ok(snapshots)
}
pub(super) fn publish(
    terminal: &mut Terminal,
    shared: &Shared,
    id: &str,
    epoch: u64,
) -> Result<()> {
    let snapshot = terminal.view_search(id)?;
    let authority = shared.authority.lock().unwrap();
    let mut published = shared.published.lock().unwrap();
    let snapshot = snapshot.filter(|_| {
        published.lifecycle == "running"
            && selection::visible_record(&authority, id, Some(epoch)).is_ok()
    });
    let changed = match snapshot {
        Some(snapshot) => {
            let value = (epoch, snapshot);
            if published.searches.get(id) == Some(&value) {
                false
            } else {
                published.searches.insert(id.to_owned(), value);
                true
            }
        }
        None => published.searches.remove(id).is_some(),
    };
    drop(published);
    drop(authority);
    // Annotation progress uses the snapshot wake token, never scene revision.
    if changed {
        shared.changes.notify();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
