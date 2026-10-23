//! The CLI and both MCP tools deserialize the same operation-specific records.
use clap::Args;
use serde::{Deserialize, Serialize};

macro_rules! record {
    ($name:ident { $($(#[$attr:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Args, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct $name {
            /// Stable session_key or canonical session_id; live display labels are not selectors.
            #[arg(long)]
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub session: Option<String>,
            $( $(#[$attr])* pub $field: $ty, )*
        }
    };
}
record!(Target {});
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionList {
    /// Explicitly discover other same-user live sessions (agents default to their own).
    #[arg(long)]
    #[serde(default)]
    pub all: bool,
}
record!(SessionCreate {
    /// Initial directory for the new managed shell.
    #[arg(long)] #[serde(default)] cwd: Option<String>,
});
record!(RenameSession {
    /// New display label; canonical session identity and native socket stay unchanged.
    #[arg(long)]
    name: String,
});
record!(Discovery {
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    /// Discover the entire selected session rather than the caller's workspace.
    #[arg(long)] #[serde(default)] session_scope: bool,
});
record!(Pane {
    #[arg(long)] #[serde(default)] pane: Option<String>,
});
record!(Swap {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    /// The other pane in the same native tab and layer.
    #[arg(long)] other_pane: String,
});
record!(Tab {
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
});
record!(Workspace {
    #[arg(long)] #[serde(default)] workspace: Option<String>,
});
record!(Create {
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long)] #[serde(default)] cwd: Option<String>,
    #[arg(long)] #[serde(default)] name: Option<String>,
    #[arg(long, value_parser = ["right", "down"])] #[serde(default)] direction: Option<String>,
    #[arg(long)] #[serde(default)] floating: bool,
    #[arg(long)] #[serde(default)] stacked: bool,
    #[arg(long)] #[serde(default)] in_place: bool,
    #[arg(long)] #[serde(default)] close_replaced: bool,
    #[arg(long)] #[serde(default)] close_on_exit: bool,
    #[arg(long)] #[serde(default)] harness: Option<String>,
    /// Place the new process in a fresh Bkpr workspace, without focusing it.
    #[arg(long)] #[serde(default)] new_workspace: bool,
    /// Place the new process in another tab of the selected Bkpr workspace.
    #[arg(long)] #[serde(default)] new_tab: bool,
    /// Exact executable and arguments, or harness arguments with --harness. Must follow --; no shell interpolation.
    #[arg(last = true)] #[serde(default)] argv: Vec<String>,
});
record!(WorkspaceCreate {
    #[arg(long)] #[serde(default)] cwd: Option<String>,
    #[arg(long)] #[serde(default)] name: Option<String>,
    #[arg(long)] #[serde(default)] harness: Option<String>,
    #[arg(long)] #[serde(default)] close_on_exit: bool,
    /// First terminal's executable and arguments, or harness arguments with --harness, after --. Omitted means a shell unless --harness is set.
    #[arg(last = true)] #[serde(default)] argv: Vec<String>,
});
record!(TabCreate {
    /// Existing workspace ID or unique label; defaults to the caller's workspace.
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long)] #[serde(default)] cwd: Option<String>,
    #[arg(long)] #[serde(default)] name: Option<String>,
    #[arg(long)] #[serde(default)] harness: Option<String>,
    #[arg(long)] #[serde(default)] close_on_exit: bool,
    /// First terminal's executable and arguments, or harness arguments with --harness, after --. Omitted means a shell unless --harness is set.
    #[arg(last = true)] #[serde(default)] argv: Vec<String>,
});
impl Create {
    fn tab(
        session: Option<String>,
        workspace: Option<String>,
        cwd: Option<String>,
        name: Option<String>,
        harness: Option<String>,
        argv: Vec<String>,
        close_on_exit: bool,
    ) -> Self {
        Self {
            session,
            workspace,
            cwd,
            name,
            harness,
            argv,
            close_on_exit,
            tab: None,
            pane: None,
            direction: None,
            floating: false,
            stacked: false,
            in_place: false,
            close_replaced: false,
            new_workspace: false,
            new_tab: false,
        }
    }
}
impl From<WorkspaceCreate> for Create {
    fn from(args: WorkspaceCreate) -> Self {
        Self::tab(
            args.session,
            None,
            args.cwd,
            args.name,
            args.harness,
            args.argv,
            args.close_on_exit,
        )
    }
}
impl From<TabCreate> for Create {
    fn from(args: TabCreate) -> Self {
        Self::tab(
            args.session,
            args.workspace,
            args.cwd,
            args.name,
            args.harness,
            args.argv,
            args.close_on_exit,
        )
    }
}
record!(RenamePane {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long)] name: String,
});
record!(RenameWorkspace {
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long)] name: String,
});
record!(RenameTab {
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long)] name: String,
});
record!(Resize {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    /// Native increment/decrement, not absolute tiled dimensions.
    #[arg(long, value_parser = ["increase", "decrease"])] change: String,
    #[arg(long, value_parser = ["left", "right", "up", "down"])] #[serde(default)] direction: Option<String>,
});
record!(Move {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long, value_parser = ["left", "right", "up", "down", "backward"])] #[serde(default)] direction: Option<String>,
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
});
record!(Geometry {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long)] #[serde(default)] x: Option<String>,
    #[arg(long)] #[serde(default)] y: Option<String>,
    #[arg(long)] #[serde(default)] width: Option<String>,
    #[arg(long)] #[serde(default)] height: Option<String>,
});
record!(PaneState {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long, action = clap::ArgAction::Set, required = true)] enabled: bool,
});
record!(Scroll {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long, value_parser = ["up", "down", "top", "bottom", "page-up", "page-down", "half-up", "half-down"])] direction: String,
});
record!(Read {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long)] #[serde(default)] full: bool,
    #[arg(long)] #[serde(default)] ansi: bool,
});
record!(TabMove {
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long, value_parser = ["left", "right"])] direction: String,
});
record!(TabState {
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long, action = clap::ArgAction::Set, required = true)] enabled: bool,
});
record!(Layout {
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    /// Native KDL layout text. No shell command interpolation.
    #[arg(long)] layout: String,
});
record!(LayoutCycle {
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
    #[arg(long, value_parser = ["next", "previous"])] direction: String,
});
record!(Client {
    #[arg(long)]
    client: u32,
});
record!(ClientMode {
    #[arg(long)]
    client: u32,
    #[arg(long)]
    mode: String,
});
record!(Focus {
    #[arg(long)] client: u32,
    #[arg(long)] #[serde(default)] pane: Option<String>,
    #[arg(long)] #[serde(default)] tab: Option<u64>,
    #[arg(long)] #[serde(default)] workspace: Option<String>,
});
record!(Input {
    #[arg(long)] #[serde(default)] pane: Option<String>,
    /// Read literal text from CLI stdin (never accepted over MCP).
    #[arg(long)] #[serde(skip)] stdin: bool,
    #[arg(long)] #[serde(default, skip_serializing_if = "Option::is_none")] text: Option<String>,
    #[arg(long, num_args = 1..)] #[serde(default, skip_serializing_if = "Vec::is_empty")] keys: Vec<String>,
    #[arg(long, num_args = 1..)] #[serde(default, skip_serializing_if = "Vec::is_empty")] bytes: Vec<u8>,
    /// Append Enter atomically after text/paste. Acceptance is not agent processing.
    #[arg(long)] #[serde(default, skip_serializing_if = "disabled")] submit: bool,
});
fn disabled(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Api {
    #[arg(long)]
    #[serde(default)]
    pub operation: Option<String>,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentQuery {
    #[arg(long)]
    #[serde(default)]
    pub session: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub workspace: Option<String>,
    #[arg(long, value_parser = ["workspace", "session", "repository", "all"])]
    #[serde(default)]
    pub scope: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub agent_id: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub detail: bool,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Send {
    #[arg(long)]
    pub to: String,
    #[arg(long)]
    pub text: String,
    #[arg(long, value_parser = ["workspace", "session", "repository", "all"])]
    #[serde(default)]
    pub scope: Option<String>,
    #[arg(long)]
    pub idempotency_key: String,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessagePage {
    #[arg(long)]
    #[serde(default)]
    pub limit: Option<usize>,
    #[arg(long)]
    #[serde(default)]
    pub after: Option<String>,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageId {
    #[arg(long)]
    pub message_id: String,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Title {
    #[arg(long)]
    pub title: String,
}
