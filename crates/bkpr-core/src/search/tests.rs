use crate::{Mutation, Pane, SpawnSpec, selection};
use bkpr_protocol::{PaneRequest, PaneSnapshot, SearchAction, Size, ViewSpec};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    pane: Pane,
    _root: tempfile::TempDir,
}
fn spec() -> ViewSpec {
    ViewSpec {
        size: Size {
            cols: 30,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        },
        interactive: false,
        visible: true,
        input: false,
        clipboard: false,
    }
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pane = Pane::spawn(SpawnSpec {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'hello world'; read -r answer".into(),
            ],
            home: root.path().join("home"),
            cwd: root.path().to_path_buf(),
            size: spec().size,
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
            if snapshot.frame.is_some_and(|f| {
                f.rows[0]
                    .cells
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<String>()
                    .contains("hello world")
            }) {
                break;
            }
            assert!(Instant::now() < deadline);
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
    fn search(&self, id: &str, action: SearchAction) -> serde_json::Value {
        self.pane
            .request(PaneRequest::Search {
                view_id: id.into(),
                action,
            })
            .unwrap()
            .into_json()
            .unwrap()
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
    fn complete(&self, id: &str) -> PaneSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = self.poll(id, 0);
            if snapshot.search.as_ref().is_some_and(|s| !s.running) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "search did not complete");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.pane.request(PaneRequest::Shutdown);
    }
}

#[test]
fn read_only_search_publishes_annotations_without_scene_clones_or_input_grants() {
    let f = Fixture::new();
    let reader = f.attach();
    let other = f.attach();
    // Drain accepted attach/resize reconciliation before measuring annotations.
    f.search(&reader, SearchAction::Clear);
    let before = f.poll(&reader, 0);
    let canonical = f
        .pane
        .shared
        .published
        .lock()
        .unwrap()
        .frame
        .clone()
        .unwrap();
    let result = f.search(
        &reader,
        SearchAction::Query {
            text: "HELLO".into(),
        },
    );
    let after = f.complete(&reader);
    let search = after.search.as_ref().unwrap();
    assert_eq!(search.generation, result["generation"].as_u64().unwrap());
    assert_eq!(search.total_matches, 1);
    assert_eq!(search.selected_index, Some(0));
    assert!(search.spans.iter().any(|span| span.selected));
    assert_eq!(after.revision, before.revision);
    assert!(after.snapshot_revision > before.snapshot_revision);
    assert!(Arc::ptr_eq(
        &canonical,
        f.pane
            .shared
            .published
            .lock()
            .unwrap()
            .frame
            .as_ref()
            .unwrap()
    ));
    assert!(f.poll(&other, 0).search.is_none());
    assert!(after.size_owner.is_none());
    assert!(
        f.pane
            .request(PaneRequest::Input {
                view_id: reader.clone(),
                data: b"must not arrive".to_vec()
            })
            .is_err()
    );
    let no_scene = f.poll(&reader, before.revision);
    assert!(no_scene.frame.is_none());
    f.search(&reader, SearchAction::Clear);
    assert!(f.poll(&reader, before.revision).search.is_none());
}
#[test]
fn inspection_epochs_reject_queued_queries_and_hidden_completion_without_revival() {
    let f = Fixture::new();
    let reader = f.attach();
    let epoch = selection::visible(&f.pane.shared, &reader, None).unwrap();
    let initial = f.search(
        &reader,
        SearchAction::Query {
            text: "hello".into(),
        },
    )["generation"]
        .as_u64()
        .unwrap();
    f.complete(&reader);
    // Sizing activity and independent permissions do not retire inspection.
    f.pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: spec(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    f.pane
        .command(Mutation::Search {
            view_id: reader.clone(),
            inspection_epoch: epoch,
            action: SearchAction::Next,
        })
        .unwrap();
    assert_eq!(f.poll(&reader, 0).search.unwrap().generation, initial);
    let mut hidden = spec();
    hidden.visible = false;
    f.pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: hidden,
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(f.poll(&reader, 0).search.is_none());
    assert!(
        f.pane
            .request(PaneRequest::Search {
                view_id: reader.clone(),
                action: SearchAction::Next
            })
            .is_err()
    );
    f.pane
        .request(PaneRequest::Update {
            view_id: reader.clone(),
            spec: spec(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(
        f.pane
            .command(Mutation::Search {
                view_id: reader.clone(),
                inspection_epoch: epoch,
                action: SearchAction::Query {
                    text: "world".into()
                }
            })
            .is_err()
    );
    assert!(f.poll(&reader, 0).search.is_none());
    let fresh = f.search(
        &reader,
        SearchAction::Query {
            text: "hello".into(),
        },
    )["generation"]
        .as_u64()
        .unwrap();
    assert!(fresh > initial);
    f.complete(&reader);
    f.pane
        .request(PaneRequest::Detach {
            view_id: reader.clone(),
        })
        .unwrap()
        .into_json()
        .unwrap();
    assert!(
        f.pane
            .request(PaneRequest::Search {
                view_id: reader,
                action: SearchAction::Clear
            })
            .is_err()
    );
}
#[test]
fn completion_retires_search_and_query_round_trips_disambiguate_repeated_text() {
    let f = Fixture::new();
    let reader = f.attach();
    let a = f.search(
        &reader,
        SearchAction::Query {
            text: "hello".into(),
        },
    )["generation"]
        .as_u64()
        .unwrap();
    let b = f.search(
        &reader,
        SearchAction::Query {
            text: "world".into(),
        },
    )["generation"]
        .as_u64()
        .unwrap();
    let c = f.search(
        &reader,
        SearchAction::Query {
            text: "hello".into(),
        },
    )["generation"]
        .as_u64()
        .unwrap();
    assert!(a < b && b < c);
    assert_eq!(f.complete(&reader).search.unwrap().generation, c);
    f.pane
        .request(PaneRequest::Shutdown)
        .unwrap()
        .into_json()
        .unwrap();
    assert!(f.poll(&reader, 0).search.is_none());
    assert!(
        f.pane
            .request(PaneRequest::Search {
                view_id: reader,
                action: SearchAction::Next
            })
            .is_err()
    );
}
