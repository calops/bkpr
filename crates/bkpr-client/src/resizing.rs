use super::*;
use crate::tiling::{self, Direction};

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
    content_area: Rect,
    revision: u64,
    acknowledged: LayoutNode,
    desired: LayoutNode,
    mode: Mode,
    boundary: bool,
    drag_targets: Vec<tiling::DragTarget>,
    pending_pointer: Option<(u16, u16)>,
}
struct Commit {
    generation: u64,
    workspace: String,
    window: String,
    revision: u64,
    layout: LayoutNode,
}
struct Hover {
    workspace: String,
    window: String,
    revision: u64,
    area: Rect,
    position: (u16, u16),
    feedback: Vec<Rect>,
}
struct DividerClick {
    workspace: String,
    window: String,
    pane: String,
    revision: u64,
    size: Size,
    area: Rect,
    position: (u16, u16),
    targets: Vec<tiling::DragTarget>,
    at: Instant,
    released: bool,
}

impl DividerClick {
    fn matches(&self, other: &Self) -> bool {
        self.released
            && other.at.saturating_duration_since(self.at) < Duration::from_millis(500)
            && self.workspace == other.workspace
            && self.window == other.window
            && self.pane == other.pane
            && self.revision == other.revision
            && self.size == other.size
            && self.area == other.area
            && self.position == other.position
            && self.targets == other.targets
    }
}

#[derive(Default)]
pub(super) struct State {
    generation: u64,
    gesture: Option<Gesture>,
    flight: Option<Commit>,
    hover: Option<Hover>,
    chat_hover: Option<(u16, u16)>,
    click: Option<DividerClick>,
}
impl State {
    fn materialize_pointer(&mut self) {
        if self.flight.is_some() {
            return;
        }
        if let Some(gesture) = &mut self.gesture
            && let Some((x, y)) = gesture.pending_pointer.take()
            && let Some(layout) = tiling::drag_targets(
                &gesture.acknowledged,
                gesture.content_area,
                &gesture.drag_targets,
                x,
                y,
            )
        {
            gesture.desired = layout;
        }
    }

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

impl App {
    pub(super) fn resize_capturing(&self) -> bool {
        self.resizing.capturing()
    }

    pub(super) fn retire_resize(&mut self) {
        self.resizing.hover = None;
        self.resizing.chat_hover = None;
        self.chat.cancel_divider_drag();
        self.resizing.click = None;
        self.resizing.gesture = None;
        if matches!(
            self.pointer_capture,
            Some(PointerCapture::Resize | PointerCapture::ChatDivider)
        ) {
            // Keep swallowing the physical gesture until its release, rather
            // than converting its tail into input to the pane under the mouse.
            self.pointer_capture = Some(PointerCapture::Ui);
        }
    }

    pub(super) fn observe_resize(&mut self, registry: &RegistrySnapshot) {
        if self.resizing.click.as_ref().is_some_and(|click| {
            registry
                .workspaces
                .iter()
                .find(|workspace| workspace.id == click.workspace)
                .and_then(|workspace| {
                    workspace
                        .windows
                        .iter()
                        .find(|window| window.id == click.window)
                })
                .is_none_or(|window| window.revision != click.revision)
        }) {
            self.resizing.click = None;
        }
        if !self.resizing.observes(registry) {
            self.retire_resize();
        }
    }

    pub(super) fn validate_resize(&mut self, size: Size) {
        if self.resizing.click.as_ref().is_some_and(|click| {
            self.options.read_only
                || !self.host_focused
                || click.size != size
                || click.area != self.screen_geometry(size).1
                || self.state.workspace.as_ref() != Some(&click.workspace)
                || self.state.window.as_ref() != Some(&click.window)
                || self.state.focused.as_ref() != Some(&click.pane)
                || self
                    .state
                    .current_window(&self.registry)
                    .is_none_or(|window| window.revision != click.revision)
        }) {
            self.resizing.click = None;
        }
        let Some(gesture) = &self.resizing.gesture else {
            return;
        };
        if self.options.read_only
            || !self.host_focused
            || gesture.size != size
            || gesture.content_area != self.screen_geometry(size).1
            || self.state.workspace.as_ref() != Some(&gesture.workspace)
            || self.state.window.as_ref() != Some(&gesture.window)
            || self.state.focused.as_ref() != Some(&gesture.pane)
            || self.state.form.is_some()
            || self.picker_open()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
            || !self.resizing.observes(&self.registry)
        {
            self.retire_resize();
        }
    }

    fn begin_resize(
        &mut self,
        size: Size,
        mode: Mode,
        drag_targets: Vec<tiling::DragTarget>,
    ) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot resize layouts");
        }
        if !self.host_focused
            || self.state.form.is_some()
            || self.picker_open()
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
        let content_area = self.screen_geometry(size).1;
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
        tiling::dividers(Some(layout), content_area, &mut splits);
        if splits.is_empty() {
            bail!("Resize unavailable: no representable divider");
        }
        self.resizing.generation = self.resizing.generation.wrapping_add(1);
        self.resizing.click = None;
        self.resizing.hover = None;
        self.resizing.gesture = Some(Gesture {
            generation: self.resizing.generation,
            workspace: workspace.id.clone(),
            window: window.id.clone(),
            pane: pane.clone(),
            size,
            content_area,
            revision: window.revision,
            acknowledged: layout.clone(),
            desired: layout.clone(),
            mode,
            boundary: false,
            drag_targets,
            pending_pointer: None,
        });
        Ok(())
    }

    pub(super) fn resize_hint(&self) -> Option<&'static str> {
        match self.resizing.gesture.as_ref() {
            Some(gesture) => match gesture.mode {
                Mode::Keyboard if gesture.boundary => Some("Boundary: no matching divider"),
                Mode::Keyboard => None,
                Mode::Drag => Some("Drag divider; release to finish"),
                Mode::Finishing => Some("Resize pending; latest position not confirmed"),
            },
            None if self.resizing.flight.is_some() => {
                Some("Resize update pending; outcome not confirmed")
            }
            None => None,
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
            let content_area = self.screen_geometry(size).1;
            let gesture = self.resizing.gesture.as_mut().unwrap();
            if let Some(layout) = tiling::nudge(
                &gesture.desired,
                content_area,
                &gesture.pane,
                direction,
                cells,
            ) {
                gesture.desired = layout;
                gesture.boundary = false;
                self.flush_resize()?;
            } else {
                gesture.boundary = true;
            }
        }
        Ok(true)
    }

    pub(super) fn begin_keyboard_resize(&mut self, size: Size) -> Result<()> {
        self.begin_resize(size, Mode::Keyboard, Vec::new())
    }

    pub(super) fn clear_resize_hover(&mut self) {
        self.resizing.hover = None;
        self.resizing.chat_hover = None;
    }

    pub(super) fn resize_click_pointer(&mut self, x: u16, y: u16, eligible: bool) {
        if self
            .resizing
            .click
            .as_ref()
            .is_some_and(|click| !eligible || click.position != (x, y))
        {
            self.resizing.click = None;
        }
    }

    pub(super) fn update_resize_hover(&mut self, size: Size, position: Option<(u16, u16)>) -> bool {
        let position = position.filter(|_| {
            !self.options.read_only
                && self.host_focused
                && self.state.form.is_none()
                && !self.picker_open()
                && !self.consent_prompt_open()
                && !self.chat.editor_open()
                && !self.diagnostic_active()
                && self.image_viewer.is_none()
                && self.resizing.gesture.is_none()
                && self.resizing.flight.is_none()
        });
        let previous = self
            .resizing
            .chat_hover
            .take()
            .and_then(|point| self.chat.divider(point));
        if self.chat.visible() {
            self.resizing.hover = None;
            self.resizing.chat_hover = position;
            return previous != position.and_then(|point| self.chat.divider(point));
        }
        let area = self.screen_geometry(size).1;
        let context = position.and_then(|position| {
            let workspace = self.state.current(&self.registry)?;
            let window = self.state.current_window(&self.registry)?;
            Some((workspace, window, position))
        });
        let Some((workspace, window, position)) = context else {
            return self
                .resizing
                .hover
                .take()
                .is_some_and(|hover| !hover.feedback.is_empty());
        };
        // Repeated refreshes/keys at the same pointer do no tree projection.
        // There is only one cache entry, including negative (non-frame) hits.
        if self.resizing.hover.as_ref().is_some_and(|hover| {
            hover.workspace == workspace.id
                && hover.window == window.id
                && hover.revision == window.revision
                && hover.area == area
                && hover.position == position
        }) {
            return false;
        }
        let feedback = window.layout.as_ref().map_or_else(Vec::new, |layout| {
            let targets = tiling::resize_targets(layout, area, position.0, position.1);
            tiling::resize_feedback(layout, area, &targets)
        });
        let changed = self
            .resizing
            .hover
            .as_ref()
            .map_or(!feedback.is_empty(), |hover| hover.feedback != feedback);
        self.resizing.hover = Some(Hover {
            workspace: workspace.id.clone(),
            window: window.id.clone(),
            revision: window.revision,
            area,
            position,
            feedback,
        });
        changed
    }

    pub(super) fn resize_feedback(&self, size: Size) -> Vec<Rect> {
        if self.options.read_only || !self.host_focused {
            return Vec::new();
        }
        if self.chat.visible() {
            return self
                .resizing
                .chat_hover
                .and_then(|point| self.chat.divider(point))
                .map_or_else(Vec::new, |(rect, _)| vec![rect]);
        }
        let area = self.screen_geometry(size).1;
        let Some(workspace) = self.state.current(&self.registry) else {
            return Vec::new();
        };
        let Some(window) = self.state.current_window(&self.registry) else {
            return Vec::new();
        };
        if let Some(gesture) = &self.resizing.gesture {
            if gesture.size == size
                && gesture.content_area == area
                && gesture.workspace == workspace.id
                && gesture.window == window.id
                && self.resizing.observes(&self.registry)
                && let Some(layout) = &window.layout
            {
                // The registry may publish our accepted in-flight update
                // before its job completion. Always paint that actual layout.
                return tiling::resize_feedback(layout, area, &gesture.drag_targets);
            }
            return Vec::new();
        }
        self.resizing
            .hover
            .as_ref()
            .filter(|hover| {
                hover.workspace == workspace.id
                    && hover.window == window.id
                    && hover.revision == window.revision
                    && hover.area == area
            })
            .map_or_else(Vec::new, |hover| hover.feedback.clone())
    }

    pub(super) fn begin_divider_resize(&mut self, size: Size, x: u16, y: u16) -> Result<bool> {
        self.begin_divider_resize_at(size, x, y, Instant::now())
    }

    fn begin_divider_resize_at(&mut self, size: Size, x: u16, y: u16, at: Instant) -> Result<bool> {
        let previous = self.resizing.click.take();
        let area = self.screen_geometry(size).1;
        let Some(window) = self.state.current_window(&self.registry) else {
            return Ok(false);
        };
        let Some(layout) = &window.layout else {
            return Ok(false);
        };
        let targets = tiling::resize_targets(layout, area, x, y);
        if targets.is_empty() {
            return Ok(false);
        }
        let separators = tiling::separator_targets(layout, area, &targets, x, y);
        let click = (!separators.is_empty()).then(|| DividerClick {
            workspace: self.state.workspace.clone().unwrap(),
            window: window.id.clone(),
            pane: self.state.focused.clone().unwrap_or_default(),
            revision: window.revision,
            size,
            area,
            position: (x, y),
            targets: separators,
            at,
            released: false,
        });
        let equalize = previous
            .as_ref()
            .zip(click.as_ref())
            .is_some_and(|(previous, click)| previous.matches(click));
        self.begin_resize(size, Mode::Drag, targets)?;
        self.pointer_capture = Some(PointerCapture::Resize);
        if equalize {
            let gesture = self.resizing.gesture.as_mut().unwrap();
            if let Some(layout) = tiling::equalize_targets(
                &gesture.acknowledged,
                area,
                &click.as_ref().unwrap().targets,
            ) {
                gesture.desired = layout;
            }
            self.finish_resize()?;
        } else {
            self.resizing.click = click;
        }
        Ok(true)
    }

    pub(super) fn resize_pointer(&mut self, x: u16, y: u16, release: bool) -> Result<()> {
        if self
            .resizing
            .click
            .as_ref()
            .is_some_and(|click| click.position != (x, y))
        {
            self.resizing.click = None;
        }
        if release && let Some(click) = &mut self.resizing.click {
            click.released = true;
        }
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
        self.resizing.materialize_pointer();
        let Some(gesture) = &mut self.resizing.gesture else {
            return Ok(());
        };
        if gesture.desired == gesture.acknowledged {
            if matches!(gesture.mode, Mode::Finishing) {
                self.resizing.gesture = None;
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
            self.retire_resize();
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
                self.resizing.acknowledge();
            }
            Err(error) => {
                self.resizing.gesture = None;
                self.resizing.click = None;
                let flight = self.resizing.flight.take().unwrap();
                if matches!(self.pointer_capture, Some(PointerCapture::Resize)) {
                    self.pointer_capture = Some(PointerCapture::Ui);
                }
                let outcome = if transport::outcome_unknown(&error) {
                    "outcome unknown"
                } else {
                    "rejected"
                };
                self.notify(format!(
                    "Resize {outcome} in {}: {error:#}; not replayed; start a new gesture after inspecting layout",
                    flight.window
                ));
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
                content_area: Rect {
                    x: 38,
                    y: 0,
                    width: 62,
                    height: 30,
                },
                revision: 7,
                acknowledged: layout(500),
                desired: layout(600),
                mode: Mode::Drag,
                boundary: false,
                drag_targets: Vec::new(),
                pending_pointer: None,
            }),
            flight: Some(Commit {
                generation: 1,
                workspace: "workspace".into(),
                window: "window".into(),
                revision: 7,
                layout: layout(600),
            }),
            hover: None,
            chat_hover: None,
            click: None,
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

    #[test]
    fn inflight_corner_keeps_raw_release_and_materializes_both_axes_after_one_ack() {
        let mut state = pending();
        state.flight = None;
        let gesture = state.gesture.as_mut().unwrap();
        let pair = layout(500);
        let tree = LayoutNode::Split {
            axis: Axis::Vertical,
            ratio: 500,
            first: Box::new(pair),
            second: Box::new(LayoutNode::Leaf {
                pane_id: "c".into(),
            }),
        };
        let area = gesture.content_area;
        let mut original = Vec::new();
        tiling::dividers(Some(&tree), area, &mut original);
        let (x, y) = (original[1].divider.x + 2, original[0].divider.y);
        gesture.drag_targets = tiling::resize_targets(&tree, area, x, y);
        assert_eq!(gesture.drag_targets.len(), 2);
        gesture.acknowledged = tree.clone();
        gesture.desired = tree;
        gesture.pending_pointer = Some((x + 3, y + 2));
        state.materialize_pointer();
        let gesture = state.gesture.as_ref().unwrap();
        let first = gesture.desired.clone();
        let mut splits = Vec::new();
        tiling::dividers(Some(&first), area, &mut splits);
        assert_eq!(splits[0].divider.y, original[0].divider.y + 2);
        assert_eq!(splits[1].divider.x, original[1].divider.x + 3);
        state.flight = Some(Commit {
            generation: 1,
            workspace: "workspace".into(),
            window: "window".into(),
            revision: 7,
            layout: first.clone(),
        });
        let gesture = state.gesture.as_mut().unwrap();
        gesture.mode = Mode::Finishing;
        gesture.pending_pointer = Some((x + 8, y + 5));
        state.materialize_pointer();
        assert_eq!(state.gesture.as_ref().unwrap().desired, first);
        assert_eq!(
            state.gesture.as_ref().unwrap().pending_pointer,
            Some((x + 8, y + 5))
        );
        assert!(state.acknowledge());
        state.materialize_pointer();
        let gesture = state.gesture.as_ref().unwrap();
        assert_eq!(gesture.revision, 8);
        assert_eq!(gesture.acknowledged, first);
        let mut final_splits = Vec::new();
        tiling::dividers(Some(&gesture.desired), area, &mut final_splits);
        assert_eq!(final_splits[0].divider.y, original[0].divider.y + 5);
        assert_eq!(final_splits[1].divider.x, original[1].divider.x + 8);
        state.flight = Some(Commit {
            generation: 1,
            workspace: "workspace".into(),
            window: "window".into(),
            revision: 8,
            layout: gesture.desired.clone(),
        });
        assert!(state.acknowledge());
        assert!(state.gesture.is_none());
    }

    #[test]
    fn retired_corner_never_materializes_the_queued_pointer_against_a_later_layout() {
        let mut state = pending();
        state.gesture.as_mut().unwrap().pending_pointer = Some((80, 20));
        state.gesture = None;
        state.materialize_pointer();
        assert!(!state.acknowledge());
        state.materialize_pointer();
        assert!(state.gesture.is_none());
        assert!(state.flight.is_none());
    }
}

#[cfg(test)]
mod double_click_tests {
    use super::*;

    fn fixture(axis: Axis) -> (App, Size, (u16, u16)) {
        let (mut app, _) = crate::app::tests::fixture();
        let layout = LayoutNode::Split {
            axis,
            ratio: 300,
            first: Box::new(LayoutNode::Leaf {
                pane_id: "a".into(),
            }),
            second: Box::new(LayoutNode::Split {
                axis: if axis == Axis::Horizontal {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                },
                ratio: 650,
                first: Box::new(LayoutNode::Leaf {
                    pane_id: "b".into(),
                }),
                second: Box::new(LayoutNode::Leaf {
                    pane_id: "c".into(),
                }),
            }),
        };
        app.registry = RegistrySnapshot {
            revision: 1,
            workspaces: vec![Workspace {
                id: "workspace".into(),
                name: "workspace".into(),
                revision: 1,
                windows: vec![Window {
                    id: "window".into(),
                    name: "window".into(),
                    revision: 7,
                    layout: Some(layout),
                }],
            }],
            panes: Vec::new(),
        };
        app.state.workspace = Some("workspace".into());
        app.state.window = Some("window".into());
        app.state.focused = Some("a".into());
        app.jobs = jobs::Scope::new(1).unwrap();
        let size = Size {
            cols: 140,
            rows: 44,
            cell_width: 8,
            cell_height: 16,
        };
        let mut splits = Vec::new();
        tiling::dividers(
            app.registry.workspaces[0].windows[0].layout.as_ref(),
            app.screen_geometry(size).1,
            &mut splits,
        );
        let divider = splits[0].divider;
        let point = (
            divider.x + divider.width / 2,
            divider.y + divider.height / 2,
        );
        (app, size, point)
    }

    fn click(app: &mut App, size: Size, point: (u16, u16), at: Instant) {
        assert!(
            app.begin_divider_resize_at(size, point.0, point.1, at)
                .unwrap()
        );
        app.resize_pointer(point.0, point.1, true).unwrap();
        app.pointer_capture = None;
        // Queue the second click's desired mutation without issuing native I/O.
        app.jobs
            .submit("fixture:capacity".into(), || bail!("fixture"))
            .unwrap();
    }

    #[test]
    fn double_click_equalizes_each_axis_without_changing_unrelated_splits() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let (mut app, size, point) = fixture(axis);
            let at = Instant::now();
            click(&mut app, size, point, at);
            assert!(app.resizing.gesture.is_none());
            assert!(
                app.begin_divider_resize_at(
                    size,
                    point.0,
                    point.1,
                    at + Duration::from_millis(499),
                )
                .unwrap()
            );
            let gesture = app.resizing.gesture.as_ref().unwrap();
            assert!(matches!(gesture.mode, Mode::Finishing));
            assert_eq!(gesture.revision, 7);
            assert_eq!(gesture.window, "window");
            let mut expected = gesture.acknowledged.clone();
            let LayoutNode::Split { ratio, .. } = &mut expected else {
                unreachable!()
            };
            *ratio = 500;
            assert_eq!(gesture.desired, expected);
            assert!(matches!(app.pointer_capture, Some(PointerCapture::Ui)));
            app.resize_pointer(point.0 + 5, point.1 + 3, true).unwrap();
            assert_eq!(app.resizing.gesture.as_ref().unwrap().desired, expected);
            assert!(app.resizing.flight.is_none());
            app.retire_resize();
            assert!(app.resizing.gesture.is_none());
            assert!(app.resizing.click.is_none());
            assert!(matches!(app.pointer_capture, Some(PointerCapture::Ui)));
        }
    }

    #[test]
    fn invalidated_clicks_never_equalize_a_later_matching_separator() {
        for invalidation in [
            "movement",
            "foreign",
            "geometry",
            "retired",
            "other-button",
            "timeout",
            "window",
        ] {
            let (mut app, size, point) = fixture(Axis::Horizontal);
            let at = Instant::now();
            click(&mut app, size, point, at);
            match invalidation {
                "movement" => {
                    app.resize_click_pointer(point.0 + 1, point.1, true);
                    app.resize_click_pointer(point.0, point.1, true);
                }
                "foreign" => {
                    let mut changed = app.registry.clone();
                    changed.workspaces[0].windows[0].revision += 1;
                    app.observe_resize(&changed);
                }
                "geometry" => app.validate_resize(Size {
                    cols: size.cols + 1,
                    ..size
                }),
                "retired" => app.retire_resize(),
                "other-button" => app.resize_click_pointer(point.0, point.1, false),
                "timeout" => {}
                "window" => {
                    let mut other = app.registry.workspaces[0].windows[0].clone();
                    other.id = "other-window".into();
                    app.registry.workspaces[0].windows.push(other);
                    app.state.window = Some("other-window".into());
                }
                _ => unreachable!(),
            }
            let delay = if invalidation == "timeout" { 500 } else { 100 };
            assert!(
                app.begin_divider_resize_at(
                    size,
                    point.0,
                    point.1,
                    at + Duration::from_millis(delay),
                )
                .unwrap()
            );
            let gesture = app.resizing.gesture.as_ref().unwrap();
            assert!(matches!(gesture.mode, Mode::Drag), "{invalidation}");
            assert_eq!(gesture.desired, gesture.acknowledged, "{invalidation}");
        }
    }

    #[test]
    fn readonly_modal_and_inflight_second_clicks_cannot_admit_equalization() {
        for invalidation in ["readonly", "modal", "inflight"] {
            let (mut app, size, point) = fixture(Axis::Horizontal);
            let at = Instant::now();
            click(&mut app, size, point, at);
            match invalidation {
                "readonly" => app.options.read_only = true,
                "modal" => app.state.open(FormKind::ProjectDetails, String::new()),
                "inflight" => {
                    app.resizing.flight = Some(Commit {
                        generation: 1,
                        workspace: "workspace".into(),
                        window: "window".into(),
                        revision: 7,
                        layout: app.registry.workspaces[0].windows[0]
                            .layout
                            .clone()
                            .unwrap(),
                    });
                }
                _ => unreachable!(),
            }
            assert!(
                app.begin_divider_resize_at(
                    size,
                    point.0,
                    point.1,
                    at + Duration::from_millis(100),
                )
                .is_err()
            );
            assert!(app.resizing.gesture.is_none());
            assert!(app.resizing.click.is_none());
            assert!(app.jobs.is_full());
        }
    }
}
