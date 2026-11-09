//! Native frontend bindings and their contextual guide, kept in one source of truth.
//!
//! The local-context, atomic-action and width-bounded guide semantics follow Bkpr's
//! `crates/bkpr-zellij/src/keyguide.rs` (main, 9ad93bd). This implementation does
//! not import Zellij modes, actions, SDK keymaps or terminal-output rendering.

use bkpr_protocol::{KeyAction, KeyEvent};
use bkpr_tui_framework::chord::{Chord, ChordGroup};
use bkpr_tui_framework::{EditAction, Rect, Role, RoleSpan};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Application,
    CommandPrefix,
    Sidebar { selected: bool },
    Sessions,
    Create,
    Edit,
    Confirmation,
    Finder,
    Theme,
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
    Theme,
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
                ModalContext::Theme => Context::Theme,
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
    EnterSessions,
    ReconnectSession,
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
    OpenThemes,
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
            Self::ToggleSidebar => "sidebar",
            Self::EnterSessions => "sessions",
            Self::ReconnectSession => "force reconnect session",
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
            Self::OpenThemes => "themes",
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
            Self::ReopenPane => "reopen shell",
            Self::SplitHorizontal => "split side by side",
            Self::SplitVertical => "split above / below",
            Self::RemovePaneFromLayout => "remove from layout",
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
const THEME: u16 = 1 << 13;
const SESSIONS: u16 = 1 << 14;
const SHIFT: u16 = 1;
const CTRL: u16 = 2;
const LOCKS: u16 = 16 | 32;

// Protocol modifier bits are shift/control/alt/super in the low four bits.
// Physical bindings select their structured labels at const construction, so
// every combination is supported without a render-time parser or panic.
const MODIFIER_LABELS: [&[&str]; 16] = [
    &[],
    &["Shift"],
    &["Ctrl"],
    &["Ctrl", "Shift"],
    &["Alt"],
    &["Alt", "Shift"],
    &["Ctrl", "Alt"],
    &["Ctrl", "Alt", "Shift"],
    &["Super"],
    &["Shift", "Super"],
    &["Ctrl", "Super"],
    &["Ctrl", "Shift", "Super"],
    &["Alt", "Super"],
    &["Alt", "Shift", "Super"],
    &["Ctrl", "Alt", "Super"],
    &["Ctrl", "Alt", "Shift", "Super"],
];

impl Context {
    fn mask(self) -> u16 {
        match self {
            Self::Application => APPLICATION,
            Self::CommandPrefix => PREFIX,
            Self::Sidebar { .. } => SIDEBAR,
            Self::Sessions => SESSIONS,
            Self::Create => CREATE,
            Self::Edit => EDIT,
            Self::Confirmation => CONFIRM,
            Self::Finder => FINDER,
            Self::Theme => THEME,
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
            Self::Sessions => "Sessions",
            Self::Create => "Create",
            Self::Edit => "Edit",
            Self::Confirmation => "Confirm",
            Self::Finder => "Find panes",
            Self::Theme => "Choose theme",
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
    /// Structured display chord; produced-text bindings have no extra modifier.
    pub chord: Chord<'static>,
    pub action: Action,
    pub priority: u8,
}

impl Binding {
    fn available(self, context: Context) -> bool {
        self.contexts & context.mask() != 0
            && (!self.selected
                || matches!(
                    context,
                    Context::Sidebar { selected: true } | Context::Sessions
                ))
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
    keycap: &'static str,
    action: Action,
    priority: u8,
    repeat: bool,
) -> Binding {
    // Locks are ignored by dispatch, and cannot be required by a binding.
    assert!(modifiers < MODIFIER_LABELS.len() as u16);
    Binding {
        contexts,
        key: Key::Physical(key, modifiers),
        selected: false,
        repeat,
        chord: Chord {
            key: keycap,
            modifiers: MODIFIER_LABELS[modifiers as usize],
        },
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
        chord: Chord {
            key,
            modifiers: &[],
        },
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
// Session roots and native rows share tree navigation; only session roots offer
// reconnect, and native management remains tied to an owning native context.
static BINDINGS: &[Binding] = &[
    text(SESSIONS, "R", A::ReconnectSession, 5),
    physical(
        APPLICATION | SIDEBAR | SESSIONS | HISTORY | SEARCH | RESIZE,
        "KeyB",
        CTRL,
        "b",
        A::CommandPrefix,
        1,
        false,
    ),
    physical(
        APPLICATION | SIDEBAR | SESSIONS | HISTORY,
        "Space",
        CTRL,
        "Space",
        A::ToggleSidebar,
        0,
        false,
    ),
    physical(
        APPLICATION,
        "PageUp",
        SHIFT,
        "PgUp",
        A::History(H::PageUp),
        10,
        true,
    ),
    physical(
        APPLICATION,
        "PageDown",
        SHIFT,
        "PgDn",
        A::History(H::PageDown),
        11,
        true,
    ),
    physical(PREFIX, "KeyB", CTRL, "b", A::SendPrefix, 80, false),
    physical(PREFIX, "Escape", 0, "Esc", A::Cancel, 0, false),
    text(SIDEBAR | SESSIONS, "S", A::EnterSessions, 2),
    text(PREFIX, "/", A::StartSearch, 10),
    text(PREFIX, "s", A::StartHistory, 11),
    text(PREFIX, "e", A::StartResize, 12),
    text(PREFIX, "y", A::CopySelection, 13),
    text(PREFIX, "i", A::ProjectDetails, 30),
    text(PREFIX, "g", A::ToggleChrome, 31),
    text(PREFIX, "t", A::OpenThemes, 32),
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
        "←",
        A::MovePane(D::Left),
        6,
        false,
    ),
    physical(
        PREFIX,
        "ArrowRight",
        SHIFT,
        "→",
        A::MovePane(D::Right),
        7,
        false,
    ),
    physical(PREFIX, "ArrowUp", SHIFT, "↑", A::MovePane(D::Up), 8, false),
    physical(
        PREFIX,
        "ArrowDown",
        SHIFT,
        "↓",
        A::MovePane(D::Down),
        9,
        false,
    ),
    physical(
        SIDEBAR | SESSIONS,
        "Escape",
        0,
        "Esc",
        A::LeaveSidebar,
        0,
        false,
    ),
    text(SIDEBAR, "/", A::OpenFinder, 5),
    physical(
        SIDEBAR | SESSIONS | FINDER | CHOOSER | DETAILS | THEME,
        "ArrowUp",
        0,
        "↑",
        A::Navigate(N::Previous),
        10,
        true,
    ),
    physical(
        SIDEBAR | SESSIONS | FINDER | CHOOSER | DETAILS | THEME,
        "ArrowDown",
        0,
        "↓",
        A::Navigate(N::Next),
        11,
        true,
    ),
    repeating(text(SIDEBAR | SESSIONS, "k", A::Navigate(N::Previous), 10)),
    repeating(text(SIDEBAR | SESSIONS, "j", A::Navigate(N::Next), 11)),
    physical(
        SIDEBAR | SESSIONS | FINDER | DETAILS | THEME,
        "PageUp",
        0,
        "PgUp",
        A::Navigate(N::PagePrevious),
        35,
        true,
    ),
    physical(
        SIDEBAR | SESSIONS | FINDER | DETAILS | THEME,
        "PageDown",
        0,
        "PgDn",
        A::Navigate(N::PageNext),
        36,
        true,
    ),
    physical(
        SIDEBAR | SESSIONS | CHOOSER | DETAILS,
        "Home",
        0,
        "Home",
        A::Navigate(N::First),
        33,
        true,
    ),
    physical(
        SIDEBAR | SESSIONS | CHOOSER | DETAILS,
        "End",
        0,
        "End",
        A::Navigate(N::Last),
        34,
        true,
    ),
    repeating(text(SIDEBAR | SESSIONS, "g", A::Navigate(N::First), 33)),
    repeating(text(SIDEBAR | SESSIONS, "G", A::Navigate(N::Last), 34)),
    selected(physical(
        SIDEBAR | SESSIONS,
        "ArrowLeft",
        0,
        "←",
        A::FoldSidebar,
        13,
        false,
    )),
    selected(physical(
        SIDEBAR | SESSIONS,
        "ArrowRight",
        0,
        "→",
        A::UnfoldSidebar,
        14,
        false,
    )),
    selected(physical(
        SIDEBAR | SESSIONS,
        "Space",
        0,
        "Space",
        A::ToggleFoldSidebar,
        15,
        false,
    )),
    selected(physical(
        SIDEBAR | SESSIONS,
        "Enter",
        0,
        "Enter",
        A::ActivateSelected,
        12,
        false,
    )),
    text(SIDEBAR | SESSIONS, "n", A::CreateWorkspace, 20),
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
        CREATE | EDIT | CONFIRM | FINDER | SEARCH | CHOOSER | DETAILS | THEME,
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
        CREATE | EDIT | CONFIRM | FINDER | CHOOSER | THEME,
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
        CREATE | EDIT | FINDER | SEARCH | THEME,
        "Backspace",
        0,
        "Backspace",
        A::Edit(E::Backspace),
        40,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | SEARCH | THEME,
        "KeyU",
        CTRL,
        "u",
        A::Edit(E::Clear),
        41,
        false,
    ),
    physical(CREATE, "Tab", 0, "Tab", A::CycleField, 10, false),
    physical(CREATE, "Tab", SHIFT, "Tab", A::CycleField, 10, false),
    physical(FINDER, "Tab", 0, "Tab", A::CycleView, 12, false),
    physical(
        FINDER | THEME,
        "KeyP",
        CTRL,
        "p",
        A::Navigate(N::Previous),
        10,
        true,
    ),
    physical(
        FINDER | THEME,
        "KeyN",
        CTRL,
        "n",
        A::Navigate(N::Next),
        11,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | THEME,
        "ArrowLeft",
        0,
        "←",
        A::Edit(E::Left),
        30,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | THEME,
        "ArrowRight",
        0,
        "→",
        A::Edit(E::Right),
        31,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | THEME,
        "Home",
        0,
        "Home",
        A::Edit(E::Home),
        32,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | THEME,
        "End",
        0,
        "End",
        A::Edit(E::End),
        33,
        true,
    ),
    physical(
        CREATE | EDIT | FINDER | THEME,
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
        "PgUp",
        A::History(H::PageUp),
        12,
        true,
    ),
    physical(
        HISTORY,
        "PageDown",
        SHIFT,
        "PgDn",
        A::History(H::PageDown),
        13,
        true,
    ),
    physical(HISTORY, "Home", 0, "Home", A::History(H::Oldest), 14, false),
    physical(HISTORY, "Escape", 0, "Esc", A::History(H::Live), 0, false),
    physical(HISTORY, "Enter", 0, "Enter", A::History(H::Live), 0, false),
    physical(HISTORY, "End", 0, "End", A::History(H::Live), 0, false),
    physical(SEARCH, "Enter", 0, "Enter", A::NextMatch, 10, true),
    physical(SEARCH, "Enter", SHIFT, "Enter", A::PreviousMatch, 11, true),
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
        "←",
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
        "→",
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
        "↑",
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
        "↓",
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

/// One executable action and all of its available alternative chords.
#[derive(Clone, Debug)]
pub struct HintMember {
    pub action: Action,
    pub aliases: Vec<Chord<'static>>,
}

/// A presentation row retains every concrete action; dispatch never uses rows.
#[derive(Clone, Debug)]
pub struct Hint {
    pub members: Vec<HintMember>,
    pub description: &'static str,
    pub priority: u8,
    group: Option<usize>,
}

struct PresentationGroup {
    contexts: u16,
    actions: &'static [Action],
    description: &'static str,
    priority: u8,
}

// Membership, order and row priority are product policy, not label heuristics.
// Descriptions remain truthful when capability filtering leaves a partial group.
static PRESENTATION_GROUPS: &[PresentationGroup] = &[
    PresentationGroup {
        contexts: SIDEBAR,
        actions: &[A::LeaveSidebar, A::ToggleSidebar],
        description: "return to app",
        priority: 0,
    },
    PresentationGroup {
        contexts: SIDEBAR | SESSIONS,
        actions: &[A::FoldSidebar, A::UnfoldSidebar, A::ToggleFoldSidebar],
        description: "fold/unfold workspace",
        priority: 13,
    },
    PresentationGroup {
        contexts: PREFIX,
        actions: &[
            A::FocusPane(D::Left),
            A::FocusPane(D::Right),
            A::FocusPane(D::Up),
            A::FocusPane(D::Down),
        ],
        description: "focus pane",
        priority: 2,
    },
    PresentationGroup {
        contexts: PREFIX,
        actions: &[
            A::MovePane(D::Left),
            A::MovePane(D::Right),
            A::MovePane(D::Up),
            A::MovePane(D::Down),
        ],
        description: "move pane",
        priority: 6,
    },
    PresentationGroup {
        contexts: RESIZE,
        actions: &[
            A::Resize {
                direction: D::Left,
                cells: 1,
            },
            A::Resize {
                direction: D::Right,
                cells: 1,
            },
            A::Resize {
                direction: D::Up,
                cells: 1,
            },
            A::Resize {
                direction: D::Down,
                cells: 1,
            },
        ],
        description: "resize by 1 cell",
        priority: 10,
    },
    PresentationGroup {
        contexts: RESIZE,
        actions: &[
            A::Resize {
                direction: D::Left,
                cells: 5,
            },
            A::Resize {
                direction: D::Right,
                cells: 5,
            },
            A::Resize {
                direction: D::Up,
                cells: 5,
            },
            A::Resize {
                direction: D::Down,
                cells: 5,
            },
        ],
        description: "resize by 5 cells",
        priority: 20,
    },
    PresentationGroup {
        contexts: SIDEBAR | SESSIONS | FINDER | CHOOSER | DETAILS | THEME,
        actions: &[A::Navigate(N::Previous), A::Navigate(N::Next)],
        description: "select item",
        priority: 10,
    },
    PresentationGroup {
        contexts: SIDEBAR | SESSIONS | CHOOSER | DETAILS,
        actions: &[A::Navigate(N::First), A::Navigate(N::Last)],
        description: "jump to start/end",
        priority: 33,
    },
    PresentationGroup {
        contexts: SIDEBAR | SESSIONS | DETAILS | THEME,
        actions: &[A::Navigate(N::PagePrevious), A::Navigate(N::PageNext)],
        description: "change page",
        priority: 35,
    },
    PresentationGroup {
        contexts: FINDER,
        actions: &[A::Navigate(N::PagePrevious), A::Navigate(N::PageNext)],
        description: "scroll preview",
        priority: 35,
    },
    PresentationGroup {
        contexts: PREFIX,
        actions: &[A::PreviousWorkspace, A::NextWorkspace],
        description: "switch workspace",
        priority: 16,
    },
    PresentationGroup {
        contexts: HISTORY,
        actions: &[A::History(H::LineUp), A::History(H::LineDown)],
        description: "scroll history",
        priority: 10,
    },
    PresentationGroup {
        contexts: APPLICATION,
        actions: &[A::History(H::PageUp), A::History(H::PageDown)],
        description: "scroll history",
        priority: 10,
    },
    PresentationGroup {
        contexts: HISTORY,
        actions: &[A::History(H::PageUp), A::History(H::PageDown)],
        description: "scroll history",
        priority: 12,
    },
    PresentationGroup {
        contexts: SEARCH,
        actions: &[A::NextMatch, A::PreviousMatch],
        description: "find next/previous",
        priority: 10,
    },
];

#[cfg(test)]
pub fn hints(context: Context) -> Vec<Hint> {
    hints_with(context, supported_by_default)
}

pub fn hints_with(context: Context, supported: impl Fn(Action) -> bool) -> Vec<Hint> {
    let mut hints: Vec<Hint> = Vec::new();
    for binding in bindings_with(context, supported) {
        let group = PRESENTATION_GROUPS.iter().position(|group| {
            group.contexts & context.mask() != 0 && group.actions.contains(&binding.action)
        });
        let existing = hints.iter_mut().find(|hint| {
            if let Some(group) = group {
                hint.group == Some(group)
            } else {
                hint.group.is_none()
                    && hint
                        .members
                        .iter()
                        .any(|member| member.action == binding.action)
            }
        });
        let hint = if let Some(hint) = existing {
            hint
        } else {
            let (description, priority) = group.map_or(
                (binding.action.description(context), binding.priority),
                |index| {
                    let group = &PRESENTATION_GROUPS[index];
                    (group.description, group.priority)
                },
            );
            hints.push(Hint {
                members: Vec::new(),
                description,
                priority,
                group,
            });
            hints.last_mut().unwrap()
        };
        let chord = binding.chord;
        if let Some(member) = hint
            .members
            .iter_mut()
            .find(|member| member.action == binding.action)
        {
            if !member.aliases.contains(&chord) {
                member.aliases.push(chord);
            }
        } else {
            hint.members.push(HintMember {
                action: binding.action,
                aliases: vec![chord],
            });
        }
    }
    for hint in &mut hints {
        if let Some(index) = hint.group {
            let order = PRESENTATION_GROUPS[index].actions;
            hint.members
                .sort_by_key(|member| order.iter().position(|action| *action == member.action));
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

struct Body<'a> {
    hints: &'a [Hint],
    chords: Vec<ChordGroup>,
    minimum_key: usize,
    key_width: usize,
    description: usize,
}

impl<'a> Body<'a> {
    fn new(hints: &'a [Hint]) -> Self {
        let chords: Vec<_> = hints
            .iter()
            .map(|hint| {
                ChordGroup::new(
                    hint.members
                        .iter()
                        .map(|member| member.aliases.iter().copied()),
                )
            })
            .collect();
        let minimum_key = chords.iter().map(ChordGroup::min_width).max().unwrap_or(0);
        let key_width = chords.iter().map(ChordGroup::width).max().unwrap_or(0);
        let description = hints
            .iter()
            .map(|hint| width(hint.description))
            .max()
            .unwrap_or(0);
        Self {
            hints,
            chords,
            minimum_key,
            key_width,
            description,
        }
    }

    fn render(&self, columns: usize, max_rows: usize) -> (Vec<String>, Vec<RoleSpan>) {
        if columns < 4 || max_rows == 0 || self.hints.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let key_columns = if self.key_width + 2 + self.description <= columns {
            self.key_width
        } else {
            self.minimum_key.max(self.key_width.div_ceil(2))
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
        for (hint, chords) in self.hints.iter().zip(&self.chords) {
            let Some(keys) = chords.wrap(key_columns) else {
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
                if let Some(keys) = keys.get(line) {
                    let x = key_columns - keys.width;
                    row.extend(std::iter::repeat_n(' ', x));
                    let mut push_span = |start: usize, end: usize, role| {
                        if start < end {
                            roles.push(RoleSpan {
                                rect: Rect {
                                    x: (x + start) as u16,
                                    y: rows.len() as u16,
                                    width: (end - start) as u16,
                                    height: 1,
                                },
                                role,
                            });
                        }
                    };
                    let mut end = 0;
                    for keycap in &keys.keycaps {
                        push_span(end, keycap.start, Role::Muted);
                        push_span(keycap.start, keycap.end, Role::Accent);
                        end = keycap.end;
                    }
                    push_span(end, keys.width, Role::Muted);
                    row.push_str(&keys.text);
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
    notice: Option<&str>,
) -> Guide {
    if max_columns == 0 || max_rows == 0 {
        return Guide::default();
    }
    let hints = hints_with(context, supported);
    render_hints(
        context,
        &hints,
        sidebar_columns,
        max_columns,
        max_rows,
        notice,
    )
}

fn render_hints(
    context: Context,
    hints: &[Hint],
    sidebar_columns: u16,
    max_columns: u16,
    max_rows: usize,
    notice: Option<&str>,
) -> Guide {
    if max_columns == 0 || max_rows == 0 {
        return Guide::default();
    }
    let body = Body::new(hints);
    let title_columns = width(context.title()) + 5;
    let limit = usize::from(max_columns.min(sidebar_columns.max(72)));
    let provisional = (body.key_width + 2 + body.description + 4)
        .max(title_columns)
        .max(usize::from(sidebar_columns))
        .min(limit);
    let padding = usize::from(provisional >= 8);
    let overhead = 2 + 2 * padding;
    let inner_columns = provisional.saturating_sub(overhead);
    // Reserve notice rows before laying out whole logical groups. A notice must
    // never force the caller to truncate a chord group or its explanation.
    let notice: Vec<_> = notice
        .filter(|_| provisional >= 8)
        .map(|text| {
            textwrap::wrap(text, inner_columns)
                .into_iter()
                .take(max_rows)
                .collect()
        })
        .unwrap_or_default();
    let body_rows = max_rows - notice.len();
    let (mut expanded, mut expanded_roles) = body.render(inner_columns, body_rows);
    if expanded.is_empty() && body_rows > 0 {
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
    let notice_columns = notice.iter().map(|line| width(line)).max().unwrap_or(0);
    let columns = (outline.max(notice_columns) + overhead)
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
        span.rect.y += notice.len() as u16;
    }
    let notice_padding = " ".repeat(padding);
    expanded.splice(
        0..0,
        notice
            .iter()
            .map(|line| format!("{notice_padding}{line}{notice_padding}")),
    );
    // Keep the shared right margin outside content centering and wrapping.
    let columns = columns.saturating_add(1).min(max_columns);
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

    fn hint(
        action: Action,
        aliases: &[Chord<'static>],
        description: &'static str,
        priority: u8,
    ) -> Hint {
        Hint {
            members: vec![HintMember {
                action,
                aliases: aliases.to_vec(),
            }],
            description,
            priority,
            group: None,
        }
    }

    fn plain(key: &'static str) -> Chord<'static> {
        Chord {
            key,
            modifiers: &[],
        }
    }

    fn modified(key: &'static str, modifiers: &'static [&'static str]) -> Chord<'static> {
        Chord { key, modifiers }
    }

    fn advertised(context: Context) -> impl Iterator<Item = Action> {
        hints(context)
            .into_iter()
            .flat_map(|hint| hint.members.into_iter().map(|member| member.action))
    }

    fn keycaps(rows: &[String], spans: &[RoleSpan]) -> Vec<String> {
        styled_text(rows, spans, Role::Accent)
    }

    fn styled_text(rows: &[String], spans: &[RoleSpan], role: Role) -> Vec<String> {
        spans
            .iter()
            .filter(|span| span.role == role)
            .map(|span| {
                let mut column = 0;
                rows[usize::from(span.rect.y)]
                    .chars()
                    .filter(|character| {
                        let start = column;
                        column += unicode_width::UnicodeWidthChar::width(*character).unwrap_or(0);
                        start >= usize::from(span.rect.x)
                            && start < usize::from(span.rect.x + span.rect.width)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn chord_punctuation_is_muted_but_literal_punctuation_keys_are_accented() {
        let mut group = hint(
            A::MovePane(D::Left),
            &[modified("←", &["Shift"]), modified("/", &["Shift"])],
            "move",
            0,
        );
        group.members.push(HintMember {
            action: A::MovePane(D::Right),
            aliases: vec![modified("→", &["Shift"])],
        });
        let guide = render_hints(Context::CommandPrefix, &[group], 38, 120, 8, None);
        assert_eq!(
            keycaps(&guide.expanded, &guide.expanded_roles),
            ["Shift-", "←", "/", "→"]
        );
        assert_eq!(
            styled_text(&guide.expanded, &guide.expanded_roles, Role::Muted),
            ["(", "/", ", ", ")"]
        );
    }

    #[test]
    fn sidebar_fold_and_exit_groups_retain_each_concrete_binding() {
        let context = Context::Sidebar { selected: true };
        for (actions, keys) in [
            (
                &[A::LeaveSidebar, A::ToggleSidebar][..],
                vec![
                    event("Escape", "", 0, KeyAction::Press),
                    event("Space", "", CTRL, KeyAction::Press),
                ],
            ),
            (
                &[A::FoldSidebar, A::UnfoldSidebar, A::ToggleFoldSidebar][..],
                vec![
                    event("ArrowLeft", "", 0, KeyAction::Press),
                    event("ArrowRight", "", 0, KeyAction::Press),
                    event("Space", " ", 0, KeyAction::Press),
                ],
            ),
        ] {
            let rows = hints_with(context, |action| actions.contains(&action));
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0]
                    .members
                    .iter()
                    .map(|member| member.action)
                    .collect::<Vec<_>>(),
                actions
            );
            for (action, key) in actions.iter().zip(keys) {
                assert_eq!(lookup(context, &key), Some(*action));
            }
            let partial = hints_with(context, |action| action == actions[0]);
            assert_eq!(partial[0].members.len(), 1);
            assert_eq!(partial[0].members[0].action, actions[0]);
        }
    }

    #[test]
    fn every_physical_modifier_combination_has_structured_labels() {
        for modifiers in 0..16 {
            let binding = physical(PREFIX, "KeyA", modifiers, "a", A::SendPrefix, 0, false);
            assert_eq!(binding.chord.key, "a");
            assert_eq!(
                binding.chord.modifiers.len(),
                modifiers.count_ones() as usize
            );
            for (mask, label) in [(1, "Shift"), (2, "Ctrl"), (4, "Alt"), (8, "Super")] {
                assert_eq!(
                    binding.chord.modifiers.contains(&label),
                    modifiers & mask != 0
                );
            }
            assert!(binding.matches(&event("KeyA", "", modifiers | LOCKS, KeyAction::Press)));
        }
        let produced = text(PREFIX, "%", A::SplitHorizontal, 0);
        assert_eq!(produced.chord, plain("%"));
    }

    #[test]
    fn directional_rows_retain_each_action_and_dispatch_its_own_chord() {
        let context = Context::CommandPrefix;
        let rows = hints_with(context, |action| {
            matches!(action, A::FocusPane(_) | A::MovePane(_))
        });
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().map(|row| row.priority).collect::<Vec<_>>(),
            [2, 6]
        );
        for (row, shifted) in rows.iter().zip([false, true]) {
            assert_eq!(row.members.len(), 4);
            for (member, direction) in row.members.iter().zip([D::Left, D::Right, D::Up, D::Down]) {
                let expected = if shifted {
                    A::MovePane(direction)
                } else {
                    A::FocusPane(direction)
                };
                assert_eq!(member.action, expected);
                assert_eq!(member.aliases.len(), 1);
                assert_eq!(
                    member.aliases[0].modifiers,
                    if shifted { &["Shift"][..] } else { &[][..] }
                );
                assert_eq!(
                    lookup(
                        context,
                        &event(
                            direction.key(),
                            "",
                            if shifted { SHIFT } else { 0 },
                            KeyAction::Press
                        )
                    ),
                    Some(expected)
                );
            }
        }
        let guide = render_hints(context, &rows, 38, 120, 8, None);
        assert_eq!(
            guide.expanded.len(),
            2,
            "each direction family fits one compact row"
        );
        assert_eq!(
            keycaps(&guide.expanded, &guide.expanded_roles),
            ["←", "→", "↑", "↓", "Shift-", "←", "→", "↑", "↓"]
        );
    }

    #[test]
    fn unsupported_directions_leave_only_real_members_at_declared_priority() {
        let supported = |action| matches!(action, A::FocusPane(D::Right | D::Down));
        let rows = hints_with(Context::CommandPrefix, supported);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].priority, 2,
            "priority belongs to the group, not its first surviving binding"
        );
        assert_eq!(
            rows[0]
                .members
                .iter()
                .map(|member| member.action)
                .collect::<Vec<_>>(),
            [A::FocusPane(D::Right), A::FocusPane(D::Down)]
        );
        let guide = render_hints(Context::CommandPrefix, &rows, 38, 120, 8, None);
        assert_eq!(guide.expanded.len(), 1);
        assert_eq!(keycaps(&guide.expanded, &guide.expanded_roles), ["→", "↓"]);
        for (direction, expected) in [
            (D::Left, None),
            (D::Right, Some(A::FocusPane(D::Right))),
            (D::Up, None),
            (D::Down, Some(A::FocusPane(D::Down))),
        ] {
            assert_eq!(
                lookup_with(
                    Context::CommandPrefix,
                    &event(direction.key(), "", 0, KeyAction::Press),
                    supported
                ),
                expected
            );
        }
        assert!(hints_with(Context::CommandPrefix, |_| false).is_empty());
    }

    #[test]
    fn grouped_actions_and_same_action_aliases_remain_distinct() {
        let rows = hints_with(Context::Sidebar { selected: true }, |action| {
            matches!(action, A::Navigate(N::Previous | N::Next))
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].members.len(), 2);
        assert_eq!(rows[0].members[0].aliases, [plain("↑"), plain("k")]);
        assert_eq!(rows[0].members[1].aliases, [plain("↓"), plain("j")]);
        let group = ChordGroup::new(
            rows[0]
                .members
                .iter()
                .map(|member| member.aliases.iter().copied()),
        );
        assert_eq!(group.wrap(group.width()).unwrap()[0].text, "↑/k, ↓/j");
        let aliases = hints_with(Context::CommandPrefix, |action| {
            action == A::SplitHorizontal
        });
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].members.len(), 1);
        assert_eq!(aliases[0].members[0].aliases, [plain("n"), plain("%")]);
        assert_eq!(
            lookup(
                Context::CommandPrefix,
                &event("Digit5", "%", SHIFT, KeyAction::Press)
            ),
            Some(A::SplitHorizontal)
        );
        assert_eq!(
            lookup(
                Context::CommandPrefix,
                &event("Digit5", "%", CTRL | SHIFT, KeyAction::Press)
            ),
            None
        );
    }

    #[test]
    fn resize_step_sizes_have_separate_complete_modifier_scopes() {
        let rows = hints_with(Context::Resize, |action| matches!(action, A::Resize { .. }));
        assert_eq!(rows.len(), 2);
        for (row, cells, modifiers) in [(&rows[0], 1, 0), (&rows[1], 5, SHIFT)] {
            assert_eq!(row.members.len(), 4);
            for (member, direction) in row.members.iter().zip([D::Left, D::Right, D::Up, D::Down]) {
                let expected = A::Resize { direction, cells };
                assert_eq!(member.action, expected);
                assert_eq!(
                    lookup(
                        Context::Resize,
                        &event(direction.key(), "", modifiers, KeyAction::Repeat)
                    ),
                    Some(expected)
                );
            }
        }
        let guide = render_hints(Context::Resize, &rows, 38, 120, 8, None);
        assert_eq!(guide.expanded.len(), 2);
        assert!(guide.expanded[1].contains("Shift-("));
        assert_eq!(
            keycaps(&guide.expanded, &guide.expanded_roles),
            ["←", "→", "↑", "↓", "Shift-", "←", "→", "↑", "↓"]
        );
    }

    #[test]
    fn contextual_groups_preserve_every_available_alias_exactly_once() {
        for context in [
            Context::Application,
            Context::CommandPrefix,
            Context::Sidebar { selected: true },
            Context::Sidebar { selected: false },
            Context::Sessions,
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
            Context::Theme,
        ] {
            let rows = hints(context);
            assert!(
                rows.windows(2)
                    .all(|pair| pair[0].priority <= pair[1].priority)
            );
            for binding in bindings_with(context, |_| true) {
                let members: Vec<_> = rows
                    .iter()
                    .flat_map(|row| &row.members)
                    .filter(|member| member.action == binding.action)
                    .collect();
                assert_eq!(members.len(), 1);
                assert!(members[0].aliases.contains(&binding.chord));
            }
            for row in &rows {
                if let Some(group) = row.group {
                    let declaration = &PRESENTATION_GROUPS[group];
                    assert_eq!(row.priority, declaration.priority);
                    let positions: Vec<_> = row
                        .members
                        .iter()
                        .map(|member| {
                            declaration
                                .actions
                                .iter()
                                .position(|action| *action == member.action)
                                .unwrap()
                        })
                        .collect();
                    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
                }
            }
        }
    }

    fn wrapping_group() -> Hint {
        Hint {
            members: vec![
                HintMember {
                    action: A::FocusPane(D::Left),
                    aliases: vec![modified("LongKeyA", &["Ctrl"])],
                },
                HintMember {
                    action: A::FocusPane(D::Right),
                    aliases: vec![modified("LongKeyB", &["Ctrl"])],
                },
            ],
            description: "move input",
            priority: 0,
            group: None,
        }
    }

    #[test]
    fn narrow_groups_wrap_complete_chords_or_omit_the_whole_row() {
        let row = wrapping_group();
        let (lines, roles) = Body::new(std::slice::from_ref(&row)).render(30, 8);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            keycaps(&lines, &roles),
            ["Ctrl-", "LongKeyA", "Ctrl-", "LongKeyB"]
        );
        assert!(lines.iter().all(|line| width(line) <= 30));
        for (columns, max_rows) in [(30, 1), (16, 8)] {
            let (lines, roles) = Body::new(std::slice::from_ref(&row)).render(columns, max_rows);
            assert!(
                roles.is_empty(),
                "partial logical rows must not publish any executable chord"
            );
            assert_eq!(lines.len(), 1);
        }
    }

    #[test]
    fn notices_reserve_rows_before_whole_group_layout() {
        let hints = [wrapping_group()];
        let normal = render_hints(Context::CommandPrefix, &hints, 34, 34, 2, None);
        assert_eq!(normal.expanded.len(), 2);
        assert!(!normal.expanded_roles.is_empty());
        let omitted = render_hints(Context::CommandPrefix, &hints, 34, 34, 2, Some("pending"));
        assert_eq!(omitted.expanded.len(), 2);
        assert!(omitted.expanded[0].contains("pending"));
        assert!(
            omitted.expanded_roles.is_empty(),
            "notice cannot truncate a multiline group"
        );
        let complete = render_hints(Context::CommandPrefix, &hints, 34, 34, 3, Some("pending"));
        assert_eq!(complete.expanded.len(), 3);
        assert!(complete.expanded_roles.iter().all(|span| span.rect.y >= 1));
        assert_eq!(
            keycaps(&complete.expanded, &complete.expanded_roles),
            ["Ctrl-", "LongKeyA", "Ctrl-", "LongKeyB"]
        );
        let notice_only = render_hints(Context::CommandPrefix, &hints, 34, 34, 1, Some("pending"));
        assert_eq!(notice_only.expanded.len(), 1);
        assert!(notice_only.expanded[0].contains("pending"));
        assert!(notice_only.expanded_roles.is_empty());
    }

    #[test]
    fn every_keybind_popup_extends_right_without_moving_its_content() {
        let hints = [
            hint(
                A::ActivateSelected,
                &[modified("a", &["Ctrl"]), plain("a")],
                "select",
                0,
            ),
            hint(A::ToggleSidebar, &[plain("b")], "open", 1),
        ];
        for context in [
            Context::Application,
            Context::CommandPrefix,
            Context::Sidebar { selected: true },
            Context::Sidebar { selected: false },
            Context::Sessions,
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
        ] {
            for sidebar_columns in [38, 80] {
                let constrained =
                    render_hints(context, &hints, sidebar_columns, sidebar_columns, 8, None);
                let padded = render_hints(
                    context,
                    &hints,
                    sidebar_columns,
                    sidebar_columns + 1,
                    8,
                    None,
                );
                assert_eq!(constrained.columns, sidebar_columns);
                assert_eq!(padded.columns, sidebar_columns + 1);
                assert_eq!(padded.expanded, constrained.expanded);
                assert_eq!(padded.expanded_roles, constrained.expanded_roles);
            }
        }
    }

    #[test]
    fn asymmetric_descriptions_keep_a_content_sized_gutter() {
        let hint = hint(
            A::RemovePaneFromLayout,
            &[plain("x")],
            "remove the layout entry while keeping the terminal process",
            0,
        );
        let guide = render_hints(Context::CommandPrefix, &[hint], 20, 120, 8, None);
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
    }

    #[test]
    fn short_table_aligns_aliases_and_description_columns() {
        let hints = [
            hint(
                A::ActivateSelected,
                &[modified("a", &["Ctrl"]), plain("a")],
                "select",
                0,
            ),
            hint(A::ToggleSidebar, &[plain("b")], "open", 1),
        ];
        let guide = render_hints(Context::CommandPrefix, &hints, 38, 120, 8, None);
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
    }

    #[test]
    fn omitted_long_description_does_not_expand_the_visible_card() {
        let hints = [
            hint(A::Submit, &[plain("Enter")], "apply", 0),
            hint(
                A::RemovePaneFromLayout,
                &[plain("z")],
                "this complete explanation needs several wrapped rows to describe an operation without losing any of its important terminal process lifetime and layout ownership semantics",
                1,
            ),
        ];
        let guide = render_hints(Context::CommandPrefix, &hints, 24, 120, 2, None);
        assert!(guide.expanded[0].contains("apply"));
        assert!(guide.expanded[1].contains("more"));
        assert_eq!(guide.expanded_roles.len(), 1);
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
        assert!(!advertised(context).any(|action| matches!(
            action,
            A::CreateTerminal | A::RenameSelected | A::CloseSelected | A::ActivateSelected
        )));
        assert_eq!(
            lookup(context, &event("KeyN", "n", 0, KeyAction::Press)),
            Some(A::CreateWorkspace)
        );
    }

    #[test]
    fn session_roots_share_native_tree_navigation_without_header_cycling() {
        for (key, text, expected) in [
            ("ArrowUp", "", A::Navigate(N::Previous)),
            ("ArrowDown", "", A::Navigate(N::Next)),
            ("ArrowLeft", "", A::FoldSidebar),
            ("ArrowRight", "", A::UnfoldSidebar),
            ("Space", " ", A::ToggleFoldSidebar),
            ("Home", "", A::Navigate(N::First)),
            ("End", "", A::Navigate(N::Last)),
            ("PageUp", "", A::Navigate(N::PagePrevious)),
            ("PageDown", "", A::Navigate(N::PageNext)),
            ("Enter", "", A::ActivateSelected),
            ("KeyS", "S", A::EnterSessions),
        ] {
            let event = event(key, text, 0, KeyAction::Press);
            assert_eq!(lookup(Context::Sessions, &event), Some(expected));
            assert_eq!(
                lookup(Context::Sidebar { selected: true }, &event),
                Some(expected)
            );
        }
        let reconnect = event("KeyR", "R", SHIFT, KeyAction::Press);
        assert_eq!(
            lookup(Context::Sessions, &reconnect),
            Some(A::ReconnectSession)
        );
        assert_ne!(
            lookup(Context::Sidebar { selected: true }, &reconnect),
            Some(A::ReconnectSession)
        );
        assert_eq!(
            lookup(
                Context::CommandPrefix,
                &event("KeyS", "s", 0, KeyAction::Press)
            ),
            Some(A::StartHistory)
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
                !advertised(context).any(|action| matches!(action, A::Edit(_) | A::CycleField))
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
        assert!(!advertised(context).any(|action| matches!(action, A::Submit | A::Cancel)));
    }

    #[test]
    fn backend_capabilities_gate_dispatch_and_advertising_together() {
        let context = Context::Sidebar { selected: true };
        let key = event("KeyN", "N", SHIFT, KeyAction::Press);
        assert_eq!(lookup(context, &key), Some(A::CreateTerminal));
        assert!(advertised(context).any(|action| action == A::CreateTerminal));
        let supported = |action| action != A::CreateTerminal;
        assert_eq!(lookup_with(context, &key, supported), None);
        assert!(
            !hints_with(context, supported)
                .iter()
                .flat_map(|hint| &hint.members)
                .any(|member| member.action == A::CreateTerminal)
        );
        assert_eq!(
            lookup_with(context, &key, |_| true),
            Some(A::CreateTerminal)
        );
        assert!(
            hints_with(context, |_| true)
                .iter()
                .flat_map(|hint| &hint.members)
                .any(|member| member.action == A::CreateTerminal)
        );
    }

    #[test]
    fn narrow_expanded_guide_keeps_the_complete_action_explanation() {
        let context = Context::CommandPrefix;
        let action = A::RemovePaneFromLayout;
        let guide = render_with(context, 38, 32, 8, |candidate| candidate == action, None);
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
        let hint = hint(
            A::Edit(EditAction::Left),
            &[
                modified("Left", &["Ctrl"]),
                modified("h", &["Ctrl"]),
                plain("/"),
            ],
            "move the input cursor by a complete grapheme",
            0,
        );
        let (rows, spans) = Body::new(std::slice::from_ref(&hint)).render(24, 12);
        let keys: Vec<String> = spans
            .iter()
            .filter(|span| span.role == Role::Accent)
            .map(|span| {
                rows[span.rect.y as usize]
                    .chars()
                    .skip(span.rect.x as usize)
                    .take(span.rect.width as usize)
                    .collect()
            })
            .collect();
        assert_eq!(keys, ["Ctrl-Left", "Ctrl-h", "/"]);
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
            Context::Sessions,
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
                    for guide in [
                        render_with(context, columns, columns, max_rows, |_| true, None),
                        render_with(
                            context,
                            columns,
                            columns,
                            max_rows,
                            |_| true,
                            Some("Operation pending; other panes remain interactive"),
                        ),
                    ] {
                        assert!(guide.columns <= columns);
                        assert!(guide.expanded.len() <= max_rows);
                        assert!(
                            guide
                                .expanded
                                .iter()
                                .all(|row| width(row)
                                    <= usize::from(guide.columns.saturating_sub(2)))
                        );
                        for span in &guide.expanded_roles {
                            assert!(usize::from(span.rect.y) < guide.expanded.len());
                            assert!(
                                span.rect.x + span.rect.width <= guide.columns.saturating_sub(2)
                            );
                        }
                    }
                }
            }
        }
    }
}
