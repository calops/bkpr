use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use std::{ffi::OsString, path::PathBuf};

fn shell() -> Arg {
    Arg::new("shell")
        .last(true)
        .num_args(1..)
        .value_parser(value_parser!(OsString))
}

pub fn command() -> Command {
    let mut command = Command::new("bkpr")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Persistent named local pane workspaces")
        .after_help(crate::HELP)
        .arg(
            Arg::new("session")
                .long("session")
                .short('s')
                .global(true)
                .value_name("NAME")
                .conflicts_with_all(["socket", "state-root"]),
        )
        .arg(
            Arg::new("detach-others")
                .long("detach-others")
                .short('d')
                .global(true)
                .action(ArgAction::SetTrue),
        )
        .arg(shell());
    for name in ["socket", "state-root", "cwd", "home", "bkprd"] {
        command = command.arg(
            Arg::new(name)
                .long(name)
                .global(true)
                .value_name("PATH")
                .value_parser(value_parser!(PathBuf)),
        );
    }
    for name in ["pane", "view"] {
        command = command.arg(Arg::new(name).long(name).global(true));
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
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("clipboard")
                .long("clipboard")
                .global(true)
                .value_parser(["ask", "allow", "deny"]),
        )
        .subcommand(
            Command::new("tui")
                .about("Attach to a named session, starting it if absent")
                .arg(shell()),
        )
        .subcommand(
            Command::new("list").about("List named local sessions without starting daemons"),
        )
        .subcommand(
            Command::new("stop").about("Stop only the selected session and its pane processes"),
        )
        .subcommand(Command::new("context").about("Read pane context without starting a daemon"))
        .subcommand(
            Command::new("resume")
                .about("List saved conversations read-only; --pane ID explicitly resumes one using current native configuration"),
        )
        .subcommand(
            Command::new("request")
                .about("Send one versioned typed JSON request")
                .arg(Arg::new("payload").required(true)),
        )
        .subcommand(
            Command::new("keyboard")
                .about("Send an explicit physical keyboard probe")
                .arg(Arg::new("payload").required(true)),
        )
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<ArgMatches> {
    let matches = command().try_get_matches_from(args)?;
    let name = matches.subcommand_name().unwrap_or("tui");
    if name != "tui" {
        if matches.get_flag("detach-others") {
            bail!("--detach-others is only valid for a TUI attachment");
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
    if name == "resume"
        && ["view", "cols", "rows", "cell-width", "cell-height"]
            .iter()
            .any(|key| matches.contains_id(key))
    {
        bail!("resume accepts only an exact --pane ID and session target options");
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
    fn targeting_and_takeover_have_explicit_boundaries() {
        let parsed = parse(args(&["bkpr", "-s", "a / λ", "-d"])).unwrap();
        assert_eq!(parsed.get_one::<String>("session").unwrap(), "a / λ");
        assert!(parsed.get_flag("detach-others"));
        assert!(parse(args(&["bkpr", "-s", "a", "--socket", "/tmp/a"])).is_err());
        assert!(parse(args(&["bkpr", "--state-root", "/tmp/a", "-s", "a"])).is_err());
        assert!(parse(args(&["bkpr", "context", "-d"])).is_err());
        assert!(parse(args(&["bkpr", "list", "-s", "a"])).is_err());
        assert!(parse(args(&["bkpr", "stop", "--home", "/tmp/home"])).is_err());
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
