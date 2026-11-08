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
        "request" => {
            let request: Request = serde_json::from_str(payload()?)?;
            session::validate_target(&paths)?;
            let response = bkpr_client::transport::request(&paths.socket, &request)?;
            println!("{}", serde_json::to_string(&response)?);
            Ok(())
        }
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
            session::validate_target(&paths)?;
            let result = bkpr_client::transport::checked(&paths.socket, Operation::Shutdown)?;
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
