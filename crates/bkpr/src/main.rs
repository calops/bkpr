use anyhow::{Context, Result, bail};
use bkpr_client::{LocalPaths, Options};
use bkpr_protocol::{Request, Size};
use std::{io::IsTerminal, path::PathBuf};

const HELP: &str = r#"Bkpr — persistent pane workspaces

  bkpr [tui] [--socket PATH] [--state-root PATH] [--cwd PATH]
       [--read-only] [--clipboard ask|allow|deny] [--bkprd PATH]
       [--home ISOLATED_FIXTURE_HOME] [-- SHELL ARGS...]
  bkpr context [--socket PATH]
  bkpr request [--socket PATH] 'VERSIONED_TYPED_JSON_REQUEST'
  bkpr keyboard --socket PATH --pane PANE_ID --view VIEW_ID '[PHYSICAL_BYTES]'

Bare bkpr connects or starts local bkprd, then opens the workspace TUI.
The daemon and pane processes survive detach and client exit. `request` and
`context` are headless and do not start a daemon. Server: bkprd --help.
--home explicitly selects isolated fixture child environment; the normal default
preserves your HOME, environment and shell profiles in the invocation directory.

Keyboard (Ctrl-b prefix, then one command):
  c create workspace       n / % split left/right    " split top/bottom
  r rename workspace       p rename pane            w workspace chooser
  [ / ] previous/next workspace    Tab next pane     arrows focus direction
  x remove layout leaf only (process survives)      ! close / terminate (yes)
  X remove workspace layout (confirm yes; processes survive)
  o show existing pane (including hidden/unassigned executions)
  d detach                 Ctrl-b Ctrl-b send Ctrl-b to focused pane
Ctrl-] detaches directly. Click workspace or pane to focus; application mouse
coordinates are pane-local. Forms use Enter apply / Esc cancel / Ctrl-u clear;
pane focus is restored. Clipboard requests use y allow / n deny, bound to their
originating pane. Approved device transfers do not block unrelated pane input.
"#;

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).peekable();
    if matches!(
        args.peek().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        println!("{HELP}");
        return Ok(());
    }
    if matches!(args.peek().map(String::as_str), Some("--version" | "-V")) {
        println!("bkpr {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let command = if args.peek().is_some_and(|arg| !arg.starts_with('-')) {
        args.next().unwrap()
    } else {
        "tui".into()
    };
    if !matches!(command.as_str(), "tui" | "request" | "context" | "keyboard") {
        bail!("unknown command {command}; use --help");
    }
    let mut paths = LocalPaths::defaults()?;
    let mut options = Options::inherited()?;
    let mut server = None;
    let mut pane = None;
    let mut view = None;
    let mut payload = None;
    let mut size = Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--socket" => {
                paths.socket = absolute(PathBuf::from(args.next().context("--socket needs path")?))?
            }
            "--state-root" => {
                paths.state_root = absolute(PathBuf::from(
                    args.next().context("--state-root needs path")?,
                ))?
            }
            "--home" => {
                options.home = absolute(PathBuf::from(
                    args.next().context("--home needs fixture path")?,
                ))?;
                options.isolated_environment = true;
            }
            "--cwd" => {
                options.cwd =
                    absolute(PathBuf::from(args.next().context("--cwd needs directory")?))?
            }
            "--bkprd" => {
                server = Some(absolute(PathBuf::from(
                    args.next().context("--bkprd needs executable path")?,
                ))?)
            }
            "--pane" => pane = Some(args.next().context("--pane needs ID")?),
            "--view" => view = Some(args.next().context("--view needs ID")?),
            "--read-only" => options.read_only = true,
            "--clipboard" => {
                options.clipboard_policy =
                    args.next().context("--clipboard needs ask|allow|deny")?
            }
            "--cols" => size.cols = args.next().context("--cols needs number")?.parse()?,
            "--rows" => size.rows = args.next().context("--rows needs number")?.parse()?,
            "--cell-width" => {
                size.cell_width = args.next().context("--cell-width needs number")?.parse()?
            }
            "--cell-height" => {
                size.cell_height = args.next().context("--cell-height needs number")?.parse()?
            }
            "--" => {
                options.shell = args.collect();
                break;
            }
            _ if matches!(command.as_str(), "request" | "keyboard") && payload.is_none() => {
                payload = Some(arg)
            }
            _ => bail!("unknown argument {arg}; use --help"),
        }
    }
    match command.as_str() {
        "request" => {
            let request: Request =
                serde_json::from_str(&payload.context("request needs versioned typed JSON")?)?;
            let response = bkpr_client::transport::request(&paths.socket, &request)?;
            println!("{}", serde_json::to_string(&response)?);
            Ok(())
        }
        "context" => {
            println!(
                "{}",
                serde_json::to_string(&bkpr_client::context(&paths.socket)?)?
            );
            Ok(())
        }
        "keyboard" => {
            let bytes: Vec<u8> =
                serde_json::from_str(&payload.context("keyboard needs JSON byte array")?)?;
            bkpr_client::keyboard(
                &paths.socket,
                &pane.context("--pane is required")?,
                &view.context("--view is required")?,
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
            if options.shell.is_empty() {
                bail!("-- requires a real shell command");
            }
            if !matches!(options.clipboard_policy.as_str(), "ask" | "allow" | "deny") {
                bail!("clipboard must be ask, allow or deny");
            }
            let physical = bkpr_tui_framework::dimensions()?;
            size.cols = physical.cols;
            size.rows = physical.rows;
            size.cell_width = physical.cell_width;
            size.cell_height = physical.cell_height;
            bkpr_protocol::valid_size(size)?;
            bkpr_client::ensure_server(&paths, server.as_deref(), &options, size)?;
            bkpr_client::run(&paths.socket, options)
        }
        _ => unreachable!(),
    }
}
