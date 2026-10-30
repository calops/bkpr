use anyhow::{Result, bail};
use bkpr_terminal::Size;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewSpec {
    pub size: Size,
    pub interactive: bool,
    pub visible: bool,
    pub input: bool,
    pub clipboard: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct View {
    pub id: String,
    pub spec: ViewSpec,
    pub activity: u64,
    pub scrollback_offset: u64,
    #[serde(skip)]
    pub last_seen: Instant,
}
#[derive(Default)]
pub struct Authority {
    pub views: BTreeMap<String, View>,
    pub pinned: Option<(String, Size)>,
    clock: u64,
    pub generation: u64,
}
impl Authority {
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
        if self.views.len() >= 32 {
            bail!("view limit reached");
        }
        let id = Uuid::new_v4().to_string();
        self.clock += 1;
        self.views.insert(
            id.clone(),
            View {
                id: id.clone(),
                spec,
                activity: self.clock,
                scrollback_offset: 0,
                last_seen: Instant::now(),
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
fn eligible(view: &View) -> bool {
    view.spec.visible && view.spec.interactive
}
pub fn valid_size(size: Size) -> Result<()> {
    if size.cols == 0
        || size.rows == 0
        || size.cols > 500
        || size.rows > 300
        || size.cell_width == 0
        || size.cell_height == 0
        || size.cell_width > 256
        || size.cell_height > 256
    {
        bail!("invalid or excessive terminal dimensions");
    }
    Ok(())
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
}
