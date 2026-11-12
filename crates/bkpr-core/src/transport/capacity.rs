use super::*;

// Large bodies cannot consume the reserve used by ordinary small control frames.
// Reservations include the decoder's bounded owned-data expansion, not just the
// raw frame. They are byte budgets, not watcher/client admission slots.
const SMALL_REQUEST: usize = 64 * 1024;
const SMALL_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const LARGE_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct FrameBuffers {
    small: Mutex<usize>,
    large: Mutex<usize>,
    small_limit: usize,
    large_limit: usize,
}
pub(super) struct BufferReservation<'a> {
    used: &'a Mutex<usize>,
    limit: usize,
    bytes: usize,
}
impl Default for FrameBuffers {
    fn default() -> Self {
        Self::new(SMALL_REQUEST_BYTES, LARGE_REQUEST_BYTES)
    }
}
impl FrameBuffers {
    pub(super) fn new(small_limit: usize, large_limit: usize) -> Self {
        Self {
            small: Mutex::new(0),
            large: Mutex::new(0),
            small_limit,
            large_limit,
        }
    }
    pub(super) fn reserve(&self, payload: usize) -> Result<BufferReservation<'_>> {
        let (used, limit) = if payload <= SMALL_REQUEST {
            (&self.small, self.small_limit)
        } else {
            (&self.large, self.large_limit)
        };
        let bytes = payload;
        let mut current = used.lock().unwrap();
        anyhow::ensure!(bytes <= limit - *current, "frame buffer capacity busy");
        *current += bytes;
        Ok(BufferReservation { used, limit, bytes })
    }
}
impl BufferReservation<'_> {
    pub(super) fn decode(&mut self, bytes: &[u8]) -> Result<Request> {
        wire::decode_request_charged(bytes, &mut |owned| {
            // Charge before each decoder reserve/copy. The lock covers only an
            // integer update, never parsing, allocation, network I/O or dispatch.
            let mut current = self.used.lock().unwrap();
            anyhow::ensure!(
                owned <= self.limit - *current,
                "request buffer capacity busy; request not dispatched"
            );
            *current += owned;
            self.bytes += owned;
            Ok(())
        })
        .context("decoding within request buffer capacity; request not dispatched")
    }
    fn release_raw(&mut self, bytes: usize) {
        *self.used.lock().unwrap() -= bytes;
        self.bytes -= bytes;
    }
}
impl Drop for BufferReservation<'_> {
    fn drop(&mut self) {
        *self.used.lock().unwrap() -= self.bytes;
    }
}

// Only short decode stages or bounded response encoding/writing enter these
// gates. Never retain a stage across registry dispatch, startup or observation.
pub(super) struct WorkStages {
    state: Mutex<WorkState>,
    changed: Condvar,
}
struct WorkState {
    available: usize,
    next: usize,
    serving: usize,
}
pub(super) struct WorkStage<'a>(&'a WorkStages);
impl WorkStages {
    pub(super) fn new(count: usize) -> Self {
        Self {
            state: Mutex::new(WorkState {
                available: count,
                next: 0,
                serving: 0,
            }),
            changed: Condvar::new(),
        }
    }
    pub(super) fn enter(&self) -> WorkStage<'_> {
        let mut state = self.state.lock().unwrap();
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        while state.available == 0 || state.serving != ticket {
            state = self.changed.wait(state).unwrap();
        }
        state.available -= 1;
        state.serving = state.serving.wrapping_add(1);
        self.changed.notify_all();
        WorkStage(self)
    }
}
impl Drop for WorkStage<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().available += 1;
        self.0.changed.notify_all();
    }
}

pub(super) fn read_request<'a>(
    stream: &mut UnixStream,
    buffers: &'a FrameBuffers,
    decode_work: &WorkStages,
) -> Result<(Request, BufferReservation<'a>)> {
    // No worker gate is held while the peer supplies its header or body.
    let mut header = [0; wire::HEADER_LEN];
    stream.read_exact(&mut header)?;
    let length = wire::frame_payload_len(&header, Kind::Request, MAX_REQUEST)?;
    let mut reservation = buffers
        .reserve(length)
        .context("reserving request buffer; request not dispatched")?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .context("allocating request buffer; request not dispatched")?;
    bytes.resize(length, 0);
    stream.read_exact(&mut bytes)?;
    let work = decode_work.enter();
    let request = reservation.decode(&bytes)?;
    drop(bytes);
    reservation.release_raw(length);
    drop(work);
    Ok((request, reservation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_request_decoding_survives_large_body_pressure_and_releases_its_actual_charge() {
        let buffers = FrameBuffers::new(1024, MAX_REQUEST);
        let _large_body = buffers.reserve(MAX_REQUEST).unwrap();
        let work = WorkStages::new(1);
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let mut codec = Codec::new();
        let bytes = codec
            .encode_request(&Request::new(Operation::SessionInfo))
            .unwrap();
        wire::write_frame(&mut sender, Kind::Request, bytes).unwrap();
        let (request, retained) = read_request(&mut receiver, &buffers, &work).unwrap();
        assert!(matches!(request.operation, Operation::SessionInfo));
        let mut expected = 0;
        wire::decode_request_charged(bytes, &mut |charge| {
            expected += charge;
            Ok(())
        })
        .unwrap();
        assert_eq!(*buffers.small.lock().unwrap(), expected);
        drop(request);
        drop(retained);
        assert_eq!(*buffers.small.lock().unwrap(), 0);

        let exhausted = FrameBuffers::new(bytes.len(), MAX_REQUEST);
        wire::write_frame(&mut sender, Kind::Request, bytes).unwrap();
        let result =
            read_request(&mut receiver, &exhausted, &work).map(|_| Payload::Control(json!({})));
        let reply = response(result, false);
        assert!(!reply.ok);
        assert!(reply.failure.is_none());
        assert!(reply.error.unwrap().contains("request not dispatched"));
        assert_eq!(*exhausted.small.lock().unwrap(), 0);
    }

    #[test]
    fn slow_scene_writer_does_not_retain_an_encoder_or_small_response_capacity() {
        let work = Arc::new(WorkStages::new(1));
        let buffers = Arc::new(FrameBuffers::new(1024, 2 * 1024 * 1024));
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let send_bytes = 4096_i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    writer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&send_bytes as *const i32).cast(),
                    std::mem::size_of::<i32>() as libc::socklen_t,
                )
            },
            0
        );
        reader
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let encoding = work.clone();
        let output = buffers.clone();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (completed, completion) = std::sync::mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let stage = encoding.enter();
            let _ = entered.send(());
            let result =
                write_buffered_response(&mut writer, &vec![7; 1024 * 1024], &output, stage);
            let _ = completed.send(result);
        });
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let stage = work.enter();
        let (mut control_writer, mut control_reader) = UnixStream::pair().unwrap();
        control_reader
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut codec = Codec::new();
        let reply = response(
            Ok(Payload::Control(json!({"control": "still available"}))),
            true,
        );
        write_buffered_response(
            &mut control_writer,
            codec.encode_response(&reply).unwrap(),
            &buffers,
            stage,
        )
        .unwrap();
        let bytes = wire::read_frame(&mut control_reader, Kind::Response, MAX_RESPONSE).unwrap();
        let control = wire::decode_response(&bytes).unwrap();
        assert_eq!(
            control.result.unwrap().into_json().unwrap()["control"],
            "still available"
        );
        assert!(matches!(
            completion.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        let bytes = wire::read_frame(&mut reader, Kind::Response, MAX_RESPONSE).unwrap();
        assert_eq!(bytes, vec![7; 1024 * 1024]);
        completion
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }
}
