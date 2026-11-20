use super::*;
use crate::image_resources::tests::Fixture;
use bkpr_protocol::{ImageTransfer, PixelFormat};
use std::sync::mpsc;

fn admitted(fixture: &Fixture) -> Admitted {
    let pool = PreparationPool::new().unwrap();
    let permit = pool.admit_stream(&fixture.client.id).unwrap();
    Admitted {
        observation: fixture.observe(0),
        pool,
        _permit: permit,
        started: Instant::now(),
        pane_id: fixture.pane.shared.pane_id.clone(),
        original_encodings: false,
    }
}
#[test]
fn one_native_request_stream_yields_exact_completed_pixels_and_releases_admission() {
    let fixture = Fixture::new();
    let admitted = admitted(&fixture);
    let descriptor = admitted.observation.descriptor;
    let expected = admitted.observation.image.upgrade().unwrap();
    let pool = admitted.pool.clone();
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let worker = thread::spawn(move || serve(&mut sender, admitted, &AtomicBool::new(false)));
    let mut transfer = ImageTransfer::new(descriptor).unwrap();
    let mut frames = 0;
    let image = loop {
        let body =
            wire::read_frame(&mut receiver, Kind::Response, IMAGE_RESOURCE_MAX_FRAME).unwrap();
        let response = wire::decode_response_owned(body).unwrap();
        assert!(response.ok);
        let Some(Payload::ImageResource(resource)) = response.result else {
            panic!("not an image response")
        };
        frames += 1;
        if let Some(image) = transfer.accept(resource, || false).unwrap() {
            break image;
        }
    };
    assert!(frames >= 3);
    assert_eq!(image, *expected);
    worker.join().unwrap();
    assert!(pool.admit_stream("client").is_ok());
}
#[test]
fn stalled_resource_write_cancels_without_waiting_for_the_total_deadline() {
    let fixture = Fixture::new();
    // An incompressible canonical body makes the real socket exert backpressure.
    let mut state = 0x12345678u32;
    let pixels: Vec<_> = (0..4 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    {
        let mut published = fixture.pane.shared.published.lock().unwrap();
        let mut frame = published.frame.as_ref().unwrap().as_ref().clone();
        frame.images = vec![Arc::new(bkpr_protocol::Image {
            id: 0,
            generation: 1,
            width: 1024,
            height: 1024,
            format: PixelFormat::Rgba,
            pixels,
            source: None,
        })];
        published.frame = Some(Arc::new(frame));
    }
    let admitted = admitted(&fixture);
    let pool = admitted.pool.clone();
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (done, completed) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        serve(&mut sender, admitted, &AtomicBool::new(false));
        done.send(()).unwrap();
    });
    let first = wire::read_frame(&mut receiver, Kind::Response, IMAGE_RESOURCE_MAX_FRAME).unwrap();
    assert!(matches!(
        wire::decode_response(&first).unwrap().result,
        Some(Payload::ImageResource(ImageResource::Start { .. }))
    ));
    fixture
        .pane
        .shared
        .authority
        .lock()
        .unwrap()
        .detach(&fixture.view)
        .unwrap();
    completed.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert!(pool.admit_stream("client").is_ok());
}
#[test]
fn progress_stall_and_total_deadlines_are_distinct_and_control_remains_bounded() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let start = Instant::now();
    let mut writer = ResponseWriter {
        stream: &stream,
        deadline: start + TRANSFER_TOTAL,
        progress: start,
        stall: Some(TRANSFER_STALL),
        active: None,
        sent: 0,
    };
    // A progressing transfer remains valid after the old three-second total bound.
    writer.progress = start + Duration::from_secs(4);
    assert_eq!(
        writer.remaining(start + Duration::from_secs(6)),
        Duration::from_secs(3)
    );
    assert!(writer.remaining(start + Duration::from_secs(10)).is_zero());
    writer.progress = start + Duration::from_secs(119);
    assert!(writer.remaining(start + TRANSFER_TOTAL).is_zero());
    writer.deadline = start + Duration::from_secs(3);
    writer.stall = None;
    assert!(writer.remaining(start + Duration::from_secs(4)).is_zero());
}
#[test]
fn a_saturated_slow_bulk_lane_does_not_take_control_encoding_capacity() {
    let fixture = Fixture::new();
    let admitted = admitted(&fixture);
    let second = admitted.pool.admit_stream(&fixture.client.id).unwrap();
    assert!(admitted.pool.admit_stream(&fixture.client.id).is_err());
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    let stage = WorkStages::new(1);
    let buffers = FrameBuffers::default();
    let reply = response(Ok(Payload::Control(json!({"control":"available"}))), false);
    let mut codec = Codec::new();
    write_buffered_response(
        &mut sender,
        codec.encode_response(&reply).unwrap(),
        &buffers,
        stage.enter(),
        None,
    )
    .unwrap();
    let bytes = wire::read_frame(&mut receiver, Kind::Response, MAX_RESPONSE).unwrap();
    assert_eq!(
        wire::decode_response(&bytes)
            .unwrap()
            .result
            .unwrap()
            .into_json()
            .unwrap()["control"],
        "available"
    );
    drop(second);
    drop(admitted);
}
