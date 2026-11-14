use super::*;

impl App {
    pub(super) fn history(&self, pane: &str) -> Option<(u64, u64)> {
        let attached = self.views.get(pane)?;
        let frame = attached.frame.as_ref()?;
        (!frame.alternate_screen && (attached.history || frame.scrollback_offset > 0))
            .then_some((frame.scrollback_offset, frame.scrollback_total))
    }

    pub(super) fn focused_history(&self) -> bool {
        self.state
            .focused
            .as_deref()
            .is_some_and(|pane| self.history(pane).is_some())
    }

    fn history_page(&self, pane: &str, rects: &[(String, Rect)]) -> i64 {
        let rows = rects
            .iter()
            .find(|(id, _)| id == pane)
            .map(|(_, rect)| content(*rect).height)
            .or_else(|| {
                self.views
                    .get(pane)?
                    .frame
                    .as_ref()
                    .map(|frame| frame.size.rows)
            })
            .unwrap_or(1);
        i64::from(rows.saturating_sub(1).max(1))
    }

    pub(super) fn scroll_history(&mut self, pane: &str, delta: i64, enter: bool) -> Result<()> {
        if !self.input_enabled(pane) {
            self.notify(self.placeholder(pane));
            return Ok(());
        }
        let Some(attached) = self.views.get(pane) else {
            self.notify(self.placeholder(pane));
            return Ok(());
        };
        let Some(frame) = attached.frame.as_ref() else {
            self.notify(self.placeholder(pane));
            return Ok(());
        };
        if frame.alternate_screen && delta != i64::MIN {
            self.notify(
                "History unavailable on the alternate screen; return to the primary screen first."
                    .into(),
            );
            return Ok(());
        }
        let history = delta != i64::MIN
            && enter
            && (delta >= 0 || frame.scrollback_offset > delta.unsigned_abs());
        self.enqueue(
            pane,
            PaneRequest::Scroll {
                view_id: attached.view.clone(),
                delta,
            },
        )?;
        // Pending entry is local; published live frames retire it.
        self.views.get_mut(pane).unwrap().history = history;
        Ok(())
    }

    pub(super) fn history_key(
        &mut self,
        event: &KeyEvent,
        rects: &[(String, Rect)],
    ) -> Result<bool> {
        let shifted_page =
            event.modifiers == 1 && matches!(event.key.as_str(), "PageUp" | "PageDown");
        if !shifted_page && !self.focused_history() {
            return Ok(false);
        }
        let Some(pane) = self.state.focused.clone() else {
            return Ok(true);
        };
        let page = self.history_page(&pane, rects);
        use crate::keyguide::{self, Action, Context, HistoryAction};
        let context = if self.focused_history() {
            Context::History
        } else {
            Context::Application
        };
        let delta = match keyguide::lookup(context, event) {
            Some(Action::History(HistoryAction::Live)) => i64::MIN,
            Some(Action::History(HistoryAction::Oldest)) => i64::MAX,
            Some(Action::History(HistoryAction::LineUp)) => 1,
            Some(Action::History(HistoryAction::LineDown)) => -1,
            Some(Action::History(HistoryAction::PageUp)) => page,
            Some(Action::History(HistoryAction::PageDown)) => -page,
            _ => return Ok(false),
        };
        // Native keys keep their press/repeat/release lifecycle and origin.
        if !self.held.contains_key(&event.key) {
            self.state.suppressed.insert(event.key.clone());
        }
        self.scroll_history(&pane, delta, delta != i64::MIN)?;
        Ok(true)
    }

    pub(super) fn enter_history(&mut self, rects: &[(String, Rect)]) -> Result<()> {
        if let Some(pane) = self.state.focused.clone() {
            let page = self.history_page(&pane, rects);
            self.scroll_history(&pane, page, true)?;
        }
        Ok(())
    }

    pub(super) fn paste(&mut self, text: String) -> Result<()> {
        if self.image_viewer.is_some() {
            return Ok(());
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
        {
            return Ok(());
        }
        if self.state.prefix {
            return Ok(());
        }
        if let Some(picker) = &mut self.theme_picker {
            picker.list.paste(&text);
            self.preview_theme();
            return Ok(());
        }
        if let Some(finder) = &mut self.finder {
            finder.paste(&text);
            return Ok(());
        }
        if let Some(form) = &mut self.state.form {
            if form.pending.is_none()
                && !form.ambiguous
                && matches!(
                    form.kind,
                    FormKind::CreateWorkspace | FormKind::RenameWorkspace | FormKind::RenamePane
                )
            {
                form.fields.paste(&text);
            }
            return Ok(());
        }
        if self.state.sidebar.active {
            return Ok(());
        }
        if self.search_open() {
            self.search_text(&text);
            return Ok(());
        }
        if !self.options.read_only
            && let Some((pane, view)) = self.active()
        {
            self.enqueue(
                &pane,
                PaneRequest::Paste {
                    view_id: view,
                    data: text.into_bytes(),
                },
            )?;
        }
        Ok(())
    }
}
