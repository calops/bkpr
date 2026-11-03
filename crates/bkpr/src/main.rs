mod cli;
mod context;
mod harness;
mod recovery;
mod session;

use anyhow::{Context, Result, bail};
use bkpr_client::Options;
use bkpr_protocol::{Operation, Request, Size};
use std::{io::IsTerminal, path::PathBuf};

const HELP: &str = r#"Bkpr — persistent named pane workspaces

  bkpr [tui] [-s NAME | --socket PATH --state-root PATH] [-d]
       [--cwd PATH] [--read-only] [--clipboard ask|allow|deny] [--bkprd PATH]
       [--home ISOLATED_FIXTURE_HOME] [-- SHELL ARGS...]
  bkpr list
  bkpr stop [-s NAME | --socket PATH]
  bkpr resume [-s NAME | --socket PATH] [--pane PANE_ID]
  bkpr context [-s NAME | --socket PATH] [--pane PANE_ID] [--json]
  bkpr request [-s NAME | --socket PATH] 'VERSIONED_TYPED_JSON_REQUEST'
  bkpr keyboard --socket PATH --pane PANE_ID --view VIEW_ID '[PHYSICAL_BYTES]'
  bkpr run HARNESS [--] ARGS...
  bkpr install agy

Bare bkpr connects or starts the named local session `bkpr`, then opens its TUI.
-s / --session selects an exact name (Unicode, spaces and slashes are supported).
-d / --detach-others revokes other clients once this attachment is ready; it does
not terminate their panes. Client exit/detach preserves daemon and pane processes.
`list` discovers stored sessions without starting them; `stop` stops only the
selected session and its programs. Restart restores metadata, not old commands;
the normal fresh initial shell remains explicit daemon bootstrap behavior.
Explicit --session overrides inherited BKPR_SOCKET. Headless context/request/
keyboard/resume otherwise inherit pane routing; normal TUI/stop default to `bkpr`.
Utilities do not initialize the TUI or automatically start a daemon.
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
  c create workspace       n / % split left/right    " split top/bottom
  r rename workspace       p rename pane            w workspace chooser
  [ / ] previous/next workspace    Tab next pane     arrows focus direction
  Shift+arrows exchange focused pane with its directional neighbour
  e resize: arrows 1 cell, Shift+arrows 5; Enter/Esc finish; drag dividers directly
  / search: type query, Enter next / Shift+Enter previous, Esc close
  s scrollback/history     y copy selection         i project details
  x remove layout leaf only (process survives)      ! close / terminate (yes)
  X remove workspace layout (confirm yes; processes survive)
  o show existing pane (including hidden/unassigned executions)
  d detach                 Ctrl-b Ctrl-b send Ctrl-b to focused pane
Ctrl-] detaches directly. Click workspace or pane to focus; application mouse
coordinates are pane-local. Drag selects/copies; Shift overrides application
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
        command == "tui",
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
        "tui" => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                bail!("TUI needs a terminal; use context or request for headless access");
            }
            let mut options = Options::inherited()?;
            options.read_only = matches.get_flag("read-only");
            options.detach_others = matches.get_flag("detach-others");
            if let Some(policy) = matches.get_one::<String>("clipboard") {
                options.clipboard_policy.clone_from(policy);
            }
            if let Some(path) = matches.get_one::<PathBuf>("cwd") {
                options.cwd = cli::absolute(path.clone())?;
            }
            if let Some(path) = matches.get_one::<PathBuf>("home") {
                options.home = cli::absolute(path.clone())?;
                options.isolated_environment = true;
            }
            if let Some(shell) = cli::shell_arguments(&matches)? {
                options.shell = shell;
            }
            let physical = bkpr_tui_framework::dimensions()?;
            let size = Size {
                cols: physical.cols,
                rows: physical.rows,
                cell_width: physical.cell_width,
                cell_height: physical.cell_height,
            };
            bkpr_protocol::valid_size(size)?;
            let binary = matches
                .get_one::<PathBuf>("bkprd")
                .cloned()
                .map(cli::absolute)
                .transpose()?;
            session::ensure_server(&paths, binary.as_deref(), &options, size)?;
            bkpr_client::run(&paths.socket, options)
        }
        _ => unreachable!(),
    }
}
