use super::*;
use crate::tiling::{self, DropZone, PaneDrop};

struct Gesture {
    workspace: String,
    window: String,
    pane: String,
    revision: u64,
    size: Size,
    area: Rect,
    frames: Vec<(String, Rect)>,
    hovered: Option<(usize, DropZone)>,
    drop: Option<PaneDrop>,
}

#[derive(Default)]
pub(super) struct State {
    gesture: Option<Gesture>,
}

impl App {
    pub(super) fn pane_moving(&self) -> bool {
        self.pane_move.gesture.is_some()
    }

    pub(super) fn pane_move_hint(&self) -> Option<&'static str> {
        self.pane_moving()
            .then_some("Drop on a pane to move it; Esc cancels")
    }

    /// Starts a Ctrl+left-drag move of `pane` in the selected window. The
    /// physical gesture is captured even when admission fails.
    pub(super) fn begin_pane_move(&mut self, pane: String, size: Size) -> Result<()> {
        let workspace = self.state.current(&self.registry).context("no workspace")?;
        let window = self
            .state
            .current_window(&self.registry)
            .context("no window")?;
        self.rearrangement_admitted(&window.id)?;
        if self.resize_pending() {
            bail!("Pane move waiting for resize to finish; no move started");
        }
        let layout = window.layout.as_ref().context("window has no layout")?;
        let area = self.screen_geometry(size).1;
        let mut frames = Vec::new();
        layout_rects(Some(layout), area, &mut frames);
        self.pane_move.gesture = Some(Gesture {
            workspace: workspace.id.clone(),
            window: window.id.clone(),
            pane,
            revision: window.revision,
            size,
            area,
            frames,
            hovered: None,
            drop: None,
        });
        self.pointer_capture = Some(PointerCapture::PaneMove);
        Ok(())
    }

    /// Ends the move without changing the layout; the rest of the physical
    /// gesture stays swallowed until its release.
    pub(super) fn cancel_pane_move(&mut self, notice: Option<&str>) {
        if self.pane_move.gesture.take().is_none() {
            return;
        }
        if matches!(self.pointer_capture, Some(PointerCapture::PaneMove)) {
            self.pointer_capture = Some(PointerCapture::Ui);
        }
        if let Some(notice) = notice {
            self.notify(notice.into());
        }
    }

    pub(super) fn validate_pane_move(&mut self, size: Size) {
        let Some(gesture) = &self.pane_move.gesture else {
            return;
        };
        let foreign = self
            .state
            .current_window(&self.registry)
            .is_none_or(|window| window.revision != gesture.revision);
        if foreign {
            self.cancel_pane_move(Some("Layout changed; pane not moved"));
        } else if self.options.read_only
            || !matches!(self.pointer_capture, Some(PointerCapture::PaneMove))
            || gesture.size != size
            || gesture.area != self.screen_geometry(size).1
            || self.state.workspace.as_ref() != Some(&gesture.workspace)
            || self.state.window.as_ref() != Some(&gesture.window)
            || self.state.form.is_some()
            || self.picker_open()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
        {
            self.cancel_pane_move(None);
        }
    }

    pub(super) fn pane_move_pointer(&mut self, x: u16, y: u16, action: MouseAction) -> Result<()> {
        if action == MouseAction::Press {
            // The previous release was lost; never drop on a stale gesture.
            self.cancel_pane_move(None);
            return Ok(());
        }
        let Some(gesture) = &mut self.pane_move.gesture else {
            return Ok(());
        };
        let hovered = tiling::drop_target(&gesture.frames, &gesture.pane, x, y);
        if hovered != gesture.hovered {
            gesture.hovered = hovered;
            gesture.drop = hovered.and_then(|(target, zone)| {
                let layout = self
                    .registry
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == gesture.workspace)?
                    .windows
                    .iter()
                    .find(|window| window.id == gesture.window)?
                    .layout
                    .as_ref()?;
                tiling::pane_drop(
                    layout,
                    gesture.area,
                    &gesture.frames,
                    &gesture.pane,
                    target,
                    zone,
                )
            });
        }
        if action != MouseAction::Release {
            return Ok(());
        }
        let gesture = self.pane_move.gesture.take().unwrap();
        let Some(drop) = gesture.drop else {
            return Ok(());
        };
        self.submit_rearrangement(
            gesture.workspace,
            gesture.window,
            gesture.revision,
            drop.layout,
        )
    }

    /// Esc cancels a move; its release never reaches the application.
    pub(super) fn pane_move_key(&mut self, event: &KeyEvent) -> bool {
        if !self.pane_moving() || event.action != KeyAction::Press || event.key != "Escape" {
            return false;
        }
        self.cancel_pane_move(None);
        if !self.held.contains_key(&event.key) {
            self.state.suppressed.insert(event.key.clone());
        }
        true
    }

    pub(super) fn pane_move_feedback(&self) -> Vec<Rect> {
        self.pane_move
            .gesture
            .as_ref()
            .and_then(|gesture| gesture.drop.as_ref())
            .map_or_else(Vec::new, |drop| tiling::outline(drop.preview).to_vec())
    }
}
