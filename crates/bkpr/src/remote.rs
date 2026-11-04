//! Headless remote attachment boundary. Session discovery stays on the remote host.
use anyhow::{Context, Result, ensure};
use bkpr_client::remote::{RemoteSession, RemoteTarget, ResolvedSession};
use bkpr_protocol::{Operation, SessionInfo, remote_wire::Hello};
use clap::ArgMatches;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::{FileTypeExt, MetadataExt},
    },
    path::PathBuf,
};

// JSON exists only at this CLI boundary; the SSH transport carries bounded binary frames.
const MAX_OPERATION_BYTES: usize = 8 * 1024 * 1024;
const INPUT_CHUNK_BYTES: usize = 16 * 1024;

pub fn serve_helper() -> Result<()> {
    bkpr_client::remote::serve_helper(|hello| {
        let roots = crate::session::Roots::inherited()?;
        resolve_existing(&roots, hello)
    })
}

fn resolve_existing(roots: &crate::session::Roots, hello: &Hello) -> Result<ResolvedSession> {
    let paths = roots.named(&hello.session_name)?;
    resolve_paths(paths, hello)
}

fn resolve_paths(paths: crate::session::LocalPaths, hello: &Hello) -> Result<ResolvedSession> {
    // This is deliberately the existing-only path, including private-root, manifest,
    // exact-name and logical-session checks. Never call ensure_server here.
    let info = crate::session::validate_target(&paths)?;
    ensure!(
        info.name == hello.session_name
            && !info.session_id.is_empty()
            && !info.daemon_id.is_empty(),
        "remote session did not provide the selected exact identity"
    );
    let agent_socket = if hello.forward_agent {
        let socket = std::env::var_os("SSH_AUTH_SOCK")
            .map(PathBuf::from)
            .context("agent forwarding requested, but the SSH server provided no SSH_AUTH_SOCK")?;
        validate_agent_socket(&socket)?;
        Some(socket)
    } else {
        None
    };
    Ok(ResolvedSession {
        socket: paths.socket,
        info,
        agent_socket,
    })
}

fn validate_agent_socket(socket: &std::path::Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(socket)
        .context("checking the remote forwarded SSH agent socket")?;
    ensure!(
        socket.is_absolute()
            && metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() },
        "the remote SSH_AUTH_SOCK must be an absolute owned Unix socket, not a symlink"
    );
    Ok(())
}

pub fn execute(matches: &ArgMatches) -> Result<()> {
    let remote = matches
        .subcommand_matches("remote")
        .context("missing remote command")?;
    let command = remote
        .subcommand_name()
        .context("missing remote operation")?;
    // Reject invalid one-shot input before establishing even a managed attachment.
    let operation = if command == "request" {
        Some(parse_operation(
            remote
                .subcommand_matches("request")
                .and_then(|args| args.get_one::<String>("payload"))
                .context("request requires an Operation JSON payload")?
                .as_bytes(),
        )?)
    } else {
        None
    };
    let target = RemoteTarget {
        destination: remote
            .get_one::<String>("ssh")
            .context("remote requires --ssh HOST")?
            .clone(),
        session_name: matches
            .get_one::<String>("session")
            .context("remote requires --session NAME")?
            .clone(),
        helper: remote.get_one::<PathBuf>("helper").cloned(),
        ssh_config: remote.get_one::<PathBuf>("ssh-config").cloned(),
        forward_agent: remote.get_flag("forward-agent"),
        detach_others: matches.get_flag("detach-others"),
    };
    if command == "inspect" {
        let session = RemoteSession::inspect(target)?;
        return emit(&mut std::io::stdout().lock(), &json!({"session":session}));
    }
    let mut session = RemoteSession::connect(target)?;
    let endpoint = session.endpoint();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let result = match command {
        "request" => {
            let result = endpoint.checked(operation.context("missing parsed operation")?);
            emit_result(&mut output, result)
        }
        "connect" => {
            let info = session.info().clone();
            let stdin = std::io::stdin();
            run_console(
                &mut stdin.lock(),
                &mut output,
                &info,
                session.lifetime_fd(),
                || session.poll_lifecycle(),
                |operation| endpoint.checked(operation),
            )
        }
        _ => unreachable!("clap restricts remote operations"),
    };
    // Retire only this attachment and owned SSH helper, including on output/input errors.
    // Never issue native Shutdown as part of disconnect.
    let disconnected = session.disconnect();
    result.and(disconnected)
}

fn parse_operation(bytes: &[u8]) -> Result<Operation> {
    ensure!(
        bytes.len() <= MAX_OPERATION_BYTES,
        "Operation JSON exceeds the 8 MiB limit"
    );
    let operation: Operation =
        serde_json::from_slice(bytes).context("decoding a typed Operation")?;
    ensure!(
        !matches!(
            operation,
            Operation::ClientRegister { .. }
                | Operation::Client { .. }
                | Operation::ClientActivate { .. }
                | Operation::ClientDetach
                | Operation::HarnessBegin { .. }
                | Operation::HarnessEvent { .. }
                | Operation::HarnessContext { .. }
                | Operation::Agent { .. }
        ),
        "attachment and harness authority cannot be supplied through the operation console"
    );
    Ok(operation)
}

fn emit(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn error_value(error: &anyhow::Error) -> Value {
    json!({
        "ok":false,
        "error":format!("{error:#}"),
        "outcome_unknown":bkpr_client::transport::outcome_unknown(error),
        "replayed":false,
    })
}

fn emit_result(output: &mut impl Write, result: Result<Value>) -> Result<()> {
    match result {
        Ok(value) => emit(output, &json!({"ok":true, "result":value})),
        Err(error) => {
            emit(output, &error_value(&error))?;
            Err(error)
        }
    }
}

fn dispatch_line(
    output: &mut impl Write,
    line: &[u8],
    dispatch: &mut impl FnMut(Operation) -> Result<Value>,
) -> Result<()> {
    let result = parse_operation(line).and_then(dispatch);
    // One operation failure does not erase the attachment or hide subsequent results.
    // Unknown outcomes remain unknown; no retry path exists here.
    let value = match result {
        Ok(value) => json!({"ok":true, "result":value}),
        Err(error) => error_value(&error),
    };
    emit(output, &value)
}

fn run_console(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    info: &SessionInfo,
    lifetime_fd: RawFd,
    mut detached: impl FnMut() -> Result<bool>,
    mut dispatch: impl FnMut(Operation) -> Result<Value>,
) -> Result<()> {
    emit(output, &json!({"event":"connected", "session":info}))?;
    let mut line = Vec::new();
    let mut chunk = [0_u8; INPUT_CHUNK_BYTES];
    loop {
        if detached()? {
            emit(output, &json!({"event":"detached"}))?;
            return Ok(());
        }
        let mut descriptors = [
            libc::pollfd {
                fd: input.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: lifetime_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("waiting for remote console input");
        }
        ensure!(
            descriptors
                .iter()
                .all(|descriptor| descriptor.revents & libc::POLLNVAL == 0),
            "remote console input or lifetime descriptor is invalid"
        );
        if descriptors[1].revents != 0 {
            // Consume revocation before any simultaneously readable user input.
            continue;
        }
        let count = match input.read(&mut chunk) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading remote console input"),
        };
        if count == 0 {
            if !line.is_empty() {
                dispatch_line(output, &line, &mut dispatch)?;
            }
            return Ok(());
        }
        for bytes in chunk[..count].split_inclusive(|byte| *byte == b'\n') {
            let terminated = bytes.last() == Some(&b'\n');
            let bytes = if terminated {
                &bytes[..bytes.len() - 1]
            } else {
                bytes
            };
            if bytes.len() > MAX_OPERATION_BYTES.saturating_sub(line.len()) {
                let error = anyhow::anyhow!("Operation JSON exceeds the 8 MiB limit");
                emit(output, &error_value(&error))?;
                return Err(error);
            }
            line.extend_from_slice(bytes);
            if terminated {
                // A takeover between buffered lines must not deliver stale queued input.
                if detached()? {
                    emit(output, &json!({"event":"detached"}))?;
                    return Ok(());
                }
                dispatch_line(output, &line, &mut dispatch)?;
                line.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::PaneRequest;
    use std::{net::Shutdown, os::unix::net::UnixStream};

    fn info() -> SessionInfo {
        SessionInfo {
            name: "remote / λ".into(),
            session_id: "logical".into(),
            daemon_id: "birth".into(),
        }
    }

    fn console_input(bytes: &[u8]) -> UnixStream {
        let (input, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(bytes).unwrap();
        writer.shutdown(Shutdown::Write).unwrap();
        input
    }

    fn records(output: Vec<u8>) -> Vec<Value> {
        std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn console_keeps_state_and_preserves_raw_native_input_without_replay() {
        let mut input = console_input(concat!(
            "{\"op\":\"inspect\"}\n",
            "not json\n",
            "{\"op\":\"pane\",\"pane_id\":\"p\",\"request\":{\"op\":\"input\",\"view_id\":\"v\",\"data\":[0,255,27,10]}}\n",
            "{\"op\":\"inspect\"}",
        ).as_bytes());
        let mut output = Vec::new();
        let mut calls = 0;
        let mut received = Vec::new();
        run_console(
            &mut input,
            &mut output,
            &info(),
            -1,
            || Ok(false),
            |operation| {
                calls += 1;
                match operation {
                    Operation::Inspect => Ok(json!({"operations":calls})),
                    Operation::Pane {
                        pane_id,
                        request: PaneRequest::Input { view_id, data },
                    } => {
                        assert_eq!((pane_id.as_str(), view_id.as_str()), ("p", "v"));
                        received.extend(data);
                        let (mut stream, peer) = UnixStream::pair().unwrap();
                        drop(peer);
                        bkpr_client::transport::checked_stream(&mut stream, Operation::Shutdown)
                    }
                    _ => panic!("unexpected operation"),
                }
            },
        )
        .unwrap();
        assert_eq!(received, [0, 255, 27, 10]);
        assert_eq!(calls, 3);
        let rows = records(output);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["session"], serde_json::to_value(info()).unwrap());
        assert_eq!(rows[1]["result"]["operations"], 1);
        assert_eq!(rows[2]["ok"], false);
        assert_eq!(rows[2]["outcome_unknown"], false);
        assert_eq!(rows[3]["ok"], false);
        assert_eq!(rows[3]["outcome_unknown"], true);
        assert_eq!(rows[3]["replayed"], false);
        assert_eq!(rows[4]["result"]["operations"], 3);
    }

    #[test]
    fn console_rejects_authority_envelopes_before_dispatch() {
        let mut input = console_input(
            concat!(
                "{\"op\":\"client_register\",\"session_id\":\"s\",\"daemon_id\":\"d\"}\n",
                "{\"op\":\"client_activate\",\"detach_others\":true,\"agent_socket\":null}\n",
                "{\"op\":\"harness_begin\",\"kind\":\"omp\"}\n",
                "{\"op\":\"client_detach\"}\n",
            )
            .as_bytes(),
        );
        let mut output = Vec::new();
        run_console(
            &mut input,
            &mut output,
            &info(),
            -1,
            || Ok(false),
            |_| panic!("authority operation reached native dispatch"),
        )
        .unwrap();
        let rows = records(output);
        assert_eq!(rows.len(), 5);
        assert!(
            rows[1..]
                .iter()
                .all(|row| row["ok"] == false && row["outcome_unknown"] == false)
        );
    }

    #[test]
    fn takeover_discards_already_buffered_following_operations() {
        let mut input = console_input(b"{\"op\":\"inspect\"}\n{\"op\":\"shutdown\"}\n");
        let mut output = Vec::new();
        let calls = std::cell::Cell::new(0);
        run_console(
            &mut input,
            &mut output,
            &info(),
            -1,
            || Ok(calls.get() != 0),
            |_| {
                calls.set(calls.get() + 1);
                Ok(json!({"observed":true}))
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(records(output).last().unwrap()["event"], "detached");
    }

    #[test]
    fn lifecycle_wakes_the_console_while_stdin_remains_open_and_silent() {
        let (mut input, _stdin_writer) = UnixStream::pair().unwrap();
        let (lifetime, mut notifier) = UnixStream::pair().unwrap();
        notifier.write_all(b"x").unwrap();
        let checks = std::cell::Cell::new(0);
        let mut output = Vec::new();
        run_console(
            &mut input,
            &mut output,
            &info(),
            lifetime.as_raw_fd(),
            || {
                let previous = checks.get();
                checks.set(previous + 1);
                Ok(previous != 0)
            },
            |_| panic!("lifecycle revocation must not dispatch an operation"),
        )
        .unwrap();
        let rows = records(output);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["event"], "detached");
    }

    #[test]
    fn eof_does_not_dispatch_or_stop_a_remote_session() {
        let mut input = console_input(b"");
        let mut output = Vec::new();
        run_console(
            &mut input,
            &mut output,
            &info(),
            -1,
            || Ok(false),
            |_| panic!("EOF must not become a native operation"),
        )
        .unwrap();
        assert_eq!(records(output).len(), 1);
    }

    #[test]
    fn absent_named_target_is_read_only_and_never_bootstrapped() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let state = directory.path().join("state");
        let paths = crate::session::LocalPaths {
            socket: runtime.join("hash/s"),
            state_root: state.join("hash"),
            name: Some(info().name.clone()),
        };
        let hello = Hello {
            session_name: info().name,
            native_family: bkpr_protocol::PROTOCOL_VERSION,
            required_capabilities: 0,
            connection_nonce: [1; 32],
            forward_agent: false,
        };
        assert!(resolve_paths(paths, &hello).is_err());
        assert!(!runtime.exists());
        assert!(!state.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn forwarding_requires_an_actual_owned_remote_socket_not_a_file_or_alias() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink, net::UnixListener};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory
            .path()
            .join(std::ffi::OsString::from_vec(b"agent-\xff".to_vec()));
        let _listener = UnixListener::bind(&socket).unwrap();
        validate_agent_socket(&socket).unwrap();
        let file = directory.path().join("file");
        std::fs::write(&file, b"not an agent").unwrap();
        assert!(validate_agent_socket(&file).is_err());
        let alias = directory.path().join("alias");
        symlink(&socket, &alias).unwrap();
        assert!(validate_agent_socket(&alias).is_err());
        assert!(validate_agent_socket(std::path::Path::new("relative-agent")).is_err());
    }

    #[test]
    fn bounded_lines_fail_without_dispatching_an_oversized_operation() {
        assert!(parse_operation(&vec![b' '; MAX_OPERATION_BYTES + 1]).is_err());
        // Decode native Shutdown normally; the managed native dispatcher rejects it.
        assert!(matches!(
            parse_operation(b"{\"op\":\"shutdown\"}").unwrap(),
            Operation::Shutdown
        ));
    }
}
