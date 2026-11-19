use super::*;
use bkpr_protocol::{Operation, PaneRequest, PixelFormat, Request, SessionInfo, Size, ViewSpec};
use std::{os::unix::net::UnixStream, time::Instant};

pub(crate) struct Fixture {
    pub(crate) pane: Pane,
    pub(crate) client: Arc<ClientLease>,
    pub(crate) view: String,
    registry: crate::registry::Registry,
    _lifetime: UnixStream,
    _root: tempfile::TempDir,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::open(&root.path().join("state")).unwrap();
        let session: SessionInfo = serde_json::from_value(
            registry
                .request(Request::new(Operation::SessionInfo))
                .unwrap()
                .into_json()
                .unwrap()["session"]
                .clone(),
        )
        .unwrap();
        let (server, lifetime) = UnixStream::pair().unwrap();
        let (client, registered) = registry
            .register_client(
                &server,
                &session.session_id,
                &session.daemon_id,
                &session.session_instance_id,
            )
            .unwrap();
        let authority = serde_json::from_value(registered["client"].clone()).unwrap();
        registry
            .client_request(
                authority,
                Operation::ClientActivate {
                    detach_others: false,
                    agent_socket: None,
                },
                &server,
            )
            .unwrap();
        let size = Size {
            cols: 4,
            rows: 2,
            cell_width: 8,
            cell_height: 16,
        };
        let pane = Pane::spawn(crate::SpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
            cwd: root.path().to_owned(),
            home: root.path().join("home"),
            size,
            isolated_environment: true,
            isolated_process: false,
        })
        .unwrap();
        let view = pane
            .shared
            .authority
            .lock()
            .unwrap()
            .attach_owned(
                ViewSpec {
                    size,
                    visible: true,
                    interactive: false,
                    input: false,
                    clipboard: false,
                },
                Some(client.clone()),
            )
            .unwrap();
        let fixture = Self {
            pane,
            client,
            view,
            registry,
            _lifetime: lifetime,
            _root: root,
        };
        fixture.images(1);
        fixture
    }
    pub(crate) fn images(&self, count: usize) {
        let mut published = self.pane.shared.published.lock().unwrap();
        let mut frame = published.frame.as_ref().unwrap().as_ref().clone();
        frame.images = (0..count)
            .map(|id| {
                Arc::new(Image {
                    id: id as u32,
                    generation: 1,
                    width: 1,
                    height: 1,
                    format: PixelFormat::Rgba,
                    pixels: vec![id as u8, 21, 92, 0],
                })
            })
            .collect();
        frame.placements.clear();
        published.frame = Some(Arc::new(frame));
    }
    pub(crate) fn observe(&self, id: u32) -> Observation {
        Observation::new(
            self.pane.clone(),
            self.client.clone(),
            self.view.clone(),
            ImageKey { id, generation: 1 },
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.pane.shared.forgotten.store(false, Ordering::Release);
        let _ = self.pane.request(PaneRequest::Shutdown);
        let _ = self.registry.shutdown();
    }
}
fn ready(preparation: &Preparation) -> std::result::Result<Arc<Encoded>, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = preparation.wait(Duration::from_millis(50)) {
            return result;
        }
        assert!(Instant::now() < deadline, "preparation failed to settle");
    }
}
#[test]
fn coalesced_observers_and_cache_hits_share_exact_bytes_without_reencoding() {
    let fixture = Fixture::new();
    let pool = PreparationPool::new().unwrap();
    let first = fixture.observe(0);
    let a = pool.request(&first).unwrap();
    let b = pool.request(&fixture.observe(0)).unwrap();
    let a = ready(&a).unwrap();
    let b = ready(&b).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let pixels = first.image.upgrade().unwrap();
    assert_eq!(a.bytes.as_ref(), pixels.pixels.as_slice());
    assert_eq!(a.bytes.as_ptr(), pixels.pixels.as_ptr());
    let warm = ready(&pool.request(&first).unwrap()).unwrap();
    assert!(Arc::ptr_eq(&a, &warm));
    assert_eq!(pool.encoded.load(Ordering::Relaxed), 1);
    assert_eq!(pool.state.lock().unwrap().bytes, pixels.pixels.capacity());
}
#[test]
fn foreign_view_pane_generation_and_revoked_cache_reads_fail_closed() {
    let a = Fixture::new();
    let b = Fixture::new();
    let pool = PreparationPool::new().unwrap();
    let observation = a.observe(0);
    ready(&pool.request(&observation).unwrap()).unwrap();
    assert!(
        Observation::new(
            a.pane.clone(),
            b.client.clone(),
            a.view.clone(),
            observation.descriptor.key
        )
        .is_err()
    );
    assert!(
        Observation::new(
            b.pane.clone(),
            a.client.clone(),
            a.view.clone(),
            observation.descriptor.key
        )
        .is_err()
    );
    assert!(
        Observation::new(
            a.pane.clone(),
            a.client.clone(),
            a.view.clone(),
            ImageKey {
                id: 0,
                generation: 2
            }
        )
        .is_err()
    );
    a.pane
        .shared
        .authority
        .lock()
        .unwrap()
        .detach(&a.view)
        .unwrap();
    assert!(pool.request(&observation).is_err());
}
#[test]
fn replaced_and_retired_preparations_never_publish_stale_cache_completions() {
    for retire in [false, true] {
        let fixture = Fixture::new();
        let pool = PreparationPool::new().unwrap();
        let gate = Arc::new(std::sync::Barrier::new(2));
        pool.before_encode.lock().unwrap().push_back(gate.clone());
        let observation = fixture.observe(0);
        let pending = pool.request(&observation).unwrap();
        gate.wait();
        if retire {
            fixture.pane.shared.forgotten.store(true, Ordering::Release);
            pool.retire(fixture.pane.shared.image_incarnation);
        } else {
            fixture.images(1);
        }
        gate.wait();
        assert!(ready(&pending).is_err());
        assert!(pool.state.lock().unwrap().cache.is_empty());
        assert!(observation.validate().is_err());
    }
}
#[test]
fn one_cancelled_view_does_not_cancel_another_coalesced_observer() {
    let fixture = Fixture::new();
    let pool = PreparationPool::new().unwrap();
    let gate = Arc::new(std::sync::Barrier::new(2));
    pool.before_encode.lock().unwrap().push_back(gate.clone());
    let first = fixture.observe(0);
    let cancelled = pool.request(&first).unwrap();
    gate.wait();
    let other_view = {
        let mut authority = fixture.pane.shared.authority.lock().unwrap();
        let spec = authority.views[&fixture.view].spec.clone();
        authority
            .attach_owned(spec, Some(fixture.client.clone()))
            .unwrap()
    };
    let other = Observation::new(
        fixture.pane.clone(),
        fixture.client.clone(),
        other_view,
        first.descriptor.key,
    )
    .unwrap();
    let retained = pool.request(&other).unwrap();
    fixture
        .pane
        .shared
        .authority
        .lock()
        .unwrap()
        .detach(&fixture.view)
        .unwrap();
    assert!(first.validate().is_err());
    drop(cancelled);
    drop(first);
    gate.wait();
    assert_eq!(ready(&retained).unwrap().bytes.as_ref(), &[0, 21, 92, 0]);
    assert_eq!(pool.encoded.load(Ordering::Relaxed), 1);
}
#[test]
fn saturated_preparation_and_stream_admission_release_capacity_after_cancellation() {
    let fixture = Fixture::new();
    fixture.images(7);
    let pool = PreparationPool::new().unwrap();
    let gate = Arc::new(std::sync::Barrier::new(3));
    pool.before_encode
        .lock()
        .unwrap()
        .extend([gate.clone(), gate.clone()]);
    let a = pool.request(&fixture.observe(0)).unwrap();
    let b = pool.request(&fixture.observe(1)).unwrap();
    gate.wait();
    let queued: Vec<_> = (2..6)
        .map(|id| pool.request(&fixture.observe(id)).unwrap())
        .collect();
    assert!(pool.request(&fixture.observe(6)).is_err());
    let permit_a = pool.admit_stream("client").unwrap();
    let permit_b = pool.admit_stream("client").unwrap();
    assert!(pool.admit_stream("client").is_err());
    drop(permit_a);
    assert!(pool.admit_stream("client").is_ok());
    drop(permit_b);
    drop(queued);
    drop(a);
    drop(b);
    gate.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if pool.state.lock().unwrap().inputs == 0 {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(ready(&pool.request(&fixture.observe(6)).unwrap()).is_ok());
}
#[test]
fn pane_incarnations_do_not_share_numeric_keys_and_retirement_purges_reuse() {
    let a = Fixture::new();
    let b = Fixture::new();
    let pool = PreparationPool::new().unwrap();
    let encoded_a = ready(&pool.request(&a.observe(0)).unwrap()).unwrap();
    let encoded_b = ready(&pool.request(&b.observe(0)).unwrap()).unwrap();
    assert!(!Arc::ptr_eq(&encoded_a, &encoded_b));
    pool.retire(a.pane.shared.image_incarnation);
    let state = pool.state.lock().unwrap();
    assert_eq!(state.cache.len(), 1);
    assert_eq!(
        state.cache[0].0.incarnation,
        b.pane.shared.image_incarnation
    );
}

#[test]
fn two_clients_queue_two_images_each_and_cancellation_preserves_fifo_fairness() {
    let pool = PreparationPool::new().unwrap();
    let a1 = pool.admit_stream("a").unwrap();
    let a2 = pool.admit_stream("a").unwrap();
    let b1 = pool.admit_stream("b").unwrap();
    let b2 = pool.admit_stream("b").unwrap();
    assert!(a1.activate(Duration::ZERO));
    assert!(a2.activate(Duration::ZERO));
    assert!(!b1.activate(Duration::ZERO) && !b2.activate(Duration::ZERO));
    drop(a1);
    assert!(!b2.activate(Duration::ZERO));
    assert!(b1.activate(Duration::ZERO));
    drop(a2);
    assert!(b2.activate(Duration::ZERO));
    let c1 = pool.admit_stream("c").unwrap();
    let c2 = pool.admit_stream("c").unwrap();
    let d1 = pool.admit_stream("d").unwrap();
    let d2 = pool.admit_stream("d").unwrap();
    assert!(pool.admit_stream("e").is_err());
    drop(c1);
    drop(b1);
    assert!(c2.activate(Duration::ZERO));
    assert!(!d1.activate(Duration::ZERO));
    drop(b2);
    assert!(d1.activate(Duration::ZERO));
    drop(c2);
    drop(d1);
    drop(d2);
    assert!(pool.streams.lock().unwrap().waiting.is_empty());
    assert!(pool.streams.lock().unwrap().active.is_empty());
}

#[test]
fn bounded_reuse_evicts_without_invalidating_pinned_completed_pixels() {
    let fixture = Fixture::new();
    fixture.images(CACHE_ENTRIES + 1);
    let pool = PreparationPool::new().unwrap();
    let pinned = ready(&pool.request(&fixture.observe(0)).unwrap()).unwrap();
    for id in 1..=CACHE_ENTRIES as u32 {
        ready(&pool.request(&fixture.observe(id)).unwrap()).unwrap();
    }
    {
        let state = pool.state.lock().unwrap();
        assert_eq!(state.cache.len(), CACHE_ENTRIES);
        assert!(state.bytes <= CACHE_BYTES);
    }
    assert_eq!(pinned.bytes.as_ref(), &[0, 21, 92, 0]);
    let reloaded = ready(&pool.request(&fixture.observe(0)).unwrap()).unwrap();
    assert!(!Arc::ptr_eq(&pinned, &reloaded));
    assert_eq!(pinned.bytes, reloaded.bytes);
}

#[test]
fn shared_compression_reuses_compact_storage_and_completes_lossless_pixels() {
    let fixture = Fixture::new();
    let expected = Arc::new(Image {
        id: 0,
        generation: 1,
        width: 1024,
        height: 1024,
        format: PixelFormat::Rgba,
        pixels: vec![0; 4 * 1024 * 1024],
    });
    {
        let mut published = fixture.pane.shared.published.lock().unwrap();
        let mut frame = published.frame.as_ref().unwrap().as_ref().clone();
        frame.images = vec![expected.clone()];
        published.frame = Some(Arc::new(frame));
    }
    let pool = PreparationPool::new().unwrap();
    let observation = fixture.observe(0);
    let first = pool.request(&observation).unwrap();
    let second = pool.request(&observation).unwrap();
    let first = ready(&first).unwrap();
    let second = ready(&second).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.encoding, ImageEncoding::Zstd);
    assert_eq!(first.retained_bytes, first.bytes.len());
    assert!(first.bytes.len() < expected.pixels.len() / 100);
    assert_eq!(pool.state.lock().unwrap().bytes, first.bytes.len());
    let mut receiver = bkpr_protocol::ImageTransfer::new(observation.descriptor).unwrap();
    receiver
        .accept(
            bkpr_protocol::ImageResource::Start {
                descriptor: observation.descriptor,
                encoding: first.encoding,
                encoded_len: first.bytes.len() as u64,
            },
            || false,
        )
        .unwrap();
    for offset in (0..first.bytes.len()).step_by(bkpr_protocol::IMAGE_CHUNK_BYTES) {
        let end = (offset + bkpr_protocol::IMAGE_CHUNK_BYTES).min(first.bytes.len());
        receiver
            .accept(
                bkpr_protocol::ImageResource::Data {
                    key: expected.key(),
                    offset: offset as u64,
                    data: first.bytes.slice(offset..end),
                },
                || false,
            )
            .unwrap();
    }
    let image = receiver
        .accept(
            bkpr_protocol::ImageResource::End {
                key: expected.key(),
                encoded_len: first.bytes.len() as u64,
            },
            || false,
        )
        .unwrap()
        .unwrap();
    assert_eq!(image, *expected);
    assert_eq!(pool.encoded.load(Ordering::Relaxed), 1);
}

#[test]
fn unchanged_image_survives_scroll_projection_and_size_policy_but_not_visibility_retirement() {
    let fixture = Fixture::new();
    let observation = fixture.observe(0);
    let pool = PreparationPool::new().unwrap();
    let gate = Arc::new(std::sync::Barrier::new(2));
    pool.before_encode.lock().unwrap().push_back(gate.clone());
    let pending = pool.request(&observation).unwrap();
    gate.wait();
    let mut spec = {
        let mut authority = fixture.pane.shared.authority.lock().unwrap();
        let mut spec = authority.views[&fixture.view].spec.clone();
        spec.size.cols += 1;
        authority.update(&fixture.view, spec.clone()).unwrap();
        authority
            .views
            .get_mut(&fixture.view)
            .unwrap()
            .scrollback_offset = 3;
        spec
    };
    {
        let mut published = fixture.pane.shared.published.lock().unwrap();
        let mut projected = published.frame.as_ref().unwrap().as_ref().clone();
        projected.scrollback_total += 3;
        projected.scrollback_offset = 3;
        published
            .scroll_frames
            .insert(fixture.view.clone(), Arc::new(projected));
    }
    observation.validate().unwrap();
    gate.wait();
    let encoded = ready(&pending).unwrap();
    observation.validate().unwrap();
    let mut receiver = bkpr_protocol::ImageTransfer::new(observation.descriptor).unwrap();
    receiver
        .accept(
            bkpr_protocol::ImageResource::Start {
                descriptor: observation.descriptor,
                encoding: encoded.encoding,
                encoded_len: encoded.bytes.len() as u64,
            },
            || false,
        )
        .unwrap();
    receiver
        .accept(
            bkpr_protocol::ImageResource::Data {
                key: observation.descriptor.key,
                offset: 0,
                data: encoded.bytes.clone(),
            },
            || false,
        )
        .unwrap();
    let image = receiver
        .accept(
            bkpr_protocol::ImageResource::End {
                key: observation.descriptor.key,
                encoded_len: encoded.bytes.len() as u64,
            },
            || false,
        )
        .unwrap()
        .unwrap();
    assert_eq!(image.pixels, [0, 21, 92, 0]);
    let mut authority = fixture.pane.shared.authority.lock().unwrap();
    spec.visible = false;
    authority.update(&fixture.view, spec.clone()).unwrap();
    spec.visible = true;
    authority.update(&fixture.view, spec).unwrap();
    drop(authority);
    assert!(observation.validate().is_err());
    assert!(pool.request(&observation).is_err());
    assert!(pool.request(&fixture.observe(0)).is_ok());
}

#[test]
fn separate_registrations_share_one_generation_preparation_and_warm_cache() {
    let fixture = Fixture::new();
    let session: SessionInfo = serde_json::from_value(
        fixture
            .registry
            .request(Request::new(Operation::SessionInfo))
            .unwrap()
            .into_json()
            .unwrap()["session"]
            .clone(),
    )
    .unwrap();
    let (server, _lifetime) = UnixStream::pair().unwrap();
    let (second_client, registered) = fixture
        .registry
        .register_client(
            &server,
            &session.session_id,
            &session.daemon_id,
            &session.session_instance_id,
        )
        .unwrap();
    let authority = serde_json::from_value(registered["client"].clone()).unwrap();
    fixture
        .registry
        .client_request(
            authority,
            Operation::ClientActivate {
                detach_others: false,
                agent_socket: None,
            },
            &server,
        )
        .unwrap();
    assert_ne!(fixture.client.id, second_client.id);
    assert_eq!(fixture.client.runtime, second_client.runtime);
    let second_view = {
        let mut authority = fixture.pane.shared.authority.lock().unwrap();
        let spec = authority.views[&fixture.view].spec.clone();
        authority
            .attach_owned(spec, Some(second_client.clone()))
            .unwrap()
    };
    let first = fixture.observe(0);
    let second = Observation::new(
        fixture.pane.clone(),
        second_client,
        second_view,
        first.descriptor.key,
    )
    .unwrap();
    let pool = PreparationPool::new().unwrap();
    let gate = Arc::new(std::sync::Barrier::new(2));
    pool.before_encode.lock().unwrap().push_back(gate.clone());
    let a = pool.request(&first).unwrap();
    gate.wait();
    let b = pool.request(&second).unwrap();
    assert!(Arc::ptr_eq(&a.0, &b.0));
    gate.wait();
    let a = ready(&a).unwrap();
    let b = ready(&b).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.bytes.as_ref(), &[0, 21, 92, 0]);
    let warm = ready(&pool.request(&second).unwrap()).unwrap();
    assert!(Arc::ptr_eq(&a, &warm));
    assert_eq!(pool.encoded.load(Ordering::Relaxed), 1);
}
