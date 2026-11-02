use super::*;
use crate::tiling::{self, Direction};

const KEY_HINT: &str = "RESIZE | arrows 1 cell  Shift+arrows 5 | Enter/Esc finish";
const DRAG_HINT: &str = "RESIZE | drag divider; release to finish  Esc finish";

enum Mode {
    Keyboard,
    Drag,
    Finishing,
}
struct Gesture {
    generation: u64,
    workspace: String,
    window: String,
    pane: String,
    size: Size,
    revision: u64,
    acknowledged: LayoutNode,
    desired: LayoutNode,
    mode: Mode,
    boundary: bool,
    drag_path: Option<Vec<bool>>,
    pending_pointer: Option<(u16, u16)>,
}
struct Commit {
    generation: u64,
    workspace: String,
    window: String,
    revision: u64,
    layout: LayoutNode,
}
#[derive(Default)]
pub(super) struct State {
    generation: u64,
    gesture: Option<Gesture>,
    flight: Option<Commit>,
}
impl State {
    fn capturing(&self) -> bool {
        self.gesture
            .as_ref()
            .is_some_and(|gesture| !matches!(gesture.mode, Mode::Finishing))
    }

    fn observes(&self, registry: &RegistrySnapshot) -> bool {
        let Some(gesture) = &self.gesture else {
            return true;
        };
        registry
            .workspaces
            .iter()
            .find(|workspace| workspace.id == gesture.workspace)
            .and_then(|workspace| {
                workspace
                    .windows
                    .iter()
                    .find(|window| window.id == gesture.window)
            })
            .is_some_and(|window| {
                window.revision == gesture.revision
                    && window.layout.as_ref() == Some(&gesture.acknowledged)
                    || self.flight.as_ref().is_some_and(|flight| {
                        flight.generation == gesture.generation
                            && flight.window == window.id
                            && flight.revision.checked_add(1) == Some(window.revision)
                            && window.layout.as_ref() == Some(&flight.layout)
                    })
            })
    }

    fn acknowledge(&mut self) -> bool {
        let Some(flight) = self.flight.take() else {
            return false;
        };
        let Some(gesture) = self
            .gesture
            .as_mut()
            .filter(|gesture| gesture.generation == flight.generation)
        else {
            return false;
        };
        gesture.revision = flight.revision + 1;
        gesture.acknowledged = flight.layout;
        if matches!(gesture.mode, Mode::Finishing)
            && gesture.desired == gesture.acknowledged
            && gesture.pending_pointer.is_none()
        {
            self.gesture = None;
        }
        true
    }
}

impl App<'_> {
    pub(super) fn resize_capturing(&self) -> bool {
        self.resizing.capturing()
    }

    pub(super) fn retire_resize(&mut self, reason: &str) {
        if self.resizing.gesture.take().is_some() {
            self.state.status =
                format!("Resize stopped: {reason}; accepted update may still finish; not replayed");
        }
        if matches!(self.pointer_capture, Some(PointerCapture::Resize)) {
            // Keep swallowing the physical gesture until its release, rather
            // than converting its tail into input to the pane under the mouse.
            self.pointer_capture = Some(PointerCapture::Ui);
        }
    }

    pub(super) fn observe_resize(&mut self, registry: &RegistrySnapshot) {
        if !self.resizing.observes(registry) {
            self.retire_resize("window changed elsewhere");
        }
    }

    pub(super) fn validate_resize(&mut self, size: Size) {
        let Some(gesture) = &self.resizing.gesture else {
            return;
        };
        let reason = if self.options.read_only {
            Some("client is read-only")
        } else if !self.host_focused {
            Some("terminal focus lost")
        } else if gesture.size != size {
            Some("terminal geometry changed")
        } else if self.state.workspace.as_ref() != Some(&gesture.workspace)
            || self.state.window.as_ref() != Some(&gesture.window)
            || self.state.focused.as_ref() != Some(&gesture.pane)
        {
            Some("pane, window, or workspace focus changed")
        } else if self.state.form.is_some()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
        {
            Some("modal opened")
        } else if !self.resizing.observes(&self.registry) {
            Some("window changed elsewhere")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.retire_resize(reason);
        }
    }

    fn begin_resize(&mut self, size: Size, mode: Mode, drag_path: Option<Vec<bool>>) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot resize layouts");
        }
        if !self.host_focused
            || self.state.form.is_some()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
        {
            bail!("resize requires terminal and pane focus without a modal");
        }
        if self.resizing.flight.is_some() || self.resizing.gesture.is_some() {
            bail!("Resize waiting for previous gesture to finish; no new gesture started");
        }
        let workspace = self.state.current(&self.registry).context("no workspace")?;
        let window = self
            .state
            .current_window(&self.registry)
            .context("no window")?;
        let pane = self.state.focused.as_ref().context("no focused pane")?;
        if self.jobs.contains(&format!("manage:window:{}", window.id))
            || self
                .jobs
                .contains(&format!("manage:workspace:{}", workspace.id))
        {
            bail!("Resize waiting for pending window management; no gesture started");
        }
        let layout = window.layout.as_ref().context("window has no layout")?;
        let mut splits = Vec::new();
        tiling::dividers(Some(layout), screen_geometry(size).1, &mut splits);
        if splits.is_empty() {
            bail!("Resize unavailable: no representable divider");
        }
        self.resizing.generation = self.resizing.generation.wrapping_add(1);
        self.resizing.gesture = Some(Gesture {
            generation: self.resizing.generation,
            workspace: workspace.id.clone(),
            window: window.id.clone(),
            pane: pane.clone(),
            size,
            revision: window.revision,
            acknowledged: layout.clone(),
            desired: layout.clone(),
            mode,
            boundary: false,
            drag_path,
            pending_pointer: None,
        });
        self.resize_hint();
        Ok(())
    }

    pub(super) fn resize_hint(&mut self) {
        if let Some(gesture) = &self.resizing.gesture {
            let hint = match gesture.mode {
                Mode::Keyboard if gesture.boundary => {
                    "RESIZE | arrows 1 cell  Shift+arrows 5 | Enter/Esc finish | boundary/no matching divider"
                }
                Mode::Keyboard => KEY_HINT,
                Mode::Drag => DRAG_HINT,
                Mode::Finishing => "Resize finishing latest position; layout update pending",
            };
            if self.state.status != hint {
                self.state.status.clear();
                self.state.status.push_str(hint);
            }
        }
    }

    fn finish_resize(&mut self) -> Result<()> {
        if let Some(gesture) = &mut self.resizing.gesture {
            gesture.mode = Mode::Finishing;
            if self.resizing.flight.is_none()
                && gesture.desired == gesture.acknowledged
                && gesture.pending_pointer.is_none()
            {
                self.resizing.gesture = None;
                self.state.status = "Resize finished".into();
            } else {
                self.resize_hint();
            }
        }
        if matches!(self.pointer_capture, Some(PointerCapture::Resize)) {
            self.pointer_capture = Some(PointerCapture::Ui);
        }
        self.flush_resize()
    }

    pub(super) fn resize_dragging(&self) -> bool {
        self.resizing
            .gesture
            .as_ref()
            .is_some_and(|gesture| matches!(gesture.mode, Mode::Drag))
    }
    pub(super) fn resize_key(&mut self, event: &KeyEvent, size: Size) -> Result<bool> {
        if event.key == "BracketRight" && event.modifiers & 2 != 0 {
            return Ok(false);
        }
        if !self.resize_capturing() {
            return Ok(false);
        }
        // Mouse resizing retains the normal command prefix so opening a
        // modal or changing focus can retire the capture immediately.
        if matches!(self.resizing.gesture.as_ref().unwrap().mode, Mode::Drag)
            && (self.state.prefix || event.key == "KeyB" && event.modifiers & 2 != 0)
        {
            return Ok(false);
        }
        // App-owned keys accepted before resize mode still owe their release
        // to the original view. key() handles those releases before this path.
        if !self.held.contains_key(&event.key) {
            self.state.suppressed.insert(event.key.clone());
        }
        use crate::keyguide::{self, Action, Context};
        let command = keyguide::lookup(Context::Resize, event);
        if command == Some(Action::FinishResize) {
            self.finish_resize()?;
            return Ok(true);
        }
        if let Some(Action::Resize { direction, cells }) = command
            && matches!(self.resizing.gesture.as_ref().unwrap().mode, Mode::Keyboard)
        {
            let direction = match direction {
                keyguide::Direction::Left => Direction::Left,
                keyguide::Direction::Right => Direction::Right,
                keyguide::Direction::Up => Direction::Up,
                keyguide::Direction::Down => Direction::Down,
            };
            let gesture = self.resizing.gesture.as_mut().unwrap();
            if let Some(layout) = tiling::nudge(
                &gesture.desired,
                screen_geometry(size).1,
                &gesture.pane,
                direction,
                cells,
            ) {
                gesture.desired = layout;
                gesture.boundary = false;
                self.resize_hint();
                self.flush_resize()?;
            } else {
                gesture.boundary = true;
                self.resize_hint();
            }
        }
        Ok(true)
    }

    pub(super) fn begin_keyboard_resize(&mut self, size: Size) -> Result<()> {
        self.begin_resize(size, Mode::Keyboard, None)
    }

    pub(super) fn begin_divider_resize(&mut self, size: Size, x: u16, y: u16) -> Result<bool> {
        let mut splits = Vec::new();
        tiling::dividers(
            self.state
                .current_window(&self.registry)
                .and_then(|window| window.layout.as_ref()),
            screen_geometry(size).1,
            &mut splits,
        );
        let Some(split) = splits
            .into_iter()
            .find(|split| split.divider.contains(x, y))
        else {
            return Ok(false);
        };
        self.begin_resize(size, Mode::Drag, Some(split.path))?;
        self.pointer_capture = Some(PointerCapture::Resize);
        Ok(true)
    }

    pub(super) fn resize_pointer(&mut self, x: u16, y: u16, release: bool) -> Result<()> {
        let Some(gesture) = &mut self.resizing.gesture else {
            return Ok(());
        };
        if !matches!(gesture.mode, Mode::Drag) {
            return Ok(());
        }
        // Raw motion only replaces this bounded intent. Materialize a tree
        // once the preceding exact-revision commit has actually completed.
        gesture.pending_pointer = Some((x, y));
        if release {
            self.finish_resize()
        } else {
            self.flush_resize()
        }
    }

    pub(super) fn flush_resize(&mut self) -> Result<()> {
        if self.resizing.flight.is_some() || self.jobs.is_full() {
            return Ok(());
        }
        if let Some(size) = self.resizing.gesture.as_ref().map(|gesture| gesture.size) {
            self.validate_resize(size);
        }
        let Some(gesture) = &mut self.resizing.gesture else {
            return Ok(());
        };
        if let Some((x, y)) = gesture.pending_pointer.take()
            && let Some(path) = &gesture.drag_path
        {
            let mut splits = Vec::new();
            tiling::dividers(
                Some(&gesture.acknowledged),
                screen_geometry(gesture.size).1,
                &mut splits,
            );
            if let Some(split) = splits.iter().find(|split| split.path == *path)
                && let Some(layout) = tiling::drag(&gesture.acknowledged, split, x, y)
            {
                gesture.desired = layout;
            }
        }
        if gesture.desired == gesture.acknowledged {
            if matches!(gesture.mode, Mode::Finishing) {
                self.resizing.gesture = None;
                self.state.status = "Resize finished".into();
            }
            return Ok(());
        }
        let flight = Commit {
            generation: gesture.generation,
            workspace: gesture.workspace.clone(),
            window: gesture.window.clone(),
            revision: gesture.revision,
            layout: gesture.desired.clone(),
        };
        let command = management::Command::Single(Operation::LayoutUpdate {
            workspace_id: flight.workspace.clone(),
            window_id: flight.window.clone(),
            expected_revision: flight.revision,
            layout: Some(flight.layout.clone()),
        });
        let socket = self.socket.to_owned();
        if let Err(error) = self
            .jobs
            .submit(format!("manage:window:{}", flight.window), move || {
                management::execute(&socket, command).map(Background::Resize)
            })
        {
            self.retire_resize("layout update was not admitted");
            return Err(error);
        }
        self.resizing.flight = Some(flight);
        Ok(())
    }

    pub(super) fn owns_resize_job(&self, key: &str) -> bool {
        self.resizing.flight.as_ref().is_some_and(|flight| {
            key.strip_prefix("manage:window:") == Some(flight.window.as_str())
        })
    }

    pub(super) fn complete_resize(&mut self, result: Result<management::Completion>) {
        let Some(flight) = &self.resizing.flight else {
            return;
        };
        let result = result.and_then(|result| {
            if result
                .registry
                .workspaces
                .iter()
                .find(|workspace| workspace.id == flight.workspace)
                .and_then(|workspace| {
                    workspace
                        .windows
                        .iter()
                        .find(|window| window.id == flight.window)
                })
                .is_some_and(|window| {
                    flight.revision.checked_add(1) == Some(window.revision)
                        && window.layout.as_ref() == Some(&flight.layout)
                })
            {
                Ok(result)
            } else {
                Err(transport::mark_outcome_unknown(anyhow::anyhow!(
                    "resize succeeded but response did not confirm the exact layout and revision"
                )))
            }
        });
        match result {
            Ok(result) => {
                self.accept_management(result, None);
                if self.resizing.acknowledge() {
                    if self.resizing.gesture.is_some() {
                        self.resize_hint();
                    } else {
                        self.state.status = "Resize finished".into();
                    }
                }
            }
            Err(error) => {
                let live = self.resizing.gesture.take().is_some();
                let flight = self.resizing.flight.take().unwrap();
                if matches!(self.pointer_capture, Some(PointerCapture::Resize)) {
                    self.pointer_capture = Some(PointerCapture::Ui);
                }
                if live || transport::outcome_unknown(&error) {
                    let outcome = if transport::outcome_unknown(&error) {
                        "outcome unknown"
                    } else {
                        "rejected"
                    };
                    self.state.status = format!(
                        "Resize {outcome} in {}: {error:#}; not replayed; start a new gesture after inspecting layout",
                        flight.window
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(ratio: u16) -> LayoutNode {
        LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio,
            first: Box::new(LayoutNode::Leaf {
                pane_id: "a".into(),
            }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: "b".into(),
            }),
        }
    }

    fn pending() -> State {
        State {
            generation: 1,
            gesture: Some(Gesture {
                generation: 1,
                workspace: "workspace".into(),
                window: "window".into(),
                pane: "a".into(),
                size: Size {
                    cols: 100,
                    rows: 30,
                    cell_width: 8,
                    cell_height: 16,
                },
                revision: 7,
                acknowledged: layout(500),
                desired: layout(600),
                mode: Mode::Drag,
                boundary: false,
                drag_path: Some(Vec::new()),
                pending_pointer: None,
            }),
            flight: Some(Commit {
                generation: 1,
                workspace: "workspace".into(),
                window: "window".into(),
                revision: 7,
                layout: layout(600),
            }),
        }
    }

    fn registry(revision: u64, ratio: u16) -> RegistrySnapshot {
        RegistrySnapshot {
            revision: 100,
            workspaces: vec![
                Workspace {
                    id: "other".into(),
                    name: "unrelated changes".into(),
                    revision: 99,
                    windows: Vec::new(),
                },
                Workspace {
                    id: "workspace".into(),
                    name: "target".into(),
                    revision: 900,
                    windows: vec![Window {
                        id: "window".into(),
                        name: "target window".into(),
                        revision,
                        layout: Some(layout(ratio)),
                    }],
                },
            ],
            panes: Vec::new(),
        }
    }

    #[test]
    fn own_registry_publication_before_completion_is_not_a_foreign_layout_change() {
        let mut state = pending();
        assert!(state.observes(&registry(7, 500)));
        assert!(state.observes(&registry(8, 600)));
        assert!(!state.observes(&registry(8, 700)));
        assert!(!state.observes(&registry(9, 600)));
        assert!(state.acknowledge());
        assert!(state.observes(&registry(8, 600)));
        assert!(!state.observes(&registry(7, 500)));
        assert_eq!(state.gesture.as_ref().unwrap().revision, 8);
    }

    #[test]
    fn unrelated_window_and_workspace_edits_do_not_cancel_target_resize() {
        let state = pending();
        let mut snapshot = registry(7, 500);
        snapshot.workspaces[1].revision += 25;
        snapshot.workspaces[1].windows.push(Window {
            id: "other-window".into(),
            name: "other".into(),
            revision: 22,
            layout: None,
        });
        assert!(state.observes(&snapshot));
        snapshot.workspaces[1].windows[1].revision += 1;
        assert!(state.observes(&snapshot));
        snapshot.workspaces[1].windows[0].revision += 1;
        assert!(!state.observes(&snapshot));
    }

    #[test]
    fn completion_preserves_latest_raw_release_until_its_final_commit_is_confirmed() {
        let mut state = pending();
        let gesture = state.gesture.as_mut().unwrap();
        gesture.mode = Mode::Finishing;
        gesture.pending_pointer = Some((90, 4));
        assert!(state.acknowledge());
        let gesture = state.gesture.as_mut().unwrap();
        assert_eq!(gesture.pending_pointer, Some((90, 4)));
        assert_eq!(gesture.revision, 8);
        assert_eq!(gesture.acknowledged, layout(600));
        gesture.pending_pointer = None;
        gesture.desired = layout(700);
        state.flight = Some(Commit {
            generation: 1,
            workspace: "workspace".into(),
            window: "window".into(),
            revision: 8,
            layout: layout(700),
        });
        assert!(state.acknowledge());
        assert!(state.gesture.is_none());
        assert!(state.flight.is_none());
    }

    #[test]
    fn retired_gesture_is_not_recreated_by_a_late_accepted_completion() {
        let mut state = pending();
        state.gesture = None;
        assert!(!state.acknowledge());
        assert!(state.gesture.is_none());
        assert!(state.flight.is_none());
    }
}
