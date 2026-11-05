use super::*;

const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);
const MAX_PENDING_COPIES: usize = 2;

pub(super) struct Drag {
    origin: Arc<PointerOrigin>,
    event: SelectionEvent,
    deadline: Option<Instant>,
}
struct Release {
    origin: Arc<PointerOrigin>,
    event: SelectionEvent,
    generation: u64,
    focus_epoch: u64,
}
struct Copy {
    pane: String,
    view: String,
    generation: u64,
    focus_epoch: u64,
    automatic: bool,
    receipt: updates::ResponseReceipt,
}
pub(super) struct State {
    clock: Instant,
    generation: u64,
    hidden: HashSet<String>,
    drag: Option<Drag>,
    release: Option<Release>,
    tick: Option<updates::ResponseReceipt>,
    copies: VecDeque<Copy>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            clock: Instant::now(),
            generation: 0,
            hidden: HashSet::new(),
            drag: None,
            release: None,
            tick: None,
            copies: VecDeque::new(),
        }
    }
}
impl State {
    fn time_ms(&self) -> u64 {
        self.clock.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

impl App<'_> {
    pub(super) fn selection_spans(&self, pane: &str) -> &[SelectionSpan] {
        self.views
            .get(pane)
            .filter(|view| {
                !view.retirement_requested && !self.terminal_selection.hidden.contains(&view.view)
            })
            .and_then(|view| view.metadata.selection.as_ref())
            .map_or(&[], |selection| selection.spans.as_slice())
    }

    pub(super) fn selection_active(&self, pane: &str) -> bool {
        self.views.get(pane).is_some_and(|view| {
            !self.terminal_selection.hidden.contains(&view.view)
                && (view
                    .metadata
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.active)
                    || self
                        .terminal_selection
                        .drag
                        .as_ref()
                        .is_some_and(|drag| drag.origin.pane == pane)
                    || self
                        .terminal_selection
                        .copies
                        .iter()
                        .any(|copy| copy.pane == pane))
        })
    }

    fn selection_view(&self, pane: &str, view: &str) -> bool {
        !self.view_failures.contains_key(pane)
            && self.views.get(pane).is_some_and(|attached| {
                attached.view == view && attached.spec.visible && !attached.retirement_requested
            })
    }

    pub(super) fn clear_selection(&mut self, pane: &str) -> Result<()> {
        let Some(view) = self.views.get(pane).map(|view| view.view.clone()) else {
            return Ok(());
        };
        self.terminal_selection.generation = self.terminal_selection.generation.wrapping_add(1);
        self.terminal_selection.hidden.insert(view.clone());
        if self
            .terminal_selection
            .drag
            .as_ref()
            .is_some_and(|drag| drag.origin.pane == pane)
        {
            self.terminal_selection.drag = None;
            self.terminal_selection.tick = None;
            self.pointer_capture = Some(PointerCapture::Ui);
        }
        self.enqueue(
            pane,
            PaneRequest::Select {
                view_id: view,
                event: SelectionEvent {
                    action: SelectionAction::Clear,
                    x: 0.0,
                    y: 0.0,
                    modifiers: 0,
                    time_ms: self.terminal_selection.time_ms(),
                },
            },
        )
    }

    pub(super) fn selection_key(&mut self, event: &KeyEvent) -> Result<bool> {
        let Some(pane) = self.state.focused.clone() else {
            return Ok(false);
        };
        if !self.selection_active(&pane) || modifier_key(&event.key) {
            return Ok(false);
        }
        if event.key == "Escape" {
            // Releases accepted before selection still owe their original pane.
            if !self.held.contains_key(&event.key) {
                self.state.suppressed.insert(event.key.clone());
            }
            self.clear_selection(&pane)?;
            return Ok(true);
        }
        if !self.focused_history() {
            self.clear_selection(&pane)?;
        }
        Ok(false)
    }

    pub(super) fn select_pointer(
        &mut self,
        origin: Arc<PointerOrigin>,
        action: SelectionAction,
        mouse: termwiz::input::MouseEvent,
        rect: Rect,
    ) -> Result<()> {
        if action == SelectionAction::Press {
            self.flush_selection_release()?;
        }
        if !self.selection_view(&origin.pane, &origin.view) {
            self.terminal_selection.drag = None;
            self.pointer_capture = Some(PointerCapture::Ui);
            return Ok(());
        }
        let event = SelectionEvent {
            action,
            // SGR gives a cell, not subcell pixels. Use its center and retain
            // signed outside coordinates during an origin-captured drag.
            x: f32::from(mouse.x) - f32::from(rect.x) + 0.5,
            y: f32::from(mouse.y) - f32::from(rect.y) + 0.5,
            modifiers: mouse_modifiers(mouse.modifiers),
            time_ms: self.terminal_selection.time_ms(),
        };
        if action == SelectionAction::Press
            && self
                .views
                .get(&origin.pane)
                .and_then(|view| view.frame.as_ref())
                .is_none_or(|frame| {
                    event.x < 0.0
                        || event.y < 0.0
                        || event.x >= f32::from(frame.size.cols.min(rect.width))
                        || event.y >= f32::from(frame.size.rows.min(rect.height))
                })
        {
            return Ok(());
        }
        if action == SelectionAction::Release {
            self.terminal_selection.generation = self.terminal_selection.generation.wrapping_add(1);
            self.terminal_selection.drag = None;
            self.terminal_selection.tick = None;
            self.terminal_selection.release = Some(Release {
                origin,
                event,
                generation: self.terminal_selection.generation,
                focus_epoch: self.selection_epoch,
            });
            return self.flush_selection_release();
        }
        self.enqueue(
            &origin.pane,
            PaneRequest::Select {
                view_id: origin.view.clone(),
                event,
            },
        )?;
        match action {
            SelectionAction::Press => {
                self.terminal_selection.generation =
                    self.terminal_selection.generation.wrapping_add(1);
                self.terminal_selection.hidden.remove(&origin.view);
                self.terminal_selection.drag = Some(Drag {
                    origin: origin.clone(),
                    event,
                    deadline: None,
                });
                self.pointer_capture = Some(PointerCapture::Selection(origin));
            }
            SelectionAction::Drag => {
                self.terminal_selection.generation =
                    self.terminal_selection.generation.wrapping_add(1);
                if let Some(drag) = &mut self.terminal_selection.drag {
                    drag.event = event;
                }
            }
            SelectionAction::Release => unreachable!(),
            _ => {}
        }
        Ok(())
    }

    pub(super) fn flush_selection_release(&mut self) -> Result<()> {
        let Some(release) = self.terminal_selection.release.take() else {
            return Ok(());
        };
        if !self.selection_view(&release.origin.pane, &release.origin.view) {
            return Ok(());
        }
        if let Err(error) = self.enqueue(
            &release.origin.pane,
            PaneRequest::Select {
                view_id: release.origin.view.clone(),
                event: release.event,
            },
        ) {
            // Only local non-admission is retried. Once queued, the existing
            // FIFO and unknown-outcome handling own this release exactly once.
            self.terminal_selection.release = Some(release);
            return Err(error);
        }
        if release.generation == self.terminal_selection.generation
            && release.focus_epoch == self.selection_epoch
        {
            self.copy_selection(&release.origin.pane, &release.origin.view, true)?;
        }
        Ok(())
    }

    pub(super) fn cancel_selection(&mut self, pane: &str, view: &str) {
        self.terminal_selection.hidden.remove(view);
        self.terminal_selection
            .copies
            .retain(|copy| copy.pane != pane || copy.view != view);
        if self
            .terminal_selection
            .release
            .as_ref()
            .is_some_and(|release| release.origin.pane == pane && release.origin.view == view)
        {
            self.terminal_selection.release = None;
        }
        if self
            .terminal_selection
            .drag
            .as_ref()
            .is_some_and(|drag| drag.origin.pane == pane && drag.origin.view == view)
        {
            self.terminal_selection.drag = None;
            self.terminal_selection.tick = None;
        }
        if matches!(&self.pointer_capture, Some(PointerCapture::Selection(origin)) if origin.pane == pane && origin.view == view)
        {
            self.pointer_capture = Some(PointerCapture::Ui);
        }
    }

    pub(super) fn selection_focus_lost(&mut self) {
        let generation = self.terminal_selection.generation;
        self.terminal_selection.generation = generation.wrapping_add(1);
        self.terminal_selection.copies.clear();
        self.terminal_selection.tick = None;
        if let Some(drag) = self.terminal_selection.drag.take() {
            let mut event = drag.event;
            event.action = SelectionAction::Release;
            event.time_ms = self.terminal_selection.time_ms();
            // Retire at the last observed position, without turning loss of
            // host/modal focus into an export gesture. Late physical release
            // stays captured by the UI rather than landing in another pane.
            self.terminal_selection.release = Some(Release {
                origin: drag.origin,
                event,
                generation,
                focus_epoch: self.selection_epoch,
            });
            self.pointer_capture = Some(PointerCapture::Ui);
        }
    }

    pub(super) fn selection_tick(&mut self) -> Result<()> {
        if let Some(receipt) = &self.terminal_selection.tick {
            match receipt.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Ok(Err(error)) => {
                    self.notify(format!(
                        "Selection autoscroll failed: {error}; not replayed"
                    ));
                    self.terminal_selection.drag = None;
                    self.pointer_capture = Some(PointerCapture::Ui);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.notify(
                        "Selection autoscroll completion disconnected; not replayed".into(),
                    );
                    self.terminal_selection.drag = None;
                    self.pointer_capture = Some(PointerCapture::Ui);
                }
                Ok(Ok(_)) => {}
            }
            self.terminal_selection.tick = None;
        }
        let Some(drag) = &self.terminal_selection.drag else {
            return Ok(());
        };
        if !self.selection_view(&drag.origin.pane, &drag.origin.view) {
            self.terminal_selection.drag = None;
            return Ok(());
        }
        let autoscroll = self
            .views
            .get(&drag.origin.pane)
            .and_then(|view| view.metadata.selection.as_ref())
            .is_some_and(|selection| selection.dragging && selection.autoscroll != 0);
        let now = Instant::now();
        let drag = self.terminal_selection.drag.as_mut().unwrap();
        if !autoscroll {
            drag.deadline = None;
            return Ok(());
        }
        let deadline = drag.deadline.get_or_insert(now + AUTOSCROLL_INTERVAL);
        if now < *deadline {
            return Ok(());
        }
        drag.deadline = None;
        let origin = drag.origin.clone();
        let mut event = drag.event;
        event.action = SelectionAction::Tick;
        event.time_ms = self.terminal_selection.time_ms();
        // The native gesture performs its own scroll and updates this view's
        // tracked history anchor. Never issue a second synthetic Scroll here.
        match self.input.response(Operation::Pane {
            pane_id: origin.pane.clone(),
            request: PaneRequest::Select {
                view_id: origin.view.clone(),
                event,
            },
        }) {
            Ok(receipt) => self.terminal_selection.tick = Some(receipt),
            Err(error) => {
                if error
                    .downcast_ref::<updates::InputRejected>()
                    .is_some_and(|rejected| rejected.0.stopped)
                {
                    self.terminal_selection.drag = None;
                    self.pointer_capture = Some(PointerCapture::Ui);
                }
                return Err(error);
            }
        }
        self.terminal_selection.generation = self.terminal_selection.generation.wrapping_add(1);
        Ok(())
    }

    pub(super) fn selection_timeout(&self, timeout: i32) -> i32 {
        self.terminal_selection
            .drag
            .as_ref()
            .and_then(|drag| drag.deadline)
            .map_or(timeout, |deadline| {
                timeout.min(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .max(1) as i32,
                )
            })
    }

    pub(super) fn copy_focused_selection(&mut self) -> Result<()> {
        if let Some((pane, view)) = self.state.focused.as_ref().and_then(|pane| {
            self.views
                .get(pane)
                .map(|view| (pane.clone(), view.view.clone()))
        }) {
            self.copy_selection(&pane, &view, false)?;
        } else {
            self.notify("No attached selection to copy".into());
        }
        Ok(())
    }

    fn copy_selection(&mut self, pane: &str, view: &str, automatic: bool) -> Result<()> {
        if self.options.clipboard_policy == "deny" {
            self.notify("Selection retained; host clipboard export denied by client policy".into());
            return Ok(());
        }
        if !self.selection_view(pane, view) {
            return Ok(());
        }
        if self.terminal_selection.hidden.contains(view) {
            if !automatic {
                self.notify("No selection to copy".into());
            }
            return Ok(());
        }
        let generation = self.terminal_selection.generation;
        let focus_epoch = self.selection_epoch;
        self.terminal_selection
            .copies
            .retain(|copy| copy.generation == generation && copy.focus_epoch == focus_epoch);
        if self.terminal_selection.copies.len() >= MAX_PENDING_COPIES {
            self.notify("Selection copy queue full; request was not admitted".into());
            return Ok(());
        }
        let receipt = self.input.response(Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::CopySelection {
                view_id: view.into(),
            },
        })?;
        self.terminal_selection.copies.push_back(Copy {
            pane: pane.into(),
            view: view.into(),
            generation: self.terminal_selection.generation,
            focus_epoch: self.selection_epoch,
            automatic,
            receipt,
        });
        Ok(())
    }

    pub(super) fn selection_clipboard(&mut self, out: &mut impl Write) -> Result<()> {
        loop {
            let Some(copy) = self.terminal_selection.copies.front() else {
                return Ok(());
            };
            let valid = copy.generation == self.terminal_selection.generation
                && copy.focus_epoch == self.selection_epoch
                && self.host_focused
                && self.state.focused.as_deref() == Some(copy.pane.as_str())
                && self.state.form.is_none()
                && !self
                    .consent
                    .as_ref()
                    .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
                && self.selection_view(&copy.pane, &copy.view)
                && !self.terminal_selection.hidden.contains(&copy.view)
                && self.options.clipboard_policy != "deny";
            if !valid {
                self.terminal_selection.copies.pop_front();
                continue;
            }
            // Do not interleave a user export into the existing application
            // clipboard/paste exchange. The bounded original response waits.
            if self.consent.is_some() || self.paste_offer.is_some() {
                return Ok(());
            }
            let result = match copy.receipt.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(transport::mark_outcome_unknown(anyhow::anyhow!(
                        "selection copy completion disconnected; outcome unknown; not replayed"
                    )))
                }
            };
            let copy = self.terminal_selection.copies.pop_front().unwrap();
            let result = result.and_then(|value| {
                #[derive(serde::Deserialize)]
                struct Copied {
                    copied: bool,
                    contents: Vec<ClipboardContent>,
                }
                let copied: Copied = serde_json::from_value(value)?;
                if !copied.copied {
                    if !copied.contents.is_empty() {
                        bail!("invalid empty selection response");
                    }
                    if !copy.automatic {
                        self.notify("No selection to copy".into());
                    }
                    return Ok(());
                }
                if copied.contents.len() != 1 || copied.contents[0].mime != "text/plain" {
                    bail!("selection copy returned invalid clipboard contents");
                }
                if copied.contents[0].data.len() > MAX_INPUT_BYTES {
                    bail!("selection exceeds clipboard payload limit; nothing sent");
                }
                if copied.contents[0].data.is_empty() {
                    return Ok(());
                }
                let mut consent = Consent {
                    id: String::new(),
                    request: ClipboardRequest {
                        operation: ClipboardOperation::Write {
                            contents: copied.contents,
                        },
                        location: ClipboardLocation::Standard,
                        name: String::new(),
                        granted: true,
                        can_remember: false,
                        paste_offer_id: None,
                    },
                    started: Instant::now(),
                    sent: false,
                    allowed: true,
                    contents: Vec::new(),
                    available: Vec::new(),
                    enhanced_supported: self.enhanced_supported,
                    paste: false,
                    password: None,
                };
                let answer = begin_clipboard(out, &mut consent)?;
                if !answer.is_some_and(|answer| answer.result == ClipboardResult::Success) {
                    bail!("selection clipboard export was not sent");
                }
                self.notify(
                    "Selection sent to host clipboard; host permissions remain authoritative"
                        .into(),
                );
                Ok(())
            });
            if let Err(error) = result {
                if error.downcast_ref::<std::io::Error>().is_some() {
                    return Err(error.context(
                        "selection clipboard host transport failed; outcome unknown; not replayed",
                    ));
                }
                self.notify(format!("Selection copy failed: {error:#}; not replayed"));
            }
        }
    }
}

fn modifier_key(key: &str) -> bool {
    matches!(
        key,
        "ShiftLeft"
            | "ShiftRight"
            | "ControlLeft"
            | "ControlRight"
            | "AltLeft"
            | "AltRight"
            | "MetaLeft"
            | "MetaRight"
            | "SuperLeft"
            | "SuperRight"
            | "HyperLeft"
            | "HyperRight"
            | "CapsLock"
            | "NumLock"
            | "ScrollLock"
    )
}

pub(super) fn mouse_modifiers(modifiers: Modifiers) -> u8 {
    [
        (Modifiers::SHIFT, 1),
        (Modifiers::CTRL, 2),
        (Modifiers::ALT, 4),
        (Modifiers::SUPER, 8),
    ]
    .into_iter()
    .fold(0, |bits, (flag, bit)| {
        bits | if modifiers.contains(flag) { bit } else { 0 }
    })
}
