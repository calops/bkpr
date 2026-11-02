use super::*;

impl App<'_> {
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
            self.state.status = self.placeholder(pane);
            return Ok(());
        }
        let Some(attached) = self.views.get(pane) else {
            self.state.status = self.placeholder(pane);
            return Ok(());
        };
        let Some(frame) = attached.frame.as_ref() else {
            self.state.status = self.placeholder(pane);
            return Ok(());
        };
        if frame.alternate_screen && delta != i64::MIN {
            self.state.status =
                "History unavailable on the alternate screen; return to the primary screen first."
                    .into();
            return Ok(());
        }
        self.enqueue(
            pane,
            PaneRequest::Scroll {
                view_id: attached.view.clone(),
                delta,
            },
        )?;
        // Admission is ordered on the existing pane input lane, never retried.
        // A historical frame keeps input blocked until the live frame arrives.
        self.views.get_mut(pane).unwrap().history = delta != i64::MIN && enter;
        self.state.status.clear();
        Ok(())
    }

    pub(super) fn history_key(
        &mut self,
        event: &KeyEvent,
        rects: &[(String, Rect)],
    ) -> Result<bool> {
        if event.action == KeyAction::Repeat
            && self
                .held
                .get(&event.key)
                .is_some_and(|(pane, _)| self.history(pane).is_some())
        {
            return Ok(true);
        }
        let shifted_page =
            event.modifiers == 1 && matches!(event.key.as_str(), "PageUp" | "PageDown");
        if !shifted_page && !self.focused_history() {
            return Ok(false);
        }
        // A key pressed before entering history still owes its original pane a
        // release. Only suppress lifecycles that were entirely client-local.
        if !self.held.contains_key(&event.key) {
            self.state.suppressed.insert(event.key.clone());
        }
        let Some(pane) = self.state.focused.clone() else {
            return Ok(true);
        };
        let page = self.history_page(&pane, rects);
        let delta = match event.key.as_str() {
            "Escape" | "Enter" | "End" => i64::MIN,
            "Home" => i64::MAX,
            "ArrowUp" => 1,
            "ArrowDown" => -1,
            "PageUp" => page,
            "PageDown" => -page,
            _ => return Ok(true),
        };
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
        if self.search_open() {
            self.search_text(&text);
            return Ok(());
        }
        if let Some(form) = self.state.form.as_mut() {
            if !matches!(
                form.kind,
                FormKind::ChooseWorkspace
                    | FormKind::ChoosePane
                    | FormKind::ProjectDetails
                    | FormKind::ReopenPane { .. }
            ) && form.value.len() + text.len() <= 1024
            {
                form.value.push_str(&safe_text(&text));
            }
        } else if !self
            .consent
            .as_ref()
            .is_some_and(|c| !c.state.allowed && !c.state.sent)
            && !self.options.read_only
            && !self.focused_history()
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
