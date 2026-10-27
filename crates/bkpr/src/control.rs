//! Typed operator CLI and authenticated MCP dispatch. Native Zellij retains all PTYs.
mod arguments;
mod native;
mod operations;

use anyhow::{Context, Result, bail};
use arguments::*;
use clap::Subcommand;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub(crate) enum Caller {
    Operator,
    Agent {
        agent_id: String,
        context: bkpr::context_contract::PaneContext,
    },
}

macro_rules! command_group {
    ($name:ident { $($variant:ident($args:ty) => $operation:literal),* $(,)? }) => {
        #[derive(Debug, Subcommand)]
        pub(crate) enum $name {
            $( #[command(about = operations::help($operation), after_help = "Use `bkpr api schema --operation <operation>` for the matching MCP argument schema. Pane/workspace selectors are live identities, never the human's focused view. Exact argv follows `--`; creation never focuses a client.")]
            $variant($args), )*
        }
        impl $name {
            fn request(self) -> Result<Value> {
                match self { $( Self::$variant(arguments) => Ok(json!({"operation":$operation,"arguments":serde_json::to_value(arguments)?})), )* }
            }
        }
    };
}
command_group!(SessionCommands { List(SessionList) => "session.list", Inspect(Target) => "session.inspect", Save(Target) => "session.save", Stop(Target) => "session.stop" });
command_group!(ClientCommands { List(Target) => "client.list", Detach(Client) => "client.detach", Mode(ClientMode) => "client.mode", Focus(Focus) => "client.focus" });
command_group!(WorkspaceCommands { List(Discovery) => "workspace.list", Inspect(Workspace) => "workspace.inspect", Create(WorkspaceCreate) => "workspace.create", Rename(RenameWorkspace) => "workspace.rename", Close(Workspace) => "workspace.close" });
command_group!(TabCommands { List(Discovery) => "tab.list", Inspect(Tab) => "tab.inspect", Create(TabCreate) => "tab.create", Close(Tab) => "tab.close", Rename(RenameTab) => "tab.rename", Unrename(Tab) => "tab.unrename", Move(TabMove) => "tab.move", Focus(Focus) => "tab.focus", Sync(TabState) => "tab.sync", Floating(TabState) => "tab.floating" });
command_group!(PaneCommands { List(Discovery) => "pane.list", Inspect(Pane) => "pane.inspect", Create(Create) => "pane.create", Split(Create) => "pane.split", Close(Pane) => "pane.close", Rename(RenamePane) => "pane.rename", Unrename(Pane) => "pane.unrename", Resize(Resize) => "pane.resize", Geometry(Geometry) => "pane.geometry", Move(Move) => "pane.move", Pin(PaneState) => "pane.pin", Border(PaneState) => "pane.border", Scroll(Scroll) => "pane.scroll", Read(Read) => "pane.read", Clear(Pane) => "pane.clear", Focus(Focus) => "pane.focus" });
command_group!(LayoutCommands { Dump(Target) => "layout.dump", Cycle(LayoutCycle) => "layout.cycle" });
command_group!(InputCommands { Text(Input) => "input.text", Paste(Input) => "input.paste", Keys(Input) => "input.keys", Bytes(Input) => "input.bytes" });
command_group!(MessageCommands { Send(Send) => "message.send", Inbox(MessagePage) => "message.inbox", History(MessagePage) => "message.history", Read(MessageId) => "message.read", Ack(MessageId) => "message.ack", Status(MessageId) => "message.status" });
/// Public agent commands keep their output flag local so the legacy inspection
/// request's `--json STRING` cannot collide with a propagated boolean flag.
#[derive(Debug, clap::Args)]
pub(crate) struct AgentOutput<T: clap::Args> {
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    arguments: T,
}
#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommands {
    /// Discover live agents in the current workspace; opt into wider --scope explicitly.
    List(AgentOutput<AgentQuery>),
    /// Read this registered launch's authenticated identity and current live context.
    Whoami {
        #[arg(long)]
        json: bool,
    },
    /// Inspect actual bridge/receiver readiness, not guessed model prompt readiness.
    Ready(AgentOutput<AgentQuery>),
    /// Launch a supported harness through bkpr run; --new-workspace creates managed placement.
    Launch(AgentOutput<Create>),
    /// Set this registered launch's short discoverable collaboration title.
    SetTitle(AgentOutput<Title>),
    /// Internal foreground-process inspection used by existing native sessions.
    #[command(hide = true)]
    Inspect {
        #[arg(long)]
        json: String,
    },
}
#[derive(Debug, Subcommand)]
pub(crate) enum ApiCommands {
    /// List operations, or show the exact argument schema for --operation.
    Schema(Api),
    /// Explain operation semantics and scope, optionally for --operation.
    Help(Api),
}
#[derive(Debug, Subcommand)]
pub(crate) enum SkillCommands {
    /// Print the distributed control skill without installing anything.
    Show,
    /// Export the skill to an explicitly chosen directory.
    Export { directory: PathBuf },
}
macro_rules! root_commands {
    ($($name:ident($group:ty)),* $(,)?) => {
        #[derive(Debug, Subcommand)]
        pub(crate) enum Commands {
            $( #[command(about = group_help(stringify!($name)), after_help = "Examples:\n  bkpr --session work pane create --harness codex -- --help\n  bkpr --session work workspace list --session-scope --json\n  bkpr pane split --pane terminal_3 --direction right -- /bin/sh\n  bkpr api schema --operation pane.create\n\nInside Bkpr, selectors default to your live pane/workspace. Outside Bkpr, select --session explicitly. No command changes another client's focus as a targeting shortcut.")]
            $name { #[arg(long, global = true)] json: bool, #[command(subcommand)] command: $group }, )*
            /// Discover agents, launch supported harnesses and inspect real bridge readiness.
            Agent { #[command(subcommand)] command: AgentCommands },
            /// Discover implemented baseline control operations and attached native clients.
            Capabilities { #[arg(long)] json: bool, #[arg(long)] session: Option<String> },
            /// Emit completion definitions for the typed control groups.
            Completion { #[arg(value_parser = ["bash", "fish", "zsh", "powershell", "elvish"])] shell: String },
        }
    };
}
root_commands!(
    Session(SessionCommands),
    Client(ClientCommands),
    Workspace(WorkspaceCommands),
    Tab(TabCommands),
    Pane(PaneCommands),
    Layout(LayoutCommands),
    Message(MessageCommands),
    TerminalInput(InputCommands),
    Api(ApiCommands),
    Skill(SkillCommands)
);

fn group_help(group: &str) -> &'static str {
    match group {
        "Session" => "List, inspect, save and stop authenticated native Bkpr sessions.",
        "Client" => "Inspect and explicitly control attached native clients.",
        "Workspace" => "Create and manage Bkpr workspaces and their native terminal membership.",
        "Tab" => "Manage native tabs within Bkpr workspaces, without implicit focus changes.",
        "Pane" => "Create, inspect, move, resize, read and manage exact native terminal panes.",
        "Layout" => "Read native KDL layouts and cycle an explicitly selected tab's swap layouts.",
        "Agent" => "Discover agents, launch supported harnesses and inspect real bridge readiness.",
        "Message" => "Use durable future-turn collaboration, separate from terminal input.",
        "TerminalInput" => {
            "Explicitly write literal text, bracketed paste, keys or bytes to an exact PTY."
        }
        "Api" => "Discover the progressive typed CLI/MCP operation contract.",
        "Skill" => "Show or explicitly export the distributed Bkpr control skill.",
        _ => "Control native Bkpr sessions.",
    }
}

pub(crate) fn instructions() -> &'static str {
    "Use control with operation=api.schema to discover operations, then api.schema arguments.operation for a specific typed schema. Native control never sends terminal input; use terminal_input explicitly. Default scope is your live session/workspace. Explicit arguments.session can select another verified same-user session; foreign targets require explicit pane/workspace/client rather than borrowing your source pane. For agents, session.list widens only with all:true. Creation does not focus another client or grant harness/plugin approvals. Native acceptance is not model processing or prompt readiness. Read the bkpr-control skill resource for examples."
}

pub(crate) fn notify_agent_registry_changed(
    session: &crate::context::LiveSession,
    change: &bkpr::agent_contract::AgentRegistryChanged,
) -> Result<()> {
    native::notify_agent_registry_changed(session, change)
}

pub(crate) fn tool_schema(terminal_input: bool) -> Value {
    json!({"type":"object","additionalProperties":false,"required":["operation","arguments"],"properties":{"operation":{"type":"string","description":if terminal_input { "input.text, input.paste, input.keys, input.bytes; use control api.schema for details" } else { "Typed operation, e.g. api.schema, pane.create, pane.read, workspace.list. Discover through api.schema." }},"arguments":{"type":"object"}}})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    arguments: Value,
}

pub(crate) fn dispatch(request: Value, caller: Caller, terminal_input: bool) -> Result<Value> {
    dispatch_with(request, caller, terminal_input, None)
}
fn dispatch_with(
    request: Value,
    caller: Caller,
    terminal_input: bool,
    zellij: Option<&std::path::Path>,
) -> Result<Value> {
    let request: Request = serde_json::from_value(request)
        .context("control request must contain only operation and arguments")?;
    if !request.arguments.is_object() {
        bail!("arguments must be an object");
    }
    if request.operation.starts_with("input.") != terminal_input {
        bail!(
            "operation {} belongs to the {} tool",
            request.operation,
            if terminal_input {
                "control"
            } else {
                "terminal_input"
            }
        );
    }
    let response = operations::dispatch(&request.operation, request.arguments, &caller, zellij)?;
    if zellij.is_none() && request.operation == "pane.read" {
        bound_read_response(&response, bkpr::MAX_MESSAGE_BYTES)?;
    }
    Ok(response)
}

/// Account for JSON escaping, not only UTF-8 content bytes. Reject oversized MCP
/// reads explicitly; raw CLI reads never pass through this transport bound.
pub(crate) fn bound_read_response(response: &Value, maximum: usize) -> Result<()> {
    struct Bounded(usize);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(io::Error::other(
                    "pane.read MCP response exceeds transport limit; request --tail/--offset (tail/offset arguments), or use raw CLI --full",
                ));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Bounded(maximum), response).context(
        "pane.read exceeds MCP transport bound; use tail/offset to select fewer retained rows",
    )?;
    Ok(())
}

pub(crate) fn run(command: Commands, launch: crate::launcher::Options) -> Result<()> {
    let (mut request, compact, input) = match command {
        Commands::Session { command, json } => (command.request()?, json, false),
        Commands::Client { command, json } => (command.request()?, json, false),
        Commands::Workspace { command, json } => (command.request()?, json, false),
        Commands::Tab { command, json } => (command.request()?, json, false),
        Commands::Pane {
            command: PaneCommands::Read(mut arguments),
            json: false,
        } => {
            if let Some(session) = launch.session.as_deref()
                && arguments
                    .session
                    .as_deref()
                    .is_some_and(|selected| selected != session)
            {
                bail!("conflicting root and operation --session selectors");
            }
            if arguments.session.is_none() {
                arguments.session = launch.session.clone();
            }
            let host = native::Native::resolve(
                arguments.session.as_deref(),
                &Caller::Operator,
                Some(&launch.zellij),
            )?;
            let pane = host.pane(arguments.pane.as_deref())?;
            return host.read_raw(
                &pane,
                arguments.full,
                arguments.ansi,
                arguments.tail,
                arguments.offset,
                &mut io::stdout().lock(),
            );
        }
        Commands::Pane { command, json } => (command.request()?, json, false),
        Commands::Layout { command, json } => (command.request()?, json, false),
        Commands::Message { command, json } => (command.request()?, json, false),
        Commands::TerminalInput { command, json } => {
            let mut command = command;
            let (arguments, accepts_text) = match &mut command {
                InputCommands::Text(arguments) | InputCommands::Paste(arguments) => {
                    (arguments, true)
                }
                InputCommands::Keys(arguments) | InputCommands::Bytes(arguments) => {
                    (arguments, false)
                }
            };
            if arguments.stdin {
                if !accepts_text || arguments.text.is_some() {
                    bail!("--stdin is exclusive with --text and only valid for text/paste");
                }
                use std::io::Read;
                let mut bytes = Vec::new();
                io::stdin().take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 64 * 1024 {
                    bail!("terminal text exceeds 65536 bytes");
                }
                arguments.text = Some(
                    String::from_utf8(bytes)
                        .context("stdin text is not UTF-8; use explicit bytes for binary input")?,
                );
                arguments.stdin = false;
            }
            (command.request()?, json, true)
        }
        Commands::Agent {
            command: AgentCommands::Inspect { json },
            ..
        } => {
            if launch.session.is_some() {
                bail!("agent inspect does not accept an operator session selector");
            }
            let response = serde_json::to_vec(&crate::agent::inspect(&json)?)?;
            if response.len() > bkpr::MAX_MESSAGE_BYTES {
                bail!(
                    "agent inspect response exceeds {} bytes",
                    bkpr::MAX_MESSAGE_BYTES
                );
            }
            let mut stdout = io::stdout().lock();
            stdout.write_all(&response)?;
            writeln!(stdout)?;
            stdout.flush()?;
            return Ok(());
        }
        Commands::Agent { command } => {
            let (operation, arguments, json) = match command {
                AgentCommands::List(output) => (
                    "agent.list",
                    serde_json::to_value(output.arguments)?,
                    output.json,
                ),
                AgentCommands::Whoami { json } => ("agent.whoami", json!({}), json),
                AgentCommands::Ready(output) => (
                    "agent.ready",
                    serde_json::to_value(output.arguments)?,
                    output.json,
                ),
                AgentCommands::Launch(output) => (
                    "agent.launch",
                    serde_json::to_value(output.arguments)?,
                    output.json,
                ),
                AgentCommands::SetTitle(output) => (
                    "agent.set_title",
                    serde_json::to_value(output.arguments)?,
                    output.json,
                ),
                AgentCommands::Inspect { .. } => unreachable!(),
            };
            (
                json!({"operation":operation,"arguments":arguments}),
                json,
                false,
            )
        }
        Commands::Api { command, json } => {
            let (operation, arguments) = match command {
                ApiCommands::Schema(arguments) => ("api.schema", arguments),
                ApiCommands::Help(arguments) => ("api.help", arguments),
            };
            (
                json!({"operation":operation,"arguments":arguments}),
                json,
                false,
            )
        }
        Commands::Capabilities { json, session } => (
            json!({"operation":"capabilities","arguments":{"session":session}}),
            json,
            false,
        ),
        Commands::Skill { command, json } => {
            if launch.session.is_some() {
                bail!("skill export/show does not accept --session");
            }
            return skill(command, json);
        }
        Commands::Completion { shell } => {
            if launch.session.is_some() {
                bail!("completion does not accept --session");
            }
            return completion(&shell);
        }
    };
    if let Some(session) = launch.session {
        let arguments = request["arguments"]
            .as_object_mut()
            .expect("CLI emits an object");
        if arguments
            .get("session")
            .and_then(Value::as_str)
            .is_some_and(|selected| selected != session)
        {
            bail!("conflicting root and operation --session selectors");
        }
        if operations::accepts_session(request["operation"].as_str().unwrap()) {
            request["arguments"]["session"] = json!(session);
        } else {
            bail!("this operation does not accept --session");
        }
    }
    let native_text = !compact && request["operation"] == "layout.dump";
    let result = dispatch_with(request, Caller::Operator, input, Some(&launch.zellij))?;
    let mut output = io::stdout().lock();
    let text =
        if native_text && matches!(result["outcome"].as_str(), Some("completed" | "dispatched")) {
            result["result"]["stdout"].as_str()
        } else {
            None
        };
    if let Some(text) = text {
        output.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            writeln!(output)?;
        }
    } else {
        if compact {
            serde_json::to_writer(&mut output, &result)?;
        } else {
            serde_json::to_writer_pretty(&mut output, &result)?;
        }
        writeln!(output)?;
    }
    output.flush()?;
    if matches!(result["outcome"].as_str(), Some("failed" | "unknown")) {
        bail!(
            "native operation {}; receipt printed above; do not replay unknown completion automatically",
            result["outcome"]
        );
    }
    Ok(())
}

fn skill(command: SkillCommands, compact: bool) -> Result<()> {
    const SKILL: &str = include_str!("../../../integration/control/SKILL.md");
    const REFERENCE: &str = include_str!("../../../integration/control/references/control.md");
    match command {
        SkillCommands::Show => {
            if compact {
                println!("{}", json!({"skill":SKILL,"reference":REFERENCE}));
            } else {
                print!("{SKILL}");
            }
            io::stdout().flush()?;
        }
        SkillCommands::Export { directory } => {
            std::fs::create_dir_all(directory.join("references"))?;
            for (path, text) in [
                (directory.join("SKILL.md"), SKILL),
                (directory.join("references/control.md"), REFERENCE),
            ] {
                use std::fs::OpenOptions;
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .with_context(|| format!("export without overwriting {}", path.display()))?;
                file.write_all(text.as_bytes())?;
            }
            let receipt = json!({"exported":[directory.join("SKILL.md"),directory.join("references/control.md")]});
            if compact {
                println!("{receipt}");
            } else {
                println!("Exported Bkpr control skill to {}", directory.display());
            }
        }
    }
    Ok(())
}
fn completion(shell: &str) -> Result<()> {
    use clap::CommandFactory;
    let shell: clap_complete::Shell = shell.parse().map_err(anyhow::Error::msg)?;
    let mut command = crate::Cli::command();
    clap_complete::generate(shell, &mut command, "bkpr", &mut io::stdout().lock());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Commands,
    }

    #[test]
    fn legacy_inspection_json_remains_a_request_not_an_output_flag() {
        let cli =
            Cli::try_parse_from(["bkpr", "agent", "inspect", "--json", "{\"version\":1}"]).unwrap();
        match cli.command {
            Commands::Agent {
                command: AgentCommands::Inspect { json },
            } => assert_eq!(json, "{\"version\":1}"),
            _ => panic!("legacy native consumer command changed"),
        }
    }

    #[test]
    fn input_and_control_cannot_cross_tool_boundaries() {
        assert!(
            dispatch(
                json!({"operation":"input.text","arguments":{"text":"hello"}}),
                Caller::Operator,
                false
            )
            .is_err()
        );
        assert!(
            dispatch(
                json!({"operation":"pane.close","arguments":{"pane":"terminal_1"}}),
                Caller::Operator,
                true
            )
            .is_err()
        );
        assert!(
            dispatch(
                json!({"operation":"api.schema","arguments":{},"token":"forged"}),
                Caller::Operator,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn exact_creation_argv_preserves_shell_metacharacters_and_flag_values() {
        let cli = Cli::try_parse_from([
            "bkpr",
            "pane",
            "create",
            "--session",
            "work",
            "--",
            "/bin/printf",
            "%s",
            "$(not-a-command)",
            "--json",
        ])
        .unwrap();
        let Commands::Pane {
            command: PaneCommands::Create(arguments),
            json,
        } = cli.command
        else {
            panic!("not pane creation")
        };
        assert!(!json);
        assert_eq!(
            arguments.argv,
            ["/bin/printf", "%s", "$(not-a-command)", "--json"]
        );
    }
}
