use super::*;
use crate::{Pane, SpawnSpec};
use bkpr_protocol::{PaneRequest, PaneSnapshot, SelectionAction, SelectionEvent, Size, ViewSpec};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    pane: Pane,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pane = Pane::spawn(SpawnSpec {
            // Only POSIX shell builtins; no user-profile/account wrappers.
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'hello world'; read -r answer".into(),
            ],
            home: root.path().join("home"),
            cwd: root.path().to_path_buf(),
            size: size(),
            isolated_process: false,
            isolated_environment: true,
        })
        .unwrap();
        let fixture = Self { pane, _root: root };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot: PaneSnapshot = serde_json::from_value(
                fixture
                    .pane
                    .request(PaneRequest::Inspect)
                    .unwrap()
                    .into_json()
                    .unwrap(),
            )
            .unwrap();
            if snapshot.frame.as_ref().is_some_and(|frame| {
                frame.rows[0]
                    .cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("hello world")
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "fixture output did not publish");
            thread::sleep(Duration::from_millis(2));
        }
        fixture
    }
    fn attach(&self) -> String {
        self.pane
            .request(PaneRequest::Attach { spec: spec() })
            .unwrap()
            .into_json()
            .unwrap()["view_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn select(&self, id: &str, action: SelectionAction, x: f32) {
        self.pane
            .request(PaneRequest::Select {
                view_id: id.into(),
                event: event(action, x),
            })
            .unwrap()
            .into_json()
            .unwrap();
    }
    fn poll(&self, id: &str, frame_after: u64) -> PaneSnapshot {
        serde_json::from_value(
            self.pane
                .request(PaneRequest::Poll {
                    view_id: id.into(),
                    after: 0,
                    frame_after,
                    resources: vec![],
                    snapshot_after: 0,
                    wait_ms: 0,
                    row_deltas: true,
                    scene_encoding: None,
                })
                .unwrap()
                .into_json()
                .unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.pane.request(PaneRequest::Shutdown);
    }
}
fn size() -> Size {
    Size {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    }
}
fn spec() -> ViewSpec {
    ViewSpec {
        size: size(),
        interactive: false,
        visible: true,
        input: false,
        clipboard: false,
    }
}
fn event(action: SelectionAction, x: f32) -> SelectionEvent {
    SelectionEvent {
        action,
        x,
        y: 0.5,
        modifiers: 0,
        time_ms: 100,
    }
}

#[test]
fn read_only_selection_copies_exact_bytes_without_scene_or_authority_changes() {
    let fixture = Fixture::new();
    let reader = fixture.attach();
    let other = fixture.attach();
    assert_eq!(
        fixture
            .pane
            .request(PaneRequest::CopySelection {
                view_id: reader.clone()
            })
            .unwrap()
            .into_json()
            .unwrap(),
        serde_json::json!({"copied":false,"contents":[]})
    );
    fixture.select(&reader, SelectionAction::Press, 0.1);
    let before = fixture.poll(&reader, 0);
    let canonical = fixture
        .pane
        .shared
        .published
        .lock()
        .unwrap()
        .frame
        .clone()
        .unwrap();
    fixture.select(&reader, SelectionAction::Drag, 4.9);
    fixture.select(&reader, SelectionAction::Release, 4.9);
    let after = fixture.poll(&reader, before.revision);
    assert_eq!(after.revision, before.revision);
    assert!(after.snapshot_revision > before.snapshot_revision);
    assert!(after.frame.is_none());
    assert!(after.selection.as_ref().unwrap().active);
    assert!(!after.selection.as_ref().unwrap().dragging);
    assert!(fixture.poll(&other, before.revision).selection.is_none());
    assert!(Arc::ptr_eq(
        &canonical,
        fixture
            .pane
            .shared
            .published
            .lock()
            .unwrap()
            .frame
            .as_ref()
            .unwrap()
    ));
    assert!(after.size_owner.is_none());
    let response = fixture
        .pane
        .request(PaneRequest::CopySelection {
            view_id: reader.clone(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        response,
        serde_json::json!({"copied":true,"contents":[{"mime":"text/plain","data":b"hello"}]})
    );
    assert!(
        fixture
            .pane
            .request(PaneRequest::Input {
                view_id: reader.clone(),
                data: b"must not arrive".to_vec()
            })
            .is_err()
    );
    fixture.select(&reader, SelectionAction::Clear, 0.0);
    assert!(fixture.poll(&reader, before.revision).selection.is_none());
    assert_eq!(
        fixture
            .pane
            .request(PaneRequest::CopySelection { view_id: reader })
            .unwrap()
            .into_json()
            .unwrap()["copied"],
        false
    );
}

#[test]
fn hidden_detached_and_retired_pane_copies_fail_without_reviving_old_selection() {
    let fixture = Fixture::new();
    let reader = fixture.attach();
    fixture.select(&reader, SelectionAction::Press, 0.1);
    fixture.select(&reader, SelectionAction::Drag, 4.9);
    fixture.select(&reader, SelectionAction::Release, 4.9);
    let old_activity = visible(&fixture.pane.shared, &reader, None).unwrap();
    let mut hidden = spec();
    hidden.visible = false;
    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: hidden,
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(
        fixture
            .pane
            .request(PaneRequest::CopySelection {
                view_id: reader.clone()
            })
            .is_err()
    );
    assert!(
        fixture
            .pane
            .request(PaneRequest::Select {
                view_id: reader.clone(),
                event: event(SelectionAction::Press, 1.0)
            })
            .is_err()
    );
    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: spec(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(visible(&fixture.pane.shared, &reader, Some(old_activity)).is_err());
    assert_eq!(
        fixture
            .pane
            .request(PaneRequest::CopySelection {
                view_id: reader.clone()
            })
            .unwrap()
            .into_json()
            .unwrap()["copied"],
        false
    );
    fixture
        .pane
        .request(PaneRequest::Detach {
            view_id: reader.clone(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(
        fixture
            .pane
            .request(PaneRequest::CopySelection {
                view_id: reader.clone()
            })
            .is_err()
    );
    assert!(
        fixture
            .pane
            .request(PaneRequest::Select {
                view_id: reader.clone(),
                event: event(SelectionAction::Clear, 0.0)
            })
            .is_err()
    );
    let live = fixture.attach();
    fixture
        .pane
        .request(PaneRequest::Shutdown)
        .unwrap()
        .into_json()
        .unwrap();
    assert!(
        fixture
            .pane
            .request(PaneRequest::CopySelection {
                view_id: live.clone()
            })
            .is_err()
    );
    assert!(fixture.poll(&live, 0).selection.is_none());
}

#[test]
fn queued_copy_cannot_export_after_originating_view_policy_is_retired() {
    let fixture = Fixture::new();
    let reader = fixture.attach();
    fixture.select(&reader, SelectionAction::Press, 0.1);
    fixture.select(&reader, SelectionAction::Drag, 4.9);
    fixture.select(&reader, SelectionAction::Release, 4.9);
    let inspection_epoch = visible(&fixture.pane.shared, &reader, None).unwrap();
    // Exercise the actual worker's second authority boundary: a queued copy
    // stamped by an earlier policy cannot read after hide -> show, even though
    // the opaque view ID still exists and is visible again.
    let mut hidden = spec();
    hidden.visible = false;
    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: hidden,
        })
        .unwrap()
        .into_json()
        .unwrap();
    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: spec(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(
        fixture
            .pane
            .command(crate::Mutation::Select {
                view_id: reader.clone(),
                inspection_epoch,
                event: event(SelectionAction::Press, 0.1),
            })
            .is_err()
    );
    assert!(
        fixture
            .pane
            .command(crate::Mutation::CopySelection {
                view_id: reader,
                inspection_epoch
            })
            .is_err()
    );
}

#[test]
fn queued_selection_and_copy_survive_same_visible_sizing_and_capability_updates() {
    let fixture = Fixture::new();
    let reader = fixture.attach();
    let inspection_epoch = visible(&fixture.pane.shared, &reader, None).unwrap();
    // A focus path may reassert an unchanged sizing lease after queuing input.
    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: spec(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    fixture
        .pane
        .command(crate::Mutation::Select {
            view_id: reader.clone(),
            inspection_epoch,
            event: event(SelectionAction::Press, 0.1),
        })
        .unwrap();

    // Actual native resize and independent permission changes must preserve
    // the same inspection lifetime and the already-held native gesture.
    let mut changed = spec();
    changed.size.cols = 24;
    changed.interactive = true;
    changed.input = true;
    changed.clipboard = true;
    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: changed,
        })
        .unwrap()
        .into_json()
        .unwrap();
    for action in [SelectionAction::Drag, SelectionAction::Release] {
        fixture
            .pane
            .command(crate::Mutation::Select {
                view_id: reader.clone(),
                inspection_epoch,
                event: event(action, 4.9),
            })
            .unwrap();
    }
    let expected = serde_json::json!({
        "copied":true,
        "contents":[{"mime":"text/plain","data":b"hello"}],
    });
    assert_eq!(
        fixture
            .pane
            .command(crate::Mutation::CopySelection {
                view_id: reader.clone(),
                inspection_epoch,
            })
            .unwrap()
            .response(),
        expected
    );

    fixture
        .pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: spec(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        fixture
            .pane
            .command(crate::Mutation::CopySelection {
                view_id: reader.clone(),
                inspection_epoch,
            })
            .unwrap()
            .response(),
        expected
    );
    assert!(fixture.poll(&reader, 0).selection.unwrap().active);
}
