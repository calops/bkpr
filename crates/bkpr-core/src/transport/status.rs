use super::*;
use crate::harness::{NativeStatusStream, Peer};
use std::time::Instant;

const MAX_STATUS_REQUEST: usize = 4096;
const FRAME_DEADLINE: Duration = Duration::from_secs(3);

/// Shared transport budgets used by one retained native-status connection.
pub(super) struct Resources<'a> {
    /// Reusable request-buffer admission.
    pub request_buffers: &'a FrameBuffers,
    /// Bounded request-decoding work.
    pub decode_work: &'a WorkStages,
    /// Bounded response-encoding work.
    pub response_work: &'a WorkStages,
    /// Reusable response-buffer admission.
    pub response_buffers: &'a FrameBuffers,
}

/// Serve ordered status reports until enrollment revocation, disconnect or shutdown.
pub(super) fn serve(
    socket: &mut UnixStream,
    registry: &Registry,
    enrollment: &NativeStatusStream,
    peer: &Peer,
    shutdown: &AtomicBool,
    resources: Resources<'_>,
) -> Result<()> {
    let mut codec = Codec::new();
    let mut bytes = [0; MAX_STATUS_REQUEST];
    loop {
        wait_readable(socket, enrollment, shutdown, None)?;
        let deadline = Instant::now() + FRAME_DEADLINE;
        let mut dispatched = false;
        let result = (|| -> Result<Payload> {
            let mut header = [0; wire::HEADER_LEN];
            read_exact(socket, &mut header, enrollment, shutdown, deadline)?;
            let length = wire::frame_payload_len(&header, Kind::Request, MAX_STATUS_REQUEST)?;
            let mut reservation = resources.request_buffers.reserve(length)?;
            read_exact(socket, &mut bytes[..length], enrollment, shutdown, deadline)?;
            let decode = resources.decode_work.enter();
            let request = reservation.decode(&bytes[..length])?;
            drop(decode);
            dispatched = true;
            registry
                .report_native_status(enrollment, request, peer, &|| {
                    !shutdown.load(Ordering::Acquire) && peer_connected(socket)
                })
                .map(Payload::Control)
        })();
        let accepted = result.is_ok();
        let reply = response(result, dispatched);
        let stage = resources.response_work.enter();
        write_buffered_response(
            socket,
            codec.encode_response(&reply)?,
            resources.response_buffers,
            stage,
        )?;
        if !accepted {
            return Ok(());
        }
    }
}

fn wait_readable(
    socket: &UnixStream,
    enrollment: &NativeStatusStream,
    shutdown: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<()> {
    loop {
        enrollment.ensure_active()?;
        anyhow::ensure!(
            !shutdown.load(Ordering::Acquire),
            "native status daemon is closing"
        );
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        anyhow::ensure!(
            !remaining.is_some_and(|remaining| remaining.is_zero()),
            "native status frame timed out"
        );
        let timeout = remaining
            .unwrap_or(Duration::from_millis(200))
            .min(Duration::from_millis(200));
        let mut descriptor = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout.as_millis().max(1) as i32) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if descriptor.revents & libc::POLLIN != 0 {
            return Ok(());
        }
        anyhow::ensure!(descriptor.revents == 0, "native status socket disconnected");
    }
}

fn read_exact(
    socket: &UnixStream,
    mut bytes: &mut [u8],
    enrollment: &NativeStatusStream,
    shutdown: &AtomicBool,
    deadline: Instant,
) -> Result<()> {
    while !bytes.is_empty() {
        wait_readable(socket, enrollment, shutdown, Some(deadline))?;
        let read = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if read > 0 {
            bytes = &mut bytes[read as usize..];
        } else if read == 0 {
            bail!("native status socket disconnected");
        } else {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                return Err(error.into());
            }
        }
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use bkpr_protocol::{CaptureGrant, ConversationSource, HarnessKind, NativeConversation};

    fn enrollment() -> (NativeStatusStream, Peer) {
        let process = crate::harness_process::process(std::process::id() as i32)
            .unwrap()
            .unwrap();
        let origin = crate::ControlLease::default();
        origin.bind(&process, &process).unwrap();
        let launch = crate::ControlLease::default();
        launch.bind(&process, &process).unwrap();
        (
            NativeStatusStream {
                grant: CaptureGrant {
                    launch_id: "fixture".into(),
                    token: "private".into(),
                    pane_id: "fixture-pane".into(),
                    kind: HarnessKind::Opencode,
                },
                binding: NativeConversation {
                    id: "fixture-session".into(),
                    source: ConversationSource::OpencodePlugin,
                    generation: 1,
                },
                origin,
                launch,
            },
            Peer {
                pid: process.pid,
                uid: unsafe { libc::geteuid() },
                birth: process.identity,
            },
        )
    }

    #[test]
    fn oversized_status_frames_are_rejected_before_decoding_or_acknowledgement() {
        let root = tempfile::tempdir().unwrap();
        let registry = Registry::open(&root.path().join("state")).unwrap();
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let (enrollment, peer) = enrollment();
        let worker = thread::spawn(move || {
            let buffers = FrameBuffers::default();
            let work = WorkStages::new(1);
            serve(
                &mut receiver,
                &registry,
                &enrollment,
                &peer,
                &AtomicBool::new(false),
                Resources {
                    request_buffers: &buffers,
                    decode_work: &work,
                    response_work: &work,
                    response_buffers: &buffers,
                },
            )
            .unwrap();
        });
        wire::write_frame(&mut sender, Kind::Request, &vec![0; MAX_STATUS_REQUEST + 1]).unwrap();
        let bytes = wire::read_frame(&mut sender, Kind::Response, 4096).unwrap();
        let response = wire::decode_response(&bytes).unwrap();
        assert!(!response.ok && response.result.is_none());
        worker.join().unwrap();
    }

    #[test]
    fn idle_stream_cancellation_and_replacement_do_not_require_another_frame() {
        let (_sender, receiver) = UnixStream::pair().unwrap();
        let (enrollment, _) = enrollment();
        assert!(wait_readable(&receiver, &enrollment, &AtomicBool::new(true), None).is_err());
        enrollment.origin.revoke();
        assert!(wait_readable(&receiver, &enrollment, &AtomicBool::new(false), None).is_err());
    }
}
