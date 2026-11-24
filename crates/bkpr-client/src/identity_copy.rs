//! Explicit local clipboard exports of currently presented managed identities.
use super::*;
use crate::metadata::IdentityCopy;

/// Presentation that authorized an identity export.
#[derive(Clone, Copy)]
pub(super) enum Source {
    /// Current sidebar metadata card, possibly owned by another attachment.
    Popover,
    /// Current attachment's selected finder preview.
    Finder,
}

struct Pending {
    owner: TreeOwner,
    target: IdentityCopy,
    source: Source,
}

/// One bounded user gesture, drained only through the coordinated output path.
#[derive(Default)]
pub(super) struct State {
    pending: Option<Pending>,
}

impl State {
    /// Retire unsent UI gestures on focus or modal ownership transitions.
    pub(super) fn cancel(&mut self) {
        self.pending = None;
    }
}

impl App {
    /// Capture a user copy gesture without a backend mutation or input grant.
    pub(super) fn queue_identity_copy(
        &mut self,
        owner: TreeOwner,
        target: IdentityCopy,
        source: Source,
    ) {
        if self.options.clipboard_policy == "deny" {
            self.notify("Identity copy denied by client clipboard policy".into());
        } else if target.value().len() > MAX_INPUT_BYTES {
            self.notify("Identity exceeds clipboard payload limit; nothing sent".into());
        } else if self.identity_copy.pending.is_some()
            || self.consent.is_some()
            || self.paste_offer.is_some()
        {
            self.notify("Clipboard exchange busy; identity copy was not queued".into());
        } else {
            self.identity_copy.pending = Some(Pending {
                owner,
                target,
                source,
            });
        }
    }

    /// Revalidate current owner, foreground process and binding before host output.
    pub(super) fn identity_clipboard(&mut self, out: &mut impl Write) -> Result<()> {
        let Some(copy) = self.identity_copy.pending.take() else {
            return Ok(());
        };
        let valid = self.host_focused
            && !self.backgrounded
            && self.options.clipboard_policy != "deny"
            && self.consent.is_none()
            && self.paste_offer.is_none()
            && {
                let tree = self.tree.lock().unwrap();
                let visible = match copy.source {
                    Source::Popover => self.popover.shows_copy(&tree, copy.owner, &copy.target),
                    Source::Finder => {
                        self.tree_owner() == copy.owner
                            && self
                                .finder
                                .as_ref()
                                .is_some_and(|finder| finder.shows_copy(&copy.target))
                    }
                };
                visible
                    && tree.session(copy.owner).is_some_and(|session| {
                        session.group.status == SessionStatus::Online
                            && session
                                .registry
                                .panes
                                .iter()
                                .find(|pane| pane.id == copy.target.pane())
                                .is_some_and(|pane| {
                                    copy.target.matches(pane, session.snapshots.get(&pane.id))
                                })
                    })
            };
        if !valid {
            self.notify("Identity copy expired or clipboard exchange changed; nothing sent".into());
            return Ok(());
        }
        let result = self.export_user_clipboard(
            out,
            vec![ClipboardContent {
                mime: "text/plain".into(),
                data: copy.target.value().as_bytes().to_vec(),
            }],
            copy.target.label(),
        );
        match result {
            Err(error) if error.downcast_ref::<std::io::Error>().is_some() => Err(error.context(
                "identity clipboard host transport failed; outcome unknown; not replayed",
            )),
            Err(error) => {
                self.notify(format!("Identity copy failed: {error:#}; not replayed"));
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONVERSATION: &str = "native-é-0123456789-abcdefghijklmnopqrstuvwxyz";
    const LAUNCH: &str = "launch-0123456789-abcdefghijklmnopqrstuvwxyz";
    const CLIPBOARD_OUTPUTS: [&[u8]; 2] = [
        b"\x1b]52;c;bmF0aXZlLcOpLTAxMjM0NTY3ODktYWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo=\x1b\\",
        b"\x1b]52;c;bGF1bmNoLTAxMjM0NTY3ODktYWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo=\x1b\\",
    ];

    fn publish(app: &mut App) {
        let owner = app.tree_owner();
        let session = crate::tree::Session {
            owner,
            can_stop: true,
            project_host: Default::default(),
            group: SessionGroup {
                name: "identity fixture".into(),
                host: None,
                status: SessionStatus::Online,
                detail: String::new(),
            },
            registry: Arc::new(app.registry.clone()),
            rows: Default::default(),
            workspace: None,
            window: None,
            focused: app.state.focused.clone(),
            snapshots: Arc::new(
                app.views
                    .iter()
                    .map(|(id, view)| (id.clone(), view.metadata.clone()))
                    .collect(),
            ),
            statuses: Default::default(),
            history: Default::default(),
        };
        app.tree
            .lock()
            .unwrap()
            .reconcile(vec![Arc::new(session)], owner);
    }

    fn fixture() -> (App, updates::Capture, Size) {
        let (mut app, capture) = super::super::tests::fixture();
        app.options.read_only = true;
        super::super::tests::attach_named_fixture(&mut app, "identity-pane", "identity-view");
        app.registry.panes[0].harness = Some(HarnessInfo {
            kind: HarnessKind::Pi,
            pid: 123,
            birth: "original-birth".into(),
            cwd: "/fixture".into(),
            managed: Some(ManagedHarness {
                launch_id: LAUNCH.into(),
                conversation: Some(NativeConversation {
                    id: CONVERSATION.into(),
                    source: ConversationSource::PiExtension,
                    generation: 7,
                }),
                error: None,
                first_prompt_submitted_at_ms: None,
            }),
        });
        app.views
            .get_mut("identity-pane")
            .unwrap()
            .metadata
            .program_identity = Some(ProcessIdentity {
            pid: 123,
            birth: "original-birth".into(),
        });
        publish(&mut app);
        let size = Size {
            cols: 105,
            rows: 48,
            cell_width: 9,
            cell_height: 18,
        };
        (app, capture, size)
    }

    fn finder_fixture() -> (App, updates::Capture, Size, OverlayPlacement) {
        let (mut app, capture, size) = fixture();
        app.begin_finder();
        let (outer, content) = app.modal_geometry(size);
        let placement = OverlayPlacement { outer, content };
        app.overlays
            .present("ui:modal", placement, OverlayPolicy::MODAL, None);
        (app, capture, size, placement)
    }

    fn paint_finder(app: &mut App, size: Size, content: Rect) -> Vec<(u16, u16)> {
        let local = content.size(size);
        let finder = app.finder.as_mut().unwrap();
        let mut surface = UiSurface::new(local);
        finder.paint(&mut surface, local);
        if let Some(sections) = crate::finder::Finder::sections(local) {
            sections.decorate(&mut surface.frame);
        }
        let mut buttons = Vec::new();
        let cursor = surface.frame.cursor.clone();
        finder.paint_buttons(&mut surface.frame, &Theme::default(), None, |_, rect, _| {
            buttons.push(rect);
            false
        });
        assert_eq!(surface.frame.cursor, cursor);
        buttons
            .into_iter()
            .map(|rect| {
                let row = &surface.frame.rows[usize::from(rect.y)].cells;
                let cells = &row[usize::from(rect.x)..usize::from(rect.x + rect.width)];
                assert_eq!(cells.len(), 4);
                assert_eq!(cells[0].text, "[");
                assert_eq!(cells[1].text, "\u{f018f}");
                assert_eq!(cells[2].text, " ");
                assert_eq!(cells[3].text, "]");
                assert!(
                    row[..usize::from(rect.x)]
                        .iter()
                        .any(|cell| cell.text == "…")
                );
                (content.x + rect.x + 1, content.y + rect.y)
            })
            .collect()
    }

    fn pointer(app: &mut App, size: Size, point: (u16, u16), action: MouseAction) {
        pointer_button(app, size, point, MouseButton::Left, action);
    }

    fn pointer_button(
        app: &mut App,
        size: Size,
        point: (u16, u16),
        button: MouseButton,
        action: MouseAction,
    ) {
        let code = match button {
            MouseButton::Left => 0,
            MouseButton::Right => 2,
            _ => unreachable!(),
        };
        let packet = format!(
            "\x1b[<{code};{};{}{}",
            point.0 + 1,
            point.1 + 1,
            if action == MouseAction::Release {
                'm'
            } else {
                'M'
            },
        );
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let mut events =
            decode_input(&mut parser, demux.feed(packet.as_bytes()), false).into_iter();
        let Some(ClientEvent::Mouse {
            event,
            button: decoded_button,
            action: decoded,
        }) = events.next()
        else {
            panic!("expected a physical mouse event");
        };
        assert_eq!(decoded_button, button);
        assert_eq!(decoded, action);
        assert!(events.next().is_none());
        let hits = Scene {
            size,
            surfaces: Vec::new(),
        }
        .hit_map();
        app.mouse(
            event,
            Some((decoded_button, decoded)),
            size,
            &[],
            &[],
            &hits,
        )
        .unwrap();
    }

    #[test]
    fn finder_buttons_export_both_unclipped_ids_on_read_only_pointer_release() {
        let (mut app, capture, size, placement) = finder_fixture();
        let buttons = paint_finder(&mut app, size, placement.content);
        assert_eq!(buttons.len(), CLIPBOARD_OUTPUTS.len());
        for (point, expected) in buttons.into_iter().zip(CLIPBOARD_OUTPUTS) {
            pointer(&mut app, size, point, MouseAction::Press);
            let mut out = Vec::new();
            app.identity_clipboard(&mut out).unwrap();
            assert!(out.is_empty(), "press must not export");
            pointer(&mut app, size, point, MouseAction::Release);
            app.identity_clipboard(&mut out).unwrap();
            assert_eq!(out, expected);
        }
        assert!(
            capture.try_recv().is_err(),
            "copy must not become pane input"
        );
    }

    #[test]
    fn finder_rejects_bare_release_off_button_release_and_replaced_copy_target() {
        let (mut app, _, size, placement) = finder_fixture();
        let buttons = paint_finder(&mut app, size, placement.content);
        let point = buttons[0];
        let mut out = Vec::new();
        pointer(&mut app, size, point, MouseAction::Release);
        app.identity_clipboard(&mut out).unwrap();
        assert!(out.is_empty());

        pointer(&mut app, size, point, MouseAction::Press);
        let content = placement.content;
        pointer(&mut app, size, (content.x, content.y), MouseAction::Release);
        pointer(&mut app, size, point, MouseAction::Release);
        app.identity_clipboard(&mut out).unwrap();
        assert!(out.is_empty());

        pointer(&mut app, size, point, MouseAction::Press);
        let outside = (placement.outer.x - 1, placement.outer.y);
        pointer(&mut app, size, outside, MouseAction::Release);
        assert!(app.finder.is_some(), "release must not dismiss the picker");
        pointer(&mut app, size, point, MouseAction::Release);
        app.identity_clipboard(&mut out).unwrap();
        assert!(
            out.is_empty(),
            "a release intercepted outside the modal content retires the press"
        );

        pointer(&mut app, size, point, MouseAction::Press);
        app.registry.revision += 1;
        let identity = app.registry.panes[0]
            .harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .conversation
            .as_mut()
            .unwrap();
        identity.id = format!("{CONVERSATION}-replacement");
        identity.generation += 1;
        publish(&mut app);
        let finder = app.finder.as_mut().unwrap();
        finder.update_registry(
            &app.registry,
            &crate::project_colors::ProjectSlots::default(),
            |id| app.views.get(id).map(|view| &view.metadata),
        );
        let replaced = paint_finder(&mut app, size, content);
        assert!(
            replaced.contains(&point),
            "the replacement is painted at the same pointer location"
        );
        pointer(&mut app, size, point, MouseAction::Release);
        app.identity_clipboard(&mut out).unwrap();
        assert!(
            out.is_empty(),
            "the predecessor cannot authorize the replacement"
        );
    }

    #[test]
    fn secondary_gesture_outside_finder_preserves_primary_copy() {
        let (mut app, _, size, placement) = finder_fixture();
        let point = paint_finder(&mut app, size, placement.content)[0];
        let outside = (placement.outer.x - 1, placement.outer.y);
        pointer(&mut app, size, point, MouseAction::Press);
        pointer_button(
            &mut app,
            size,
            outside,
            MouseButton::Right,
            MouseAction::Press,
        );
        pointer_button(
            &mut app,
            size,
            outside,
            MouseButton::Right,
            MouseAction::Release,
        );
        assert!(app.finder.is_some());
        assert!(app.pointer_capture.is_some());
        assert_eq!(app.overlays.pointer_button(), Some(MouseButton::Left));

        let mut out = Vec::new();
        pointer(&mut app, size, point, MouseAction::Release);
        app.identity_clipboard(&mut out).unwrap();
        assert_eq!(out, CLIPBOARD_OUTPUTS[0]);
        assert!(app.pointer_capture.is_none());
    }

    #[test]
    fn sidebar_copy_exports_exact_ids_only_after_release_in_the_card() {
        let (mut app, capture, size) = fixture();
        app.registry.panes[0].workspace_id = Some("workspace".into());
        app.registry.panes[0].window_id = Some("window".into());
        app.registry.workspaces.push(bkpr_protocol::Workspace {
            id: "workspace".into(),
            name: "Fixture".into(),
            revision: 1,
            windows: vec![bkpr_protocol::Window {
                id: "window".into(),
                name: "Fixture".into(),
                revision: 1,
                layout: None,
            }],
        });
        publish(&mut app);
        let source = crate::tree::Target {
            owner: app.tree_owner(),
            node: crate::tree::Node::Native(SidebarTarget::Pane {
                workspace: "workspace".into(),
                pane: "identity-pane".into(),
            }),
        };
        let outer = {
            let mut tree = app.tree.lock().unwrap();
            tree.hover = Some(source.clone());
            app.popover
                .paint_tree(&tree, Some(&source), 3, 24, size)
                .unwrap()
        };
        let mut buttons = Vec::new();
        app.popover
            .paint_buttons(&Theme::default(), None, None, false, |_, rect, _| {
                buttons.push((outer.x + rect.x, outer.y + rect.y));
                false
            });
        assert_eq!(buttons.len(), CLIPBOARD_OUTPUTS.len());
        for (point, expected) in buttons.into_iter().zip(CLIPBOARD_OUTPUTS) {
            let armed = app
                .popover
                .button_at(&app.tree.lock().unwrap(), point, false)
                .unwrap();
            let mut out = Vec::new();
            app.identity_clipboard(&mut out).unwrap();
            assert!(out.is_empty(), "press must not export");

            assert!(
                app.popover
                    .release_button(&app.tree.lock().unwrap(), (outer.x, outer.y), armed, false)
                    .is_none()
            );
            let action = {
                let tree = app.tree.lock().unwrap();
                let armed = app.popover.button_at(&tree, point, false).unwrap();
                app.popover.release_button(&tree, point, armed, false)
            };
            let Some(crate::popover::ButtonAction::Copy { owner, target }) = action else {
                panic!("release must export a copy action for the painted identity");
            };
            assert_eq!(owner, source.owner);
            app.queue_identity_copy(owner, target, Source::Popover);
            app.identity_clipboard(&mut out).unwrap();
            assert_eq!(out, expected);
        }
        assert!(
            capture.try_recv().is_err(),
            "copy must not become pane input"
        );
    }

    #[test]
    fn stale_identity_gestures_never_export_or_replay_after_source_retirement() {
        for change in 0..8 {
            let (mut app, _, size, placement) = finder_fixture();
            let buttons = paint_finder(&mut app, size, placement.content);
            pointer(&mut app, size, buttons[0], MouseAction::Press);
            pointer(&mut app, size, buttons[0], MouseAction::Release);
            match change {
                0 => {
                    app.registry.panes[0]
                        .harness
                        .as_mut()
                        .unwrap()
                        .managed
                        .as_mut()
                        .unwrap()
                        .conversation
                        .as_mut()
                        .unwrap()
                        .generation += 1
                }
                1 => {
                    app.registry.panes[0]
                        .harness
                        .as_mut()
                        .unwrap()
                        .managed
                        .as_mut()
                        .unwrap()
                        .launch_id = "replacement".into()
                }
                2 => app.registry.panes[0].status = PaneStatus::Exited,
                3 => {
                    app.registry.panes[0].harness.as_mut().unwrap().birth = "reused-pid".into();
                    app.views
                        .get_mut("identity-pane")
                        .unwrap()
                        .metadata
                        .program_identity
                        .as_mut()
                        .unwrap()
                        .birth = "reused-pid".into();
                }
                4 => app.namespace.1 += 1,
                5 => app.finder = None,
                6 => app.options.clipboard_policy = "deny".into(),
                7 => app.selection_focus_lost(),
                _ => unreachable!(),
            }
            publish(&mut app);
            let mut out = Vec::new();
            app.identity_clipboard(&mut out).unwrap();
            app.options.clipboard_policy = "ask".into();
            app.identity_clipboard(&mut out).unwrap();
            assert!(
                out.is_empty(),
                "retired identity exported for transition {change}"
            );
        }
    }
}
