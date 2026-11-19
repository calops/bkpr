use super::*;
use crate::image_resources::{self, Observation, PreparationPool, StreamPermit};
use bkpr_protocol::{
    ClientAuthority, IMAGE_CHUNK_BYTES, IMAGE_RESOURCE_MAX_FRAME, ImageResource, PaneRequest,
};
use std::time::Instant;

// Explicit safety bounds, independent of measured compression/transfer latency.
const PREPARE_TOTAL: Duration = Duration::from_secs(30);
const TRANSFER_TOTAL: Duration = Duration::from_secs(120);
const TRANSFER_STALL: Duration = Duration::from_secs(5);

/// Authorized observation and bounded stream admission retained through completion or cancellation.
pub(super) struct Admitted {
    /// Validated resource shape for the initial Preparing response.
    pub(super) observation: Observation,
    pool: Arc<PreparationPool>,
    _permit: StreamPermit,
    started: Instant,
    pane_id: String,
}
/// Authenticate the native client and exact pane/view/image, then reserve bounded FIFO admission.
pub(super) fn admit(
    registry: &Registry,
    authority: &ClientAuthority,
    operation: Operation,
    stream: &UnixStream,
) -> Result<Admitted> {
    let client = registry.client_lease(authority, &operation, stream)?;
    let Operation::Pane {
        pane_id,
        request: PaneRequest::ImageResource { view_id, key },
    } = operation
    else {
        bail!("not an image resource request");
    };
    let observation = registry.image_observation(client.clone(), &pane_id, view_id, key)?;
    let pool = image_resources::shared_pool()?;
    let permit = pool.admit_stream(&client.id)?;
    Ok(Admitted {
        observation,
        pool,
        _permit: permit,
        started: Instant::now(),
        pane_id,
    })
}

/// Stream bounded native resource frames with progress, total-lifetime and authority cancellation checks.
pub(super) fn serve(stream: &mut UnixStream, admitted: Admitted, shutdown: &AtomicBool) {
    let stream = &*stream;
    let active = || {
        !shutdown.load(Ordering::Acquire)
            && peer_connected(stream)
            && admitted.observation.validate().is_ok()
    };
    let mut writer = ResponseWriter {
        stream,
        deadline: admitted.started + TRANSFER_TOTAL,
        progress: Instant::now(),
        stall: Some(TRANSFER_STALL),
        active: Some(&active),
        sent: 0,
    };
    let mut codec = Codec::new();
    let result = transfer(&admitted, &mut writer, &mut codec, &active);
    if let Err(error) = result
        && !error.is::<WriteFailed>()
    {
        let timed_out = error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut);
        crate::diagnostics::response_failure(
            Some(&admitted.pane_id),
            crate::diagnostics::ResponseFailure {
                stage: "image.transfer",
                expected_payload: 0,
                expected_framed: 0,
                sent: writer.sent as u64,
                elapsed_ms: admitted.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                kind: if timed_out {
                    "timeout"
                } else if active() {
                    "resource"
                } else {
                    "cancelled"
                },
            },
        );
        if active() {
            let reply = response(Err(error), false);
            if let Ok(bytes) = codec.encode_response(&reply) {
                let _ = wire::write_frame(&mut writer, Kind::Response, bytes);
            }
        }
    }
}
fn transfer(
    admitted: &Admitted,
    writer: &mut ResponseWriter<'_>,
    codec: &mut Codec,
    active: &dyn Fn() -> bool,
) -> Result<()> {
    let mut heartbeat = Instant::now() + Duration::from_secs(1);
    while !admitted._permit.activate(Duration::from_millis(100)) {
        check_wait(active, writer.deadline)?;
        if Instant::now() >= heartbeat {
            send(
                admitted,
                writer,
                codec,
                ImageResource::Preparing {
                    descriptor: admitted.observation.descriptor,
                },
                "image.queued",
            )?;
            heartbeat = Instant::now() + Duration::from_secs(1);
        }
    }
    let preparation = admitted.pool.request(&admitted.observation)?;
    let preparation_deadline = (Instant::now() + PREPARE_TOTAL).min(writer.deadline);
    heartbeat = Instant::now() + Duration::from_secs(1);
    let encoded = loop {
        check_wait(active, preparation_deadline)?;
        if let Some(result) = preparation.wait(Duration::from_millis(100)) {
            break result.map_err(anyhow::Error::msg)?;
        }
        if Instant::now() >= heartbeat {
            send(
                admitted,
                writer,
                codec,
                ImageResource::Preparing {
                    descriptor: admitted.observation.descriptor,
                },
                "image.preparing",
            )?;
            heartbeat = Instant::now() + Duration::from_secs(1);
        }
    };
    admitted.observation.validate()?;
    let descriptor = admitted.observation.descriptor;
    let encoded_len = encoded.bytes.len() as u64;
    send(
        admitted,
        writer,
        codec,
        ImageResource::Start {
            descriptor,
            encoding: encoded.encoding,
            encoded_len,
        },
        "image.start",
    )?;
    for offset in (0..encoded.bytes.len()).step_by(IMAGE_CHUNK_BYTES) {
        admitted.observation.validate()?;
        let end = offset
            .saturating_add(IMAGE_CHUNK_BYTES)
            .min(encoded.bytes.len());
        send(
            admitted,
            writer,
            codec,
            ImageResource::Data {
                key: descriptor.key,
                offset: offset as u64,
                data: encoded.bytes.slice(offset..end),
            },
            "image.data",
        )?;
    }
    admitted.observation.validate()?;
    send(
        admitted,
        writer,
        codec,
        ImageResource::End {
            key: descriptor.key,
            encoded_len,
        },
        "image.end",
    )
}
fn send(
    admitted: &Admitted,
    writer: &mut ResponseWriter<'_>,
    codec: &mut Codec,
    resource: ImageResource,
    stage: &'static str,
) -> Result<()> {
    let reply = response(Ok(Payload::ImageResource(resource)), false);
    let bytes = codec.encode_response(&reply)?;
    anyhow::ensure!(
        bytes.len() <= IMAGE_RESOURCE_MAX_FRAME,
        "image response frame limit"
    );
    let before = writer.sent;
    let result = wire::write_frame(writer, Kind::Response, bytes);
    if let Err(error) = &result {
        writer.failure(
            Some(&admitted.pane_id),
            stage,
            bytes.len(),
            before,
            admitted.started,
            error,
        );
    }
    result.context(WriteFailed)
}

fn check_wait(active: &dyn Fn() -> bool, deadline: Instant) -> Result<()> {
    if !active() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "image observation cancelled or retired",
        )
        .into());
    }
    if Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "image preparation or admission deadline exceeded",
        )
        .into());
    }
    Ok(())
}
#[derive(Debug)]
struct WriteFailed;
impl std::fmt::Display for WriteFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("image response write failed")
    }
}
impl std::error::Error for WriteFailed {}

#[cfg(test)]
mod tests;
