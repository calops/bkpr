use anyhow::{Context, Result, bail};
use bkpr_mux::{Pane, SpawnSpec, render, transport};
use bkpr_terminal::Size;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    if command == "help" || command == "--help" {
        println!(
            "Bkpr native pane runtime (protocol v1)\n\n  bkpr daemon --socket PATH --home ABSOLUTE_ISOLATED_HOME [--cols 80 --rows 24 --cell-width 8 --cell-height 16] [-- /bin/sh -i]\n  bkpr request --socket PATH 'JSON_REQUEST'\n  bkpr tui --socket PATH [--read-only] [--clipboard ask|allow|deny]\n\nCreate the socket parent directory mode 0700 before daemon launch. The daemon owns one PTY independently of views. Ctrl-] detaches a TUI without terminating the shell. Use request op=shutdown for explicit owned-process cleanup. request emits versioned JSON, including authority errors. Operations: attach/update/poll/input/paste/paste_contents/key/mouse/focus/scroll/pin/clipboard/colors/terminate/detach/inspect/shutdown. ViewSpec fields: size{{cols,rows,cell_width,cell_height}},interactive,visible,input,clipboard. No credentials or user configuration are inherited by the child shell.\n\nThis runtime does not yet provide workspace management, harness integration, agent messaging, Git/CI metadata or recovery. Initial Kitty graphics presentation remains an unaccepted runtime gate."
        );
        println!(
            "  bkpr keyboard --socket PATH --view VIEW_ID '[PHYSICAL_INPUT_BYTES]'\n    Uses the same ordered decoder, FIFO IPC and native encoder as tui; reports queue acceptance, not child processing."
        );
        return Ok(());
    }
    let mut socket = None;
    let mut home = None;
    let mut view = None;
    let mut size = Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let mut read_only = false;
    let mut policy = "ask".to_owned();
    let mut argv = Vec::new();
    let mut payload = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = Some(PathBuf::from(args.next().context("--socket needs path")?)),
            "--view" => view = Some(args.next().context("--view needs an opaque view ID")?),
            "--home" => home = Some(PathBuf::from(args.next().context("--home needs path")?)),
            "--cols" => size.cols = args.next().context("--cols needs number")?.parse()?,
            "--rows" => size.rows = args.next().context("--rows needs number")?.parse()?,
            "--cell-width" => {
                size.cell_width = args.next().context("--cell-width needs number")?.parse()?
            }
            "--cell-height" => {
                size.cell_height = args.next().context("--cell-height needs number")?.parse()?
            }
            "--read-only" => read_only = true,
            "--clipboard" => policy = args.next().context("--clipboard needs policy")?,
            "--" => {
                argv.extend(args);
                break;
            }
            _ if matches!(command.as_str(), "request" | "keyboard") && payload.is_none() => {
                payload = Some(arg)
            }
            _ => bail!("unknown argument {arg}"),
        }
    }
    let socket = socket.context("--socket is required")?;
    match command.as_str() {
        "daemon" => {
            if argv.is_empty() {
                argv = vec!["/bin/sh".into(), "-i".into()];
            }
            let pane = Pane::spawn(SpawnSpec {
                argv,
                home: home.context("--home isolation root is required")?,
                size,
                isolated_process: true,
            })?;
            let result = transport::serve(&socket, pane.clone());
            let cleanup = pane.request(bkpr_mux::Request::Shutdown);
            result.and(cleanup.map(|_| ()))
        }
        "request" => {
            let value = serde_json::from_str(&payload.context("request needs JSON")?)?;
            println!("{}", transport::request(&socket, &value)?);
            Ok(())
        }
        "keyboard" => {
            let bytes: Vec<u8> =
                serde_json::from_str(&payload.context("keyboard needs JSON byte array")?)?;
            render::keyboard(&socket, &view.context("--view is required")?, &bytes)?;
            println!("{{\"version\":1,\"ok\":true,\"semantics\":\"queued_once_no_replay\"}}");
            Ok(())
        }
        "tui" => render::tui(&socket, read_only, &policy),
        _ => bail!("unknown command {command}; use --help"),
    }
}
