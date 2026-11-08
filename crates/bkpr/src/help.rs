use anyhow::{Context, Result};
use clap::Command;

const OVERVIEW: &str = "Examples:\n  bkpr                          Start or reattach to the default session\n  bkpr -s work                  Open a named local session\n  bkpr -s work -s devbox:build   Show local and remote sessions together\n\nMore help:\n  bkpr tui --help               Connection and terminal options\n  bkpr COMMAND --help           Help for one command\n  bkpr keys                     Keyboard shortcuts";

pub(crate) const KEYS: &str = r#"Keyboard shortcuts

Sidebar: Ctrl-Space, then
  arrows / j / k   Navigate             Enter       Activate
  left / right    Fold / unfold        Esc         Return to pane
  n               New workspace        N           New terminal window
  r               Rename selected      x / Delete  Close selected (confirm)
  S               Select session root  R           Reconnect session
  /               Find panes

Panes: Ctrl-b, then
  n / %           Split side by side   "           Split above / below
  arrows          Focus pane           Tab         Next pane
  Shift-arrows    Exchange panes       [ / ]       Previous / next workspace
  w / W / o       Choose workspace / window / pane (including hidden panes)
  e               Resize: arrows 1 cell, Shift-arrows 5; Enter/Esc finish
  x               Remove from layout; keep the process running
  R               Reopen unavailable pane as a shell

History and presentation: Ctrl-b, then
  /               Search               s           Scrollback
  y               Copy selection       i           Project details
  g               Fancy/plain chrome   t           Theme picker
  d               Detach; keep programs running
  Ctrl-b          Send literal Ctrl-b to the application

In history: arrows scroll; PgUp/PgDn page; Esc/Enter/End return live.
In search: type a query; Enter/Shift-Enter next/previous match; Esc closes.
Drag to select and copy; hold Shift to override application mouse tracking.
Forms: Enter applies, Esc cancels. Clipboard requests: y allows, n denies.
"#;

// Materialize inherited globals before adjusting visibility. Parsing still accepts
// the same options; each help page shows only the options its command can use.
pub(crate) fn configure(mut command: Command) -> Command {
    command.build();
    scoped(command, "bkpr")
}

fn scoped(command: Command, scope: &str) -> Command {
    let name = command.get_name().to_owned();
    let scope = if scope == "bkpr" {
        name.as_str()
    } else {
        scope
    };
    let visible: &[&str] = match (scope, name.as_str()) {
        ("bkpr", _) => &["session", "remote", "detach-others"],
        ("tui", _) => &[
            "session",
            "remote",
            "detach-others",
            "read-only",
            "clipboard",
            "cwd",
            "home",
            "bkprd",
            "socket",
            "state-root",
            "ssh-config",
            "remote-bkpr-bin",
            "forward-agent",
        ],
        ("stop", _) => &["session", "socket", "state-root"],
        ("resume" | "context", _) => &["session", "socket", "state-root", "pane"],
        ("keyboard", _) => &["session", "socket", "state-root", "pane", "view"],
        ("api", "call") => &[
            "session",
            "socket",
            "state-root",
            "remote",
            "ssh-config",
            "remote-bkpr-bin",
            "forward-agent",
        ],
        ("remote", _) => &["session", "remote", "ssh-config", "remote-bkpr-bin"],
        _ => &[],
    };
    let command = command
        .term_width(80)
        .next_line_help(false)
        .mut_args(|argument| {
            let id = argument.get_id().as_str();
            let visible = (scope != "bkpr" || id != "shell")
                && (!argument.is_global_set() && id != "session"
                    || visible.contains(&id)
                    || scope == "remote" && name == "connect" && id == "detach-others"
                    || scope == "remote" && name != "inspect" && id == "forward-agent");
            let grouped = scope == "tui" && id != "help" && id != "version";
            let heading = match id {
                "session" | "remote" | "detach-others" => "Sessions",
                "ssh-config" | "remote-bkpr-bin" | "forward-agent" => "SSH",
                "read-only" | "clipboard" => "Terminal",
                "socket" | "state-root" => "Advanced",
                _ => "Local startup",
            };
            let argument = if id == "session" && scope != "bkpr" && scope != "tui" {
                argument
                    .value_name("NAME")
                    .help("Target one exact session name")
            } else {
                argument
            };
            let argument = if grouped {
                argument.help_heading(heading)
            } else {
                argument
            };
            argument.hide(!visible)
        })
        .mut_subcommands(|child| scoped(child, scope));
    match (scope, name.as_str()) {
        ("bkpr", _) => command
            .override_usage("bkpr [OPTIONS] [COMMAND]")
            .after_help(OVERVIEW),
        ("tui", _) => command.after_help("Examples:\n  bkpr -s work\n  bkpr -r devbox -s work\n  bkpr -s local:work -s devbox:build\n  bkpr -- /bin/bash --noprofile\n\nLocal sessions start if absent; remote sessions must already exist.\nRepeat -s to combine sessions. local:NAME escapes a literal local colon.\nRemote host keys must already be trusted. Forwarding is opt-in.\n\nKeyboard shortcuts: bkpr keys"),
        ("stop", _) => command.after_help("Stops this session and its programs; keeps recovery metadata.\nTo leave programs running, detach with Ctrl-b d instead.\n\nExample: bkpr stop -s work"),
        ("resume", _) => command.after_help("Without --pane, lists saved native conversations without starting them.\nWith --pane, resumes using current native configuration, not saved commands.\n\nExamples:\n  bkpr resume -s work\n  bkpr resume -s work --pane ID"),
        ("context", _) => command.after_help("Inside a pane, uses verified execution membership, not UI focus.\nOutside a pane, select --session and --pane. Does not start a daemon."),
        ("api", "call") => command.after_help("Managed agents default to their live workspace. Otherwise select a session.\nText input never adds Enter. Acceptance does not prove application consumption.\nIf a mutation's outcome is unknown, inspect state before acting again.\n\nDiscover arguments: bkpr api schema OPERATION"),
        ("remote", "remote") => command.after_help("Headless access only; for the terminal UI use bkpr -r HOST -s NAME.\nNever installs, starts or restarts a remote session.\n\nExample: bkpr remote -r devbox -s work inspect"),
        ("run", _) => command.after_help("Harnesses: claude, codex, omp, agy, opencode, cursor, copilot,\n           aider, cline, goose\n\nArguments after HARNESS belong to the harness, including --help.\nExample: bkpr run omp -- --help"),
        ("install", _) => command.after_help("Explicit installation only; normal launches do not change user settings."),
        _ => command,
    }
}

pub(crate) fn print_command(name: &str) -> Result<()> {
    let mut command = configure(crate::cli::command());
    for name in name.split_whitespace() {
        command = command
            .find_subcommand(name)
            .context("missing help command")?
            .clone();
    }
    command.print_help()?;
    Ok(())
}
