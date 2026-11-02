//! Native frontend bindings and their contextual guide, kept in one source of truth.
//!
//! The local-context, atomic-action and width-bounded guide semantics follow Bkpr's
//! `crates/bkpr-zellij/src/keyguide.rs` (main, 9ad93bd). This implementation does
//! not import Zellij modes, actions, SDK keymaps or terminal-output rendering.

use bkpr_protocol::{KeyAction, KeyEvent};
use bkpr_tui_framework::EditAction;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Application,
    CommandPrefix,
    Sidebar { selected: bool },
    Create,
    Edit,
    Confirmation,
    Finder,
    Chooser,
    Details,
    ClipboardConsent,
    History,
    Search,
    Resize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalContext {
    Create,
    Edit,
    Confirmation,
    Finder,
    Chooser,
    Details,
    ClipboardConsent,
}

/// Resolve overlapping frontend state before both dispatch and guide construction.
/// A modal owns its keys; resizing captures before the prefix, which in turn
/// overrides search/sidebar/history. `None` means the sidebar does not own focus.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextState {
    pub modal: Option<ModalContext>,
    pub resizing: bool,
    pub prefix: bool,
    pub searching: bool,
    pub sidebar: Option<bool>,
    pub history: bool,
}

impl ContextState {
    pub fn context(self) -> Context {
        if let Some(modal) = self.modal {
            return match modal {
                ModalContext::Create => Context::Create,
                ModalContext::Edit => Context::Edit,
                ModalContext::Confirmation => Context::Confirmation,
                ModalContext::Finder => Context::Finder,
                ModalContext::Chooser => Context::Chooser,
                ModalContext::Details => Context::Details,
                ModalContext::ClipboardConsent => Context::ClipboardConsent,
            };
        }
        if self.resizing {
            Context::Resize
        } else if self.prefix {
            Context::CommandPrefix
        } else if self.searching {
            Context::Search
        } else if let Some(selected) = self.sidebar {
            Context::Sidebar { selected }
        } else if self.history {
            Context::History
        } else {
            Context::Application
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}
impl Direction {
    pub fn key(self) -> &'static str {
        match self {
            Self::Left => "ArrowLeft",
            Self::Right => "ArrowRight",
            Self::Up => "ArrowUp",
            Self::Down => "ArrowDown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    Previous,
    Next,
    First,
    Last,
    PagePrevious,
    PageNext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryAction {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Oldest,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CommandPrefix,
    SendPrefix,
    ToggleSidebar,
    EnterSidebar,
    LeaveSidebar,
    Navigate(Navigation),
    CycleView,
    CycleField,
    FoldSidebar,
    UnfoldSidebar,
    ToggleFoldSidebar,
    ActivateSelected,
    CreateWorkspace,
    /// A separate terminal within the selected workspace, not a split.
    CreateTerminal,
    RenameSelected,
    /// Close the selected product entity, not merely remove it from a layout.
    CloseSelected,
    OpenFinder,
    Submit,
    Cancel,
    AllowClipboard,
    DenyClipboard,
    Edit(EditAction),
    Detach,
    StartSearch,
    StartHistory,
    StartResize,
    CopySelection,
    ProjectDetails,
    RenameWorkspace,
    RenamePane,
    TerminatePane,
    RemoveWorkspace,
    ChooseWorkspace,
    ChoosePane,
    ReopenPane,
    SplitHorizontal,
    SplitVertical,
    RemovePaneFromLayout,
    PreviousWorkspace,
    NextWorkspace,
    NextPane,
    FocusPane(Direction),
    MovePane(Direction),
    History(HistoryAction),
    NextMatch,
    PreviousMatch,
    Resize {
        direction: Direction,
        cells: u16,
    },
    FinishResize,
}

impl Action {
    pub fn description(self, context: Context) -> &'static str {
        match self {
            Self::CommandPrefix => "commands",
            Self::SendPrefix => "send Ctrl-b to app",
            Self::ToggleSidebar if matches!(context, Context::Sidebar { .. }) => "return to app",
            Self::ToggleSidebar | Self::EnterSidebar => "sidebar",
            Self::LeaveSidebar => "return to app",
            Self::Navigate(Navigation::Previous) => "select previous",
            Self::Navigate(Navigation::Next) => "select next",
            Self::Navigate(Navigation::First) => "first item",
            Self::Navigate(Navigation::Last) => "last item",
            Self::Navigate(Navigation::PagePrevious) if context == Context::Finder => {
                "preview page up"
            }
            Self::Navigate(Navigation::PageNext) if context == Context::Finder => {
                "preview page down"
            }
            Self::Navigate(Navigation::PagePrevious) => "previous page",
            Self::Navigate(Navigation::PageNext) => "next page",
            Self::CycleView => "cycle input / list / preview",
            Self::CycleField => "next field",
            Self::FoldSidebar => "fold workspace",
            Self::UnfoldSidebar => "unfold workspace",
            Self::ToggleFoldSidebar => "fold / unfold workspace",
            Self::ActivateSelected => "activate",
            Self::CreateWorkspace => "new workspace",
            Self::CreateTerminal => "new terminal",
            Self::RenameSelected => "rename",
            Self::CloseSelected => "close",
            Self::OpenFinder => "find panes",
            Self::Submit => match context {
                Context::Create => "create",
                Context::Edit => "save",
                Context::Finder => "open",
                _ => "confirm",
            },
            Self::Cancel => match context {
                Context::Finder => "close finder",
                Context::Search => "close search",
                Context::Details => "close inspector",
                _ => "cancel",
            },
            Self::AllowClipboard => "allow clipboard access",
            Self::DenyClipboard => "deny clipboard access",
            Self::Edit(EditAction::Left) => "cursor left",
            Self::Edit(EditAction::Right) => "cursor right",
            Self::Edit(EditAction::Home) if context == Context::Finder => "start of query",
            Self::Edit(EditAction::End) if context == Context::Finder => "end of query",
            Self::Edit(EditAction::Home) => "start of field",
            Self::Edit(EditAction::End) => "end of field",
            Self::Edit(EditAction::Backspace) => "erase previous",
            Self::Edit(EditAction::Delete) => "erase next",
            Self::Edit(EditAction::Clear) => "clear field",
            Self::Detach => "detach",
            Self::StartSearch => "search history",
            Self::StartHistory => "history",
            Self::StartResize => "resize pane",
            Self::CopySelection => "copy selection",
            Self::ProjectDetails => "project details",
            Self::RenameWorkspace => "rename workspace",
            Self::RenamePane => "rename pane",
            Self::TerminatePane => "terminate pane",
            Self::RemoveWorkspace => "remove workspace",
            Self::ChooseWorkspace => "choose workspace",
            Self::ChoosePane => "choose pane",
            Self::ReopenPane => "reopen shell",
            Self::SplitHorizontal => "split side by side",
            Self::SplitVertical => "split above / below",
            Self::RemovePaneFromLayout => "remove from layout (keep process)",
            Self::PreviousWorkspace => "previous workspace",
            Self::NextWorkspace => "next workspace",
            Self::NextPane => "next pane",
            Self::FocusPane(Direction::Left) => "focus left",
            Self::FocusPane(Direction::Right) => "focus right",
            Self::FocusPane(Direction::Up) => "focus up",
            Self::FocusPane(Direction::Down) => "focus down",
            Self::MovePane(Direction::Left) => "move pane left",
            Self::MovePane(Direction::Right) => "move pane right",
            Self::MovePane(Direction::Up) => "move pane up",
            Self::MovePane(Direction::Down) => "move pane down",
            Self::History(HistoryAction::LineUp) => "line up",
            Self::History(HistoryAction::LineDown) => "line down",
            Self::History(HistoryAction::PageUp) => "page up",
            Self::History(HistoryAction::PageDown) => "page down",
            Self::History(HistoryAction::Oldest) => "oldest history",
            Self::History(HistoryAction::Live) => "return to live",
            Self::NextMatch => "next match",
            Self::PreviousMatch => "previous match",
            Self::Resize { direction, cells } => match (direction, cells > 1) {
                (Direction::Left, false) => "resize left",
                (Direction::Right, false) => "resize right",
                (Direction::Up, false) => "resize up",
                (Direction::Down, false) => "resize down",
                (Direction::Left, true) => "resize left 5 cells",
                (Direction::Right, true) => "resize right 5 cells",
                (Direction::Up, true) => "resize up 5 cells",
                (Direction::Down, true) => "resize down 5 cells",
            },
            Self::FinishResize => "finish resize",
        }
    }
}

const APPLICATION: u16 = 1;
const PREFIX: u16 = 1 << 1;
const SIDEBAR: u16 = 1 << 2;
const CREATE: u16 = 1 << 3;
const EDIT: u16 = 1 << 4;
const CONFIRM: u16 = 1 << 5;
const FINDER: u16 = 1 << 6;
const HISTORY: u16 = 1 << 7;
const SEARCH: u16 = 1 << 8;
const RESIZE: u16 = 1 << 9;
const CHOOSER: u16 = 1 << 10;
const DETAILS: u16 = 1 << 11;
const CLIPBOARD_CONSENT: u16 = 1 << 12;
const ALL: u16 = (1 << 13) - 1;
const SHIFT: u16 = 1;
const CTRL: u16 = 2;
const LOCKS: u16 = 16 | 32;

impl Context {
    fn mask(self) -> u16 {
        match self {
            Self::Application => APPLICATION,
            Self::CommandPrefix => PREFIX,
            Self::Sidebar { .. } => SIDEBAR,
            Self::Create => CREATE,
            Self::Edit => EDIT,
            Self::Confirmation => CONFIRM,
            Self::Finder => FINDER,
            Self::History => HISTORY,
            Self::Search => SEARCH,
            Self::Resize => RESIZE,
            Self::Chooser => CHOOSER,
            Self::Details => DETAILS,
            Self::ClipboardConsent => CLIPBOARD_CONSENT,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Application => "Application",
            Self::CommandPrefix => "Ctrl-b commands",
            Self::Sidebar { .. } => "Sidebar",
            Self::Create => "Create",
            Self::Edit => "Edit",
            Self::Confirmation => "Confirm",
            Self::Finder => "Find panes",
            Self::History => "History",
            Self::Search => "Search",
            Self::Resize => "Resize",
            Self::Chooser => "Choose",
            Self::Details => "Project details",
            Self::ClipboardConsent => "Clipboard permission",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Key {
    Physical(&'static str, u16),
    /// Match produced text, including shifted punctuation and uppercase letters.
    /// Shift is allowed, but Ctrl/Alt/Super are never text-binding aliases.
    Text(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub struct Binding {
    contexts: u16,
    key: Key,
    selected: bool,
    repeat: bool,
    pub label: &'static str,
    pub action: Action,
    pub priority: u8,
}

impl Binding {
    fn available(self, context: Context) -> bool {
        self.contexts & context.mask() != 0
            && (!self.selected || matches!(context, Context::Sidebar { selected: true }))
    }

    pub fn matches(self, event: &KeyEvent) -> bool {
        if event.action == KeyAction::Release || event.action == KeyAction::Repeat && !self.repeat {
            return false;
        }
        let modifiers = event.modifiers & !LOCKS;
        match self.key {
            Key::Physical(key, required) => event.key == key && modifiers == required,
            Key::Text(text) => event.text == text && modifiers & !SHIFT == 0,
        }
    }
}

const fn physical(
    contexts: u16,
    key: &'static str,
    modifiers: u16,
    label: &'static str,
    action: Action,
    priority: u8,
    repeat: bool,
) -> Binding {
    Binding {
        contexts,
        key: Key::Physical(key, modifiers),
        selected: false,
        repeat,
        label,
        action,
        priority,
    }
}

const fn text(contexts: u16, key: &'static str, action: Action, priority: u8) -> Binding {
    Binding {
        contexts,
        key: Key::Text(key),
        selected: false,
        repeat: false,
        label: key,
        action,
        priority,
    }
}

const fn selected(mut binding: Binding) -> Binding {
    binding.selected = true;
    binding
}

const fn repeating(mut binding: Binding) -> Binding {
    binding.repeat = true;
    binding
}

use Action as A;
use Direction as D;
use EditAction as E;
use HistoryAction as H;
use Navigation as N;

// These definitions drive lookup and both guide layouts. Prefix commands are
// deliberately single-press operations; repeated destructive/create commands
// must never execute because the command key was held down.
static BINDINGS: &[Binding] = &[
    physical(ALL, "BracketRight", CTRL, "Ctrl-]", A::Detach, 90, false),
    physical(
        APPLICATION | SIDEBAR | HISTORY | SEARCH | RESIZE,
        "KeyB",
        CTRL,
        "Ctrl-b",
        A::CommandPrefix,
        1,
        false,
    ),
    physical(
        APPLICATION | SIDEBAR | HISTORY,
        "Space",
        CTRL,
        "Ctrl-Space",
        A::ToggleSidebar,
        0,
        false,
    ),
    physical(
        APPLICATION,
        "PageUp",
        SHIFT,
        "Shift-PgUp",
        A::History(H::PageUp),
        10,
        true,
    ),
    physical(
        APPLICATION,
        "PageDown",
        SHIFT,
        "Shift-PgDn",
        A::History(H::PageDown),
        11,
        true,
    ),
    physical(PREFIX, "KeyB", CTRL, "Ctrl-b", A::SendPrefix, 80, false),
    physical(PREFIX, "Escape", 0, "Esc", A::Cancel, 0, false),
    text(PREFIX, "b", A::EnterSidebar, 1),
    text(PREFIX, "/", A::StartSearch, 10),
    text(PREFIX, "s", A::StartHistory, 11),
    text(PREFIX, "e", A::StartResize, 12),
    text(PREFIX, "y", A::CopySelection, 13),
    text(PREFIX, "i", A::ProjectDetails, 30),
    text(PREFIX, "c", A::CreateWorkspace, 20),
    text(PREFIX, "r", A::RenameWorkspace, 23),
    text(PREFIX, "p", A::RenamePane, 24),
    text(PREFIX, "!", A::TerminatePane, 40),
    text(PREFIX, "X", A::RemoveWorkspace, 41),
    text(PREFIX, "w", A::ChooseWorkspace, 14),
    text(PREFIX, "o", A::ChoosePane, 15),
    text(PREFIX, "R", A::ReopenPane, 42),
    text(PREFIX, "n", A::SplitHorizontal, 21),
    text(PREFIX, "%", A::SplitHorizontal, 21),
    text(PREFIX, "\"", A::SplitVertical, 22),
    text(PREFIX, "x", A::RemovePaneFromLayout, 43),
    text(PREFIX, "[", A::PreviousWorkspace, 16),
    text(PREFIX, "]", A::NextWorkspace, 17),
    text(PREFIX, "d", A::Detach, 90),
    physical(PREFIX, "Tab", 0, "Tab", A::NextPane, 18, false),
    physical(PREFIX, "ArrowLeft", 0, "←", A::FocusPane(D::Left), 2, false),
    physical(
        PREFIX,
        "ArrowRight",
        0,
        "→",
        A::FocusPane(D::Right),
        3,
        false,
    ),
    physical(PREFIX, "ArrowUp", 0, "↑", A::FocusPane(D::Up), 4, false),
    physical(PREFIX, "ArrowDown", 0, "↓", A::FocusPane(D::Down), 5, false),
    physical(
        PREFIX,
        "ArrowLeft",
        SHIFT,
        "Shift-←",
        A::MovePane(D::Left),
        6,
        false,
    ),
    physical(
        PREFIX,
        "ArrowRight",
        SHIFT,
        "Shift-→",
        A::MovePane(D::Right),
        7,
        false,
    ),
    physical(
        PREFIX,
        "ArrowUp",
        SHIFT,
        "Shift-↑",
        A::MovePane(D::Up),
        8,
        false,
    ),
    physical(
        PREFIX,
        "ArrowDown",
        SHIFT,
        "Shift-↓",
        A::MovePane(D::Down),
        9,
        false,
    ),
    physical(SIDEBAR, "Escape", 0, "Esc", A::LeaveSidebar, 0, false),
    text(SIDEBAR, "/", A::OpenFinder, 5),
    physical(
        SIDEBAR | FINDER | CHOOSER | DETAILS,
        "ArrowUp",
        0,
        "↑",
        A::Navigate(N::Previous),
        10,
        true,
    ),
    physical(
        SIDEBAR | FINDER | CHOOSER | DETAILS,
        "ArrowDown",
        0,
        "↓",
        A::Navigate(N::Next),
        11,
        true,
    ),
    repeating(text(SIDEBAR, "k", A::Navigate(N::Previous), 10)),
    repeating(text(SIDEBAR, "j", A::Navigate(N::Next), 11)),
    physical(
        SIDEBAR | FINDER | DETAILS,
        "PageUp",
        0,
        "PgUp",
        A::Navigate(N::PagePrevious),
        35,
        true,
    ),
    physical(
        SIDEBAR | FINDER | DETAILS,
        "PageDown",
        0,
        "PgDn",
        A::Navigate(N::PageNext),
        36,
        true,
    ),
    physical(
        SIDEBAR | CHOOSER | DETAILS,
        "Home",
        0,
        "Home",
        A::Navigate(N::First),
        33,
        true,
    ),
    physical(
        SIDEBAR | CHOOSER | DETAILS,
        "End",
        0,
        "End",
        A::Navigate(N::Last),
        34,
        true,
    ),
    repeating(text(SIDEBAR, "g", A::Navigate(N::First), 33)),
    repeating(text(SIDEBAR, "G", A::Navigate(N::Last), 34)),
    selected(physical(
        SIDEBAR,
        "ArrowLeft",
        0,
        "←",
        A::FoldSidebar,
        13,
        false,
    )),
    selected(physical(
        SIDEBAR,
        "ArrowRight",
        0,
        "→",
        A::UnfoldSidebar,
        14,
        false,
    )),
    selected(physical(
        SIDEBAR,
        "Space",
        0,
        "Space",
        A::ToggleFoldSidebar,
        15,
        false,
    )),
    selected(physical(
        SIDEBAR,
        "Enter",
        0,
        "Enter",
        A::ActivateSelected,
        12,
        false,
    )),
    text(SIDEBAR, "n", A::CreateWorkspace, 20),
    selected(text(SIDEBAR, "N", A::CreateTerminal, 21)),
    selected(text(SIDEBAR, "r", A::RenameSelected, 25)),
    selected(text(SIDEBAR, "x", A::CloseSelected, 26)),
    selected(physical(
        SIDEBAR,
        "Delete",
        0,
        "Del",
        A::CloseSelected,
        26,
        false,
    )),
    physical(
        CREATE | EDIT | CONFIRM | FINDER | SEARCH | CHOOSER | DETAILS,
        "Escape",
        0,
        "Esc",
        A::Cancel,
        0,
        false,
    ),
    text(CONFIRM, "y", A::Submit, 1),
    text(CONFIRM, "Y", A::Submit, 1),
    text(CONFIRM, "n", A::Cancel, 0),
    text(CONFIRM, "N", A::Cancel, 0),
    physical(
        CREATE | EDIT | CONFIRM | FINDER | CHOOSER,
        "Enter",
        0,
        "Enter",
        A::Submit,
        1,
        false,
    ),
    physical(DETAILS, "Enter", 0, "Enter", A::Cancel, 0, false),
    text(CLIPBOARD_CONSENT, "n", A::DenyClipboard, 0),
    text(CLIPBOARD_CONSENT, "N", A::DenyClipboard, 0),
    text(CLIPBOARD_CONSENT, "y", A::AllowClipboard, 1),
    text(CLIPBOARD_CONSENT, "Y", A::AllowClipboard, 1),
    physical(
        CREATE | EDIT | FINDER | SEARCH,
        "Backspace",
        0,
        "Backspace",
        A::Edit(E::Backspace),
        40,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | SEARCH,
        "KeyU",
        CTRL,
        "Ctrl-u",
        A::Edit(E::Clear),
        41,
        false,
    ),
    physical(CREATE, "Tab", 0, "Tab", A::CycleField, 10, false),
    physical(CREATE, "Tab", SHIFT, "Shift-Tab", A::CycleField, 10, false),
    physical(FINDER, "Tab", 0, "Tab", A::CycleView, 12, false),
    physical(
        FINDER,
        "KeyP",
        CTRL,
        "Ctrl-p",
        A::Navigate(N::Previous),
        10,
        true,
    ),
    physical(
        FINDER,
        "KeyN",
        CTRL,
        "Ctrl-n",
        A::Navigate(N::Next),
        11,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER,
        "ArrowLeft",
        0,
        "←",
        A::Edit(E::Left),
        30,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER,
        "ArrowRight",
        0,
        "→",
        A::Edit(E::Right),
        31,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER,
        "Home",
        0,
        "Home",
        A::Edit(E::Home),
        32,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER,
        "End",
        0,
        "End",
        A::Edit(E::End),
        33,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER,
        "Delete",
        0,
        "Del",
        A::Edit(E::Delete),
        42,
        true,
    ),
    physical(HISTORY, "ArrowUp", 0, "↑", A::History(H::LineUp), 10, true),
    physical(
        HISTORY,
        "ArrowDown",
        0,
        "↓",
        A::History(H::LineDown),
        11,
        true,
    ),
    physical(
        HISTORY,
        "PageUp",
        0,
        "PgUp",
        A::History(H::PageUp),
        12,
        true,
    ),
    physical(
        HISTORY,
        "PageDown",
        0,
        "PgDn",
        A::History(H::PageDown),
        13,
        true,
    ),
    physical(
        HISTORY,
        "PageUp",
        SHIFT,
        "Shift-PgUp",
        A::History(H::PageUp),
        12,
        true,
    ),
    physical(
        HISTORY,
        "PageDown",
        SHIFT,
        "Shift-PgDn",
        A::History(H::PageDown),
        13,
        true,
    ),
    physical(HISTORY, "Home", 0, "Home", A::History(H::Oldest), 14, false),
    physical(HISTORY, "Escape", 0, "Esc", A::History(H::Live), 0, false),
    physical(HISTORY, "Enter", 0, "Enter", A::History(H::Live), 0, false),
    physical(HISTORY, "End", 0, "End", A::History(H::Live), 0, false),
    physical(SEARCH, "Enter", 0, "Enter", A::NextMatch, 10, true),
    physical(
        SEARCH,
        "Enter",
        SHIFT,
        "Shift-Enter",
        A::PreviousMatch,
        11,
        true,
    ),
    physical(RESIZE, "Enter", 0, "Enter", A::FinishResize, 0, false),
    physical(RESIZE, "Escape", 0, "Esc", A::FinishResize, 0, false),
    physical(
        RESIZE,
        "ArrowLeft",
        0,
        "←",
        A::Resize {
            direction: D::Left,
            cells: 1,
        },
        10,
        true,
    ),
    physical(
        RESIZE,
        "ArrowRight",
        0,
        "→",
        A::Resize {
            direction: D::Right,
            cells: 1,
        },
        11,
        true,
    ),
    physical(
        RESIZE,
        "ArrowUp",
        0,
        "↑",
        A::Resize {
            direction: D::Up,
            cells: 1,
        },
        12,
        true,
    ),
    physical(
        RESIZE,
        "ArrowDown",
        0,
        "↓",
        A::Resize {
            direction: D::Down,
            cells: 1,
        },
        13,
        true,
    ),
    physical(
        RESIZE,
        "ArrowLeft",
        SHIFT,
        "Shift-←",
        A::Resize {
            direction: D::Left,
            cells: 5,
        },
        20,
        true,
    ),
    physical(
        RESIZE,
        "ArrowRight",
        SHIFT,
        "Shift-→",
        A::Resize {
            direction: D::Right,
            cells: 5,
        },
        21,
        true,
    ),
    physical(
        RESIZE,
        "ArrowUp",
        SHIFT,
        "Shift-↑",
        A::Resize {
            direction: D::Up,
            cells: 5,
        },
        22,
        true,
    ),
    physical(
        RESIZE,
        "ArrowDown",
        SHIFT,
        "Shift-↓",
        A::Resize {
            direction: D::Down,
            cells: 5,
        },
        23,
        true,
    ),
];

/// Until the caller confirms product support, do not advertise creation of a
/// separate terminal or closing an unspecified sidebar entity. Pane-only close
/// and workspace cascade-close have different backend requirements.
fn supported_by_default(action: Action) -> bool {
    !matches!(action, Action::CreateTerminal | Action::CloseSelected)
}

/// Use the same capability predicate for dispatch and hints. It can inspect
/// the selected entity and actual backend support without inventing keymaps.
/// This predicate is the full policy, allowing callers to enable product actions
/// once they can really execute them.
pub fn bindings_with(
    context: Context,
    supported: impl Fn(Action) -> bool,
) -> impl Iterator<Item = &'static Binding> {
    BINDINGS
        .iter()
        .filter(move |binding| binding.available(context) && supported(binding.action))
}

/// Stateless lookup never consumes releases or executes repeat-unsafe actions.
/// The caller still owns held-key release routing and capture suppression; a
/// `None` in an owning local context is not permission to forward to a PTY.
pub fn lookup(context: Context, event: &KeyEvent) -> Option<Action> {
    lookup_with(context, event, supported_by_default)
}

pub fn lookup_with(
    context: Context,
    event: &KeyEvent,
    supported: impl Fn(Action) -> bool,
) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|binding| {
            binding.available(context) && binding.matches(event) && supported(binding.action)
        })
        .map(|binding| binding.action)
}

#[derive(Clone, Debug)]
pub struct Hint {
    pub keys: Vec<&'static str>,
    pub action: Action,
    pub description: &'static str,
    pub priority: u8,
}

#[cfg(test)]
pub fn hints(context: Context) -> Vec<Hint> {
    hints_with(context, supported_by_default)
}

pub fn hints_with(context: Context, supported: impl Fn(Action) -> bool) -> Vec<Hint> {
    let mut hints: Vec<Hint> = Vec::new();
    for binding in bindings_with(context, supported) {
        if let Some(hint) = hints.iter_mut().find(|hint| hint.action == binding.action) {
            if !hint.keys.contains(&binding.label) {
                hint.keys.push(binding.label);
            }
        } else {
            hints.push(Hint {
                keys: vec![binding.label],
                action: binding.action,
                description: binding.action.description(context),
                priority: binding.priority,
            });
        }
    }
    hints.sort_by_key(|hint| hint.priority);
    hints
}

fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn fit(text: &str, columns: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + cells > columns {
            break;
        }
        result.push(character);
        used += cells;
    }
    result
}

/// One plain-text footer row, using complete bindings rather than truncated
/// key chords or instructions. Narrow viewports may show only the context title.
#[cfg(test)]
pub fn footer(context: Context, columns: usize) -> String {
    footer_with(context, columns, supported_by_default)
}

pub fn footer_with(context: Context, columns: usize, supported: impl Fn(Action) -> bool) -> String {
    let mut line = String::new();
    let mut used = 0;
    for hint in hints_with(context, supported) {
        let key = hint.keys[0];
        let needed = width(key) + 1 + width(hint.description);
        let separator = if line.is_empty() { 0 } else { 2 };
        if used + separator + needed > columns {
            continue;
        }
        if !line.is_empty() {
            line.push_str("  ");
        }
        line.push_str(key);
        line.push(' ');
        line.push_str(hint.description);
        used += separator + needed;
    }
    if line.is_empty() {
        fit(context.title(), columns)
    } else {
        line
    }
}

fn hint_row(hint: &Hint, columns: usize) -> String {
    let aliases = hint.keys.join("/");
    let keys = if width(&aliases) + 2 < columns {
        aliases.as_str()
    } else {
        hint.keys[0]
    };
    let key_width = width(keys);
    if key_width + 2 >= columns {
        // Do not present a chopped key chord as if it were executable.
        return fit(hint.description, columns);
    }
    let mut row = String::from(keys);
    row.push_str("  ");
    row.push_str(&fit(hint.description, columns - key_width - 2));
    row
}

/// Width-bounded application hints. The framework owns panel decoration,
/// positioning and physical presentation.
#[cfg(test)]
pub fn expanded(context: Context, columns: usize, max_rows: usize) -> Vec<String> {
    expanded_with(context, columns, max_rows, supported_by_default)
}

pub fn expanded_with(
    context: Context,
    columns: usize,
    max_rows: usize,
    supported: impl Fn(Action) -> bool,
) -> Vec<String> {
    if columns == 0 || max_rows == 0 {
        return Vec::new();
    }
    let hints = hints_with(context, supported);
    let shown = if hints.len() > max_rows {
        max_rows - 1
    } else {
        hints.len()
    };
    let mut rows = Vec::with_capacity(hints.len().min(max_rows));
    for hint in hints.iter().take(shown) {
        rows.push(hint_row(hint, columns));
    }
    if shown < hints.len() {
        rows.push(fit(&format!("… {} more", hints.len() - shown), columns));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: &str, text: &str, modifiers: u16, action: KeyAction) -> KeyEvent {
        KeyEvent {
            key: key.into(),
            text: text.into(),
            modifiers,
            action,
            unshifted_codepoint: 0,
        }
    }

    #[test]
    fn owning_context_takes_precedence_over_sidebar_and_prefix() {
        let state = ContextState {
            modal: Some(ModalContext::Finder),
            prefix: true,
            sidebar: Some(true),
            history: true,
            searching: true,
            resizing: true,
        };
        assert_eq!(state.context(), Context::Finder);
        assert_eq!(
            lookup(state.context(), &event("KeyN", "n", 0, KeyAction::Press)),
            None
        );
        let state = ContextState {
            modal: None,
            resizing: false,
            ..state
        };
        assert_eq!(state.context(), Context::CommandPrefix);
        assert_eq!(
            lookup(state.context(), &event("KeyN", "n", 0, KeyAction::Press)),
            Some(A::SplitHorizontal)
        );
    }

    #[test]
    fn modifiers_and_key_lifecycles_do_not_trigger_spurious_actions() {
        let sidebar = Context::Sidebar { selected: true };
        assert_eq!(
            lookup_with(
                sidebar,
                &event("KeyN", "N", SHIFT, KeyAction::Press),
                |_| true
            ),
            Some(A::CreateTerminal)
        );
        assert_eq!(
            lookup(sidebar, &event("KeyN", "n", CTRL, KeyAction::Press)),
            None
        );
        assert_eq!(
            lookup(sidebar, &event("KeyN", "n", 8, KeyAction::Press)),
            None
        );
        assert_eq!(
            lookup(sidebar, &event("KeyN", "n", 0, KeyAction::Repeat)),
            None
        );
        assert_eq!(
            lookup(sidebar, &event("ArrowDown", "", 0, KeyAction::Repeat)),
            Some(A::Navigate(N::Next))
        );
        assert_eq!(
            lookup(sidebar, &event("ArrowDown", "", 0, KeyAction::Release)),
            None
        );
        assert_eq!(
            lookup(
                Context::Application,
                &event("KeyB", "b", CTRL | LOCKS, KeyAction::Press)
            ),
            Some(A::CommandPrefix)
        );
        assert_eq!(
            lookup(
                Context::CommandPrefix,
                &event("ArrowDown", "", 0, KeyAction::Repeat)
            ),
            None
        );
    }

    #[test]
    fn empty_sidebar_never_advertises_selection_actions() {
        let context = Context::Sidebar { selected: false };
        assert_eq!(
            lookup(context, &event("KeyR", "r", 0, KeyAction::Press)),
            None
        );
        assert!(!hints(context).iter().any(|hint| matches!(
            hint.action,
            A::CreateTerminal | A::RenameSelected | A::CloseSelected | A::ActivateSelected
        )));
        assert_eq!(
            lookup(context, &event("KeyN", "n", 0, KeyAction::Press)),
            Some(A::CreateWorkspace)
        );
    }

    #[test]
    fn form_editing_does_not_leak_into_confirmation_or_cycle_on_repeat() {
        for context in [Context::Create, Context::Edit] {
            assert_eq!(
                lookup(context, &event("ArrowLeft", "", 0, KeyAction::Repeat)),
                Some(A::Edit(E::Left))
            );
            assert_eq!(
                lookup(context, &event("Delete", "", 0, KeyAction::Press)),
                Some(A::Edit(E::Delete))
            );
        }
        for modifiers in [0, SHIFT] {
            assert_eq!(
                lookup(
                    Context::Create,
                    &event("Tab", "", modifiers, KeyAction::Press)
                ),
                Some(A::CycleField)
            );
            assert_eq!(
                lookup(
                    Context::Create,
                    &event("Tab", "", modifiers, KeyAction::Repeat)
                ),
                None
            );
        }
        assert_eq!(
            lookup(Context::Edit, &event("Tab", "", 0, KeyAction::Press)),
            None
        );
        assert_eq!(
            lookup(
                Context::Confirmation,
                &event("Delete", "", 0, KeyAction::Press)
            ),
            None
        );
        assert_eq!(
            lookup(Context::Finder, &event("Tab", "", 0, KeyAction::Press)),
            Some(A::CycleView)
        );
    }

    #[test]
    fn readonly_picker_and_details_have_no_editing_bindings() {
        for context in [Context::Chooser, Context::Details] {
            assert!(
                !hints(context)
                    .iter()
                    .any(|hint| matches!(hint.action, A::Edit(_) | A::CycleField))
            );
            assert_eq!(
                lookup(context, &event("ArrowDown", "", 0, KeyAction::Repeat)),
                Some(A::Navigate(N::Next))
            );
            assert_eq!(
                lookup(context, &event("Home", "", 0, KeyAction::Repeat)),
                Some(A::Navigate(N::First))
            );
            assert_eq!(
                lookup(context, &event("Enter", "", 0, KeyAction::Repeat)),
                None
            );
            assert_eq!(
                lookup(context, &event("Escape", "", 0, KeyAction::Repeat)),
                None
            );
        }
        assert_eq!(
            lookup(Context::Chooser, &event("Enter", "", 0, KeyAction::Press)),
            Some(A::Submit)
        );
        assert_eq!(
            lookup(Context::Details, &event("Enter", "", 0, KeyAction::Press)),
            Some(A::Cancel)
        );
        assert_eq!(
            lookup(
                Context::Chooser,
                &event("PageDown", "", 0, KeyAction::Press)
            ),
            None
        );
        assert_eq!(
            lookup(
                Context::Details,
                &event("PageDown", "", 0, KeyAction::Repeat)
            ),
            Some(A::Navigate(N::PageNext))
        );
    }

    #[test]
    fn clipboard_consent_never_treats_enter_or_generic_confirmation_as_approval() {
        let context = Context::ClipboardConsent;
        assert_eq!(
            lookup(context, &event("KeyY", "y", 0, KeyAction::Press)),
            Some(A::AllowClipboard)
        );
        assert_eq!(
            lookup(context, &event("KeyN", "n", 0, KeyAction::Press)),
            Some(A::DenyClipboard)
        );
        assert_eq!(
            lookup(context, &event("KeyY", "Y", SHIFT, KeyAction::Press)),
            Some(A::AllowClipboard)
        );
        assert_eq!(
            lookup(context, &event("KeyY", "y", 0, KeyAction::Repeat)),
            None
        );
        assert_eq!(
            lookup(context, &event("KeyY", "y", 0, KeyAction::Release)),
            None
        );
        assert_eq!(
            lookup(context, &event("KeyY", "y", CTRL, KeyAction::Press)),
            None
        );
        assert_eq!(
            lookup(context, &event("Enter", "", 0, KeyAction::Press)),
            None
        );
        assert_eq!(
            lookup(context, &event("Escape", "", 0, KeyAction::Press)),
            None
        );
        assert!(
            !hints(context)
                .iter()
                .any(|hint| matches!(hint.action, A::Submit | A::Cancel))
        );
    }

    #[test]
    fn backend_capabilities_gate_dispatch_and_advertising_together() {
        let context = Context::Sidebar { selected: true };
        let key = event("KeyN", "N", SHIFT, KeyAction::Press);
        assert_eq!(lookup(context, &key), None);
        assert!(
            !hints(context)
                .iter()
                .any(|hint| matches!(hint.action, A::CreateTerminal | A::CloseSelected))
        );
        let supported = |action| action != A::CreateTerminal;
        assert_eq!(lookup_with(context, &key, supported), None);
        assert!(
            !hints_with(context, supported)
                .iter()
                .any(|hint| hint.action == A::CreateTerminal)
        );
        assert_eq!(
            lookup_with(context, &key, |_| true),
            Some(A::CreateTerminal)
        );
        assert!(
            hints_with(context, |_| true)
                .iter()
                .any(|hint| hint.action == A::CreateTerminal)
        );
        assert!(!footer_with(context, 1000, supported).contains("new terminal"));
        assert!(
            !expanded_with(context, 100, 100, supported)
                .iter()
                .any(|row| row.contains("new terminal"))
        );
    }

    #[test]
    fn footer_and_card_fit_every_small_viewport() {
        let contexts = [
            Context::Application,
            Context::CommandPrefix,
            Context::Sidebar { selected: true },
            Context::Sidebar { selected: false },
            Context::Create,
            Context::Edit,
            Context::Confirmation,
            Context::Finder,
            Context::Chooser,
            Context::Details,
            Context::ClipboardConsent,
            Context::History,
            Context::Search,
            Context::Resize,
        ];
        for context in contexts {
            for columns in 0..=100 {
                assert!(width(&footer(context, columns)) <= columns);
                for max_rows in 0..=8 {
                    let rows = expanded(context, columns, max_rows);
                    assert!(rows.len() <= max_rows);
                    assert!(rows.iter().all(|row| width(row) <= columns));
                }
            }
        }
    }
}
