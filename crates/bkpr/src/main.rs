mod api;
mod cli;
mod context;
mod harness;
mod recovery;
mod remote;
mod session;

use anyhow::{Context, Result, bail};
use bkpr_client::{Options, SessionTarget, SpawnDefaults};
use bkpr_protocol::{Operation, Request};
use std::{io::IsTerminal, path::PathBuf};

const HELP: &str = r#"Bkpr — persistent named pane workspaces

  bkpr [tui] [-s [HOST:]NAME ... | --socket PATH --state-root PATH] [-d]
       [-r DEFAULT_HOST] [--cwd PATH] [--read-only] [--clipboard ask|allow|deny]
       [--bkprd PATH] [--home ISOLATED_FIXTURE_HOME] [-- SHELL ARGS...]
       [--remote-bkpr-bin REMOTE_EXECUTABLE] [--ssh-config PATH] [--forward-agent]
  bkpr list
  bkpr stop [-s NAME | --socket PATH]
  bkpr resume [-s NAME | --socket PATH] [--pane PANE_ID]
  bkpr context [-s NAME | --socket PATH] [--pane PANE_ID] [--json]
  bkpr api help
  bkpr api schema [OPERATION]
  bkpr api call OPERATION [--arguments JSON_OBJECT] [--scope workspace|session|all]
  bkpr request [-s NAME | --socket PATH] 'VERSIONED_TYPED_JSON_REQUEST'
  bkpr keyboard --socket PATH --pane PANE_ID --view VIEW_ID '[PHYSICAL_BYTES]'
  bkpr remote -r HOST [-s NAME] [--ssh-config PATH] [--remote-bkpr-bin REMOTE_EXECUTABLE]
       inspect | request 'OPERATION_JSON' [--forward-agent] | connect [--forward-agent] [-d]
  bkpr run HARNESS [--] ARGS...
  bkpr install agy

Bare bkpr connects or starts the named local session `bkpr`, then opens its TUI.
-s / --session repeats to attach independent local and remote sessions in one TUI.
Use -s foo -s bar -s devbox:baz; local:foo:bar selects the literal local name foo:bar.
Names remain exact (Unicode, spaces, slashes and colons are supported).
-d / --detach-others revokes other clients once this attachment is ready; it does
not terminate their panes. Client exit/detach preserves daemon and pane processes.
`list` discovers stored sessions without starting them; `stop` stops only the
selected session and its programs. A cold named-session restart restores owned
layouts and saved directories, using the current shell or a verified active
native conversation's fixed resume adapter with fresh launch authority.
Old arbitrary commands/settings are not replayed. Failed restoration retains
intent without a fallback; direct bkprd startup without argv stays headless.
Explicit --session overrides inherited BKPR_SOCKET. Headless context/request/
keyboard/resume otherwise inherit pane routing; normal TUI/stop default to `bkpr`.
Utilities do not initialize the TUI or automatically start a daemon.
-r / --remote supplies the default host for ALL unqualified TUI targets, regardless
of argument position. Explicit HOST:NAME and local:NAME override it. Remote names
default to `bkpr`, independently of local pane routing. The TUI connects directly
over SSH without creating, installing or restarting remotely.
--remote-bkpr-bin selects the remote helper executable (default: remote PATH `bkpr`).
`remote inspect` validates session identity read-only, without attaching or forwarding
agents. `remote connect` keeps one attachment for bounded Operation JSON lines.
Only the TUI and remote connect accept -d takeover. SSH host keys must already be
trusted; agent forwarding is disabled unless explicitly requested by a remote TUI,
request or connect invocation. Remote paths and process operations stay remote;
terminal theme, capabilities and clipboard policy stay local.
Local startup options apply only to local targets; remote pane defaults stay remote.
Utilities accept at most one exact -s name (no host-prefix interpretation).
Initial failures remain offline for explicit retry. Previously active attachments
reconnect after transport loss, never restarting programs or replaying operations.
Sidebar S selects the session tree context; R on a session row forces reconnect.
An ended/detached session leaves the tree; ending the last exits after TTY restoration.
`resume` lists saved native conversations read-only; --pane selects one exact ID.
Resume uses current native configuration, not saved argv/settings; process start
is pending authenticated conversation binding, not proof the conversation loaded.
`context` reads current execution membership from the caller's verified native PTY,
not inherited labels or client focus. Outside a pane, select an exact --pane ID.
`run` preserves native arguments and captures supported interactive harnesses in
an authenticated Bkpr pane. Recognized generic harnesses run without capture.
`install agy` explicitly registers the launch-scoped context/capture MCP server.
Server: bkprd --help.
--home explicitly selects isolated fixture child environment; the normal default
preserves your HOME, environment and shell profiles in the invocation directory.

Keyboard (Ctrl-b prefix, then one command):
  n / % split left/right   " split top/bottom        w workspace chooser
  W window chooser         o pane chooser (including hidden/unassigned)
  [ / ] previous/next workspace    Tab next pane     arrows focus direction
  Shift+arrows exchange focused pane with its directional neighbour
  e resize: arrows 1 cell, Shift+arrows 5; Enter/Esc finish; drag dividers directly
  / search: type query, Enter next / Shift+Enter previous, Esc close
  s scrollback/history     y copy selection         i project details
  x remove layout leaf only (process survives)      R reopen unavailable pane
  g toggle fancy/plain chrome                      t theme chooser
  d detach                 Ctrl-b Ctrl-b send Ctrl-b to focused pane
Ctrl-Space enters the sidebar: navigate/fold with arrows; Enter activates;
S selects session root; R reconnects a session row. Sidebar n creates a workspace,
N a separate terminal window, r renames, and x/Delete closes after confirmation.
Click workspace or pane to focus; application mouse coordinates are pane-local.
Drag selects/copies; Shift overrides application
mouse tracking. History uses arrows/PgUp/PgDn/Home; Esc returns to live output.
Forms use Enter apply / Esc cancel / Ctrl-u clear; pane focus is restored.
Clipboard requests use y allow / n deny, bound to their originating pane.
Approved device transfers do not block unrelated pane input.
"#;

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
