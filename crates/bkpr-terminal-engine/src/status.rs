use super::*;

/// Borrowed canonical OSC 7501 records, including nearest-ancestor app inheritance.
#[derive(Clone, Copy)]
pub struct ProgramStatusView<'a> {
    records: &'a [ProgramStatus],
}
impl<'a> ProgramStatusView<'a> {
    /// Iterate records and their effective app without copying status text.
    pub fn iter(self) -> impl Iterator<Item = (&'a ProgramStatus, Option<&'a str>)> {
        self.records.iter().map(move |record| {
            let mut app = record.app.as_deref();
            let mut id = record.id.as_str();
            while app.is_none() && !id.is_empty() {
                id = id.rsplit_once('/').map_or("", |(parent, _)| parent);
                app = self
                    .records
                    .iter()
                    .find(|parent| parent.id == id)
                    .and_then(|parent| parent.app.as_deref());
            }
            (record, app)
        })
    }
}

#[derive(Default)]
pub(super) struct StatusRecords {
    records: Vec<ProgramStatus>,
}
impl StatusRecords {
    pub(super) fn update(&mut self, effect: &Effect) {
        let Effect::ProgramStatus {
            state,
            kind,
            app,
            message,
            id,
            title,
            progress,
        } = effect
        else {
            return;
        };
        if state == "clear" {
            self.records.retain(|r| {
                !(id.is_empty()
                    || r.id == *id
                    || r.id
                        .strip_prefix(id.as_str())
                        .is_some_and(|suffix| suffix.starts_with('/')))
            });
            return;
        }
        if let Some(index) = self.records.iter().position(|r| r.id == *id) {
            self.records.remove(index);
        }
        if self.records.len() == 256 {
            self.records.remove(0);
        }
        self.records.push(ProgramStatus {
            state: state.clone(),
            kind: kind.clone(),
            app: app.clone(),
            message: message.clone(),
            id: id.clone(),
            title: title.clone(),
            progress: *progress,
        });
    }
    pub(super) fn prompt_or_exit(&mut self) {
        self.records
            .retain(|r| !matches!(r.state.as_str(), "working" | "blocked" | "idle"));
    }
    pub(super) fn seen(&mut self) {
        self.records
            .retain(|r| !matches!(r.state.as_str(), "done" | "error"));
    }
    pub(super) fn view(&self) -> ProgramStatusView<'_> {
        ProgramStatusView {
            records: &self.records,
        }
    }
    pub(super) fn snapshot(&self) -> Vec<ProgramStatus> {
        self.view()
            .iter()
            .map(|(record, app)| {
                let mut owned = record.clone();
                owned.app = app.map(str::to_owned);
                owned
            })
            .collect()
    }
}
impl Terminal {
    /// Current OSC 7501 records with native replacement, subtree clear,
    /// nearest-ancestor app inheritance and oldest-updated eviction (256 max).
    pub fn program_status(&self) -> Vec<ProgramStatus> {
        self.callbacks.borrow().status.snapshot()
    }
    /// A foreground execution change invalidates every record, including completed results.
    pub fn clear_program_status(&mut self) {
        self.callbacks.borrow_mut().status.records.clear();
    }
    /// Call when the PTY child exits. Terminal output alone never implies exit.
    pub fn program_exited(&mut self) {
        self.callbacks.borrow_mut().status.prompt_or_exit();
        self.clear_view_selections();
    }
    /// Call when an authorized interactive viewer has actually seen results.
    pub fn mark_status_seen(&mut self) {
        self.callbacks.borrow_mut().status.seen();
    }
}
