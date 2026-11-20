//! Recovery overlay ownership, worker receipts and local scene presentation.
use super::recovery_picker::{self as model, Key};
use super::*;
use bkpr_tui_framework::search_list::SearchListIntent;
use tokio_util::sync::CancellationToken;

/// Worker results remain bound to their original attachment and UI generation.
pub(super) enum Completion {
    /// Read-only listing for one still-current picker generation.
    Listed {
        owner: TreeOwner,
        epoch: u64,
        result: Result<model::Listing>,
    },
    /// Single native resume invocation, separate from eventual native binding.
    Resumed {
        owner: TreeOwner,
        serial: u64,
        result: std::result::Result<Box<model::Started>, model::ResumeFailure>,
    },
}

struct Mutation {
    serial: u64,
    epoch: u64,
    token: CancellationToken,
}
struct Waiting {
    expected: HarnessRecovery,
    revision: u64,
}

/// One owner-local picker, one admitted mutation and bounded-by-pane binding waits.
#[derive(Default)]
pub(super) struct State {
    next: u64,
    view: Option<View>,
    mutation: Option<Mutation>,
    waiting: HashMap<String, Waiting>,
}

struct View {
    epoch: u64,
    token: CancellationToken,
    picker: Option<model::Picker>,
    loading: bool,
    error: Option<String>,
    ambiguous: bool,
    owner_label: String,
    details: UiSurface,
    list_rect: Rect,
    detail_rect: Rect,
    button: Option<Rect>,
    detail_focus: bool,
    scroll: usize,
    lines: Vec<crate::metadata::PreviewLine>,
    selected: Option<Key>,
    width: u16,
    dirty: bool,
    foregrounds: Vec<ForegroundSpan>,
    line_revision: u64,
    painted: Option<DetailsPaint>,
    detail_roles: Vec<RoleSpan>,
}

#[derive(Clone, Copy, PartialEq)]
struct DetailsPaint {
    size: Size,
    origin: u16,
    revision: u64,
    scroll: usize,
    focused: bool,
    enabled: bool,
    busy: bool,
}

impl Drop for View {
    fn drop(&mut self) {
        self.token.cancel();
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(mutation) = &self.mutation {
            mutation.token.cancel();
        }
    }
}

impl State {
    /// Whether the modal currently owns a recovery selection surface.
    pub(super) fn open(&self) -> bool {
        self.view.is_some()
    }
    /// An invoked mutation is never refreshed or submitted a second time.
    pub(super) fn starting(&self) -> bool {
        self.mutation.is_some()
    }
    /// Only an available exact candidate can offer the submit action.
    pub(super) fn can_resume(&self) -> bool {
        self.mutation.is_none()
            && self.view.as_ref().is_some_and(|view| {
                !view.loading
                    && !view.ambiguous
                    && view.picker.as_ref().is_some_and(model::Picker::can_resume)
            })
    }
    /// Current list projection, excluding the independently composed details.
    pub(super) fn list_rect(&self, body: Rect) -> Rect {
        self.view.as_ref().map_or(body, |view| Rect {
            x: body.x + view.list_rect.x,
            y: body.y + view.list_rect.y,
            ..view.list_rect
        })
    }
    /// Retained details frame and its position within the same modal owner.
    pub(super) fn details(&self, body: Rect) -> Option<(&Frame, Rect, bool)> {
        let view = self.view.as_ref()?;
        (view.detail_rect.height > 0).then(|| {
            (
                &view.details.frame,
                Rect {
                    x: body.x + view.detail_rect.x,
                    y: body.y + view.detail_rect.y,
                    ..view.detail_rect
                },
                view.detail_focus,
            )
        })
    }
    /// Details focus hides the query caret but retains its logical input state.
    pub(super) fn list_focused(&self) -> bool {
        self.view.as_ref().is_none_or(|view| !view.detail_focus)
    }
    /// Paint child surfaces without copying terminal scenes or writing host bytes.
    pub(super) fn paint(&mut self, list: &mut UiSurface, size: Size, theme: &Theme) {
        let enabled = self.can_resume();
        let busy = self.mutation.is_some();
        let Some(view) = &mut self.view else { return };
        let list_height = if size.rows <= 4 {
            size.rows.min(2)
        } else {
            (size.rows / 3).clamp(3, 10)
        };
        view.list_rect = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: list_height,
        };
        view.detail_rect = Rect {
            x: 0,
            y: list_height,
            width: size.cols,
            height: size.rows.saturating_sub(list_height),
        };
        let local = view.list_rect.size(size);
        if let Some(picker) = &mut view.picker {
            picker.list.set_title(&view.owner_label);
            picker.list.paint(list, local);
            view.foregrounds.clear();
            view.foregrounds
                .extend(
                    picker
                        .list
                        .visible_items(local)
                        .map(|(rect, key)| ForegroundSpan {
                            rect,
                            foreground: ColorSource::Rgb(
                                theme.style(picker.row_status(key)).foreground,
                            ),
                        }),
                );
            picker.list.apply_theme(list, theme, &view.foregrounds);
        } else {
            list.paint(
                local,
                vec![
                    (
                        Rect {
                            x: 0,
                            y: 0,
                            width: size.cols,
                            height: 1,
                        },
                        view.owner_label.clone(),
                        false,
                    ),
                    (
                        Rect {
                            x: 0,
                            y: 1,
                            width: size.cols,
                            height: 1,
                        },
                        if view.loading {
                            "Loading saved conversations…"
                        } else {
                            "Recovery list unavailable"
                        }
                        .into(),
                        false,
                    ),
                ],
                None,
            );
            list.apply_theme(theme, Role::Panel, &[]);
        }
        if view.detail_focus {
            list.frame.cursor.visible = false;
        }
        let selected_changed = view
            .picker
            .as_ref()
            .and_then(|picker| picker.list.selected())
            != view.selected.as_ref();
        if selected_changed || view.width != size.cols || view.dirty {
            view.selected = view
                .picker
                .as_ref()
                .and_then(|picker| picker.list.selected())
                .cloned();
            view.width = size.cols;
            view.lines.clear();
            if let Some(error) = &view.error {
                crate::metadata::wrap(
                    &[crate::metadata::Paragraph::new(error, Role::Error)],
                    size.cols,
                    &mut view.lines,
                );
            }
            if let Some(picker) = &mut view.picker {
                crate::metadata::wrap(picker.details(), size.cols, &mut view.lines);
            } else {
                crate::metadata::wrap(
                    &[crate::metadata::Paragraph::new(
                        "Uses current native configuration and permissions. Native state/provider availability is not preflighted; starting a process is not verified binding.",
                        Role::Muted,
                    )],
                    size.cols,
                    &mut view.lines,
                );
            }
            if selected_changed {
                view.scroll = 0;
            }
            view.dirty = false;
            view.line_revision = view.line_revision.wrapping_add(1);
        }
        let local = view.detail_rect.size(size);
        let capacity = usize::from(local.rows.saturating_sub(2));
        view.scroll = view.scroll.min(view.lines.len().saturating_sub(capacity));
        let painted = DetailsPaint {
            size: local,
            origin: view.detail_rect.y,
            revision: view.line_revision,
            scroll: view.scroll,
            focused: view.detail_focus,
            enabled,
            busy,
        };
        if view.painted == Some(painted) {
            view.details
                .apply_theme(theme, Role::Panel, &view.detail_roles);
            return;
        }
        view.painted = Some(painted);
        let mut rows = Vec::with_capacity(usize::from(local.rows));
        let roles = &mut view.detail_roles;
        roles.clear();
        if local.rows > 0 {
            rows.push((
                Rect {
                    x: 0,
                    y: 0,
                    width: local.cols,
                    height: 1,
                },
                "─".repeat(usize::from(local.cols)),
                false,
            ));
            roles.push(RoleSpan {
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: local.cols,
                    height: 1,
                },
                role: Role::Muted,
            });
        }
        for (index, line) in view
            .lines
            .iter()
            .skip(view.scroll)
            .take(capacity)
            .enumerate()
        {
            let rect = Rect {
                x: 0,
                y: index as u16 + 1,
                width: local.cols,
                height: 1,
            };
            rows.push((rect, line.text.clone(), false));
            roles.push(RoleSpan {
                rect,
                role: line.role,
            });
        }
        view.button = None;
        if local.rows > 1 {
            let label = if busy {
                "[ Starting… ]"
            } else if enabled {
                "[ Resume ]"
            } else {
                "[ Unavailable ]"
            };
            let width = text_width(label).min(usize::from(local.cols)) as u16;
            let rect = Rect {
                x: local.cols.saturating_sub(width),
                y: local.rows - 1,
                width,
                height: 1,
            };
            let help = if busy {
                "Esc hide · start may already be accepted"
            } else if view.detail_focus {
                "Tab list · PgUp/PgDn scroll · Esc close"
            } else {
                "Tab details · Ctrl-r refresh · Esc close"
            };
            rows.push((
                Rect {
                    x: 0,
                    width: rect.x.saturating_sub(1),
                    ..rect
                },
                help.into(),
                false,
            ));
            rows.push((rect, label.into(), false));
            roles.push(RoleSpan {
                rect,
                role: if enabled { Role::Accent } else { Role::Muted },
            });
            if enabled {
                view.button = Some(Rect {
                    y: view.detail_rect.y + rect.y,
                    ..rect
                });
            }
        }
        view.details.paint(
            local,
            rows,
            Some(Cursor {
                x: 0,
                y: u16::from(local.rows > 1),
                visible: false,
                blinking: false,
                shape: CursorShape::Block,
                color: None,
            }),
        );
        view.details.apply_theme(theme, Role::Panel, roles);
    }
}

impl App {
    /// Start read-only discovery on the currently admitted attachment, never a guessed host.
    pub(super) fn begin_recovery(&mut self, size: Size) {
        self.begin_modal();
        self.finder = None;
        self.state.cancel();
        self.retire_resize("recovery picker opened");
        self.selection_focus_lost();
        self.close_search();
        self.popover.hide();
        self.recovery.next = self.recovery.next.wrapping_add(1);
        let owner_label = self
            .tree
            .lock()
            .unwrap()
            .session(self.tree_owner())
            .map(|session| {
                format!(
                    "Resume conversations · {}{}",
                    session.group.name,
                    session
                        .group
                        .host
                        .as_ref()
                        .map_or_else(String::new, |host| format!(" @ {host}"))
                )
            })
            .unwrap_or_else(|| "Resume conversations".into());
        self.recovery.view = Some(View {
            epoch: self.recovery.next,
            token: CancellationToken::new(),
            picker: None,
            loading: false,
            error: None,
            ambiguous: false,
            owner_label,
            details: UiSurface::new(size),
            list_rect: Rect::default(),
            detail_rect: Rect::default(),
            button: None,
            detail_focus: false,
            scroll: 0,
            lines: Vec::new(),
            selected: None,
            width: 0,
            dirty: true,
            foregrounds: Vec::new(),
            line_revision: 0,
            painted: None,
            detail_roles: Vec::new(),
        });
        self.refresh_recovery();
    }
    /// Retire an unsent selection; an already invoked mutation still drains its receipt.
    pub(super) fn cancel_recovery(&mut self) {
        self.recovery.view = None;
    }
    /// Refresh observations without replaying any mutation or replacing native settings.
    pub(super) fn refresh_recovery(&mut self) {
        let owner = self.tree_owner();
        let Some(view) = &mut self.recovery.view else {
            return;
        };
        if self.recovery.mutation.is_some() {
            return;
        }
        view.token.cancel();
        view.token = CancellationToken::new();
        self.recovery.next = self.recovery.next.wrapping_add(1);
        view.epoch = self.recovery.next;
        view.loading = true;
        view.error = None;
        view.dirty = true;
        let epoch = view.epoch;
        let cancellation = view.token.clone();
        let endpoint = self.socket.clone();
        let result = self.jobs.submit(format!("recovery:list:{epoch}"), move || {
            Ok(Background::Recovery(Box::new(Completion::Listed {
                owner,
                epoch,
                result: model::fetch_cancellable(&endpoint, &cancellation),
            })))
        });
        if let Err(error) = result {
            self.recovery_read_failed(epoch, error);
        }
    }
    fn recovery_read_failed(&mut self, epoch: u64, error: anyhow::Error) {
        self.report_failure("Couldn't list saved conversations.", &error);
        if let Some(view) = self
            .recovery
            .view
            .as_mut()
            .filter(|view| view.epoch == epoch)
        {
            view.loading = false;
            view.error = Some(format!("{error:#}"));
            view.dirty = true;
        }
    }
    /// Admit one exact descriptor. Repeats and read-only transitions cannot submit another.
    pub(super) fn submit_recovery(&mut self) {
        if self.options.read_only || !self.recovery.can_resume() {
            return;
        }
        let owner = self.tree_owner();
        let view = self.recovery.view.as_ref().unwrap();
        let key = view
            .picker
            .as_ref()
            .and_then(|picker| picker.list.selected())
            .unwrap()
            .clone();
        let epoch = view.epoch;
        let cancellation = view.token.clone();
        self.recovery.next = self.recovery.next.wrapping_add(1);
        let serial = self.recovery.next;
        let endpoint = self.socket.clone();
        self.recovery.mutation = Some(Mutation {
            serial,
            epoch,
            token: cancellation.clone(),
        });
        let result = self
            .jobs
            .submit(format!("recovery:resume:{serial}"), move || {
                Ok(Background::Recovery(Box::new(Completion::Resumed {
                    owner,
                    serial,
                    result: model::resume(&endpoint, &key, &cancellation).map(Box::new),
                })))
            });
        if let Err(error) = result {
            self.recovery.mutation = None;
            self.recovery_read_failed(epoch, error);
        }
    }
    /// Consume recovery UI keys, including releases, without delivering them to a pane.
    pub(super) fn recovery_key(&mut self, event: &KeyEvent, size: Size) {
        if event.action == KeyAction::Release {
            return;
        }
        use crate::keyguide::{self, Action, Context, Navigation};
        let action = keyguide::lookup_with(Context::Recovery, event, |action| {
            self.supports_action(action)
        });
        match action {
            Some(Action::Cancel) => {
                self.cancel_modal();
                return;
            }
            Some(Action::Submit) => {
                self.submit_recovery();
                return;
            }
            Some(Action::RefreshRecovery) => {
                self.refresh_recovery();
                return;
            }
            _ => {}
        }
        let Some(view) = &mut self.recovery.view else {
            return;
        };
        if action == Some(Action::CycleView) {
            view.detail_focus = !view.detail_focus;
            return;
        }
        if view.detail_focus {
            let page = usize::from(view.detail_rect.height.saturating_sub(2).max(1));
            match action {
                Some(Action::Navigate(Navigation::Previous)) => {
                    view.scroll = view.scroll.saturating_sub(1)
                }
                Some(Action::Navigate(Navigation::Next)) => {
                    view.scroll = view.scroll.saturating_add(1)
                }
                Some(Action::Navigate(Navigation::PagePrevious)) => {
                    view.scroll = view.scroll.saturating_sub(page)
                }
                Some(Action::Navigate(Navigation::PageNext)) => {
                    view.scroll = view.scroll.saturating_add(page)
                }
                Some(Action::Edit(EditAction::Home)) => view.scroll = 0,
                Some(Action::Edit(EditAction::End)) => view.scroll = view.lines.len(),
                _ if !event.text.is_empty() && event.modifiers & 14 == 0 => {
                    view.detail_focus = false
                }
                _ => return,
            }
            if view.detail_focus {
                return;
            }
        }
        if let Some(picker) = &mut view.picker {
            picker.list.handle_key(event, view.list_rect.size(size));
        }
    }
    /// Paste remains bounded query input owned by the modal, never resumed application input.
    pub(super) fn recovery_paste(&mut self, text: &str) {
        if let Some(view) = &mut self.recovery.view
            && let Some(picker) = &mut view.picker
        {
            view.detail_focus = false;
            picker.list.paste(text);
        }
    }
    /// Pointer coordinates are local modal content cells, not pixel or terminal coordinates.
    pub(super) fn recovery_mouse(&mut self, event: &MouseEvent, size: Size) {
        let Some(view) = &mut self.recovery.view else {
            return;
        };
        let (x, y) = (event.x as u16, event.y as u16);
        if event.action == MouseAction::Press
            && event.button == MouseButton::Left
            && view.button.is_some_and(|rect| rect.contains(x, y))
        {
            self.submit_recovery();
            return;
        }
        if view.detail_rect.contains(x, y) {
            match event.button {
                MouseButton::WheelUp => view.scroll = view.scroll.saturating_sub(3),
                MouseButton::WheelDown => view.scroll = view.scroll.saturating_add(3),
                MouseButton::Left if event.action == MouseAction::Press => view.detail_focus = true,
                _ => {}
            }
        } else if let Some(picker) = &mut view.picker {
            if event.action == MouseAction::Press && event.button == MouseButton::Left {
                view.detail_focus = false;
            }
            let intent = picker.list.handle_mouse(event, view.list_rect.size(size));
            debug_assert!(intent != SearchListIntent::Accept);
        }
    }
    /// Handle a generation-bound worker result once; successful starts are not bindings.
    pub(super) fn complete_recovery(&mut self, completion: jobs::Completion<Background>) {
        let result = completion.result.and_then(|background| match background {
            Background::Recovery(result) => Ok(*result),
            _ => bail!("recovery worker returned an unrelated result"),
        });
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let serial = completion
                    .key
                    .strip_prefix("recovery:resume:")
                    .and_then(|value| value.parse::<u64>().ok());
                if self
                    .recovery
                    .mutation
                    .as_ref()
                    .is_some_and(|mutation| Some(mutation.serial) == serial)
                {
                    let mutation = self.recovery.mutation.take().unwrap();
                    self.recovery_failed(
                        mutation.epoch,
                        true,
                        transport::mark_outcome_unknown(error),
                    );
                } else if let Some(epoch) = completion
                    .key
                    .strip_prefix("recovery:list:")
                    .and_then(|value| value.parse::<u64>().ok())
                    && self
                        .recovery
                        .view
                        .as_ref()
                        .is_some_and(|view| view.epoch == epoch)
                {
                    self.recovery_read_failed(epoch, error);
                }
                return;
            }
        };
        match result {
            Completion::Listed {
                owner,
                epoch,
                result,
            } => {
                if owner != self.tree_owner()
                    || self
                        .recovery
                        .view
                        .as_ref()
                        .is_none_or(|view| view.epoch != epoch)
                {
                    return;
                }
                match result {
                    Ok(listing) => {
                        let view = self.recovery.view.as_mut().unwrap();
                        if let Some(picker) = &mut view.picker {
                            picker.replace(listing);
                        } else {
                            view.picker = Some(model::Picker::new(listing, self.options.read_only));
                        }
                        view.loading = false;
                        view.dirty = true;
                    }
                    Err(error) => self.recovery_read_failed(epoch, error),
                }
            }
            Completion::Resumed {
                owner,
                serial,
                result,
            } => {
                if owner != self.tree_owner()
                    || self
                        .recovery
                        .mutation
                        .as_ref()
                        .is_none_or(|mutation| mutation.serial != serial)
                {
                    return;
                }
                let mutation = self.recovery.mutation.take().unwrap();
                match result {
                    Ok(started) => {
                        let model::Started {
                            registry,
                            selection,
                            source,
                        } = *started;
                        let navigate = !self.backgrounded
                            && self
                                .recovery
                                .view
                                .as_ref()
                                .is_some_and(|view| view.epoch == mutation.epoch);
                        self.recovery.waiting.insert(
                            selection.pane.clone(),
                            Waiting {
                                expected: source.expected,
                                revision: registry.revision,
                            },
                        );
                        if navigate {
                            self.cancel_modal();
                        }
                        self.accept_management(
                            management::Completion {
                                registry,
                                selection: navigate.then_some(selection),
                            },
                            navigate.then_some(ManagementIntent {
                                epoch: self.selection_epoch,
                                selection: None,
                            }),
                        );
                        if navigate {
                            self.leave_sidebar();
                        }
                        self.notify("Resume process started; waiting for authenticated native conversation binding".into());
                    }
                    Err(failure) => {
                        self.recovery_failed(mutation.epoch, failure.attempted, failure.error)
                    }
                }
            }
        }
    }
    fn recovery_failed(&mut self, epoch: u64, attempted: bool, error: anyhow::Error) {
        let unknown = attempted && transport::outcome_unknown(&error);
        self.report_failure(
            if unknown {
                "Resume outcome unknown; not replayed."
            } else {
                "Couldn't resume this conversation."
            },
            &error,
        );
        if let Some(view) = self
            .recovery
            .view
            .as_mut()
            .filter(|view| view.epoch == epoch)
        {
            view.error = Some(if unknown {
                format!(
                    "Outcome unknown. Inspect current panes before making another explicit attempt; nothing was replayed. {error:#}"
                )
            } else {
                format!("{error:#}")
            });
            view.ambiguous = unknown;
            view.dirty = true;
        }
    }
    /// Published native identity, not process creation/output, completes a binding wait.
    pub(super) fn observe_recovery_bindings(&mut self) {
        let mut completed = Vec::new();
        for (id, waiting) in &self.recovery.waiting {
            if self.registry.revision < waiting.revision {
                continue;
            }
            let Some(pane) = self.registry.panes.iter().find(|pane| pane.id == *id) else {
                completed.push((
                    id.clone(),
                    Err(anyhow::anyhow!(
                        "resumed pane was removed before native binding"
                    )),
                ));
                continue;
            };
            if pane.status != PaneStatus::Running {
                completed.push((id.clone(), Err(anyhow::anyhow!("resume process exited before native binding (exit {:?}); saved intent remains subject to current recovery eligibility", pane.exit_code))));
                continue;
            }
            let Some(harness) = crate::controller::recognized_harness(
                pane,
                self.views.get(id).map(|view| &view.metadata),
            ) else {
                continue;
            };
            let Some(managed) = &harness.managed else {
                continue;
            };
            if let Some(error) = &managed.error {
                completed.push((id.clone(), Err(anyhow::anyhow!("{error}"))));
            } else if let Some(binding) = &managed.conversation {
                if harness.kind == waiting.expected.kind
                    && binding.id == waiting.expected.conversation_id
                {
                    completed.push((id.clone(), Ok(())));
                } else {
                    completed.push((id.clone(), Err(anyhow::anyhow!("native binding differs from the selected conversation; resume was not verified"))));
                }
            }
        }
        for (id, result) in completed {
            self.recovery.waiting.remove(&id);
            match result {
                Ok(()) => self.notify("Native conversation binding verified; native configuration remains authoritative".into()),
                Err(error) => { self.report_failure("Native recovery did not reach the requested binding.", &error); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> Size {
        Size {
            cols: 120,
            rows: 40,
            cell_width: 9,
            cell_height: 18,
        }
    }

    #[test]
    fn cancelled_start_receipt_never_closes_a_new_picker_or_steals_its_pane_focus() {
        let (mut app, capture) = super::super::tests::fixture();
        super::super::tests::attach_named_fixture(&mut app, "original", "original-view");
        app.registry.panes[0].workspace_id = Some("work".into());
        app.registry.panes[0].window_id = Some("window".into());
        app.registry.workspaces = vec![Workspace {
            id: "work".into(),
            name: "Work".into(),
            revision: 1,
            windows: vec![Window {
                id: "window".into(),
                name: "Window".into(),
                revision: 1,
                layout: Some(LayoutNode::Leaf {
                    pane_id: "original".into(),
                }),
            }],
        }];
        app.state = Controller::new(&app.registry);
        app.begin_recovery(size());
        let view = app.recovery.view.as_ref().unwrap();
        let epoch = view.epoch;
        let serial = app.recovery.next.wrapping_add(1);
        app.recovery.mutation = Some(Mutation {
            serial,
            epoch,
            token: view.token.clone(),
        });
        let owner = app.tree_owner();
        app.cancel_modal();
        app.begin_theme_picker();
        let mut registry = app.registry.clone();
        registry.revision += 1;
        let mut pane = registry.panes[0].clone();
        pane.id = "created".into();
        registry.panes.push(pane);
        registry.workspaces[0].windows[0].layout = Some(LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(LayoutNode::Leaf {
                pane_id: "original".into(),
            }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: "created".into(),
            }),
        });
        app.complete_recovery(jobs::Completion {
            key: format!("recovery:resume:{serial}"),
            result: Ok(Background::Recovery(Box::new(Completion::Resumed {
                owner,
                serial,
                result: Ok(Box::new(model::Started {
                    registry,
                    selection: management::Selection {
                        workspace: "work".into(),
                        window: "window".into(),
                        pane: "created".into(),
                    },
                    source: Key {
                        pane_id: "retained".into(),
                        expected: HarnessRecovery {
                            kind: HarnessKind::Pi,
                            conversation_id: "native".into(),
                            cwd: "/fixture".into(),
                        },
                    },
                })),
            }))),
        });
        assert!(app.theme_picker.is_some());
        assert!(!app.recovery.open());
        assert_eq!(app.state.focused.as_deref(), Some("original"));
        assert!(app.registry.panes.iter().any(|pane| pane.id == "created"));
        assert!(app.recovery.waiting.contains_key("created"));
        assert!(
            capture.try_recv().is_err(),
            "late recovery must not become application input"
        );
    }

    #[test]
    fn retired_listing_error_cannot_replace_the_new_picker_or_publish_a_failure() {
        let (mut app, _) = super::super::tests::fixture();
        app.begin_recovery(size());
        let retired = app.recovery.view.as_ref().unwrap().epoch;
        let owner = app.tree_owner();
        app.refresh_recovery();
        let current = app.recovery.view.as_ref().unwrap().epoch;
        app.complete_recovery(jobs::Completion {
            key: format!("recovery:list:{retired}"),
            result: Ok(Background::Recovery(Box::new(Completion::Listed {
                owner,
                epoch: retired,
                result: Err(anyhow::anyhow!("old endpoint failure")),
            }))),
        });
        let view = app.recovery.view.as_ref().unwrap();
        assert_eq!(view.epoch, current);
        assert!(view.loading);
        assert!(view.error.is_none());
        assert!(app.latest_diagnostic().is_none());
    }
}
