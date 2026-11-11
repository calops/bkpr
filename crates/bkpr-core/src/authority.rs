//! Pane-local view capabilities, inspection lifetimes and independent size arbitration.
use anyhow::{Result, bail};
use bkpr_protocol::{Size, ViewInfo, ViewSpec, valid_size};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Retained attachment policy and local activity/inspection state.
pub struct ViewRecord {
    /// Opaque attachment identity.
    pub id: String,
    pub(crate) client: Option<Arc<crate::registry::sessions::ClientLease>>,
    /// Accepted visibility, sizing eligibility, input and clipboard policy.
    pub spec: ViewSpec,
    /// Monotonic activity order used for owner selection.
    pub activity: u64,
    /// Inspection lifetime changes only when visibility is revoked/restored;
    /// sizing activity and independent capability updates do not retire a drag.
    pub(crate) inspection_epoch: u64,
    /// Distance above live output in retained rows.
    pub scrollback_offset: u64,
    /// Last liveness refresh; unauthenticated local views expire when idle.
    pub last_seen: Instant,
}
impl ViewRecord {
    pub(crate) fn live(&self) -> bool {
        self.client.as_ref().is_none_or(|client| client.is_active())
    }
    pub(crate) fn snapshot(&self) -> ViewInfo {
        ViewInfo {
            id: self.id.clone(),
            spec: self.spec.clone(),
            activity: self.activity,
            scrollback_offset: self.scrollback_offset,
        }
    }
}
/// View registry and desired size lease for one pane.
#[derive(Default)]
pub struct Authority {
    /// Retained attachments keyed by their identities.
    pub views: BTreeMap<String, ViewRecord>,
    /// Explicit size lease, or `None` for latest-eligible arbitration.
    pub pinned: Option<(String, Size)>,
    clock: u64,
    /// Revision of accepted attachment and lease changes.
    pub generation: u64,
}
impl Authority {
    /// Refresh liveness; reject unknown views or a revoked client attachment.
    pub fn touch(&mut self, id: &str) -> Result<()> {
        let view = self
            .views
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        if let Some(client) = &view.client {
            client.ensure_active()?;
        }
        view.last_seen = Instant::now();
        Ok(())
    }
    /// Revoke dead-client and expired unowned views, returning their identities.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let stale = self
            .views
            .values()
            .filter(|v| {
                !v.live()
                    || (v.client.is_none()
                        && now.saturating_duration_since(v.last_seen) > Duration::from_secs(10))
            })
            .map(|v| v.id.clone())
            .collect::<Vec<_>>();
        for id in &stale {
            let _ = self.detach(id);
        }
        stale
    }
    /// Add an unowned local view; reject invalid terminal dimensions.
    pub fn attach(&mut self, spec: ViewSpec) -> Result<String> {
        self.attach_owned(spec, None)
    }
    pub(crate) fn attach_owned(
        &mut self,
        spec: ViewSpec,
        client: Option<Arc<crate::registry::sessions::ClientLease>>,
    ) -> Result<String> {
        if let Some(client) = &client {
            client.ensure_active()?;
        }
        valid_size(spec.size)?;
        let id = Uuid::new_v4().to_string();
        self.clock += 1;
        self.views.insert(
            id.clone(),
            ViewRecord {
                id: id.clone(),
                client,
                spec,
                activity: self.clock,
                inspection_epoch: 1,
                scrollback_offset: 0,
                last_seen: Instant::now(),
            },
        );
        self.generation += 1;
        Ok(id)
    }
    /// Replace policy and refresh activity; reject invalid size, unknown views or revoked clients.
    pub fn update(&mut self, id: &str, spec: ViewSpec) -> Result<()> {
        valid_size(spec.size)?;
        let view = self
            .views
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        if !view.live() {
            bail!("client attachment detached");
        }
        if view.spec.visible != spec.visible {
            view.inspection_epoch = view
                .inspection_epoch
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("view inspection epoch exhausted"))?;
        }
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
    /// Remove a view and release its pin; reject unknown identities.
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
    /// Return the explicit pin or latest live, visible, interactive view's desired size.
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
    /// Acquire or release a size pin; reject ineligible views, foreign pins or invalid sizes.
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
    /// Require a live input-authorized view; sizing eligibility is not an input grant.
    pub fn input(&self, id: &str) -> Result<()> {
        let view = self
            .views
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        if !view.live() {
            bail!("client attachment detached");
        }
        if !view.spec.input {
            bail!("input capability denied");
        }
        Ok(())
    }
    /// Select the latest live, visible, interactive view with clipboard permission.
    pub fn clipboard_owner(&self) -> Option<String> {
        self.views
            .values()
            .filter(|v| eligible(v) && v.spec.clipboard)
            .max_by_key(|v| v.activity)
            .map(|v| v.id.clone())
    }
    pub(crate) fn check_client(
        &self,
        id: &str,
        client: Option<&Arc<crate::registry::sessions::ClientLease>>,
    ) -> Result<()> {
        if let Some(client) = client {
            client.ensure_active()?;
        }
        let view = self
            .views
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("stale view"))?;
        let matches = match (&view.client, client) {
            (None, None) => true,
            (Some(owner), Some(client)) if Arc::ptr_eq(owner, client) => {
                client.ensure_active()?;
                true
            }
            _ => false,
        };
        if !matches {
            bail!("view belongs to a different client authority");
        }
        Ok(())
    }
}
fn eligible(view: &ViewRecord) -> bool {
    view.live() && view.spec.visible && view.spec.interactive
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
    fn many_views_preserve_sizing_and_release_authority_on_detach_and_expiry() {
        let mut authority = Authority::default();
        let views = (0..160)
            .map(|_| authority.attach(spec(80)).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(authority.views.len(), views.len());
        assert_eq!(authority.owner().unwrap().0, views.last().unwrap());
        authority.pin(&views[0], Some(spec(90).size)).unwrap();
        authority.detach(&views[0]).unwrap();
        assert_eq!(authority.owner().unwrap().0, views.last().unwrap());
        assert!(authority.input(&views[0]).is_err());
        let expired = authority.expire(Instant::now() + Duration::from_secs(11));
        assert_eq!(expired.len(), views.len() - 1);
        assert!(authority.views.is_empty());
        assert!(authority.owner().is_none());
        assert!(authority.pinned.is_none());
    }
}
