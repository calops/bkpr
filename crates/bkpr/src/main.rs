mod api;
mod cli;
mod context;
mod harness;
mod help;
mod recovery;
mod remote;
mod session;

use anyhow::{Context, Result, bail};
use bkpr_client::{Options, SessionTarget, SpawnDefaults};
use bkpr_protocol::{Operation, Request};
use std::{io::IsTerminal, path::PathBuf};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let program = args.first().context("missing executable name")?;
    // Native harness wrappers must see their untouched argv before normal CLI parsing.
    if let Some(code) = harness::dispatch(program, args[1..].to_vec())? {
        harness::terminate(code);
    }
    let matches = match cli::parse(args) {
        Ok(matches) => matches,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<clap::Error>() {
                error.exit();
            }
            return Err(error);
        }
    };
    let command = matches.subcommand_name().unwrap_or("tui");
    if command == "__remote-helper" {
        return remote::serve_helper();
    }
    if command == "remote" {
        return remote::execute(&matches);
    }
    if command == "api" {
        return api::run(&matches);
    }
    if command == "tui" {
        return run_tui(&matches);
    }
    if command == "keys" {
        print!("{}", help::KEYS);
        return Ok(());
    }
    if let Some(arguments) = matches.subcommand_matches("completions") {
        let shell = *arguments
            .get_one::<clap_complete::Shell>("shell")
            .context("missing shell")?;
        clap_complete::generate(
            shell,
            &mut cli::command(),
            "bkpr",
            &mut std::io::stdout().lock(),
        );
        return Ok(());
    }
    let roots = session::Roots::inherited()?;
    if command == "list" {
        println!("{}", serde_json::to_string(&session::list(&roots)?)?);
        return Ok(());
    }
    if command == "daemon" {
        let mut host = roots.host()?;
        if let Some(socket) = matches.get_one::<PathBuf>("socket") {
            host.socket = cli::absolute(socket.clone())?;
        }
        let stop = matches
            .subcommand_matches("daemon")
            .context("missing daemon command")?
            .subcommand_name()
            == Some("stop");
        println!(
            "{}",
            serde_json::to_string(&session::administer(&host, stop)?)?
        );
        return Ok(());
    }
    if command == "request" {
        return run_request(&matches, &roots);
    }
    let paths = session::LocalPaths::resolve(
        &roots,
        matches.get_one::<String>("session").map(String::as_str),
        matches.get_one::<PathBuf>("socket").cloned(),
        matches.get_one::<PathBuf>("state-root").cloned(),
        std::env::var_os("BKPR_SOCKET").map(PathBuf::from),
        matches!(command, "context" | "request" | "keyboard" | "resume"),
        false,
    )?;
    let payload = || {
        matches
            .subcommand()
            .and_then(|(_, args)| args.get_one::<String>("payload"))
            .context("command needs a JSON payload")
    };
    match command {
        "context" => {
            session::validate_target(&paths)?;
            context::execute(
                &paths.socket,
                matches.get_one::<String>("pane").cloned(),
                matches
                    .subcommand_matches("context")
                    .context("missing context command arguments")?
                    .get_flag("json"),
            )
        }
        "stop" => {
            let result = session::stop(&paths)?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        "resume" => {
            let result = recovery::run(
                &paths,
                matches.get_one::<String>("pane").map(String::as_str),
            )?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        "keyboard" => {
            let bytes: Vec<u8> = serde_json::from_str(payload()?)?;
            session::validate_target(&paths)?;
            bkpr_client::keyboard(
                &paths.socket,
                matches
                    .get_one::<String>("pane")
                    .context("--pane is required")?,
                matches
                    .get_one::<String>("view")
                    .context("--view is required")?,
                &bytes,
            )?;
            println!(
                "{{\"version\":{},\"ok\":true,\"semantics\":\"queued_once_no_replay\"}}",
                bkpr_protocol::PROTOCOL_VERSION
            );
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn run_request(matches: &clap::ArgMatches, roots: &session::Roots) -> Result<()> {
    let payload = matches
        .subcommand_matches("request")
        .and_then(|arguments| arguments.get_one::<String>("payload"))
        .context("request needs a JSON payload")?;
    let request: Request = serde_json::from_str(payload)?;
    let host_only = matches!(
        &request.operation,
        Operation::DaemonInfo
            | Operation::SessionOpen { .. }
            | Operation::DaemonShutdown { .. }
            | Operation::Notifications {
                request: bkpr_protocol::NotificationRequest::PolicySet { .. }
            }
    );
    let explicit_notifications = matches!(&request.operation, Operation::Notifications { .. })
        && matches.contains_id("socket");
    let response = if host_only || explicit_notifications {
        let socket = matches.get_one::<PathBuf>("socket").context(
            "host requests require an explicit --socket; inherited BKPR_SOCKET is session-scoped",
        )?;
        anyhow::ensure!(
            !matches.contains_id("session") && !matches.contains_id("state-root"),
            "explicit endpoint requests select --socket only"
        );
        let socket = cli::absolute(socket.clone())?;
        if matches!(&request.operation, Operation::Notifications { .. }) {
            session::request_notifications(&socket, &request)?
        } else {
            session::request_host(&socket, &request)?
        }
    } else {
        let paths = session::LocalPaths::resolve(
            roots,
            matches.get_one::<String>("session").map(String::as_str),
            matches.get_one::<PathBuf>("socket").cloned(),
            matches.get_one::<PathBuf>("state-root").cloned(),
            std::env::var_os("BKPR_SOCKET").map(PathBuf::from),
            true,
            false,
        )?;
        session::validate_target(&paths)?;
        bkpr_client::transport::request(&paths.socket, &request)?
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn run_tui(matches: &clap::ArgMatches) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("TUI needs a terminal; use context, request or remote inspect for headless access");
    }
    let mut options = Options {
        read_only: matches.get_flag("read-only"),
        detach_others: matches.get_flag("detach-others"),
        ..Options::default()
    };
    if let Some(policy) = matches.get_one::<String>("clipboard") {
        options.clipboard_policy.clone_from(policy);
    }
    let specifications = cli::tui_targets(matches)?;
    let has_local = specifications
        .iter()
        .any(|target| matches!(target, cli::TuiTarget::Local(_)));
    // A remote-only launch does not require or inspect local HOME, CWD or roots.
    let local = if has_local {
        Some((session::Roots::inherited()?, local_defaults(matches)?))
    } else {
        None
    };
    let binary = matches
        .get_one::<PathBuf>("bkprd")
        .cloned()
        .map(cli::absolute)
        .transpose()?;
    let mut targets = Vec::with_capacity(specifications.len());
    for specification in specifications {
        match specification {
            cli::TuiTarget::Remote { host, name } => {
                targets.push(SessionTarget::Remote(remote::tui_target(
                    matches, host, name,
                )));
            }
            cli::TuiTarget::Local(name) => {
                let (roots, defaults) = local.as_ref().expect("local defaults resolved");
                let paths = session::LocalPaths::resolve(
                    roots,
                    (!matches.contains_id("socket")).then_some(name.as_str()),
                    matches.get_one::<PathBuf>("socket").cloned(),
                    matches.get_one::<PathBuf>("state-root").cloned(),
                    None,
                    false,
                    true,
                )?;
                let spawn_defaults = defaults.clone();
                let bootstrap_defaults = defaults.clone();
                let binary = binary.clone();
                targets.push(SessionTarget::Local {
                    name: paths.name.clone(),
                    socket: paths.socket.clone(),
                    spawn_defaults,
                    prepare: Some(Box::new(move |size, cancellation| {
                        session::ensure_server(
                            &paths,
                            binary.as_deref(),
                            &bootstrap_defaults,
                            size,
                            &cancellation,
                        )
                    })),
                });
            }
        }
    }
    bkpr_client::run_sessions(targets, options)
}

fn local_defaults(matches: &clap::ArgMatches) -> Result<SpawnDefaults> {
    Ok(SpawnDefaults {
        argv: cli::shell_arguments(matches)?.unwrap_or_else(|| {
            vec![
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                "-i".into(),
            ]
        }),
        cwd: matches
            .get_one::<PathBuf>("cwd")
            .cloned()
            .map(cli::absolute)
            .transpose()?
            .unwrap_or(std::env::current_dir()?),
        home: match matches.get_one::<PathBuf>("home") {
            Some(home) => cli::absolute(home.clone())?,
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is required")?,
        },
        isolated_environment: matches.contains_id("home"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixListener},
    };
    use std::time::Duration;

    #[test]
    fn raw_notification_request_uses_the_explicit_endpoint_without_selecting_a_session() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = directory.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let mut ready = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut ready, 1, 3000) }, 1);
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let bytes = bkpr_protocol::wire::read_frame(
                &mut stream,
                bkpr_protocol::wire::Kind::Request,
                bkpr_protocol::wire::MAX_REQUEST,
            )
            .unwrap();
            assert!(matches!(
                bkpr_protocol::wire::decode_request(&bytes)
                    .unwrap()
                    .operation,
                Operation::Notifications {
                    request: bkpr_protocol::NotificationRequest::Health {}
                }
            ));
            let response = bkpr_protocol::Response {
                version: bkpr_protocol::PROTOCOL_VERSION,
                ok: true,
                result: Some(serde_json::json!({"healthy":true}).into()),
                error: None,
                failure: None,
            };
            let mut codec = bkpr_protocol::wire::Codec::new();
            let bytes = codec.encode_response(&response).unwrap();
            bkpr_protocol::wire::write_frame(
                &mut stream,
                bkpr_protocol::wire::Kind::Response,
                bytes,
            )
            .unwrap();
        });
        let payload = serde_json::to_string(&Request::new(Operation::Notifications {
            request: bkpr_protocol::NotificationRequest::Health {},
        }))
        .unwrap();
        let matches = cli::parse(vec![
            "bkpr".into(),
            "--socket".into(),
            socket.into_os_string(),
            "request".into(),
            payload.into(),
        ])
        .unwrap();
        run_request(&matches, &session::Roots::inherited().unwrap()).unwrap();
        server.join().unwrap();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
