use anyhow::{Result, bail};
use bkpr_protocol::{MAX_ADMITTED_VIEWS, MAX_VIEWS_PER_PANE, Size, ViewInfo, ViewSpec, valid_size};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

pub struct ViewRecord {
    pub id: String,
    pub spec: ViewSpec,
    pub activity: u64,
    pub scrollback_offset: u64,
    _slot: ViewSlot,
    pub last_seen: Instant,
}
impl ViewRecord {
    pub(crate) fn snapshot(&self) -> ViewInfo {
        ViewInfo {
            id: self.id.clone(),
            spec: self.spec.clone(),
            activity: self.activity,
            scrollback_offset: self.scrollback_offset,
        }
    }
}
#[derive(Default)]
pub(crate) struct ViewBudget {
    active: AtomicUsize,
}
impl ViewBudget {
    fn acquire(self: &Arc<Self>) -> Result<ViewSlot> {
        if self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ADMITTED_VIEWS).then_some(active + 1)
            })
            .is_err()
        {
            bail!(
                "daemon view limit reached ({MAX_ADMITTED_VIEWS} admitted views); detach another view first"
            );
        }
        Ok(ViewSlot(self.clone()))
    }
}
struct ViewSlot(Arc<ViewBudget>);
impl Drop for ViewSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct Authority {
    pub views: BTreeMap<String, ViewRecord>,
    pub pinned: Option<(String, Size)>,
    clock: u64,
    pub generation: u64,
    budget: Arc<ViewBudget>,
}
impl Default for Authority {
    fn default() -> Self {
        Self::with_budget(Arc::default())
    }
}
impl Authority {
    pub(crate) fn with_budget(budget: Arc<ViewBudget>) -> Self {
        Self {
            views: BTreeMap::new(),
            pinned: None,
            clock: 0,
            generation: 0,
            budget,
        }
    }
    pub fn touch(&mut self, id: &str) -> Result<()> {
        let view = self
            .views
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        view.last_seen = Instant::now();
        Ok(())
    }
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let stale = self
            .views
            .values()
            .filter(|v| now.saturating_duration_since(v.last_seen) > Duration::from_secs(10))
            .map(|v| v.id.clone())
            .collect::<Vec<_>>();
        for id in &stale {
            let _ = self.detach(id);
        }
        stale
    }
    pub fn attach(&mut self, spec: ViewSpec) -> Result<String> {
        valid_size(spec.size)?;
        if self.views.len() >= MAX_VIEWS_PER_PANE {
            bail!("view limit reached");
        }
        let slot = self.budget.acquire()?;
        let id = Uuid::new_v4().to_string();
        self.clock += 1;
        self.views.insert(
            id.clone(),
            ViewRecord {
                id: id.clone(),
                spec,
                activity: self.clock,
                scrollback_offset: 0,
                last_seen: Instant::now(),
                _slot: slot,
            },
        );
        self.generation += 1;
        Ok(id)
    }
    pub fn update(&mut self, id: &str, spec: ViewSpec) -> Result<()> {
        valid_size(spec.size)?;
        let view = self
            .views
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        self.clock += 1;
        view.spec = spec;
        view.activity = self.clock;
        view.last_seen = Instant::now();
        if self.pinned.as_ref().is_some_and(|(owner, _)| owner == id) && !eligible(view) {
            self.pinned = None;
        }
        self.generation += 1;
        Ok(())
    }
    pub fn detach(&mut self, id: &str) -> Result<()> {
        if self.views.remove(id).is_none() {
            bail!("stale view");
        }
        if self.pinned.as_ref().is_some_and(|(owner, _)| owner == id) {
            self.pinned = None;
        }
        self.generation += 1;
        Ok(())
    }
    pub fn owner(&self) -> Option<(&str, Size)> {
        if let Some((id, size)) = &self.pinned {
            return Some((id, *size));
        }
        self.views
            .values()
            .filter(|v| eligible(v))
            .max_by_key(|v| v.activity)
            .map(|v| (v.id.as_str(), v.spec.size))
    }
    pub fn pin(&mut self, id: &str, size: Option<Size>) -> Result<()> {
        let view = self
            .views
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        if !eligible(view) {
            bail!("view is not size eligible");
        }
        if let Some((owner, _)) = &self.pinned
            && owner != id
        {
            bail!("another view owns the pin");
        }
        if let Some(size) = size {
            valid_size(size)?;
        }
        self.pinned = size.map(|size| (id.to_owned(), size));
        self.generation += 1;
        Ok(())
    }
    pub fn input(&self, id: &str) -> Result<()> {
        let view = self
            .views
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        if !view.spec.input {
            bail!("input capability denied");
        }
        Ok(())
    }
    pub fn clipboard_owner(&self) -> Option<String> {
        self.views
            .values()
            .filter(|v| eligible(v) && v.spec.clipboard)
            .max_by_key(|v| v.activity)
            .map(|v| v.id.clone())
    }
}
fn eligible(view: &ViewRecord) -> bool {
    view.spec.visible && view.spec.interactive
}
#[cfg(test)]
mod tests {
    use super::*;
    fn spec(cols: u16) -> ViewSpec {
        ViewSpec {
            size: Size {
                cols,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            interactive: true,
            visible: true,
            input: false,
            clipboard: false,
        }
    }
    #[test]
    fn latest_eligible_and_pin_release_are_deterministic() {
        let mut a = Authority::default();
        let first = a.attach(spec(80)).unwrap();
        let second = a.attach(spec(40)).unwrap();
        assert_eq!(a.owner().unwrap().0, second);
        a.pin(&first, Some(spec(90).size)).unwrap();
        a.update(&second, spec(50)).unwrap();
        assert_eq!(a.owner().unwrap().1.cols, 90);
        a.detach(&first).unwrap();
        assert_eq!(a.owner().unwrap().0, second);
        let mut hidden = spec(20);
        hidden.visible = false;
        a.update(&second, hidden).unwrap();
        assert!(a.owner().is_none());
    }
    #[test]
    fn size_and_input_authority_are_independent_and_stale_views_fail() {
        let mut a = Authority::default();
        let reader = a.attach(spec(80)).unwrap();
        assert_eq!(a.owner().unwrap().0, reader);
        assert!(a.input(&reader).is_err());
        let mut writer = spec(40);
        writer.input = true;
        writer.visible = false;
        let writer = a.attach(writer).unwrap();
        assert!(a.input(&writer).is_ok());
        assert_eq!(a.owner().unwrap().0, reader);
        a.detach(&writer).unwrap();
        assert!(a.input(&writer).is_err());
        assert!(a.update(&writer, spec(90)).is_err());
    }
    #[test]
    fn expired_pinned_owner_releases_to_live_view_without_input_grant() {
        let mut a = Authority::default();
        let first = a.attach(spec(80)).unwrap();
        let second = a.attach(spec(40)).unwrap();
        a.pin(&first, Some(spec(90).size)).unwrap();
        let now = Instant::now();
        a.views.get_mut(&first).unwrap().last_seen = now - Duration::from_secs(11);
        assert_eq!(a.expire(now), vec![first.clone()]);
        assert_eq!(a.owner().unwrap().0, second);
        assert!(a.input(&first).is_err());
    }
    #[test]
    fn daemon_budget_admits_before_success_and_releases_on_detach_expiry_and_drop() {
        let budget = Arc::new(ViewBudget::default());
        let mut authorities = (0..MAX_ADMITTED_VIEWS / MAX_VIEWS_PER_PANE)
            .map(|_| Authority::with_budget(budget.clone()))
            .collect::<Vec<_>>();
        for authority in &mut authorities {
            for _ in 0..MAX_VIEWS_PER_PANE {
                authority.attach(spec(80)).unwrap();
            }
        }
        assert_eq!(budget.active.load(Ordering::Acquire), MAX_ADMITTED_VIEWS);
        let mut blocked = Authority::with_budget(budget.clone());
        let error = blocked.attach(spec(80)).unwrap_err();
        assert!(error.to_string().contains("daemon view limit"));
        assert!(blocked.views.is_empty());
        assert_eq!(blocked.generation, 0);
        // A second registry/standalone authority has its own budget, not a global static.
        let mut independent = Authority::default();
        independent.attach(spec(80)).unwrap();
        let snapshots = authorities[0]
            .views
            .values()
            .map(ViewRecord::snapshot)
            .collect::<Vec<_>>();
        let id = snapshots[0].id.clone();
        authorities[0].detach(&id).unwrap();
        assert_eq!(
            budget.active.load(Ordering::Acquire),
            MAX_ADMITTED_VIEWS - 1,
            "serialized view metadata must not retain admission slots"
        );
        blocked.attach(spec(80)).unwrap();
        assert_eq!(budget.active.load(Ordering::Acquire), MAX_ADMITTED_VIEWS);
        let expired = authorities[1].expire(Instant::now() + Duration::from_secs(11));
        assert_eq!(expired.len(), MAX_VIEWS_PER_PANE);
        assert_eq!(
            budget.active.load(Ordering::Acquire),
            MAX_ADMITTED_VIEWS - MAX_VIEWS_PER_PANE
        );
        drop(authorities);
        drop(blocked);
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
        assert_eq!(snapshots.len(), MAX_VIEWS_PER_PANE);
    }
}
