use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use std::{ffi::OsString, path::PathBuf};

fn shell() -> Arg {
    Arg::new("shell")
        .last(true)
        .num_args(1..)
        .value_parser(value_parser!(OsString))
        .help("Command for new local terminals (after --)")
}

fn session_argument() -> Arg {
    Arg::new("session")
        .long("session")
        .short('s')
        .action(ArgAction::Append)
        .value_name("SESSION")
        .help("Session name or HOST:NAME; repeat to combine sessions")
        .conflicts_with_all(["socket", "state-root"])
}

// Clap's global propagation replaces a parent's Append values with a child's.
// Keep each group until we have counted utility targets or assembled the TUI list.
fn scoped_session_arguments(command: Command, root: bool) -> Command {
    let command = if root {
        command.mut_arg("session", |argument| argument.global(false))
    } else {
        command.arg(session_argument())
    };
    command.mut_subcommands(|subcommand| scoped_session_arguments(subcommand, false))
}

fn session_count(matches: &ArgMatches) -> usize {
    matches
        .get_many::<String>("session")
        .map_or(0, |names| names.len())
        + matches
            .subcommand()
            .map_or(0, |(_, child)| session_count(child))
}

pub fn command() -> Command {
    let mut command = Command::new("bkpr")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Persistent named pane workspaces")
        .override_usage("bkpr [OPTIONS] [COMMAND]")
        .arg(session_argument().global(true))
        .arg(
            Arg::new("detach-others")
                .long("detach-others")
                .short('d')
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Detach other clients once this attachment is ready"),
        )
        .arg(
            Arg::new("remote")
                .long("remote")
                .short('r')
                .global(true)
                .value_name("HOST")
                .help("Default SSH host for unqualified sessions"),
        )
        .arg(
            Arg::new("ssh-config")
                .long("ssh-config")
                .global(true)
                .value_name("PATH")
                .value_parser(value_parser!(PathBuf))
                .help("Use an explicit OpenSSH config file"),
        )
        .arg(
            Arg::new("remote-bkpr-bin")
                .long("remote-bkpr-bin")
                .global(true)
                .value_name("REMOTE_EXECUTABLE")
                .value_parser(value_parser!(PathBuf))
                .help("Remote bkpr executable [default: bkpr on remote PATH]"),
        )
        .arg(
            Arg::new("forward-agent")
                .long("forward-agent")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Explicitly forward the SSH agent using OpenSSH"),
        )
        .arg(shell());
    for name in ["socket", "state-root", "cwd", "home", "bkprd"] {
        command = command.arg(
            Arg::new(name)
                .long(name)
                .global(true)
                .value_name("PATH")
                .value_parser(value_parser!(PathBuf))
                .help(match name {
                    "socket" => "Connect to an explicit local daemon socket",
                    "state-root" => "Recovery storage for an explicit local socket",
                    "cwd" => "Starting directory for new local terminals",
                    "home" => "Isolated HOME for local fixtures",
                    "bkprd" => "Use an explicit local daemon executable",
                    _ => unreachable!(),
                }),
        );
    }
    for name in ["pane", "view"] {
        command = command.arg(
            Arg::new(name)
                .long(name)
                .global(true)
                .value_name("ID")
                .help(match name {
                    "pane" => "Select an exact pane ID",
                    "view" => "Select an exact attached view ID",
                    _ => unreachable!(),
                }),
        );
    }
    for name in ["cols", "rows", "cell-width", "cell-height"] {
        command = command.arg(
            Arg::new(name)
                .long(name)
                .global(true)
                .value_parser(value_parser!(u16)),
        );
    }
    command
        .arg(
            Arg::new("read-only")
                .long("read-only")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Inspect without sending input or changing pane state"),
        )
        .arg(
            Arg::new("clipboard")
                .long("clipboard")
                .global(true)
                .value_parser(["ask", "allow", "deny"])
                .value_name("POLICY")
                .hide_possible_values(true)
                .help("Clipboard access: ask, allow or deny [default: ask]"),
        )
        .subcommand(
            Command::new("tui")
                .about("Open local or existing remote sessions")
                .arg(shell()),
        )
        .subcommand(
            Command::new("list").about("List named local sessions without starting daemons"),
        )
        .subcommand(
            Command::new("stop").about("Stop only the selected session and its pane processes"),
        )
        .subcommand(
            Command::new("context")
                .about("Read live pane identity and labels without starting a daemon")
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit compact machine-readable JSON instead of indented JSON"),
                ),
        )
        .subcommand(
            Command::new("resume")
                .about("List saved conversations, or resume one with --pane ID"),
        )
        .subcommand(
            Command::new("request")
                .about("Send one versioned typed JSON request")
                .arg(Arg::new("payload").required(true)),
        )
        .subcommand(
            Command::new("api")
                .about("Discover and call native control operations")
                .subcommand_required(true)
                .disable_help_subcommand(true)
                .subcommand(Command::new("help").about("Print the backend operation catalog as JSON"))
                .subcommand(
                    Command::new("schema")
                        .about("Print JSON argument and result contracts for one operation or the catalog")
                        .arg(Arg::new("operation")),
                )
                .subcommand(
                    Command::new("call")
                        .about("Dispatch one catalog operation once; never replay unknown mutations")
                        .arg(Arg::new("operation").required(true))
                        .arg(
                            Arg::new("arguments")
                                .long("arguments")
                                .value_name("JSON_OBJECT")
                                .default_value("{}"),
                        )
                        .arg(
                            Arg::new("scope")
                                .long("scope")
                                .value_parser(["workspace", "session", "all"])
                                .help("Explicit daemon-local scope for authenticated managed calls"),
                        )
                        .arg(
                            Arg::new("reply")
                                .long("reply")
                                .value_parser(["receipt", "full"])
                                .default_value("receipt")
                                .help("Compact mutation receipt, or explicit full scoped native result"),
                        ),
                ),
        )
        .subcommand(
            Command::new("remote")
                .about("Connect headlessly to one existing named session over SSH")
                .subcommand_required(true)
                .subcommand(Command::new("inspect").about("Validate existing remote session identity read-only"))
                .subcommand(
                    Command::new("request")
                        .about("Send one typed native Operation, without replay")
                        .arg(Arg::new("payload").required(true)),
                )
                .subcommand(
                    Command::new("connect")
                        .about("Keep one attachment for a bounded JSON-lines Operation console"),
                ),
        )
        .subcommand(
            Command::new("__remote-helper")
                .hide(true)
                .arg(
                    Arg::new("stdio")
                        .long("stdio")
                        .required(true)
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("keyboard")
                .about("Send an explicit physical keyboard probe")
                .arg(Arg::new("payload").required(true)),
        )
        .subcommand(
            Command::new("completions")
                .about("Generate a shell completion script")
                .arg(Arg::new("shell").required(true).value_parser(value_parser!(clap_complete::Shell))),
        )
        .subcommand(Command::new("keys").about("Show terminal keyboard shortcuts").override_help(crate::help::KEYS))
        .subcommand(
            Command::new("run")
                .about("Launch a harness with Bkpr integration")
                .arg(Arg::new("harness").required(true).value_name("HARNESS").help("Harness to launch"))
                .arg(Arg::new("args").value_name("ARGS").help("Arguments passed unchanged to the harness").trailing_var_arg(true).num_args(0..).allow_hyphen_values(true)),
        )
        .subcommand(crate::harness::install::command())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TuiTarget {
    Local(String),
    Remote { host: String, name: String },
}

/// The CLI alone interprets host prefixes. Backend session names remain exact.
pub fn tui_targets(matches: &ArgMatches) -> Result<Vec<TuiTarget>> {
    let default_host = matches.get_one::<String>("remote");
    let names = matches.get_many::<String>("session");
    let child_names = matches
        .subcommand_matches("tui")
        .and_then(|child| child.get_many::<String>("session"));
    let count = session_count(matches);
    let default = (count == 0).then_some(crate::session::DEFAULT_SESSION);
    let mut targets = Vec::with_capacity(count);
    let mut seen = std::collections::HashSet::with_capacity(count);
    for value in names
        .into_iter()
        .flatten()
        .chain(child_names.into_iter().flatten())
        .map(String::as_str)
        .chain(default)
    {
        let target = if let Some(name) = value.strip_prefix("local:") {
            TuiTarget::Local(name.to_owned())
        } else if let Some((host, name)) = value.split_once(':') {
            TuiTarget::Remote {
                host: host.to_owned(),
                name: name.to_owned(),
            }
        } else if let Some(host) = default_host {
            TuiTarget::Remote {
                host: host.clone(),
                name: value.to_owned(),
            }
        } else {
            TuiTarget::Local(value.to_owned())
        };
        let name = match &target {
            TuiTarget::Local(name) => name,
            TuiTarget::Remote { host, name } => {
                if host.is_empty() || host.chars().any(char::is_control) {
                    bail!("remote host must be nonempty and contain no control characters");
                }
                name
            }
        };
        crate::session::validate_name(name)?;
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
    Ok(targets)
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<ArgMatches> {
    let args: Vec<_> = args.into_iter().collect();
    let command = scoped_session_arguments(command(), true);
    let help_requested = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--help" || arg == "-h" || arg == "help");
    let command = if help_requested {
        crate::help::configure(command)
    } else {
        command
    };
    let scoped = command.try_get_matches_from(args.clone())?;
    let name = scoped.subcommand_name().unwrap_or("tui");
    if matches!(name, "run" | "install") {
        bail!("{name} must precede options; see bkpr {name} --help");
    }
    if name != "tui" && session_count(&scoped) > 1 {
        bail!("multiple --session targets are only valid for a TUI attachment");
    }
    let matches = if name == "tui" {
        scoped
    } else {
        // Preserve existing utility callers' global single-target access.
        self::command().try_get_matches_from(args)?
    };
    let name = matches.subcommand_name().unwrap_or("tui");
    let remote_host = matches.get_one::<String>("remote").is_some();
    if session_count(&matches) > 0
        && ["socket", "state-root"]
            .iter()
            .any(|option| matches.contains_id(option))
    {
        bail!("--session cannot be combined with --socket or --state-root");
    }
    if remote_host && !matches!(name, "tui" | "remote" | "api") {
        bail!("--remote is only valid for a TUI attachment, remote command or api call");
    }
    let tui_targets = if name == "tui" {
        tui_targets(&matches)?
    } else {
        Vec::new()
    };
    let has_remote = remote_host
        || tui_targets
            .iter()
            .any(|target| matches!(target, TuiTarget::Remote { .. }));
    let has_local = tui_targets
        .iter()
        .any(|target| matches!(target, TuiTarget::Local(_)));
    if !has_remote
        && (matches.contains_id("ssh-config")
            || matches.contains_id("remote-bkpr-bin")
            || matches.get_flag("forward-agent"))
    {
        bail!("SSH options require a remote target or --remote HOST");
    }
    if name != "tui" {
        if matches.get_flag("detach-others") && name != "remote" {
            bail!("--detach-others is only valid for an attachment");
        }
        if matches.get_many::<OsString>("shell").is_some() {
            bail!("shell arguments are only valid for a TUI attachment");
        }
        for option in ["cwd", "home", "bkprd"] {
            if matches.contains_id(option) {
                bail!("--{option} is only valid for a TUI attachment");
            }
        }
        if matches.get_flag("read-only") || matches.contains_id("clipboard") {
            bail!("frontend options are only valid for a TUI attachment");
        }
    }
    if has_remote || matches!(name, "remote" | "__remote-helper") {
        for option in [
            "socket",
            "state-root",
            "pane",
            "view",
            "cols",
            "rows",
            "cell-width",
            "cell-height",
        ] {
            if matches.contains_id(option) {
                bail!("--{option} cannot target a remote connection");
            }
        }
        if !has_local {
            for option in ["cwd", "home", "bkprd"] {
                if matches.contains_id(option) {
                    bail!("--{option} cannot target a remote connection");
                }
            }
        }
        if !has_local && shell_arguments(&matches)?.is_some() {
            bail!("shell arguments cannot target a remote connection");
        }
        if name != "tui" && (remote_host || name == "remote") {
            crate::session::validate_name(
                matches
                    .get_one::<String>("session")
                    .map(String::as_str)
                    .unwrap_or("bkpr"),
            )?;
        }
        if name == "remote" {
            let remote = matches
                .subcommand_matches("remote")
                .context("missing remote command")?;
            matches
                .get_one::<String>("remote")
                .context("remote requires --remote HOST")?;
            if matches.get_flag("detach-others") && remote.subcommand_name() != Some("connect") {
                bail!("--detach-others is only valid for remote connect");
            }
            if matches.get_flag("forward-agent") && remote.subcommand_name() == Some("inspect") {
                bail!("remote inspect does not forward an SSH agent");
            }
        } else if name == "__remote-helper" && matches.contains_id("session") {
            bail!("the remote helper accepts only --stdio; the target is carried by its handshake");
        }
    }
    if name == "resume"
        && ["view", "cols", "rows", "cell-width", "cell-height"]
            .iter()
            .any(|key| matches.contains_id(key))
    {
        bail!("resume accepts only an exact --pane ID and session target options");
    }
    if name == "context"
        && ["view", "cols", "rows", "cell-width", "cell-height"]
            .iter()
            .any(|key| matches.contains_id(key))
    {
        bail!("context does not accept view or terminal geometry options");
    }
    if name == "api" {
        if ["pane", "view", "cols", "rows", "cell-width", "cell-height"]
            .iter()
            .any(|key| matches.contains_id(key))
        {
            bail!("api targets belong in the operation's --arguments JSON");
        }
        let api = matches
            .subcommand_matches("api")
            .context("missing api command")?;
        if api.subcommand_name() != Some("call")
            && (remote_host
                || ["session", "socket", "state-root"]
                    .iter()
                    .any(|key| matches.contains_id(key)))
        {
            bail!("api help/schema are local catalog queries and do not accept a session target");
        }
    }
    if name == "list"
        && ["session", "socket", "state-root"]
            .iter()
            .any(|key| matches.contains_id(key))
    {
        bail!("list discovers all named sessions; it does not accept a target");
    }
    Ok(matches)
}

pub fn shell_arguments(matches: &ArgMatches) -> Result<Option<Vec<String>>> {
    let source = matches.subcommand_matches("tui").unwrap_or(matches);
    source
        .get_many::<OsString>("shell")
        .map(|values| {
            values
                .map(|value| {
                    value.clone().into_string().map_err(|_| {
                        anyhow::anyhow!(
                            "shell arguments must be UTF-8; filesystem paths accept native OS bytes"
                        )
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
}

pub fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("reading invocation directory")?
            .join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn repeated_targets_keep_order_hosts_and_literal_colon_names() {
        let parsed = parse(args(&[
            "bkpr",
            "-s",
            "foo",
            "-s",
            "bar",
            "-s",
            "devbox:baz",
            "-s",
            "local:foo:bar",
            "-s",
            "foo",
            "-s",
            "devbox:baz",
        ]))
        .unwrap();
        assert_eq!(
            tui_targets(&parsed).unwrap(),
            [
                TuiTarget::Local("foo".into()),
                TuiTarget::Local("bar".into()),
                TuiTarget::Remote {
                    host: "devbox".into(),
                    name: "baz".into()
                },
                TuiTarget::Local("foo:bar".into()),
            ]
        );
    }

    #[test]
    fn default_host_is_invocation_wide_and_explicit_hosts_override_it() {
        for values in [
            vec![
                "bkpr",
                "-r",
                "default",
                "-s",
                "foo",
                "-s",
                "elsewhere:a:b",
                "-s",
                "local:x:y",
            ],
            vec![
                "bkpr",
                "-s",
                "foo",
                "-s",
                "elsewhere:a:b",
                "-s",
                "local:x:y",
                "-r",
                "default",
            ],
            vec![
                "bkpr",
                "tui",
                "-s",
                "foo",
                "-r",
                "default",
                "-s",
                "elsewhere:a:b",
                "-s",
                "local:x:y",
            ],
            vec![
                "bkpr",
                "-s",
                "foo",
                "tui",
                "-r",
                "default",
                "-s",
                "elsewhere:a:b",
                "-s",
                "local:x:y",
            ],
        ] {
            let parsed = parse(args(&values)).unwrap();
            assert_eq!(
                tui_targets(&parsed).unwrap(),
                [
                    TuiTarget::Remote {
                        host: "default".into(),
                        name: "foo".into()
                    },
                    TuiTarget::Remote {
                        host: "elsewhere".into(),
                        name: "a:b".into()
                    },
                    TuiTarget::Local("x:y".into()),
                ],
                "{values:?}"
            );
        }
    }

    #[test]
    fn utilities_reject_multiple_targets_without_reinterpreting_exact_names() {
        for command in ["stop", "context", "resume"] {
            assert!(parse(args(&["bkpr", command, "-s", "one", "-s", "two"])).is_err());
            let parsed = parse(args(&["bkpr", command, "-s", "local:a:b"])).unwrap();
            assert_eq!(parsed.get_one::<String>("session").unwrap(), "local:a:b");
        }
        for values in [
            vec!["bkpr", "request", "-s", "one", "-s", "two", "{}"],
            vec!["bkpr", "-s", "one", "request", "-s", "two", "{}"],
            vec![
                "bkpr",
                "api",
                "call",
                "session.inspect",
                "-s",
                "one",
                "-s",
                "two",
            ],
            vec![
                "bkpr",
                "-s",
                "one",
                "api",
                "call",
                "session.inspect",
                "-s",
                "two",
            ],
            vec![
                "bkpr", "remote", "-r", "host", "connect", "-s", "one", "-s", "two",
            ],
            vec!["bkpr", "-s", ":name"],
            vec!["bkpr", "-s", "host:"],
            vec!["bkpr", "-s", "local:"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn local_bootstrap_overrides_do_not_become_remote_defaults() {
        assert!(
            parse(args(&[
                "bkpr",
                "-s",
                "local:a:b",
                "-s",
                "host:remote",
                "--cwd",
                "/local/cwd",
                "--home",
                "/local/home",
                "--bkprd",
                "/local/bkprd",
                "--ssh-config",
                "ssh.conf",
                "--",
                "/bin/sh",
                "-i",
            ]))
            .is_ok()
        );
        assert!(parse(args(&["bkpr", "-s", "host:remote", "--cwd", "/local/cwd",])).is_err());
    }

    #[test]
    fn remote_tui_accepts_exact_names_and_local_frontend_policy() {
        let parsed = parse(args(&[
            "bkpr",
            "-r",
            "user@alias",
            "-s",
            "a / λ",
            "--remote-bkpr-bin",
            "relative remote/bkpr",
            "--ssh-config",
            "relative ssh/config",
            "--forward-agent",
            "-d",
            "--read-only",
            "--clipboard",
            "deny",
        ]))
        .unwrap();
        assert_eq!(parsed.subcommand_name(), None);
        assert_eq!(parsed.get_one::<String>("remote").unwrap(), "user@alias");
        assert_eq!(parsed.get_one::<String>("session").unwrap(), "a / λ");
        assert!(parsed.get_flag("detach-others"));
        assert!(parsed.get_flag("forward-agent"));
        assert!(parsed.get_flag("read-only"));
        assert_eq!(parsed.get_one::<String>("clipboard").unwrap(), "deny");
        assert_eq!(
            parsed.get_one::<PathBuf>("remote-bkpr-bin").unwrap(),
            &PathBuf::from("relative remote/bkpr")
        );
        assert_eq!(
            parsed.get_one::<PathBuf>("ssh-config").unwrap(),
            &PathBuf::from("relative ssh/config")
        );
        assert!(parse(args(&["bkpr", "tui", "--remote", "alias"])).is_ok());
    }

    #[test]
    fn headless_remote_commands_share_flags_and_default_session_selection() {
        for values in [
            vec!["bkpr", "remote", "-r", "alias", "inspect"],
            vec![
                "bkpr",
                "-r",
                "alias",
                "remote",
                "request",
                "{\"op\":\"inspect\"}",
                "--forward-agent",
            ],
            vec![
                "bkpr",
                "remote",
                "connect",
                "--remote",
                "alias",
                "-s",
                "a / λ",
                "--remote-bkpr-bin",
                "bin/bkpr",
                "--forward-agent",
                "-d",
            ],
        ] {
            let parsed = parse(args(&values)).unwrap();
            assert_eq!(parsed.get_one::<String>("remote").unwrap(), "alias");
            assert_eq!(parsed.subcommand_name(), Some("remote"));
        }
        for values in [
            vec!["bkpr", "remote", "-s", "name", "inspect"],
            vec!["bkpr", "remote", "-r", "alias", "-d", "inspect"],
            vec![
                "bkpr",
                "remote",
                "-r",
                "alias",
                "-d",
                "request",
                "{\"op\":\"inspect\"}",
            ],
            vec![
                "bkpr",
                "remote",
                "-r",
                "alias",
                "--forward-agent",
                "inspect",
            ],
            vec!["bkpr", "remote", "-r", "alias", "--read-only", "inspect"],
            vec![
                "bkpr",
                "remote",
                "-r",
                "alias",
                "--clipboard",
                "deny",
                "connect",
            ],
            vec!["bkpr", "remote", "-r", "alias", "tui"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn remote_selection_rejects_local_execution_bootstrap_and_positional_targets() {
        for option in [
            "--socket",
            "--state-root",
            "--cwd",
            "--home",
            "--bkprd",
            "--pane",
            "--view",
            "--cols",
            "--rows",
            "--cell-width",
            "--cell-height",
        ] {
            for prefix in [
                vec!["bkpr", "-r", "alias"],
                vec!["bkpr", "remote", "-r", "alias", "connect"],
            ] {
                let mut values = prefix;
                values.extend([option, "1"]);
                assert!(parse(args(&values)).is_err(), "{values:?}");
            }
        }
        for values in [
            vec!["bkpr", "-r", "alias", "--", "sh"],
            vec!["bkpr", "tui", "-r", "alias", "--", "sh"],
            vec!["bkpr", "remote", "-r", "alias", "connect", "--", "sh"],
            vec!["bkpr", "-r", "alias", "-s", "bad\nname"],
            vec!["bkpr", "-r", "alias", "-s", ""],
            vec!["bkpr", "alias:session"],
            vec!["bkpr", "tui", "alias"],
            vec!["bkpr", "attach", "-r", "alias"],
            vec!["bkpr", "list", "-r", "alias"],
            vec!["bkpr", "request", "-r", "alias", "{}"],
            vec!["bkpr", "context", "-r", "alias"],
            vec!["bkpr", "stop", "-r", "alias"],
            vec!["bkpr", "resume", "-r", "alias"],
            vec!["bkpr", "--ssh-config", "config"],
            vec!["bkpr", "--remote-bkpr-bin", "bin/bkpr"],
            vec!["bkpr", "--forward-agent"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn helper_has_only_the_fixed_stdio_invocation() {
        assert!(parse(args(&["bkpr", "__remote-helper", "--stdio"])).is_ok());
        for values in [
            vec!["bkpr", "__remote-helper"],
            vec!["bkpr", "__remote-helper", "--stdio", "--session", "name"],
            vec![
                "bkpr",
                "__remote-helper",
                "--stdio",
                "--socket",
                "/tmp/socket",
            ],
            vec!["bkpr", "__remote-helper", "--stdio", "-d"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn targeting_and_takeover_have_explicit_boundaries() {
        let parsed = parse(args(&["bkpr", "-s", "a / λ", "-d"])).unwrap();
        assert_eq!(parsed.get_one::<String>("session").unwrap(), "a / λ");
        assert!(parsed.get_flag("detach-others"));
        assert!(parse(args(&["bkpr", "-s", "a", "--socket", "/tmp/a"])).is_err());
        assert!(parse(args(&["bkpr", "--state-root", "/tmp/a", "-s", "a"])).is_err());
        assert!(parse(args(&["bkpr", "context", "-d"])).is_err());
        assert!(parse(args(&["bkpr", "list", "-s", "a"])).is_err());
        assert!(parse(args(&["bkpr", "stop", "--home", "/tmp/home"])).is_err());
        assert!(parse(args(&["bkpr", "context", "--view", "v"])).is_err());
        assert!(parse(args(&["bkpr", "context", "--cols", "80"])).is_err());
        assert!(parse(args(&["bkpr", "resume", "--json"])).is_err());
    }

    #[test]
    fn resume_is_read_only_unless_an_exact_pane_is_explicit() {
        let parsed = parse(args(&["bkpr", "resume"])).unwrap();
        assert!(parsed.get_one::<String>("pane").is_none());
        let parsed = parse(args(&[
            "bkpr", "resume", "--pane", "pane-123", "-s", "work",
        ]))
        .unwrap();
        assert_eq!(parsed.get_one::<String>("pane").unwrap(), "pane-123");
        assert_eq!(parsed.get_one::<String>("session").unwrap(), "work");
        for values in [
            vec!["bkpr", "resume", "pane-123"],
            vec!["bkpr", "resume", "--pane", "pane-123", "--pane", "other"],
            vec![
                "bkpr", "resume", "--pane", "pane-123", "--", "claude", "--resume", "other",
            ],
            vec!["bkpr", "resume", "--cwd", "/other"],
            vec!["bkpr", "resume", "--bkprd", "/other"],
            vec!["bkpr", "resume", "--home", "/other"],
            vec!["bkpr", "resume", "--view", "view-123"],
            vec!["bkpr", "resume", "--cols", "80"],
            vec!["bkpr", "resume", "-d"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn shell_arguments_are_not_options_and_native_paths_are_not_lossy() {
        let parsed = parse(args(&[
            "bkpr",
            "tui",
            "--",
            "/bin/sh",
            "-c",
            "echo --session",
        ]))
        .unwrap();
        assert_eq!(
            shell_arguments(&parsed).unwrap().unwrap(),
            ["/bin/sh", "-c", "echo --session"]
        );
        let native = OsString::from_vec(b"/tmp/nonutf8-\xff".to_vec());
        let parsed = parse(vec!["bkpr".into(), "--socket".into(), native.clone()]).unwrap();
        assert_eq!(
            parsed.get_one::<PathBuf>("socket").unwrap().as_os_str(),
            native
        );
        let parsed = parse(vec!["bkpr".into(), "--".into(), native]).unwrap();
        assert!(shell_arguments(&parsed).is_err());
    }
}
