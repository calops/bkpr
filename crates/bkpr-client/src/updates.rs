use crate::transport::Endpoint;
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::*;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

pub struct Snapshot {
    pub metadata: PaneSnapshot,
    pub scene: Option<scene_wire::DecodedScene>,
    /// Terminal payloads for the scene's newly supplied images, prepared on the poll thread.
    pub transports: Vec<bkpr_tui_framework::PreparedTransport>,
}
/// Image generations a view retains, reported on each poll so the backend omits them.
pub type HeldImages = Arc<std::sync::Mutex<Vec<ImageKey>>>;
/// Bounded single-flight receiver: output cannot grow an unbounded queue or
/// block the UI's keyboard reader. All worker state is owned, never UI-borrowed.
pub struct Pump<T> {
    receiver: Option<mpsc::Receiver<Result<T>>>,
    pub wake: UnixStream,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl<T> Pump<T> {
    /// Retire this receiver generation before its backend view is detached.
    pub fn cancel(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver.take();
    }
}
impl<T: Send + 'static> Pump<T> {
    fn start(mut next: impl FnMut() -> Result<T> + Send + 'static) -> Result<Self> {
        let (wake, mut notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let value = next();
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let failed = value.is_err();
                if sender.send(value).is_err() {
                    break;
                }
                let _ = notify.write_all(&[1]);
                if failed {
                    break;
                }
            }
        });
        Ok(Self {
            receiver: Some(receiver),
            wake,
            stop,
            worker: Some(worker),
        })
    }
    #[cfg(test)]
    pub(crate) fn capture() -> Self {
        let (_, receiver) = mpsc::sync_channel(1);
        let (wake, _) = UnixStream::pair().unwrap();
        Self {
            receiver: Some(receiver),
            wake,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }
    pub fn initial(&self) -> Result<T> {
        self.receiver
            .as_ref()
            .context("snapshot receiver cancelled")?
            .recv()
            .context("snapshot worker disconnected")?
    }
    pub fn latest(&mut self) -> Result<Option<T>> {
        let mut bytes = [0u8; 128];
        while matches!(self.wake.read(&mut bytes),Ok(n)if n>0) {}
        let Some(receiver) = self.receiver.as_ref() else {
            return Ok(None);
        };
        match receiver.try_recv() {
            Ok(value) => value.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => bail!("snapshot worker disconnected"),
        }
    }
}
impl<T> Drop for Pump<T> {
    fn drop(&mut self) {
        self.cancel();
        // Detach wakes an in-flight poll. Do not join on the input/render path:
        // transport completion/decode can still be finishing after its ACK.
        drop(self.worker.take());
    }
}
/// Observe inline scenes, in order with their images, with independent frame/metadata cursors.
/// Images arrive with the scene that first references them, as their original payload when the
/// backend retained one; `held` names the generations the view already has.
pub fn pane(
    socket: &Endpoint,
    pane_id: &str,
    view: &str,
    held: HeldImages,
) -> Result<Pump<Snapshot>> {
    let socket = socket.to_owned();
    let pane_id = pane_id.to_owned();
    let view_id = view.to_owned();
    let (mut frame_after, mut snapshot_after) = (0, 0);
    Pump::start(move || {
        // Copy in its own statement: a guard in the call expression would stay locked for
        // the whole long-poll and block the UI thread's update of `held`.
        let resources = held.lock().unwrap().clone();
        let (metadata, scene) = socket.checked_scene(Operation::Pane {
            pane_id: pane_id.clone(),
            request: PaneRequest::Poll {
                view_id: view_id.clone(),
                after: 0,
                frame_after,
                resources,
                snapshot_after,
                wait_ms: 1000,
                row_deltas: true,
                scene_encoding: Some(SOURCES_SCENE_ENCODING.into()),
            },
        })?;
        ensure!(
            scene.is_some() || metadata.revision == frame_after,
            "native pane frame revision changed without a scene"
        );
        snapshot_after = metadata.snapshot_revision;
        frame_after = metadata.revision;
        let transports = scene
            .iter()
            .flat_map(|scene| scene.images())
            .filter_map(crate::resources::prepare_transport)
            .collect();
        Ok(Snapshot {
            metadata,
            scene,
            transports,
        })
    })
}
pub fn registry(socket: &Endpoint, after: u64) -> Result<Pump<RegistrySnapshot>> {
    let socket = socket.to_owned();
    let mut after = after;
    Pump::start(move || {
        let value: RegistryResult = serde_json::from_value(socket.checked(Operation::Wait {
            after,
            wait_ms: 1000,
        })?)?;
        after = value.registry.revision;
        Ok(value.registry)
    })
}

#[path = "input.rs"]
mod input;
#[cfg(test)]
pub(crate) use input::Capture;
pub(crate) use input::{
    DeviceReservation, InputFailure, InputLane, InputPump, InputRejected, Receipt, ReservedInput,
    ResponseReceipt, cleanup,
};

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::wire::{self, Codec, Kind};
    use std::{os::unix::net::UnixListener, time::Duration};

    fn frame(text: &str) -> Frame {
        let style = Style {
            foreground: [240; 3],
            background: [20; 3],
            underline_color: [240; 3],
            foreground_source: ColorSource::Default,
            background_source: ColorSource::Palette(0),
            underline_source: ColorSource::Rgb([240; 3]),
            bold: false,
            italic: false,
            faint: false,
            blink: false,
            inverse: false,
            invisible: false,
            strikethrough: false,
            overline: false,
            underline: 0,
        };
        Frame {
            size: Size {
                cols: 2,
                rows: 1,
                cell_width: 8,
                cell_height: 16,
            },
            rows: vec![Row {
                cells: [text, " "]
                    .into_iter()
                    .map(|text| Cell {
                        text: text.into(),
                        width: 1,
                        style: style.clone(),
                        hyperlink: None,
                    })
                    .collect(),
            }],
            cursor: Cursor {
                x: 0,
                y: 0,
                visible: true,
                blinking: false,
                shape: CursorShape::Block,
                color: None,
            },
            foreground: [240; 3],
            background: [20; 3],
            palette: Vec::new(),
            images: Vec::new(),
            placements: Vec::new(),
            alternate_screen: false,
            mouse_reporting: false,
            scrollback_total: 1,
            scrollback_offset: 0,
        }
    }

    fn metadata(revision: u64, snapshot_revision: u64) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: "pane".into(),
            pid: Some(41),
            revision,
            snapshot_revision,
            frame: None,
            image_refs: Vec::new(),
            resource_protocol: 1,
            writer: None,
            lifecycle: "running".into(),
            exit_code: None,
            error: None,
            program_status: Vec::new(),
            program_identity: None,
            events: Vec::new(),
            event_sequence: 0,
            size_owner: None,
            lease_generation: 0,
            views: Vec::new(),
            clipboard: None,
            clipboard_pending: false,
            capabilities: PaneCapabilities {
                sixel: false,
                normalized_frames: true,
                native_mouse_reporting: true,
                clipboard_timeout_ms: 0,
                view_timeout_ms: 0,
                poll_wait_max_ms: 1000,
                row_deltas: true,
                scene_encodings: vec![PROGRESSIVE_SCENE_ENCODING.into()],
            },
            selection: None,
            search: None,
        }
    }

    fn invalid_update_is_not_delivered(
        invalid_scene: Option<Vec<u8>>,
        mut nested_frame: Option<Frame>,
    ) {
        let initial = Arc::new(frame("é"));
        let changed = frame("e\u{301}");
        let baseline = (1, initial.clone());
        let replies = [
            (0, None),
            (
                1,
                Some(scene_wire::encode_progressive(&initial, 1, None, 0, &[]).unwrap()),
            ),
            (1, None),
            (
                2,
                Some(scene_wire::encode_progressive(&changed, 2, Some(&baseline), 1, &[]).unwrap()),
            ),
            (3, invalid_scene),
        ];
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("updates.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let mut codec = Codec::new();
            for (index, (revision, scene)) in replies.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let bytes =
                    wire::read_frame(&mut stream, Kind::Request, wire::MAX_REQUEST).unwrap();
                let Operation::Pane {
                    pane_id,
                    request:
                        PaneRequest::Poll {
                            view_id,
                            frame_after,
                            resources,
                            snapshot_after,
                            row_deltas,
                            scene_encoding,
                            ..
                        },
                } = wire::decode_request(&bytes).unwrap().operation
                else {
                    panic!("expected a pane poll");
                };
                assert_eq!(pane_id, "pane");
                assert_eq!(view_id, "view");
                assert_eq!(frame_after, [0, 0, 1, 1, 2][index]);
                assert_eq!(
                    snapshot_after,
                    if index == 0 { 0 } else { index as u64 + 10 }
                );
                assert!(resources.is_empty());
                assert!(row_deltas);
                assert_eq!(scene_encoding.as_deref(), Some(SOURCES_SCENE_ENCODING));
                let mut snapshot = metadata(revision, index as u64 + 11);
                if index == 4 {
                    snapshot.frame = nested_frame.take();
                }
                let response = Response {
                    version: PROTOCOL_VERSION,
                    ok: true,
                    result: Some(Payload::Pane(PanePayload {
                        metadata: serde_json::to_value(snapshot).unwrap(),
                        scene: scene.map(Into::into),
                    })),
                    error: None,
                    failure: None,
                };
                wire::write_frame(
                    &mut stream,
                    Kind::Response,
                    codec.encode_response(&response).unwrap(),
                )
                .unwrap();
            }
        });
        let endpoint = Endpoint::unmanaged(&socket);
        let mut pump = pane(&endpoint, "pane", "view", HeldImages::default()).unwrap();
        let (mut revision, mut current) = (0, None);
        let empty = pump.initial().unwrap();
        assert!(empty.metadata.frame.is_none());
        assert!(empty.scene.is_none());
        assert_eq!(revision, 0);
        assert!(current.is_none());
        let first = pump.initial().unwrap();
        assert!(first.metadata.frame.is_none());
        first
            .scene
            .unwrap()
            .apply(&mut revision, &mut current)
            .unwrap();
        assert_eq!(revision, 1);
        assert_eq!(current.as_ref(), Some(initial.as_ref()));
        let second = pump.initial().unwrap();
        assert!(second.metadata.frame.is_none());
        assert!(second.scene.is_none());
        assert_eq!(revision, 1);
        assert_eq!(current.as_ref(), Some(initial.as_ref()));
        let third = pump.initial().unwrap();
        assert!(third.metadata.frame.is_none());
        third
            .scene
            .unwrap()
            .apply(&mut revision, &mut current)
            .unwrap();
        assert_eq!(revision, 2);
        assert_eq!(current.as_ref(), Some(&changed));
        assert!(pump.initial().is_err());
        assert_eq!(revision, 2);
        assert_eq!(current.as_ref(), Some(&changed));
        pump.worker.take().unwrap().join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn native_polls_decode_full_and_delta_scenes_and_reject_invalid_scene_before_delivery() {
        invalid_update_is_not_delivered(Some(vec![0]), None);
    }

    #[test]
    fn native_polls_reject_scene_revision_mismatch_before_delivery() {
        let scene = scene_wire::encode_progressive(&frame("m"), 4, None, 0, &[]).unwrap();
        invalid_update_is_not_delivered(Some(scene), None);
    }

    #[test]
    fn native_polls_reject_nested_json_frame_even_with_a_valid_binary_scene() {
        let nested = frame("n");
        let scene = scene_wire::encode_progressive(&nested, 3, None, 0, &[]).unwrap();
        invalid_update_is_not_delivered(Some(scene), Some(nested));
    }

    #[test]
    fn native_polls_reject_omitted_changed_scene_without_replacing_the_last_good_frame() {
        invalid_update_is_not_delivered(None, None);
    }

    #[test]
    fn inline_images_arrive_with_their_scene_and_held_generations_are_reported() {
        use crate::transport::test_wire::{read_request, write_response};

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("inline-polls.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let pixels = vec![9u8; 32 * 32 * 3];
        let source = {
            use std::io::Write;
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&pixels).unwrap();
            encoder.finish().unwrap()
        };
        let image = Arc::new(Image {
            id: 7,
            generation: 1,
            width: 32,
            height: 32,
            format: PixelFormat::Rgb,
            pixels: pixels.clone(),
            source: Some(Arc::new(ImageSource {
                encoding: ImageSourceEncoding::Zlib,
                bytes: source.clone(),
            })),
        });
        let key = image.key();
        // Generations the view already holds are reported verbatim on every poll.
        let other = ImageKey {
            id: 3,
            generation: 9,
        };
        let mut initial = frame("λ");
        initial.images.push(image);
        let scene = scene_wire::encode_with_sources(&initial, 1, None, 0, &[]).unwrap();
        let (polled, polled_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let poll = |listener: &UnixListener, expected: &[ImageKey]| {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                assert!(matches!(read_request(&mut stream).unwrap().operation,
                    Operation::Pane { pane_id, request: PaneRequest::Poll {
                        view_id, resources, scene_encoding, ..
                    } } if pane_id == "pane" && view_id == "view" && resources == expected
                        && scene_encoding.as_deref() == Some(SOURCES_SCENE_ENCODING)));
                stream
            };
            let reply = |stream: &mut UnixStream, snapshot_revision, scene: Option<Vec<u8>>| {
                let mut metadata = metadata(1, snapshot_revision);
                metadata.image_refs = vec![key];
                write_response(
                    stream,
                    &Response {
                        version: PROTOCOL_VERSION,
                        ok: true,
                        result: Some(Payload::Pane(PanePayload {
                            metadata: serde_json::to_value(metadata).unwrap(),
                            scene: scene.map(Into::into),
                        })),
                        error: None,
                        failure: None,
                    },
                )
                .unwrap();
            };
            let mut first = poll(&listener, &[other]);
            reply(&mut first, 11, Some(scene));
            polled.send(()).unwrap();
        });
        let endpoint = Endpoint::unmanaged(&socket);
        let held = HeldImages::new(std::sync::Mutex::new(vec![other]));
        let mut pump = pane(&endpoint, "pane", "view", held.clone()).unwrap();
        let first = pump.initial().unwrap();
        // The image arrived with its scene, decoded, with its original payload retained
        // and a terminal payload prepared on the poll thread.
        let images = first.scene.as_ref().unwrap().images();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].pixels, pixels);
        assert_eq!(images[0].source.as_ref().unwrap().bytes, source);
        assert_eq!(first.transports.len(), 1);
        polled_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        pump.cancel();
        server.join().unwrap();
    }

    #[test]
    fn cancellation_releases_buffered_scene_and_discards_late_retired_error() {
        let retained = Arc::new(());
        let weak = Arc::downgrade(&retained);
        let mut first = Some(retained);
        let (entered, running) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let mut pump = Pump::start(move || {
            if let Some(scene) = first.take() {
                return Ok(scene);
            }
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            bail!("retired view rejected its outstanding poll");
        })
        .unwrap();
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(weak.upgrade().is_some());
        pump.cancel();
        assert!(weak.upgrade().is_none());
        release.send(()).unwrap();
        pump.worker.take().unwrap().join().unwrap();
        assert!(pump.latest().unwrap().is_none());
    }
}
