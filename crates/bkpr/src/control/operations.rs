use super::{
    Caller,
    arguments::*,
    native::{Native, result},
};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn help(operation: &str) -> &'static str {
    match operation {
        "session.list" => {
            "List authenticated live Bkpr sessions without starting a server. Agents default to their own session; --all explicitly discovers other same-user sessions."
        }
        "session.inspect" => {
            "Inspect one live session's native topology, clients and Bkpr bindings."
        }
        "session.save" => "Ask native Zellij to persist the current session state immediately.",
        "session.stop" => {
            "Stop the selected native session and its programs; existing recovery records remain available."
        }
        "client.list" => {
            "Inspect attached native clients without changing any client's view or input mode."
        }
        "client.detach" => {
            "Detach exactly --client; preserve the session and its running programs."
        }
        "client.mode" => "Change only the explicitly selected attached client's native input mode.",
        "client.focus" | "tab.focus" | "pane.focus" => {
            "Focus an exact tiled terminal, or an eligible terminal in a tab, for --client only. Rejects target tabs viewed by another client (baseline stack focus is shared), unknown peer bindings, shared fullscreen, selectable floating terminals, floating/suppressed targets and empty/plugin-only tabs. Unshared target tabs remain available with multiple clients. Never used to implement targeting."
        }
        "workspace.list" => {
            "List the caller's current workspace; add --session-scope for all workspaces."
        }
        "workspace.inspect" => {
            "Inspect one Bkpr workspace and all its native tab and pane bindings."
        }
        "workspace.create" => {
            "Create a fresh Bkpr workspace with the managed sidebar; optionally run exact argv or --harness."
        }
        "workspace.rename" => {
            "Rename the Bkpr workspace label consistently across its native tab bindings."
        }
        "workspace.close" => "Close every tab and terminal in the exact selected Bkpr workspace.",
        "tab.list" => {
            "List native tabs in the current workspace; add --session-scope for the whole session."
        }
        "tab.inspect" => "Inspect a stable native tab ID and its Bkpr workspace binding.",
        "tab.create" => {
            "Create another managed tab in --workspace (default current); optionally run exact argv or a harness."
        }
        "tab.close" => "Close the selected native tab and its terminal programs.",
        "tab.rename" => "Set the selected native tab label without renaming its Bkpr workspace.",
        "tab.unrename" => "Undo the selected tab's explicit native name.",
        "tab.move" => {
            "Reorder the selected native tab left or right; stable tab identity is unchanged."
        }
        "tab.sync" => {
            "Set whether normal native input is synchronized across the selected tab's panes."
        }
        "tab.floating" => "Show or hide floating panes in the exact selected native tab.",
        "pane.list" => {
            "List terminal panes in the current workspace; add --session-scope for wider discovery."
        }
        "pane.inspect" => {
            "Inspect one exact terminal pane's Bkpr binding and native process/geometry snapshot."
        }
        "pane.create" => {
            "Create a native shell or exact argv (-- ARGV...), optionally --harness codex; never focus a client."
        }
        "pane.split" => {
            "Split relative to --pane (default live caller), e.g. --direction right -- sh -c 'printf hello; exec sh'."
        }
        "pane.close" => "Close exactly --pane and its native terminal process.",
        "pane.rename" => {
            "Set an exact terminal pane's native name, without injecting terminal input."
        }
        "pane.unrename" => "Remove an exact terminal pane's explicitly assigned native name.",
        "pane.resize" => {
            "Resize by native --change increase|decrease and optional --direction; no fake absolute tiled size."
        }
        "pane.geometry" => {
            "Set floating geometry in cells or percentages, e.g. --x 10% --width 80% --height 24."
        }
        "pane.move" => {
            "Move the exact selected pane within its current tab by direction, or rotate forward/backward. Cross-tab/workspace moves are not exposed on this baseline."
        }
        "pane.pin" => "Set --enabled true|false for an exact floating pane's always-on-top pin.",
        "pane.border" => "Set --enabled true|false for an exact pane's border.",
        "pane.scroll" => {
            "Scroll only the exact selected pane's native scrollback, without focus emulation."
        }
        "pane.read" => {
            "Read the native viewport; --full includes retained scrollback. --tail N keeps N newest rows after --offset N skips newest rows; either range option implies full. Zero tail or offset beyond retained rows returns empty. Native wrapped rows and selected newline/ANSI bytes are preserved. Raw CLI output is uncapped; MCP explicitly rejects encoded tool results above 4 MiB, including both compatibility payloads (use tail/offset)."
        }
        "pane.clear" => "Clear the exact selected pane's native viewport and scrollback buffers.",
        "layout.dump" => "Read the current native session layout as KDL.",
        "layout.cycle" => {
            "Apply the next or previous configured native swap layout to the exact selected tab."
        }
        "input.text" => {
            "Write literal --text (or CLI --stdin); --submit appends Enter in the same native write. Not peer messaging."
        }
        "input.paste" => {
            "Send native bracketed --text paste (or CLI --stdin); --submit sends Enter in a separate action. Partial acceptance is never automatically replayed."
        }
        "input.keys" => {
            "Send explicit --keys 'Ctrl c' 'Enter' using native key grammar; never interpolates a shell command."
        }
        "input.bytes" => {
            "Write explicit decimal --bytes 3 13 directly to the target PTY; no implicit submit."
        }
        "message.send" => {
            "Durably send peer text with an explicit sender-scoped --idempotency-key; not terminal input."
        }
        "message.inbox" => {
            "Read this registered launch's unacknowledged inbox, with --after pagination."
        }
        "message.history" => {
            "Inspect this registered sender's outbox receipts and delivery history; never returns message bodies."
        }
        "message.read" => {
            "Read a retained message body as the local operator (not available to an agent caller)."
        }
        "message.ack" => {
            "Explicitly acknowledge this registered recipient's processed message; repeated acknowledgment is safe."
        }
        "message.status" => {
            "Inspect one message's actual durable delivery/read/ack state, not inferred model processing."
        }
        "capabilities" => {
            "Inspect implemented baseline adapter operations; select a live session to observe attached native clients. SDK-only operations require a matching permitted owner plugin; readiness is not guessed."
        }
        "agent.list" => {
            "Discover live agents in the current workspace; explicit scope widens to session, repository or all."
        }
        "agent.whoami" => {
            "Read the registered launch's broker-authenticated identity, live pane context and project metadata."
        }
        "agent.ready" => {
            "Read point-in-time bridge/receiver readiness without polling, launching a model turn or guessing prompt readiness."
        }
        "agent.launch" => {
            "Launch any supported harness through bkpr run with fresh relay identity; optionally create a new tab/workspace. Normal harness/plugin consent is unchanged."
        }
        "agent.set_title" => {
            "Set this registered launch's short discoverable title without renaming the native pane or injecting input."
        }
        "api.schema" => {
            "List typed operations, or return the exact argument schema for one operation."
        }
        "api.help" => {
            "Explain exact operation semantics, scope, receipt boundaries and operation-specific argument schema."
        }
        _ => "Inspect the typed Bkpr control API.",
    }
}

macro_rules! inventory {
    ($m:ident) => { $m! {
        "capabilities" => Target, "session.list" => SessionList, "session.inspect" => Target,
        "session.save" => Target, "session.stop" => Target,
        "client.list" => Target, "client.detach" => Client, "client.mode" => ClientMode, "client.focus" => Focus,
        "workspace.list" => Discovery, "workspace.inspect" => Workspace, "workspace.create" => WorkspaceCreate,
        "workspace.rename" => RenameWorkspace, "workspace.close" => Workspace,
        "tab.list" => Discovery, "tab.inspect" => Tab, "tab.create" => TabCreate, "tab.close" => Tab,
        "tab.rename" => RenameTab, "tab.unrename" => Tab, "tab.move" => TabMove, "tab.focus" => Focus,
        "tab.sync" => TabState, "tab.floating" => TabState,
        "pane.list" => Discovery, "pane.inspect" => Pane, "pane.create" => Create, "pane.split" => Create,
        "pane.close" => Pane, "pane.rename" => RenamePane, "pane.unrename" => Pane,
        "pane.resize" => Resize, "pane.geometry" => Geometry, "pane.move" => Move,
        "pane.pin" => PaneState, "pane.border" => PaneState,
        "pane.scroll" => Scroll, "pane.read" => Read, "pane.clear" => Pane,
        "pane.focus" => Focus, "layout.dump" => Target, "layout.cycle" => LayoutCycle,
        "agent.list" => AgentQuery, "agent.whoami" => Empty, "agent.ready" => AgentQuery,
        "agent.launch" => Create, "agent.set_title" => Title,
        "message.send" => Send, "message.inbox" => MessagePage, "message.history" => MessagePage,
        "message.read" => MessageId, "message.ack" => MessageId, "message.status" => MessageId,
        "input.text" => Input, "input.paste" => Input, "input.keys" => Input, "input.bytes" => Input,
        "api.schema" => Api, "api.help" => Api
    } };
}
macro_rules! typed_arguments {
    (session: $($session:ident),*; plain: $($plain:ident),* $(;)?) => {
        enum Arguments { $( $session($session), )* $( $plain($plain), )* }
        impl Arguments {
            fn session(&self) -> Option<&str> {
                match self { $( Self::$session(args) => args.session.as_deref(), )* _ => None }
            }
        }
        $(
            impl TryFrom<Arguments> for $session {
                type Error = anyhow::Error;
                fn try_from(arguments: Arguments) -> Result<Self> {
                    if let Arguments::$session(args) = arguments { Ok(args) } else { bail!("internal operation argument mismatch") }
                }
            }
        )*
        $(
            impl TryFrom<Arguments> for $plain {
                type Error = anyhow::Error;
                fn try_from(arguments: Arguments) -> Result<Self> {
                    if let Arguments::$plain(args) = arguments { Ok(args) } else { bail!("internal operation argument mismatch") }
                }
            }
        )*
    };
}
typed_arguments! {
    session: Target, Discovery, Pane, Tab, Workspace, Create, WorkspaceCreate, TabCreate,
        RenamePane, RenameWorkspace, RenameTab, Resize, Move, Geometry, PaneState,
        Scroll, Read, TabMove, TabState, LayoutCycle, Client, ClientMode,
        Focus, Input, AgentQuery;
    plain: Api, Empty, SessionList, Send, MessagePage, MessageId, Title;
}
fn parse<T: TryFrom<Arguments, Error = anyhow::Error>>(arguments: Arguments) -> Result<T> {
    arguments.try_into()
}
fn decode<T: DeserializeOwned>(arguments: Value) -> Result<T> {
    serde_json::from_value(arguments).context("invalid operation arguments")
}
fn decode_operation(operation: &str, arguments: Value) -> Result<Arguments> {
    if operation.starts_with("input.") {
        validate_input_shape(operation, &arguments)?;
    }
    macro_rules! decode_operation {
        ($($name:literal => $ty:ident),* $(,)?) => {
            match operation { $( $name => Ok(Arguments::$ty(decode(arguments)?)), )* _ => bail!("unknown operation {operation}; use api.schema") }
        };
    }
    inventory!(decode_operation)
}
fn schema<T: Args>() -> Value {
    let command = T::augment_args(clap::Command::new("operation"));
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str();
        if id == "stdin" {
            continue;
        } // CLI-only adapter, never MCP transport stdin.
        let tid = arg.get_value_parser().type_id();
        let kind = if tid == std::any::TypeId::of::<bool>()
            || matches!(
                arg.get_action(),
                clap::ArgAction::SetTrue | clap::ArgAction::SetFalse
            ) {
            "boolean"
        } else if [
            std::any::TypeId::of::<u8>(),
            std::any::TypeId::of::<u32>(),
            std::any::TypeId::of::<u64>(),
            std::any::TypeId::of::<usize>(),
        ]
        .iter()
        .any(|kind| tid == *kind)
        {
            "integer"
        } else {
            "string"
        };
        let mut property = json!({"type":kind});
        if kind == "string"
            && let Some(values) = arg.get_value_parser().possible_values()
        {
            property["enum"] = json!(
                values
                    .map(|value| value.get_name().to_owned())
                    .collect::<Vec<_>>()
            );
        }
        if matches!(arg.get_action(), clap::ArgAction::Append)
            || arg.get_num_args().is_some_and(|args| args.max_values() > 1)
        {
            property = json!({"type":"array","items":property});
        }
        if let Some(help) = arg.get_help() {
            property["description"] = json!(help.to_string());
        }
        properties.insert(id.to_owned(), property);
        if arg.is_required_set() {
            required.push(id);
        }
    }
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}
fn operation_schema(operation: &str) -> Result<Value> {
    macro_rules! lookup { ($($name:literal => $ty:ty),* $(,)?) => { match operation { $( $name => Ok::<Value, anyhow::Error>(schema::<$ty>()), )* _ => bail!("unknown control operation {operation}; use api.schema"), } }; }
    let mut output: Value = inventory!(lookup)?;
    if operation.starts_with("input.") {
        let field = input_field(operation)?;
        output["properties"]
            .as_object_mut()
            .expect("schema object")
            .retain(|key, _| {
                key == "session"
                    || key == "pane"
                    || key == field
                    || (field == "text" && key == "submit")
            });
        output["required"] = json!([field]);
        if field != "text" {
            output["properties"][field]["minItems"] = json!(1);
        }
    }
    if output["properties"].get("harness").is_some() {
        use clap::ValueEnum;
        output["properties"]["harness"]["enum"] = json!(
            crate::harness::Harness::value_variants()
                .iter()
                .filter_map(|harness| harness
                    .to_possible_value()
                    .map(|value| value.get_name().to_owned()))
                .collect::<Vec<_>>()
        );
        output["properties"]["harness"]["description"] = json!(
            "Launch via bkpr run with a fresh identity; only Claude/Codex/Omp currently support messaging readiness."
        );
    }
    if operation == "pane.read" {
        output["properties"]["tail"]["minimum"] = json!(0);
        output["properties"]["offset"]["minimum"] = json!(0);
    }
    if operation == "agent.launch" {
        output["required"] = json!(["harness"]);
    }
    if operation == "input.bytes" {
        output["properties"]["bytes"]["items"]["minimum"] = json!(0);
        output["properties"]["bytes"]["items"]["maximum"] = json!(255);
    }
    Ok(output)
}
fn names() -> Vec<&'static str> {
    macro_rules! list { ($($name:literal => $ty:ty),* $(,)?) => { vec![$($name),*] }; }
    inventory!(list)
}
pub(super) fn accepts_session(operation: &str) -> bool {
    operation != "session.list"
        && !operation.starts_with("api.")
        && !operation.starts_with("message.")
        && (!operation.starts_with("agent.")
            || matches!(operation, "agent.launch" | "agent.list" | "agent.ready"))
}
fn native(arguments: &Arguments, caller: &Caller, zellij: Option<&Path>) -> Result<Native> {
    Native::resolve(arguments.session(), caller, zellij)
}
fn pane_action(
    host: &Native,
    pane: Option<&str>,
    action: &str,
    tail: Vec<String>,
) -> Result<Value> {
    let mut argv = vec![action.to_owned(), "--pane-id".into(), host.pane(pane)?];
    argv.extend(tail);
    host.action(argv)
}
fn tab_action(
    host: &Native,
    tab: Option<u64>,
    workspace: Option<&str>,
    action: &str,
    tail: Vec<String>,
) -> Result<Value> {
    let tab = host.tab(tab, workspace, &host.snapshot()?)?;
    let mut argv = vec![action.to_owned(), "--tab-id".into(), tab.to_string()];
    argv.extend(tail);
    host.action(argv)
}
fn ensure(value: &str, allowed: &[&str], field: &str) -> Result<()> {
    if !allowed.contains(&value) {
        bail!("invalid {field} {value:?}; expected {}", allowed.join(", "));
    }
    Ok(())
}

fn relay(caller: &Caller, method: &str, params: Value) -> Result<Value> {
    if let Caller::Agent { agent_id, .. } = caller {
        let identity = crate::relay::request("whoami", json!({}))?;
        if identity["id"].as_str() != Some(agent_id.as_str()) {
            bail!("relay credential identity does not match authenticated control caller");
        }
    }
    crate::relay::request(method, params)
}

fn discover(args: AgentQuery, caller: &Caller, zellij: Option<&Path>) -> Result<Value> {
    let scope = args.scope.as_deref().unwrap_or("workspace");
    ensure(
        scope,
        &["workspace", "session", "all", "repository"],
        "scope",
    )?;
    if scope != "workspace" && args.workspace.is_some() {
        bail!("workspace selector requires workspace discovery scope");
    }
    if matches!(scope, "all" | "repository") && args.session.is_some() {
        bail!(
            "session selector cannot narrow all/repository discovery; choose session scope instead"
        );
    }
    let mut response = if matches!(caller, Caller::Agent { .. }) {
        if args.session.is_some() || args.workspace.is_some() {
            bail!(
                "authenticated discovery uses live caller context; choose scope, not a foreign context"
            );
        }
        relay(caller, "discover", json!({"scope":scope}))?
    } else {
        let mut status = crate::relay::status()?;
        let Value::Array(mut agents) = status["agents"].take() else {
            bail!("bridge status lacks agents");
        };
        agents.retain(|agent| agent["active"] == true);
        if let Ok(id) = std::env::var("BKPR_RELAY_AGENT_ID")
            && let Ok(current) = crate::context::current()
            && agents.iter().any(|agent| {
                agent["id"].as_str() == Some(id.as_str())
                    && agent["context"]["session_id"].as_str() == Some(current.session_id.as_str())
                    && agent["context"]["pane_id"].as_str() == Some(current.pane_id.as_str())
            })
        {
            agents.retain(|agent| agent["id"].as_str() != Some(id.as_str()));
        }
        if matches!(scope, "workspace" | "session") {
            let host = Native::resolve(args.session.as_deref(), caller, zellij)?;
            let workspace = if scope == "workspace" {
                Some(host.workspace(args.workspace.as_deref())?)
            } else {
                None
            };
            agents.retain(|agent| {
                agent["context"]["session_id"].as_str() == Some(host.session.session_id.as_str())
                    && workspace.as_ref().is_none_or(|workspace| {
                        agent["context"]["workspace_id"].as_str() == Some(workspace.as_str())
                    })
            });
        } else if scope == "repository" {
            let me = crate::relay::request("whoami", json!({}))?;
            let repository = me["repository"].as_str().context("repository discovery requires a registered caller with canonical repository metadata")?;
            agents.retain(|agent| agent["repository"].as_str() == Some(repository));
        }
        json!({"agents":agents,"scope":scope,"broker":status["broker"]})
    };
    let agents = response["agents"]
        .as_array_mut()
        .context("agent discovery lacks agents")?;
    if let Some(id) = args.agent_id {
        agents.retain(|agent| agent["id"].as_str() == Some(id.as_str()));
    }
    if !args.detail {
        for agent in agents {
            let mut compact = json!({});
            for key in [
                "id",
                "harness",
                "title",
                "attached",
                "active",
                "readiness",
                "accepting",
                "context",
            ] {
                if let Some(value) = agent.get(key) {
                    compact[key] = value.clone();
                }
            }
            *agent = compact;
        }
    }
    Ok(response)
}

pub(super) fn dispatch(
    operation: &str,
    arguments: Value,
    caller: &Caller,
    zellij: Option<&Path>,
) -> Result<Value> {
    // Validate before looking up any session or launching any native subprocess.
    let arguments = decode_operation(operation, arguments)?;
    match operation {
        "api.schema" | "api.help" => {
            let args: Api = parse(arguments)?;
            if let Some(operation) = args.operation {
                Ok(
                    json!({"operation":operation,"description":help(&operation),"arguments":operation_schema(&operation)?,"instructions":super::instructions()}),
                )
            } else {
                Ok(
                    json!({"protocol":1,"operations":names(),"instructions":super::instructions(),"scope":"live caller workspace by default; explicit session_scope for session-wide discovery","receipts":"baseline CLI acceptance and SDK dispatch are not atomic completion; never replay unknown automatically","exclusions":["persistent configuration","plugin loading","permission changes","absolute tiled dimensions"],"cli_only":["skill show/export","completion"]}),
                )
            }
        }
        "session.list" => {
            let args: SessionList = parse(arguments)?;
            let mut sessions = crate::context::live_sessions()?;
            if !args.all
                && let Caller::Agent { context, .. } = caller
            {
                sessions.retain(|entry| entry.session_id == context.session_id);
            }
            Ok(json!({"sessions":sessions}))
        }
        "capabilities" => {
            let args: Target = parse(arguments)?;
            if args.session.is_none()
                && matches!(caller, Caller::Operator)
                && std::env::var_os("BKPR_SESSION_ID").is_none()
            {
                return Ok(
                    json!({"protocol":1,"tools":["control","terminal_input"],"operations":names(),"native":"baseline CLI adapter; select --session to observe attached clients","owner_bridge":"requires matching permitted Bkpr owner plugin; not independently probed"}),
                );
            }
            let host = Native::resolve(args.session.as_deref(), caller, zellij)?;
            Ok(
                json!({"protocol":1,"tools":["control","terminal_input"],"operations":names(),"session_id":host.session.session_id,"native":"existing CLI actions and Bkpr owner SDK bridge","owner_bridge":"requires matching permitted Bkpr owner plugin; not independently probed","clients":host.clients()?,"receipts":"completed observations and dispatched mutations are distinct; snapshots are not atomic"}),
            )
        }
        "agent.list" | "agent.ready" => discover(parse(arguments)?, caller, zellij),
        "agent.whoami" => {
            let _: Empty = parse(arguments)?;
            relay(caller, "whoami", json!({}))
        }
        "agent.set_title" => {
            let args: Title = parse(arguments)?;
            relay(caller, "set_title", json!({"title":args.title}))
        }
        "message.send" => {
            let args: Send = parse(arguments)?;
            let mut params =
                json!({"to":args.to,"text":args.text,"idempotency_key":args.idempotency_key});
            if let Some(scope) = args.scope {
                params["scope"] = json!(scope);
            }
            relay(caller, "send", params)
        }
        "message.inbox" | "message.history" => {
            let args: MessagePage = parse(arguments)?;
            let mut params = json!({});
            if let Some(limit) = args.limit {
                params["limit"] = json!(limit);
            }
            if let Some(after) = args.after {
                params["after"] = json!(after);
            }
            relay(
                caller,
                if operation == "message.history" {
                    "outbox"
                } else {
                    "inbox"
                },
                params,
            )
        }
        "message.read" => {
            let args: MessageId = parse(arguments)?;
            if !matches!(caller, Caller::Operator) {
                bail!(
                    "retained message bodies are operator-only; agents read their own message.inbox"
                );
            }
            crate::relay::inspect_message(&args.message_id)
        }
        "message.ack" => {
            let args: MessageId = parse(arguments)?;
            relay(caller, "ack", json!({"ids":[args.message_id]}))
        }
        "message.status" => {
            let args: MessageId = parse(arguments)?;
            if matches!(caller, Caller::Operator)
                && std::env::var_os("BKPR_RELAY_AGENT_ID").is_none()
            {
                let status = crate::relay::status()?;
                let message = status["messages"]
                    .as_array()
                    .and_then(|messages| {
                        messages.iter().find(|message| {
                            message["id"] == args.message_id
                                || message["message_id"] == args.message_id
                        })
                    })
                    .context("unknown retained message")?;
                Ok(message.clone())
            } else {
                relay(caller, "outbox", json!({"message_id":args.message_id}))
            }
        }
        _ => dispatch_native(operation, arguments, caller, zellij),
    }
}
#[cfg(test)]
fn validate(operation: &str, arguments: Value) -> Result<()> {
    decode_operation(operation, arguments).map(|_| ())
}

fn focus_pane(
    snapshot: &Value,
    requested: Option<&str>,
    tab: Option<u64>,
    client: u32,
) -> Result<String> {
    let panes = snapshot["native"]["panes"]
        .as_array()
        .context("native focus preflight lacks pane topology")?;
    let eligible = |pane: &&Value| {
        pane["is_plugin"] == false
            && pane["is_floating"] == false
            && pane["is_suppressed"] == false
            && pane["is_selectable"] == true
    };
    let target = if let Some(pane) = requested {
        panes
            .iter()
            .find(|entry| entry["pane_id"].as_str() == Some(pane))
            .filter(eligible)
            .context("focus target must be a live selectable tiled terminal")?
    } else {
        let tab = tab.context("focus requires an exact pane or tab")?;
        panes
            .iter()
            .filter(|pane| pane["tab_id"].as_u64() == Some(tab))
            .filter(eligible)
            .min_by_key(|pane| pane["id"].as_u64())
            .context("target tab has no eligible tiled terminal for baseline client focus")?
    };
    let tab = target["tab_id"]
        .as_u64()
        .context("focus target lacks its native tab identity")?;
    if panes.iter().any(|pane| {
        pane["tab_id"].as_u64() == Some(tab)
            && pane["is_plugin"] == false
            && pane["is_floating"] == false
            && pane["is_fullscreen"] == true
    }) {
        bail!(
            "baseline selected-client focus cannot identify the owner of shared tiled fullscreen"
        );
    }
    if panes.iter().any(|pane| {
        pane["tab_id"].as_u64() == Some(tab)
            && pane["is_plugin"] == false
            && pane["is_floating"] == true
            && pane["is_selectable"] == true
    }) {
        bail!(
            "baseline selected-client focus cannot preserve shared floating visibility in a tab containing selectable floating terminals"
        );
    }
    let clients = snapshot["native"]["clients"]
        .as_array()
        .context("native focus preflight lacks client topology")?;
    if !clients
        .iter()
        .any(|peer| peer["client_id"].as_u64() == Some(u64::from(client)))
    {
        bail!("selected client is not attached during focus preflight");
    }
    for peer in clients {
        let peer_id = peer["client_id"]
            .as_u64()
            .context("native focus preflight has an unknown client identity")?;
        if peer_id == u64::from(client) {
            continue;
        }
        let pane = peer["pane_id"]
            .as_str()
            .context("native focus preflight lacks another client's exact pane")?;
        let peer_tab = panes
            .iter()
            .find(|entry| entry["pane_id"].as_str() == Some(pane))
            .and_then(|entry| entry["tab_id"].as_u64())
            .context("cannot prove another client's tab binding; no focus was dispatched")?;
        if peer_tab == tab {
            bail!(
                "baseline stacked-pane focus can affect other clients viewing the target tab; no focus was dispatched"
            );
        }
    }
    target["pane_id"]
        .as_str()
        .map(str::to_owned)
        .context("focus target lacks its exact pane identity")
}

fn dispatch_native(
    operation: &str,
    arguments: Arguments,
    caller: &Caller,
    zellij: Option<&Path>,
) -> Result<Value> {
    let host = native(&arguments, caller, zellij)?;
    match operation {
        "session.inspect" => Ok(json!({"outcome":"completed","result":host.snapshot()?})),
        "session.stop" => host.stop(),
        "session.save" => host.action(vec!["save-session".into()]),
        "client.list" => Ok(json!({"clients":host.snapshot()?["native"]["clients"]})),
        "client.detach" => {
            let args: Client = parse(arguments)?;
            host.call("client.detach", json!({"client_id":args.client}))
        }
        "client.mode" => {
            let args: ClientMode = parse(arguments)?;
            host.call(
                "client.mode",
                json!({"client_id":args.client,"mode":args.mode}),
            )
        }
        "client.focus" | "pane.focus" | "tab.focus" => {
            let args: Focus = parse(arguments)?;
            if args.pane.is_some() && (args.tab.is_some() || args.workspace.is_some()) {
                bail!("focus accepts a pane OR a tab/workspace, not both");
            }
            let snapshot = host.snapshot()?;
            let pane = if args.pane.is_some() || operation == "pane.focus" {
                Some(host.pane(args.pane.as_deref())?)
            } else {
                None
            };
            let tab = if pane.is_none() {
                Some(host.tab(args.tab, args.workspace.as_deref(), &snapshot)?)
            } else {
                None
            };
            let target = focus_pane(&snapshot, pane.as_deref(), tab, args.client)?;
            host.call(
                "client.focus",
                json!({"client_id":args.client,"pane_id":target}),
            )
        }
        "pane.list" | "tab.list" | "workspace.list" => {
            let args: Discovery = parse(arguments)?;
            if args.session_scope && args.workspace.is_some() {
                bail!("session_scope and workspace are mutually exclusive");
            }
            let mut snapshot = host.snapshot()?;
            let workspace = if args.session_scope {
                None
            } else {
                Some(host.workspace_in(args.workspace.as_deref(), &snapshot)?)
            };
            let collection = if operation == "pane.list" {
                "panes"
            } else {
                "tabs"
            };
            let mut entries = std::mem::take(
                snapshot[collection]
                    .as_array_mut()
                    .context("native snapshot collection missing")?,
            );
            if let Some(workspace) = &workspace {
                entries.retain(|entry| entry["workspace_id"].as_str() == Some(workspace.as_str()));
            }
            if operation == "workspace.list" {
                entries.retain(|entry| entry["workspace_id"].is_string());
                let mut seen = std::collections::BTreeSet::new();
                entries.retain(|entry| {
                    seen.insert(
                        entry["workspace_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    )
                });
                for entry in &mut entries {
                    entry
                        .as_object_mut()
                        .context("native workspace binding is not an object")?
                        .retain(|key, _| key == "workspace_id" || key == "workspace_name");
                }
            }
            Ok(json!({"session_id":host.session.session_id,"scope":workspace,"items":entries}))
        }
        "pane.inspect" => {
            let args: Pane = parse(arguments)?;
            let pane = host.pane(args.pane.as_deref())?;
            let snapshot = host.snapshot()?;
            let binding = snapshot["panes"]
                .as_array()
                .and_then(|panes| {
                    panes
                        .iter()
                        .find(|entry| entry["pane_id"].as_str() == Some(&pane))
                })
                .context("pane disappeared")?;
            let native = snapshot["native"]["panes"]
                .as_array()
                .and_then(|panes| {
                    panes
                        .iter()
                        .find(|entry| entry["pane_id"].as_str() == Some(pane.as_str()))
                })
                .context("native pane disappeared")?;
            Ok(json!({"binding":binding,"native":native}))
        }
        "workspace.inspect" => {
            let args: Workspace = parse(arguments)?;
            let snapshot = host.snapshot()?;
            let workspace = host.workspace_in(args.workspace.as_deref(), &snapshot)?;
            Ok(
                json!({"workspace_id":workspace,"tabs":snapshot["tabs"].as_array().context("missing tab bindings")?.iter().filter(|entry| entry["workspace_id"].as_str()==Some(&workspace)).collect::<Vec<_>>(),"panes":snapshot["panes"].as_array().context("missing pane bindings")?.iter().filter(|entry| entry["workspace_id"].as_str()==Some(&workspace)).collect::<Vec<_>>()}),
            )
        }
        "tab.inspect" => {
            let args: Tab = parse(arguments)?;
            let snapshot = host.snapshot()?;
            let tab = host.tab(args.tab, args.workspace.as_deref(), &snapshot)?;
            let binding = snapshot["tabs"]
                .as_array()
                .and_then(|tabs| {
                    tabs.iter()
                        .find(|entry| entry["tab_id"].as_u64() == Some(tab))
                })
                .context("native tab binding disappeared")?;
            let native = snapshot["native"]["tabs"]
                .as_array()
                .and_then(|tabs| {
                    tabs.iter()
                        .find(|entry| entry["tab_id"].as_u64() == Some(tab))
                })
                .context("native tab disappeared")?;
            Ok(json!({"tab_id":tab,"binding":binding,"native":native}))
        }
        "workspace.create" => create(
            operation,
            parse::<WorkspaceCreate>(arguments)?.into(),
            &host,
        ),
        "tab.create" => create(operation, parse::<TabCreate>(arguments)?.into(), &host),
        "pane.create" | "pane.split" | "agent.launch" => {
            create(operation, parse(arguments)?, &host)
        }
        "workspace.rename" => {
            let args: RenameWorkspace = parse(arguments)?;
            host.call(
                "workspace.rename",
                json!({"workspace_id":host.workspace(args.workspace.as_deref())?,"name":args.name}),
            )
        }
        "workspace.close" => {
            let args: Workspace = parse(arguments)?;
            host.call(
                "workspace.close",
                json!({"workspace_id":host.workspace(args.workspace.as_deref())?}),
            )
        }
        "pane.close" | "pane.clear" | "pane.unrename" => {
            let args: Pane = parse(arguments)?;
            pane_action(
                &host,
                args.pane.as_deref(),
                match operation {
                    "pane.close" => "close-pane",
                    "pane.clear" => "clear",
                    _ => "undo-rename-pane",
                },
                vec![],
            )
        }
        "pane.rename" => {
            let args: RenamePane = parse(arguments)?;
            pane_action(
                &host,
                args.pane.as_deref(),
                "rename-pane",
                vec!["--".into(), args.name],
            )
        }
        "pane.resize" => {
            let args: Resize = parse(arguments)?;
            ensure(&args.change, &["increase", "decrease"], "change")?;
            let mut tail = vec![args.change];
            if let Some(direction) = args.direction {
                ensure(&direction, &["left", "right", "up", "down"], "direction")?;
                tail.push(direction);
            }
            pane_action(&host, args.pane.as_deref(), "resize", tail)
        }
        "pane.geometry" => {
            let args: Geometry = parse(arguments)?;
            let mut tail = Vec::new();
            for (key, value) in [
                ("x", args.x),
                ("y", args.y),
                ("width", args.width),
                ("height", args.height),
            ] {
                if let Some(value) = value {
                    coordinate(&value, key == "width" || key == "height")?;
                    tail.extend([format!("--{key}"), value]);
                }
            }
            if tail.is_empty() {
                bail!("geometry requires at least one coordinate");
            }
            pane_action(
                &host,
                args.pane.as_deref(),
                "change-floating-pane-coordinates",
                tail,
            )
        }
        "pane.move" => {
            let args: Move = parse(arguments)?;
            let pane = host.pane(args.pane.as_deref())?;
            let direction = args.direction.as_deref();
            if let Some(direction) = direction {
                ensure(
                    direction,
                    &["left", "right", "up", "down", "backward"],
                    "direction",
                )?;
            }
            pane_action(
                &host,
                Some(&pane),
                if direction == Some("backward") {
                    "move-pane-backwards"
                } else {
                    "move-pane"
                },
                direction
                    .filter(|value| *value != "backward")
                    .map(|value| vec![value.into()])
                    .unwrap_or_default(),
            )
        }
        "pane.pin" | "pane.border" => {
            let args: PaneState = parse(arguments)?;
            let mut params = json!({"pane_id":host.pane(args.pane.as_deref())?});
            let field = if operation == "pane.pin" {
                "pinned"
            } else {
                "borderless"
            };
            params[field] = json!(if operation == "pane.border" {
                !args.enabled
            } else {
                args.enabled
            });
            host.call("pane.set_state", params)
        }
        "pane.scroll" => {
            let args: Scroll = parse(arguments)?;
            let action = match args.direction.as_str() {
                "up" => "scroll-up",
                "down" => "scroll-down",
                "top" => "scroll-to-top",
                "bottom" => "scroll-to-bottom",
                "page-up" => "page-scroll-up",
                "page-down" => "page-scroll-down",
                "half-up" => "half-page-scroll-up",
                "half-down" => "half-page-scroll-down",
                _ => bail!("invalid scroll direction"),
            };
            pane_action(&host, args.pane.as_deref(), action, vec![])
        }
        "pane.read" => {
            let args: Read = parse(arguments)?;
            let pane = host.pane(args.pane.as_deref())?;
            host.read(&pane, args.full, args.ansi, args.tail, args.offset)
        }
        "tab.close" | "tab.unrename" => {
            let args: Tab = parse(arguments)?;
            let tab = host.tab(args.tab, args.workspace.as_deref(), &host.snapshot()?)?;
            if operation == "tab.close" {
                host.action(vec!["close-tab-by-id".into(), tab.to_string()])
            } else {
                tab_action(&host, Some(tab), None, "undo-rename-tab", vec![])
            }
        }
        "tab.rename" => {
            let args: RenameTab = parse(arguments)?;
            let tab = host.tab(args.tab, args.workspace.as_deref(), &host.snapshot()?)?;
            host.action(vec![
                "rename-tab-by-id".into(),
                tab.to_string(),
                "--".into(),
                args.name,
            ])
        }
        "tab.move" => {
            let args: TabMove = parse(arguments)?;
            ensure(&args.direction, &["left", "right"], "direction")?;
            tab_action(
                &host,
                args.tab,
                args.workspace.as_deref(),
                "move-tab",
                vec![args.direction],
            )
        }
        "tab.sync" | "tab.floating" => {
            let args: TabState = parse(arguments)?;
            if operation == "tab.sync" {
                let snapshot = host.snapshot()?;
                let tab = host.tab(args.tab, args.workspace.as_deref(), &snapshot)?;
                let synced = snapshot["native"]["tabs"]
                    .as_array()
                    .and_then(|tabs| {
                        tabs.iter()
                            .find(|entry| entry["tab_id"].as_u64() == Some(tab))
                    })
                    .and_then(|entry| entry["is_sync_panes_active"].as_bool())
                    .context("native tab sync state is unavailable")?;
                if synced == args.enabled {
                    Ok(
                        json!({"outcome":"completed","result":{"tab_id":tab,"enabled":synced,"observation":"already in requested state at snapshot time"}}),
                    )
                } else {
                    let mut receipt = host.action(vec![
                        "toggle-active-sync-tab".into(),
                        "--tab-id".into(),
                        tab.to_string(),
                    ])?;
                    receipt["consistency"] = json!(
                        "read then exact-tab toggle; concurrent native changes are not atomic"
                    );
                    Ok(receipt)
                }
            } else {
                tab_action(
                    &host,
                    args.tab,
                    args.workspace.as_deref(),
                    if args.enabled {
                        "show-floating-panes"
                    } else {
                        "hide-floating-panes"
                    },
                    vec![],
                )
            }
        }
        "layout.dump" => host.action(vec!["dump-layout".into()]),
        "layout.cycle" => {
            let args: LayoutCycle = parse(arguments)?;
            ensure(&args.direction, &["next", "previous"], "direction")?;
            tab_action(
                &host,
                args.tab,
                args.workspace.as_deref(),
                if args.direction == "next" {
                    "next-swap-layout"
                } else {
                    "previous-swap-layout"
                },
                vec![],
            )
        }
        operation if operation.starts_with("input.") => input(operation, parse(arguments)?, &host),
        _ => bail!("unknown operation {operation}"),
    }
}
fn coordinate(value: &str, positive: bool) -> Result<()> {
    let (number, percent) = value
        .strip_suffix('%')
        .map(|value| (value, true))
        .unwrap_or((value, false));
    let number: u32 = number
        .parse()
        .context("coordinate must be a nonnegative integer or percentage")?;
    if (positive && number == 0) || (percent && number > 100) {
        bail!("coordinate out of range");
    }
    Ok(())
}
fn create(operation: &str, args: Create, host: &Native) -> Result<Value> {
    if args.new_workspace && args.new_tab {
        bail!("new_workspace and new_tab are mutually exclusive");
    }
    let new_workspace = operation == "workspace.create" || args.new_workspace;
    let tab_create = matches!(operation, "workspace.create" | "tab.create")
        || args.new_workspace
        || args.new_tab;
    if operation == "agent.launch" && args.harness.is_none() {
        bail!("agent.launch requires --harness");
    }
    if let Some(direction) = &args.direction {
        ensure(direction, &["right", "down"], "direction")?;
    }
    let layers =
        usize::from(args.floating) + usize::from(args.stacked) + usize::from(args.in_place);
    if layers > 1 || (args.direction.is_some() && layers > 0) {
        bail!("floating, stacked, in_place and directional split are mutually exclusive");
    }
    if args.close_replaced && !args.in_place {
        bail!("close_replaced requires in_place");
    }
    let mut argv = if let Some(harness) = args.harness {
        use clap::ValueEnum;
        let parsed =
            crate::harness::Harness::from_str(&harness, true).map_err(anyhow::Error::msg)?;
        let name = parsed
            .to_possible_value()
            .expect("harness has CLI name")
            .get_name()
            .to_owned();
        let mut argv = vec![
            std::env::current_exe()?
                .to_str()
                .context("bkpr executable path is not UTF-8")?
                .to_owned(),
            "run".into(),
            name,
            "--".into(),
        ];
        argv.extend(args.argv);
        argv
    } else {
        args.argv
    };
    let mut command = vec![
        if tab_create { "new-tab" } else { "new-pane" }.to_owned(),
        "--no-focus".into(),
    ];
    let mut anchor = None;
    if tab_create {
        if args.tab.is_some()
            || args.pane.is_some()
            || args.floating
            || args.stacked
            || args.in_place
            || args.direction.is_some()
        {
            bail!("new workspace/tab cannot target an existing tab/pane or pane placement");
        }
        if new_workspace && args.workspace.is_some() {
            bail!("new workspace cannot also target an existing workspace");
        }
        let workspace_snapshot = if new_workspace {
            None
        } else {
            Some(host.snapshot()?)
        };
        let workspace = workspace_snapshot
            .as_ref()
            .map(|snapshot| host.workspace_in(args.workspace.as_deref(), snapshot))
            .transpose()?;
        let label = if let Some(workspace) = &workspace {
            workspace_snapshot
                .as_ref()
                .expect("existing workspace snapshot")["tabs"]
                .as_array()
                .and_then(|tabs| {
                    tabs.iter()
                        .find(|entry| entry["workspace_id"].as_str() == Some(workspace.as_str()))
                })
                .and_then(|entry| entry["workspace_name"].as_str())
                .context("workspace disappeared")?
        } else {
            args.name.as_deref().unwrap_or("Workspace")
        };
        let cwd = args.cwd.as_deref().map(str::to_owned).unwrap_or(
            std::env::current_dir()?
                .to_str()
                .context("cwd is not UTF-8")?
                .to_owned(),
        );
        let layout = crate::launcher::control_workspace_layout(
            &host.session.runtime_name,
            label,
            &cwd,
            workspace.as_deref(),
        )?;
        command.extend(["--layout-string".into(), layout, "--cwd".into(), cwd]);
    } else {
        if args.in_place {
            command.push("--in-place".into());
            command.extend(["--pane-id".into(), host.pane(args.pane.as_deref())?]);
            if args.close_replaced {
                command.push("--close-replaced-pane".into());
            }
        } else if args.pane.is_some()
            || operation == "pane.split"
            || args.direction.is_some()
            || args.stacked
        {
            if args.tab.is_some() || args.workspace.is_some() {
                bail!("relative pane creation cannot also target a tab/workspace");
            }
            anchor = Some(host.pane(args.pane.as_deref())?);
            command.push("--near-current-pane".into());
        } else {
            let tab = host.tab(args.tab, args.workspace.as_deref(), &host.snapshot()?)?;
            command.extend(["--tab-id".into(), tab.to_string()]);
        }
        if let Some(cwd) = args.cwd {
            command.extend(["--cwd".into(), cwd]);
        }
        if args.floating {
            command.push("--floating".into());
        }
        if args.stacked {
            command.push("--stacked".into());
        }
        if let Some(direction) = args.direction {
            command.extend(["--direction".into(), direction]);
        }
    }
    if let Some(name) = args.name {
        command.extend(["--name".into(), name]);
    }
    if args.close_on_exit {
        if argv.is_empty() {
            bail!("close_on_exit requires argv or harness");
        }
        command.push("--close-on-exit".into());
    }
    if !argv.is_empty() {
        command.push("--".into());
        command.append(&mut argv);
    }
    let mut receipt = if let Some(anchor) = anchor {
        host.action_from(&anchor, command)?
    } else {
        host.action(command)?
    };
    if matches!(receipt["outcome"].as_str(), Some("unknown" | "failed")) {
        return Ok(receipt);
    }
    // Bind only by IDs in the native creation receipt, never by listing differences.
    let created = result(&receipt)?;
    let tab = created["tab_id"].as_u64();
    let pane = created["pane_id"].as_str().map(str::to_owned);
    if tab.is_some() || pane.is_some() {
        match host.snapshot() {
            Ok(snapshot) => {
                let binding = if let Some(pane) = pane {
                    snapshot["panes"].as_array().and_then(|panes| {
                        panes
                            .iter()
                            .find(|entry| entry["pane_id"].as_str() == Some(pane.as_str()))
                    })
                } else {
                    snapshot["tabs"]
                        .as_array()
                        .and_then(|tabs| tabs.iter().find(|entry| entry["tab_id"].as_u64() == tab))
                };
                if let Some(binding) = binding {
                    receipt["binding"] = binding.clone();
                }
            }
            Err(error) => receipt["binding_diagnostic"] = json!(error.to_string()),
        }
    }
    receipt["readiness"] = json!({"native_creation":receipt["outcome"],"process_start":"not independently observed","relay_registration":"not independently observed","notification_readiness":"not independently observed","prompt_readiness":"unknown"});
    Ok(receipt)
}
fn input_field(operation: &str) -> Result<&'static str> {
    match operation {
        "input.text" | "input.paste" => Ok("text"),
        "input.keys" => Ok("keys"),
        "input.bytes" => Ok("bytes"),
        _ => bail!("unknown terminal input operation"),
    }
}
fn validate_input_shape(operation: &str, arguments: &Value) -> Result<()> {
    let field = input_field(operation)?;
    let fields = arguments
        .as_object()
        .context("input arguments must be an object")?;
    for key in fields.keys() {
        if key != "session"
            && key != "pane"
            && key != field
            && !(field == "text" && key == "submit")
        {
            bail!("field {key} is not accepted by {operation}");
        }
    }
    let payload = fields
        .get(field)
        .context("terminal input payload is required")?;
    if field == "text" {
        if !payload.is_string() {
            bail!("text must be a literal UTF-8 string");
        }
    } else if payload.as_array().is_none_or(|values| values.is_empty()) {
        bail!("{field} must be a nonempty array");
    }
    Ok(())
}
fn input(operation: &str, args: Input, host: &Native) -> Result<Value> {
    let kind = operation.trim_start_matches("input.");
    match kind {
        "text" | "paste" => {
            if args.text.is_none() || !args.keys.is_empty() || !args.bytes.is_empty() {
                bail!("text/paste requires text and rejects keys/bytes");
            }
        }
        "keys" => {
            if args.text.is_some() || args.keys.is_empty() || !args.bytes.is_empty() || args.submit
            {
                bail!("keys requires a nonempty keys array; rejects text/bytes/submit");
            }
        }
        "bytes" => {
            if args.text.is_some() || !args.keys.is_empty() || args.bytes.is_empty() || args.submit
            {
                bail!("bytes requires a nonempty byte array; rejects text/keys/submit");
            }
        }
        _ => bail!("unknown terminal input kind"),
    }
    let pane = host.pane(args.pane.as_deref())?;
    match kind {
        "text" => {
            let mut text = args.text.expect("validated text");
            if args.submit {
                text.push('\r');
            }
            host.action(vec![
                "write-chars".into(),
                "--pane-id".into(),
                pane,
                "--".into(),
                text,
            ])
        }
        "paste" => {
            let mut receipt = host.action(vec![
                "paste".into(),
                "--pane-id".into(),
                pane.clone(),
                "--".into(),
                args.text.expect("validated paste"),
            ])?;
            if args.submit && receipt["outcome"] == "dispatched" {
                match host.action(vec!["write".into(), "--pane-id".into(), pane, "13".into()]) {
                    Ok(enter) if enter["outcome"] == "dispatched" => {
                        receipt["result"]["submit"] = enter;
                    }
                    Ok(enter) => {
                        receipt["outcome"] = json!("unknown");
                        receipt["error"] = json!({"stage":"submit","message":"paste accepted, Enter not confirmed; do not replay paste","submit_receipt":enter});
                    }
                    Err(error) => {
                        receipt["outcome"] = json!("unknown");
                        receipt["error"] = json!({"stage":"submit","message":format!("paste accepted, Enter completion unknown: {error}; do not replay paste")});
                    }
                }
            }
            receipt["atomic"] = json!(false);
            Ok(receipt)
        }
        "keys" => {
            let mut argv = vec!["send-keys".into(), "--pane-id".into(), pane, "--".into()];
            argv.extend(args.keys);
            host.action(argv)
        }
        _ => {
            let mut argv = vec!["write".into(), "--pane-id".into(), pane];
            argv.extend(args.bytes.iter().map(u8::to_string));
            host.action(argv)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_input_boundaries_reject_cross_kind_fields_and_preserve_literal_text() {
        assert!(validate("input.text", json!({"text":"hello","keys":[]})).is_err());
        assert!(validate("input.keys", json!({"keys":["Enter"],"submit":true})).is_err());
        assert!(validate("input.bytes", json!({"bytes":[256]})).is_err());
        assert!(validate("input.text", json!({"stdin":true})).is_err());
        let literal = "a\\n$(never-interpolate)\n雪";
        let arguments: Input =
            parse(decode_operation("input.paste", json!({"text":literal,"submit":true})).unwrap())
                .unwrap();
        assert_eq!(arguments.text.as_deref(), Some(literal));
        assert!(arguments.submit);
        let schema = operation_schema("input.bytes").unwrap();
        assert!(schema["properties"].get("text").is_none());
        assert!(schema["properties"].get("submit").is_none());
    }
    use super::*;
    #[test]
    fn typed_requests_reject_foreign_authority_and_unknown_fields() {
        assert!(
            validate(
                "pane.close",
                json!({"pane":"terminal_1","owner_token":"secret"})
            )
            .is_err()
        );
        assert!(
            validate(
                "pane.resize",
                json!({"pane":"terminal_1","change":"increase","columns":80})
            )
            .is_err()
        );
        assert!(
            validate(
                "message.send",
                json!({"to":"a","text":"hello","token":"forged"})
            )
            .is_err()
        );
        assert!(validate("client.focus", json!({"pane":"terminal_1"})).is_err());
        assert!(validate("workspace.create", json!({"floating":true})).is_err());
    }
    #[test]
    fn geometry_preserves_native_cells_and_percent_contract() {
        assert!(coordinate("0", false).is_ok());
        assert!(coordinate("0", true).is_err());
        assert!(coordinate("101%", false).is_err());
        assert!(coordinate("80%", true).is_ok());
        assert!(coordinate("-3", false).is_err());
    }
    #[test]
    fn operation_schema_is_progressive_and_excludes_unknown_fields() {
        let schema = operation_schema("pane.create").unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["argv"]["type"], "array");
        assert!(schema["properties"].get("owner_token").is_none());
    }
    #[test]
    fn deferred_operations_are_absent_and_reads_expose_bottom_relative_ranges() {
        for operation in [
            "session.create",
            "session.rename",
            "layout.apply",
            "pane.swap",
            "pane.watch",
            "pane.float",
            "pane.embed",
            "pane.fullscreen",
        ] {
            assert!(!names().contains(&operation));
            assert!(operation_schema(operation).is_err());
            assert!(decode_operation(operation, json!({})).is_err());
        }
        assert!(validate("pane.move", json!({"tab":3})).is_err());
        assert!(validate("pane.move", json!({"workspace":"other"})).is_err());
        let schema = operation_schema("pane.read").unwrap();
        assert_eq!(schema["properties"]["tail"]["type"], "integer");
        assert_eq!(schema["properties"]["offset"]["type"], "integer");
        let args: Read =
            parse(decode_operation("pane.read", json!({"tail":0,"offset":usize::MAX})).unwrap())
                .unwrap();
        assert_eq!(args.tail, Some(0));
        assert_eq!(args.offset, Some(usize::MAX));
    }

    #[test]
    fn client_focus_preflight_rejects_hidden_or_visible_floating_siblings() {
        let tiled = json!({"id":9,"pane_id":"terminal_9","tab_id":3,"is_plugin":false,"is_floating":false,"is_suppressed":false,"is_selectable":true});
        let snapshot = json!({"native":{"panes":[tiled.clone()],"clients":[{"client_id":1,"pane_id":"terminal_9"}]}});
        assert_eq!(
            focus_pane(&snapshot, Some("terminal_9"), None, 1).unwrap(),
            "terminal_9"
        );
        assert_eq!(
            focus_pane(&snapshot, None, Some(3), 1).unwrap(),
            "terminal_9"
        );
        assert!(focus_pane(&snapshot, None, Some(4), 1).is_err());
        for suppressed in [false, true] {
            let floating = json!({"id":10,"pane_id":"terminal_10","tab_id":3,"is_plugin":false,"is_floating":true,"is_suppressed":suppressed,"is_selectable":true});
            let snapshot = json!({"native":{"panes":[tiled.clone(),floating],"clients":[{"client_id":1,"pane_id":"terminal_9"}]}});
            assert!(focus_pane(&snapshot, Some("terminal_9"), None, 1).is_err());
            assert!(focus_pane(&snapshot, Some("terminal_10"), None, 1).is_err());
            assert!(focus_pane(&snapshot, None, Some(3), 1).is_err());
        }
        let mut fullscreen = snapshot;
        fullscreen["native"]["panes"][0]["is_fullscreen"] = json!(true);
        assert!(focus_pane(&fullscreen, Some("terminal_9"), None, 1).is_err());
        assert!(focus_pane(&fullscreen, None, Some(3), 1).is_err());
    }

    #[test]
    fn client_focus_rejects_only_other_clients_viewing_target_tab_and_unknown_bindings() {
        let target = json!({"id":9,"pane_id":"terminal_9","tab_id":3,"is_plugin":false,"is_floating":false,"is_suppressed":false,"is_selectable":true});
        let other = json!({"id":10,"pane_id":"terminal_10","tab_id":4,"is_plugin":false,"is_floating":false,"is_suppressed":false,"is_selectable":true});
        let mut snapshot = json!({"native":{"panes":[target,other],"clients":[{"client_id":1,"pane_id":"terminal_10"},{"client_id":2,"pane_id":"terminal_10"}]}});
        assert_eq!(
            focus_pane(&snapshot, Some("terminal_9"), None, 1).unwrap(),
            "terminal_9"
        );
        assert_eq!(
            focus_pane(&snapshot, None, Some(3), 1).unwrap(),
            "terminal_9"
        );
        snapshot["native"]["clients"][1]["pane_id"] = json!("terminal_9");
        assert!(focus_pane(&snapshot, Some("terminal_9"), None, 1).is_err());
        assert!(focus_pane(&snapshot, None, Some(3), 1).is_err());
        snapshot["native"]["clients"][1]["pane_id"] = json!("terminal_999");
        assert!(focus_pane(&snapshot, Some("terminal_9"), None, 1).is_err());
    }
}
