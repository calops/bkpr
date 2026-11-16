use super::*;
use bkpr_protocol::{administration as admin, wire::administration_legacy as legacy};
use std::os::unix::net::UnixStream;

fn connect(socket: &Path) -> Result<UnixStream> {
    validate_endpoint_socket(socket)?;
    let stream = UnixStream::connect(socket).context("connecting to selected shutdown endpoint")?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}

fn inspect(socket: &Path) -> Result<admin::Response> {
    let mut stream = connect(socket)?;
    admin::write_request(&mut stream, &admin::Request::Inspect {})?;
    admin::read_response(&mut stream)
}

fn stop(mut stream: UnixStream, identity: admin::Identity) -> Result<serde_json::Value> {
    admin::write_request(&mut stream, &admin::Request::Stop { expected: identity })
        .context("shutdown outcome unknown after dispatch; do not replay automatically")?;
    match admin::read_response(&mut stream)
        .context("shutdown outcome unknown after dispatch; do not replay automatically")?
    {
        admin::Response::Stopped {} => Ok(serde_json::json!({"stopped":true})),
        admin::Response::Rejected { error } => bail!("shutdown rejected: {error}"),
        _ => bail!("unexpected shutdown reply; outcome unknown, do not replay automatically"),
    }
}

fn legacy_request(
    stream: &mut UnixStream,
    family: u32,
    operation: legacy::Operation,
) -> Result<serde_json::Value> {
    let frame = legacy::encode_request(family, &operation)?;
    let mutating = matches!(
        operation,
        legacy::Operation::Shutdown | legacy::Operation::DaemonShutdown { .. }
    );
    let exchange = (|| {
        stream.write_all(&frame)?;
        legacy::read_response(stream, family)
    })();
    match exchange {
        Err(error) if mutating && !error.is::<legacy::Rejected>() => {
            Err(error
                .context("shutdown outcome unknown after dispatch; do not replay automatically"))
        }
        result => result,
    }
}

fn required_identity(value: &serde_json::Value, key: &str) -> Result<String> {
    let value = value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("shutdown endpoint omitted {key}"))?;
    ensure!(!value.is_empty(), "shutdown endpoint returned empty {key}");
    Ok(value.to_owned())
}

fn expected_session(paths: &LocalPaths) -> Result<Option<StoredSession>> {
    if let Some(name) = &paths.name {
        ensure!(
            named_directories(paths, false)?,
            "named session directories are absent"
        );
        let stored =
            read_manifest(&paths.state_root)?.context("named session has no identity manifest")?;
        ensure!(
            &stored.name == name,
            "stored session name does not match selected name"
        );
        Ok(Some(stored))
    } else {
        Ok(None)
    }
}

/// Stop one exact named or explicitly selected session, never its host.
pub(super) fn stop_session(paths: &LocalPaths) -> Result<serde_json::Value> {
    let expected = expected_session(paths)?;
    // Connect the mutation socket before observation: a legacy listener replaced
    // between the requests cannot redirect this connection into its successor.
    let mut pinned = connect(&paths.socket)?;
    match inspect(&paths.socket) {
        Ok(admin::Response::Identified { identity }) => {
            let admin::Identity::Session { session_id, .. } = &identity else {
                bail!("selected endpoint is a host; use bkpr daemon stop explicitly");
            };
            if let Some(expected) = expected {
                ensure!(
                    session_id == &expected.session_id,
                    "live session identity does not match selected named session"
                );
            }
            stop(pinned, identity)
        }
        Ok(admin::Response::Rejected { error }) => bail!("shutdown inspection rejected: {error}"),
        Ok(_) => bail!("endpoint did not identify itself before shutdown"),
        Err(error) => {
            let Some(admin::NativeFamily(family)) = error.downcast_ref::<admin::NativeFamily>()
            else {
                return Err(error.context("cannot inspect selected shutdown endpoint"));
            };
            let family = *family;
            let mut observation = connect(&paths.socket)?;
            let result = legacy_request(&mut observation, family, legacy::Operation::SessionInfo)?;
            let info = result
                .get("session")
                .context("selected endpoint is not a session")?;
            let session_id = required_identity(info, "session_id")?;
            required_identity(info, "daemon_id")?;
            if let Some(expected) = expected {
                ensure!(
                    required_identity(info, "name")? == expected.name
                        && session_id == expected.session_id,
                    "live session identity does not match selected named session"
                );
            }
            legacy_request(&mut pinned, family, legacy::Operation::Shutdown)
        }
    }
}

/// Stop the explicitly selected host incarnation and all its sessions.
pub(super) fn stop_host(host: &HostPaths) -> Result<serde_json::Value> {
    let mut pinned = connect(&host.socket)?;
    match inspect(&host.socket) {
        Ok(admin::Response::Identified { identity }) => {
            ensure!(
                matches!(&identity, admin::Identity::Host { .. }),
                "selected endpoint is not a host daemon"
            );
            stop(pinned, identity)
        }
        Ok(admin::Response::Rejected { error }) => bail!("shutdown inspection rejected: {error}"),
        Ok(_) => bail!("endpoint did not identify itself before shutdown"),
        Err(error) => {
            let Some(admin::NativeFamily(family)) = error.downcast_ref::<admin::NativeFamily>()
            else {
                return Err(error.context("cannot inspect selected host shutdown endpoint"));
            };
            let family = *family;
            let mut observation = connect(&host.socket)?;
            let result = legacy_request(&mut observation, family, legacy::Operation::DaemonInfo)?;
            let info = result
                .get("daemon")
                .context("selected endpoint is not a host daemon")?;
            let daemon_id = required_identity(info, "daemon_id")?;
            legacy_request(
                &mut pinned,
                family,
                legacy::Operation::DaemonShutdown { daemon_id },
            )
        }
    }
}
