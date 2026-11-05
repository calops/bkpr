//! Discoverable CLI boundary for the same operation catalog as MCP.
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::control::{self, ControlAction};
use clap::ArgMatches;
use serde_json::{Value, json};
use std::path::PathBuf;

fn invocation(matches: &ArgMatches) -> Result<(String, Value, Option<String>)> {
    let api = matches
        .subcommand_matches("api")
        .context("missing api command")?;
    let (name, args) = api.subcommand().context("missing api operation")?;
    match name {
        "help" => Ok(("api.help".into(), json!({}), None)),
        "schema" => Ok((
            "api.schema".into(),
            args.get_one::<String>("operation")
                .map(|operation| json!({"operation":operation}))
                .unwrap_or_else(|| json!({})),
            None,
        )),
        "call" => {
            let operation = args
                .get_one::<String>("operation")
                .context("missing operation")?
                .clone();
            let arguments: Value = serde_json::from_str(
                args.get_one::<String>("arguments")
                    .context("missing arguments")?,
            )
            .context("--arguments must be a JSON object")?;
            ensure!(arguments.is_object(), "--arguments must be a JSON object");
            Ok((
                operation,
                arguments,
                args.get_one::<String>("scope").cloned(),
            ))
        }
        _ => bail!("unknown api command"),
    }
}

fn execute(matches: &ArgMatches) -> Result<Value> {
    let (operation, arguments, scope) = invocation(matches)?;
    let action = control::resolve(&operation, arguments.clone())?;
    if let ControlAction::Local(value) = action {
        return Ok(value);
    }
    let managed = std::env::var_os("BKPR_CAPTURE_LAUNCH_ID").is_some()
        || std::env::var_os("BKPR_AGENT_RELAY_SOCKET").is_some();
    if managed {
        ensure!(
            !["session", "socket", "state-root", "remote"]
                .iter()
                .any(|key| matches.contains_id(key)),
            "managed api calls use authenticated capture in their own daemon; select wider --scope explicitly, not another session/socket/host"
        );
        let mut request = json!({
            "method":if control::is_input(&operation) { "terminal_input" } else { "control" },
            "operation":operation,
            "arguments":arguments,
        });
        if let Some(scope) = scope {
            request["scope"] = json!(scope);
        }
        return crate::harness::control_request(request);
    }
    ensure!(
        scope.is_none(),
        "--scope requires an authenticated managed launch"
    );
    let ControlAction::Native(operation) = action else {
        unreachable!("local action returned above")
    };
    if matches.get_one::<String>("remote").is_some() {
        return crate::remote::api_call(matches, operation);
    }
    ensure!(
        matches.contains_id("session") || matches.contains_id("socket"),
        "outside a managed launch, api call requires explicit --session NAME or --socket PATH (or --remote HOST)"
    );
    let roots = crate::session::Roots::inherited()?;
    let paths = crate::session::LocalPaths::resolve(
        &roots,
        matches.get_one::<String>("session").map(String::as_str),
        matches.get_one::<PathBuf>("socket").cloned(),
        matches.get_one::<PathBuf>("state-root").cloned(),
        None,
        false,
        false,
    )?;
    crate::session::validate_target(&paths)?;
    bkpr_client::transport::checked(&paths.socket, operation)
}

pub fn run(matches: &ArgMatches) -> Result<()> {
    match execute(matches) {
        Ok(value) => {
            println!("{}", serde_json::to_string(&value)?);
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                json!({
                    "ok":false,
                    "error":format!("{error:#}"),
                    "outcome_unknown":crate::harness::outcome_unknown(&error),
                    "replayed":false,
                })
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> ArgMatches {
        crate::cli::parse(args.iter().map(OsString::from)).unwrap()
    }

    #[test]
    fn help_and_schema_resolve_without_runtime_or_target() {
        for args in [
            vec!["bkpr", "api", "help"],
            vec!["bkpr", "api", "schema"],
            vec!["bkpr", "api", "schema", "pane.create"],
        ] {
            let (operation, arguments, scope) = invocation(&parse(&args)).unwrap();
            assert!(scope.is_none());
            assert!(matches!(
                control::resolve(&operation, arguments).unwrap(),
                ControlAction::Local(_)
            ));
        }
    }

    #[test]
    fn call_preserves_explicit_arguments_and_scope() {
        let matches = parse(&[
            "bkpr",
            "api",
            "call",
            "input.text",
            "--arguments",
            "{\"pane_id\":\"exact\",\"text\":\"literal\\n\"}",
            "--scope",
            "session",
        ]);
        let (operation, arguments, scope) = invocation(&matches).unwrap();
        assert_eq!(operation, "input.text");
        assert_eq!(arguments["text"], "literal\n");
        assert_eq!(scope.as_deref(), Some("session"));
        let bad = parse(&["bkpr", "api", "call", "recovery.list", "--arguments", "[]"]);
        assert!(invocation(&bad).is_err());
    }
}
