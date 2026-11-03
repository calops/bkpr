//! Native frontend bindings and their contextual guide, kept in one source of truth.
//!
//! The local-context, atomic-action and width-bounded guide semantics follow Bkpr's
//! `crates/bkpr-zellij/src/keyguide.rs` (main, 9ad93bd). This implementation does
//! not import Zellij modes, actions, SDK keymaps or terminal-output rendering.

use bkpr_protocol::{KeyAction, KeyEvent};
use bkpr_tui_framework::{EditAction, Rect, Role, RoleSpan};
use unicode_width::UnicodeWidthStr;

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
    ToggleChrome,
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
    CloseWorkspace,
    ChooseWorkspace,
    ChooseWindow,
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
            Self::ToggleChrome => "toggle fancy / normal chrome",
            Self::StartSearch => "search history",
            Self::StartHistory => "history",
            Self::StartResize => "resize pane",
            Self::CopySelection => "copy selection",
            Self::ProjectDetails => "project details",
            Self::RenameWorkspace => "rename workspace",
            Self::RenamePane => "rename pane",
            Self::TerminatePane => "terminate pane",
            Self::CloseWorkspace => "close workspace",
            Self::ChooseWorkspace => "choose workspace",
            Self::ChooseWindow => "choose window",
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

// These definitions drive lookup and the contextual popup. Prefix commands are
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
    text(PREFIX, "g", A::ToggleChrome, 31),
    text(PREFIX, "c", A::CreateWorkspace, 20),
    text(PREFIX, "N", A::CreateTerminal, 19),
    text(PREFIX, "r", A::RenameWorkspace, 23),
    text(PREFIX, "p", A::RenamePane, 24),
    text(PREFIX, "!", A::TerminatePane, 40),
    text(PREFIX, "X", A::CloseWorkspace, 41),
    text(PREFIX, "w", A::ChooseWorkspace, 14),
    text(PREFIX, "W", A::ChooseWindow, 15),
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

fn supported_by_default(_action: Action) -> bool {
    true
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

/// Product-selected guide geometry and exact keycap spans. Layout is prepared
/// with binding/capability changes, never inferred back from rendered text.
#[derive(Default)]
pub struct Guide {
    pub columns: u16,
    pub expanded: Vec<String>,
    pub expanded_roles: Vec<RoleSpan>,
}

fn alias_width(keys: &[&str]) -> usize {
    keys.iter().map(|key| width(key)).sum::<usize>() + keys.len().saturating_sub(1)
}

/// Keep each executable chord intact; only the separators between aliases wrap.
fn alias_rows(keys: &[&str], columns: usize) -> Option<Vec<std::ops::Range<usize>>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (index, key) in keys.iter().enumerate() {
        let cells = width(key);
        if cells > columns {
            return None;
        }
        if index > start && used + 1 + cells > columns {
            rows.push(start..index);
            start = index;
            used = 0;
        }
        used += usize::from(index > start) + cells;
    }
    if start < keys.len() {
        rows.push(start..keys.len());
    }
    Some(rows)
}

struct Body<'a> {
    hints: &'a [Hint],
    single_key: usize,
    aliases: usize,
    description: usize,
}

impl<'a> Body<'a> {
    fn new(hints: &'a [Hint]) -> Self {
        let mut single_key = 0;
        let mut aliases = 0;
        let mut description = 0;
        for hint in hints {
            let mut full = hint.keys.len().saturating_sub(1);
            for key in &hint.keys {
                let cells = width(key);
                single_key = single_key.max(cells);
                full += cells;
            }
            aliases = aliases.max(full);
            description = description.max(width(hint.description));
        }
        Self {
            hints,
            single_key,
            aliases,
            description,
        }
    }

    fn render(&self, columns: usize, max_rows: usize) -> (Vec<String>, Vec<RoleSpan>) {
        if columns < 4 || max_rows == 0 || self.hints.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let key_columns = if self.aliases + 2 + self.description <= columns {
            self.aliases
        } else {
            self.single_key.max(self.aliases.div_ceil(2))
        }
        .clamp(1, columns - 3);
        let description_columns = columns - key_columns - 2;
        let options = textwrap::Options::new(description_columns)
            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
            .word_splitter(textwrap::WordSplitter::NoHyphenation)
            .break_words(false);
        let mut rows = Vec::new();
        let mut roles = Vec::new();
        let mut omitted = 0;
        let mut last_group = None;
        for hint in self.hints {
            let Some(keys) = alias_rows(&hint.keys, key_columns) else {
                omitted += 1;
                continue;
            };
            let descriptions = textwrap::wrap(hint.description, &options);
            let count = keys.len().max(descriptions.len());
            if count > max_rows - rows.len()
                || descriptions
                    .iter()
                    .any(|line| width(line) > description_columns)
            {
                omitted += 1;
                continue;
            }
            last_group = Some((rows.len(), roles.len()));
            for line in 0..count {
                let mut row = String::with_capacity(columns);
                if let Some(range) = keys.get(line) {
                    let aliases = &hint.keys[range.clone()];
                    let mut x = key_columns - alias_width(aliases);
                    row.extend(std::iter::repeat_n(' ', x));
                    for (index, key) in aliases.iter().enumerate() {
                        if index > 0 {
                            row.push('/');
                            x += 1;
                        }
                        let cells = width(key);
                        roles.push(RoleSpan {
                            rect: Rect {
                                x: x as u16,
                                y: rows.len() as u16,
                                width: cells as u16,
                                height: 1,
                            },
                            role: Role::Accent,
                        });
                        row.push_str(key);
                        x += cells;
                    }
                } else {
                    row.extend(std::iter::repeat_n(' ', key_columns));
                }
                row.push_str("  ");
                if let Some(description) = descriptions.get(line) {
                    row.push_str(description);
                }
                rows.push(row);
            }
        }
        if omitted > 0 {
            if rows.len() == max_rows
                && let Some((start, role_start)) = last_group
            {
                // Never leave half an explanation just to make room for overflow.
                rows.truncate(start);
                roles.truncate(role_start);
                omitted += 1;
            }
            let message = if rows.is_empty() {
                "Widen to see shortcuts".to_owned()
            } else {
                format!("… {omitted} more")
            };
            rows.push(bkpr_tui_framework::window::fit(&message, columns as u16));
        }
        (rows, roles)
    }
}

/// A sidebar-bounded popup fitted to its visible alias/description outline.
/// The whole table, rather than its two-cell gutter, is centered when it fits.
/// `max_rows` bounds body rows; callers reserve two additional fallback-frame
/// rows. Each body row includes main's inner padding inside that frame.
pub fn render_with(
    context: Context,
    sidebar_columns: u16,
    max_columns: u16,
    max_rows: usize,
    supported: impl Fn(Action) -> bool,
) -> Guide {
    if max_columns == 0 || max_rows == 0 {
        return Guide::default();
    }
    let mut hints = hints_with(context, supported);
    for hint in &mut hints {
        hint.keys.sort_by_key(|key| {
            let arrow = key.chars().last().is_some_and(|c| "←↓↑→".contains(c));
            (
                !arrow,
                width(key),
                key.len() == 1 && key.as_bytes()[0].is_ascii_uppercase(),
                *key,
            )
        });
    }
    render_hints(context, &hints, sidebar_columns, max_columns, max_rows)
}

fn render_hints(
    context: Context,
    hints: &[Hint],
    sidebar_columns: u16,
    max_columns: u16,
    max_rows: usize,
) -> Guide {
    if max_columns == 0 || max_rows == 0 {
        return Guide::default();
    }
    let body = Body::new(hints);
    let title_columns = width(context.title()) + 5;
    let limit = usize::from(max_columns.min(sidebar_columns.max(72)));
    let provisional = (body.aliases + 2 + body.description + 4)
        .max(title_columns)
        .max(usize::from(sidebar_columns))
        .min(limit);
    let padding = usize::from(provisional >= 8);
    let overhead = 2 + 2 * padding;
    let inner_columns = provisional.saturating_sub(overhead);
    let (mut expanded, mut expanded_roles) = body.render(inner_columns, max_rows);
    if expanded.is_empty() {
        let message = if hints.is_empty() {
            "No configured shortcuts"
        } else {
            "Widen to see shortcuts"
        };
        expanded.push(bkpr_tui_framework::window::fit(
            message,
            inner_columns as u16,
        ));
    }
    let left = expanded
        .iter()
        .filter(|row| !row.trim().is_empty())
        .map(|row| row.len() - row.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let outline = expanded
        .iter()
        .map(|row| width(row.trim_end()))
        .max()
        .unwrap_or(0)
        .saturating_sub(left);
    let columns = (outline + overhead)
        .max(title_columns)
        .max(usize::from(sidebar_columns))
        .min(limit) as u16;
    let offset = padding + (usize::from(columns).saturating_sub(overhead + outline)) / 2;
    let indent = " ".repeat(offset);
    for row in &mut expanded {
        row.truncate(row.trim_end().len());
        row.replace_range(..left.min(row.len()), "");
        row.reserve(indent.len() + padding);
        row.insert_str(0, &indent);
        row.extend(std::iter::repeat_n(' ', padding));
    }
    for span in &mut expanded_roles {
        span.rect.x = span.rect.x.saturating_sub(left as u16) + offset as u16;
    }
    Guide {
        columns,
        expanded,
        expanded_roles,
    }
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
    fn active_context_replaces_commands_and_capability_denial_removes_them() {
        for action in [
            A::SplitHorizontal,
            A::CreateWorkspace,
            A::RemovePaneFromLayout,
        ] {
            let normal = hints_with(Context::Application, |_| true);
            assert!(!normal.iter().any(|hint| hint.action == action));
            let prefix = hints_with(Context::CommandPrefix, |_| true);
            assert!(prefix.iter().any(|hint| hint.action == action));
            let denied = hints_with(Context::CommandPrefix, |candidate| candidate != action);
            assert!(!denied.iter().any(|hint| hint.action == action));
        }
        for context in [Context::Search, Context::History, Context::Resize] {
            assert!(
                !hints_with(context, |_| true)
                    .iter()
                    .any(|hint| matches!(hint.action, A::SplitHorizontal | A::CreateWorkspace))
            );
        }
    }

    #[test]
    fn popup_prefers_sidebar_width_and_only_expands_for_content() {
        let context = Context::CommandPrefix;
        let compact = render_with(context, 38, 120, 10, |action| action == A::Cancel);
        assert_eq!(compact.columns, 38);
        let overflow = render_with(context, 38, 120, 10, |action| {
            action == A::RemovePaneFromLayout
        });
        assert!(overflow.columns > 38 && overflow.columns <= 72);
        let wide_sidebar = render_with(context, 100, 120, 10, |_| true);
        assert_eq!(wide_sidebar.columns, 100);
        let constrained = render_with(context, 38, 30, 10, |_| true);
        assert_eq!(constrained.columns, 30);
    }

    fn assert_balanced_outline(guide: &Guide) {
        let left = guide
            .expanded
            .iter()
            .filter(|row| !row.trim().is_empty())
            .map(|row| row.len() - row.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        let end = guide
            .expanded
            .iter()
            .map(|row| width(row.trim_end()))
            .max()
            .unwrap_or(0);
        let right = usize::from(guide.columns.saturating_sub(2)).saturating_sub(end);
        assert!(left.abs_diff(right) <= 1, "{left} left / {right} right");
    }

    #[test]
    fn popup_centers_the_visible_outline_not_the_gutter() {
        let hint = Hint {
            keys: vec!["x"],
            action: A::RemovePaneFromLayout,
            description: "remove the layout entry while keeping the terminal process",
            priority: 0,
        };
        let guide = render_hints(Context::CommandPrefix, &[hint], 20, 120, 8);
        let key = &guide.expanded_roles[0].rect;
        let key_end = key.x + key.width;
        assert!(
            key_end < guide.columns / 3,
            "an asymmetric table must not center its gutter"
        );
        assert_eq!(
            guide.expanded[0].find("remove the layout entry"),
            Some(usize::from(key_end + 2))
        );
        assert_balanced_outline(&guide);
    }

    #[test]
    fn short_visible_table_keeps_a_shared_gutter_and_balanced_sidebar_space() {
        let hints = [
            Hint {
                keys: vec!["Ctrl-a", "a"],
                action: A::ActivateSelected,
                description: "select",
                priority: 0,
            },
            Hint {
                keys: vec!["b"],
                action: A::EnterSidebar,
                description: "open",
                priority: 1,
            },
        ];
        let guide = render_hints(Context::Application, &hints, 38, 120, 8);
        assert_eq!(guide.columns, 38);
        let endpoints: Vec<_> = (0..guide.expanded.len())
            .map(|row| {
                guide
                    .expanded_roles
                    .iter()
                    .filter(|span| usize::from(span.rect.y) == row)
                    .map(|span| span.rect.x + span.rect.width)
                    .max()
                    .unwrap()
            })
            .collect();
        assert!(endpoints.iter().all(|end| *end == endpoints[0]));
        for (row, hint) in hints.iter().enumerate() {
            assert_eq!(
                guide.expanded[row].find(hint.description),
                Some(usize::from(endpoints[row] + 2))
            );
        }
        assert_balanced_outline(&guide);
    }

    #[test]
    fn omitted_long_description_does_not_expand_the_visible_card() {
        let hints = [
            Hint {
                keys: vec!["Enter"],
                action: A::Submit,
                description: "apply",
                priority: 0,
            },
            Hint {
                keys: vec!["z"],
                action: A::RemovePaneFromLayout,
                description: "this complete explanation needs several wrapped rows to describe an operation without losing any of its important terminal process lifetime and layout ownership semantics",
                priority: 1,
            },
        ];
        let guide = render_hints(Context::CommandPrefix, &hints, 24, 120, 2);
        assert_eq!(guide.columns, 24);
        assert!(guide.expanded[0].contains("apply"));
        assert!(guide.expanded[1].contains("more"));
        assert_eq!(guide.expanded_roles.len(), 1);
        assert_balanced_outline(&guide);
    }

    #[test]
    fn overflowing_card_fits_its_outline_without_a_minimum_width_floor() {
        let hint = Hint {
            keys: vec!["x"],
            action: A::RemovePaneFromLayout,
            description: "keep this terminal process running",
            priority: 0,
        };
        let needed = width(hint.description) + 1 + 2 + 4;
        let guide = render_hints(Context::CommandPrefix, &[hint], 20, 120, 8);
        assert_eq!(usize::from(guide.columns), needed);
        assert!(guide.columns < 48);
        assert_balanced_outline(&guide);
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
    fn separate_window_creation_and_selector_are_discoverable_without_global_key_hijacks() {
        let create = event("KeyN", "N", SHIFT, KeyAction::Press);
        let choose = event("KeyW", "W", SHIFT, KeyAction::Press);
        assert_eq!(
            lookup(Context::CommandPrefix, &create),
            Some(A::CreateTerminal)
        );
        assert_eq!(
            lookup(Context::CommandPrefix, &choose),
            Some(A::ChooseWindow)
        );
        assert_eq!(
            lookup(
                Context::CommandPrefix,
                &event("KeyW", "w", 0, KeyAction::Press)
            ),
            Some(A::ChooseWorkspace)
        );
        assert_eq!(lookup(Context::Application, &create), None);
        assert_eq!(lookup(Context::Application, &choose), None);
        let hints = hints(Context::CommandPrefix);
        assert!(hints.iter().any(|hint| hint.action == A::CreateTerminal));
        assert!(hints.iter().any(|hint| hint.action == A::ChooseWindow));
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
        assert_eq!(lookup(context, &key), Some(A::CreateTerminal));
        assert!(
            hints(context)
                .iter()
                .any(|hint| hint.action == A::CreateTerminal)
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
    }

    #[test]
    fn narrow_expanded_guide_keeps_the_complete_action_explanation() {
        let context = Context::CommandPrefix;
        let action = A::RemovePaneFromLayout;
        let guide = render_with(context, 38, 32, 8, |candidate| candidate == action);
        let visible = guide
            .expanded
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            visible.ends_with(action.description(context)),
            "the guide cut off the operation's lifetime semantics: {visible}"
        );
    }
    #[test]
    fn wrapped_aliases_keep_complete_keycaps_and_never_style_description_continuations() {
        let hint = Hint {
            keys: vec!["Ctrl-Left", "Ctrl-h", "/"],
            action: A::Edit(EditAction::Left),
            description: "move the input cursor by a complete grapheme",
            priority: 0,
        };
        let (rows, spans) = Body::new(std::slice::from_ref(&hint)).render(24, 12);
        let keys: Vec<String> = spans
            .iter()
            .map(|span| {
                rows[span.rect.y as usize]
                    .chars()
                    .skip(span.rect.x as usize)
                    .take(span.rect.width as usize)
                    .collect()
            })
            .collect();
        assert_eq!(keys, hint.keys);
        assert!(rows.iter().all(|row| width(row) <= 24));
        let (_, clipped) = Body::new(&[hint]).render(24, 1);
        assert!(
            clipped.is_empty(),
            "an incomplete action must not publish executable keycaps"
        );
    }
    #[test]
    fn popup_card_fits_every_small_viewport() {
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
                for max_rows in 0..=8 {
                    let guide = render_with(context, columns, columns, max_rows, |_| true);
                    assert!(guide.columns <= columns);
                    assert!(guide.expanded.len() <= max_rows);
                    assert!(
                        guide
                            .expanded
                            .iter()
                            .all(|row| width(row) <= usize::from(guide.columns.saturating_sub(2)))
                    );
                    for span in &guide.expanded_roles {
                        assert!(usize::from(span.rect.y) < guide.expanded.len());
                        assert!(span.rect.x + span.rect.width <= guide.columns.saturating_sub(2));
                    }
                }
            }
        }
    }
}
