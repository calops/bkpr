use crate::ui;
use std::fmt::Write;
use zellij_tile::prelude::actions::{Action, SearchDirection, SearchOption};
use zellij_tile::prelude::{BareKey, Direction, InputMode, KeyWithModifier, KeybindsVec, Resize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Application,
    Sidebar { selected: bool },
    Edit { multiple_fields: bool },
    Create { multiple_fields: bool },
    Confirm,
    Finder,
}

#[derive(Clone, Copy)]
pub struct Mode<'a> {
    pub input: InputMode,
    pub keybinds: Option<&'a KeybindsVec>,
}

struct Hint {
    keys: Vec<String>,
    description: String,
    priority: u8,
}

type Bindings = [(KeyWithModifier, Vec<Action>)];

fn mode_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Normal => "Unlocked",
        InputMode::Locked => "Locked",
        InputMode::Pane => "Panes",
        InputMode::Tab => "Tabs",
        InputMode::Resize => "Resize",
        InputMode::Move => "Move",
        InputMode::Scroll => "Scroll",
        InputMode::Search => "Search",
        InputMode::EnterSearch => "Search input",
        InputMode::RenamePane => "Rename pane",
        InputMode::RenameTab => "Rename tab",
        InputMode::Session => "Session",
        InputMode::Prompt => "Prompt",
        InputMode::Tmux => "Tmux",
    }
}

fn bindings<'a>(mode: Mode<'a>) -> &'a Bindings {
    // InitialKeybinds already contains the effective, merged keymap.
    mode.keybinds
        .into_iter()
        .flatten()
        .find(|(candidate, _)| *candidate == mode.input)
        .map_or(&[], |(_, bindings)| bindings.as_slice())
}

fn key_name(key: &KeyWithModifier) -> String {
    let mut name = String::new();
    for modifier in &key.key_modifiers {
        let _ = write!(name, "{modifier}+");
    }
    let label = match key.bare_key {
        BareKey::Char(' ') => "Space",
        BareKey::Char('/') => "Slash",
        BareKey::Enter => "Enter",
        BareKey::Esc => "Esc",
        BareKey::Tab => "Tab",
        BareKey::Home => "Home",
        BareKey::End => "End",
        BareKey::Backspace => "Backspace",
        BareKey::Delete => "Del",
        _ => {
            let _ = write!(name, "{}", key.bare_key);
            if let std::borrow::Cow::Owned(clean) = ui::clean(&name) {
                return clean;
            }
            return name;
        }
    };
    name.push_str(label);
    name
}

fn add_hint(hints: &mut Vec<Hint>, key: String, description: String, priority: u8) {
    if let Some(hint) = hints
        .iter_mut()
        .find(|hint| hint.priority == priority && hint.description == description)
    {
        if !hint.keys.contains(&key) {
            hint.keys.push(key);
        }
    } else {
        hints.push(Hint {
            keys: vec![key],
            description,
            priority,
        });
    }
}

fn direction_name(direction: &Direction) -> &'static str {
    match direction {
        Direction::Left => "split left",
        Direction::Right => "split right",
        Direction::Up => "split up",
        Direction::Down => "split down",
    }
}

fn is_sidebar_mode(mode: InputMode) -> bool {
    matches!(mode, InputMode::Normal | InputMode::Locked)
}

// A binding is described as a whole: unsupported side effects make it unsuitable for a concise
// hint, and the last transition (not the first SwitchToMode) determines its destination.
fn native_hint(actions: &[Action], mode: InputMode, context: Context) -> Option<(String, u8)> {
    let mut destination = None;
    let mut global_destination = None;
    for action in actions {
        match action {
            Action::SwitchToMode { input_mode } => {
                destination = Some(*input_mode);
            }
            Action::SwitchModeForAllClients { input_mode } => {
                destination = Some(*input_mode);
                global_destination = Some(*input_mode);
            }
            _ => {}
        }
    }
    let application = context == Context::Application;
    let local = is_sidebar_mode(mode) && !application;
    let mut description = String::new();
    let mut priority = u8::MAX;
    for action in actions {
        let (text, rank) = match action {
            Action::NoOp | Action::SwitchToMode { .. } | Action::SwitchModeForAllClients { .. } => {
                continue;
            }
            Action::KeybindPipe {
                name,
                payload,
                plugin,
                plugin_id,
                ..
            } if name.as_deref() == Some(crate::CONTROL_PIPE)
                && plugin.is_none()
                && plugin_id.is_none() =>
            {
                match payload.as_deref() {
                    Some("toggle-borders") => ("fancy borders", 40),
                    Some("toggle") => (
                        if application
                            || matches!(context, Context::Finder | Context::Create { .. })
                        {
                            "sidebar"
                        } else {
                            "return to app"
                        },
                        0,
                    ),
                    Some("leave") if context == Context::Finder => ("close finder", 0),
                    Some("leave") if matches!(context, Context::Create { .. }) => ("cancel", 0),
                    Some("leave") if !application => ("return to app", 0),
                    Some("leave") => continue,
                    _ => return None,
                }
            }
            // These initialize a native text mode, rather than being separate user operations.
            Action::PaneNameInput { input }
                if input.as_slice() == [0] && destination == Some(InputMode::RenamePane) =>
            {
                continue;
            }
            Action::TabNameInput { input }
                if input.as_slice() == [0] && destination == Some(InputMode::RenameTab) =>
            {
                continue;
            }
            Action::SearchInput { input }
                if input.as_slice() == [0] && destination == Some(InputMode::EnterSearch) =>
            {
                continue;
            }
            Action::UndoRenamePane | Action::UndoRenameTab => ("cancel rename", 0),
            Action::Confirm => ("confirm", 1),
            Action::Deny => ("cancel", 0),
            Action::MoveFocus { .. } => (
                "focus panes",
                if mode == InputMode::Normal { 20 } else { 10 },
            ),
            Action::MoveFocusOrTab { .. } => (
                "focus panes / tabs",
                if mode == InputMode::Normal { 20 } else { 10 },
            ),
            Action::FocusNextPane | Action::FocusPreviousPane | Action::SwitchFocus => {
                ("cycle panes", 10)
            }
            Action::FocusLastPane => ("last pane", if mode == InputMode::Normal { 21 } else { 12 }),
            Action::NewPane { direction, .. } => (
                direction.as_ref().map_or("new pane", direction_name),
                if mode == InputMode::Normal { 30 } else { 20 },
            ),
            Action::NewStackedPane { .. } => ("stack pane", 22),
            Action::CloseFocus => ("close pane", 30),
            Action::ToggleFocusFullscreen => ("fullscreen", 25),
            Action::ToggleFocusNoUiFullscreen => ("fullscreen, hide UI", 26),
            Action::ToggleFloatingPanes => ("floating panes", 28),
            Action::TogglePaneEmbedOrFloating => ("float / embed", 29),
            Action::TogglePaneFrames => ("pane frames", 40),
            Action::TogglePanePinned => ("pin pane", 40),
            Action::NewTab { .. } => ("new tab", 20),
            Action::GoToPreviousTab | Action::GoToNextTab | Action::ToggleTab => ("change tab", 10),
            Action::CloseTab => ("close tab", 30),
            Action::ToggleActiveSyncTab => ("sync tab input", 35),
            Action::BreakPane => ("pane to new tab", 25),
            Action::BreakPaneLeft => ("pane to left tab", 26),
            Action::BreakPaneRight => ("pane to right tab", 26),
            Action::MoveTab { .. } => ("move tab", 20),
            Action::MovePane { .. } | Action::MovePaneBackwards => ("move pane", 10),
            Action::Resize {
                resize: Resize::Increase,
                ..
            } => ("grow pane", 10),
            Action::Resize {
                resize: Resize::Decrease,
                ..
            } => ("shrink pane", 11),
            Action::ScrollUp | Action::ScrollDown => ("scroll", 10),
            Action::PageScrollUp | Action::PageScrollDown => ("page", 11),
            Action::HalfPageScrollUp | Action::HalfPageScrollDown => ("half page", 12),
            Action::ScrollToTop => ("scroll to top", 14),
            Action::ScrollToBottom => ("scroll to bottom", 0),
            Action::ScrollToPreviousPrompt | Action::ScrollToNextPrompt => {
                ("previous / next prompt", 15)
            }
            Action::EditScrollback { .. } => ("edit scrollback", 25),
            Action::Copy => ("copy", 20),
            Action::CopyLastCommandOutput => ("copy command output", 21),
            Action::SelectCommandAtScrollPosition => ("select command", 22),
            Action::Search {
                direction: SearchDirection::Down,
            } => ("next match", 10),
            Action::Search {
                direction: SearchDirection::Up,
            } => ("previous match", 11),
            Action::SearchToggleOption {
                option: SearchOption::CaseSensitivity,
            } => ("match case", 20),
            Action::SearchToggleOption {
                option: SearchOption::WholeWord,
            } => ("whole words", 21),
            Action::SearchToggleOption {
                option: SearchOption::Wrap,
            } => ("wrap search", 22),
            Action::Detach => ("detach", 25),
            Action::FocusHostSession => ("host session", 10),
            Action::FocusGuestSession => ("nested session", 11),
            Action::ToggleHostFullscreen => ("host fullscreen", 20),
            Action::NextSwapLayout | Action::PreviousSwapLayout => ("change layout", 35),
            _ => return None,
        };
        // Keep global shortcuts from crowding out a submode's own controls or local editor.
        let relevant = match mode {
            InputMode::Locked => rank == 0,
            InputMode::Normal if local => rank == 0,
            InputMode::Normal => matches!(
                action,
                Action::KeybindPipe { .. }
                    | Action::MoveFocus { .. }
                    | Action::MoveFocusOrTab { .. }
                    | Action::FocusNextPane
                    | Action::FocusPreviousPane
                    | Action::FocusLastPane
                    | Action::SwitchFocus
                    | Action::NewPane { .. }
            ),
            InputMode::Pane => matches!(
                action,
                Action::MoveFocus { .. }
                    | Action::FocusNextPane
                    | Action::FocusPreviousPane
                    | Action::SwitchFocus
                    | Action::FocusLastPane
                    | Action::NewPane { .. }
                    | Action::NewStackedPane { .. }
                    | Action::CloseFocus
                    | Action::ToggleFocusFullscreen
            ),
            InputMode::Tab => matches!(
                action,
                Action::NewTab { .. }
                    | Action::GoToPreviousTab
                    | Action::GoToNextTab
                    | Action::ToggleTab
                    | Action::CloseTab
                    | Action::BreakPane
            ),
            InputMode::Resize => matches!(action, Action::Resize { .. }),
            InputMode::Move => {
                matches!(action, Action::MovePane { .. } | Action::MovePaneBackwards)
            }
            InputMode::Scroll | InputMode::Search => matches!(
                action,
                Action::ScrollUp
                    | Action::ScrollDown
                    | Action::PageScrollUp
                    | Action::PageScrollDown
                    | Action::HalfPageScrollUp
                    | Action::HalfPageScrollDown
                    | Action::ScrollToTop
                    | Action::ScrollToBottom
                    | Action::ScrollToPreviousPrompt
                    | Action::ScrollToNextPrompt
                    | Action::EditScrollback { .. }
                    | Action::Copy
                    | Action::CopyLastCommandOutput
                    | Action::SelectCommandAtScrollPosition
                    | Action::Search { .. }
                    | Action::SearchToggleOption { .. }
            ),
            InputMode::RenamePane | InputMode::RenameTab => {
                matches!(action, Action::UndoRenamePane | Action::UndoRenameTab)
            }
            InputMode::EnterSearch => false,
            InputMode::Prompt => matches!(action, Action::Confirm | Action::Deny),
            InputMode::Session => matches!(
                action,
                Action::Detach
                    | Action::FocusHostSession
                    | Action::FocusGuestSession
                    | Action::ToggleHostFullscreen
            ),
            InputMode::Tmux => true,
        };
        if !relevant {
            return None;
        }
        if !description.is_empty() {
            description.push_str(" + ");
        }
        description.push_str(text);
        priority = priority.min(rank);
    }
    if let Some(destination) = destination {
        if description.is_empty() {
            let relevant = destination != mode
                && (matches!(destination, InputMode::Normal | InputMode::Locked)
                    || (mode == InputMode::Normal
                        && application
                        && matches!(
                            destination,
                            InputMode::Pane | InputMode::Tab | InputMode::Resize
                        ))
                    || matches!(
                        (mode, destination),
                        (InputMode::Pane, InputMode::RenamePane)
                            | (InputMode::Tab, InputMode::RenameTab)
                            | (InputMode::Scroll, InputMode::EnterSearch)
                            | (
                                InputMode::EnterSearch,
                                InputMode::Scroll | InputMode::Search
                            )
                            | (InputMode::RenamePane, InputMode::Pane)
                            | (InputMode::RenameTab, InputMode::Tab)
                    ));
            if !relevant {
                return None;
            }
            description.push_str(match (mode, destination) {
                (InputMode::Locked, InputMode::Normal) => "unlock",
                (_, InputMode::Locked) => "lock",
                (_, InputMode::Normal) => "Unlocked",
                _ => mode_name(destination),
            });
            priority = match destination {
                InputMode::Normal | InputMode::Locked => 1,
                InputMode::Pane => 12,
                InputMode::Tab => 13,
                InputMode::Resize => 14,
                _ => 12,
            };
        } else if destination != mode
            && !(description == "return to app" && destination == InputMode::Locked)
        {
            description.push_str(" → ");
            description.push_str(mode_name(destination));
        }
        if let Some(global_destination) = global_destination {
            let _ = write!(description, " (all: {})", mode_name(global_destination));
        }
    }
    (!description.is_empty()).then_some((description, priority))
}

fn local_hint(
    hints: &mut Vec<Hint>,
    bindings: &Bindings,
    keys: &[BareKey],
    ctrl: bool,
    description: &str,
    priority: u8,
) {
    for bare_key in keys {
        let mut key = KeyWithModifier::new(*bare_key);
        if ctrl {
            key = key.with_ctrl_modifier();
        }
        // Even NoOp is an interception. Unbound keys in Locked/Normal reach the plugin editor.
        if bindings.iter().any(|(bound, _)| *bound == key) {
            continue;
        }
        add_hint(hints, key_name(&key), description.to_owned(), priority);
    }
}

fn hints(mode: Mode<'_>, context: Context) -> Vec<Hint> {
    let bindings = bindings(mode);
    let mut hints = Vec::new();
    for (key, actions) in bindings {
        if let Some((description, priority)) = native_hint(actions, mode.input, context) {
            add_hint(&mut hints, key_name(key), description, priority);
        }
    }
    if is_sidebar_mode(mode.input) {
        let mut local = |keys: &[BareKey], ctrl, text, priority| {
            local_hint(&mut hints, bindings, keys, ctrl, text, priority);
        };
        use BareKey::*;
        match context {
            Context::Application => {}
            Context::Sidebar { selected } => {
                local(&[Char('/')], false, "find panes", 5);
                local(&[Up, Down, Char('j'), Char('k')], false, "select", 10);
                if selected {
                    local(&[Enter], false, "activate", 11);
                    local(&[Left, Right, Char(' ')], false, "fold workspace", 12);
                }
                local(&[Char('n')], false, "new workspace", 20);
                if selected {
                    local(&[Char('N')], false, "new terminal", 21);
                    local(&[Char('r')], false, "rename", 25);
                    local(&[Char('x'), Delete], false, "close", 26);
                }
                local(&[Char('R')], false, "refresh", 30);
                local(
                    &[Home, End, Char('g'), Char('G')],
                    false,
                    "first / last",
                    35,
                );
                local(&[PageUp, PageDown], false, "page", 36);
            }
            Context::Edit { multiple_fields } | Context::Create { multiple_fields } => {
                local(&[Esc], false, "cancel", 0);
                local(
                    &[Enter],
                    false,
                    if matches!(context, Context::Create { .. }) {
                        "create"
                    } else {
                        "save"
                    },
                    1,
                );
                if multiple_fields {
                    local(&[Tab], false, "next field", 10);
                }
                local(&[Left, Right], false, "move cursor", 20);
                local(&[Home, End], false, "start / end", 21);
                local(&[Backspace, Delete], false, "erase", 25);
                local(&[Char('u')], true, "clear field", 26);
            }
            Context::Confirm => {
                local(&[Esc, Char('n')], false, "cancel", 0);
                local(&[Enter, Char('y')], false, "confirm", 1);
            }
            Context::Finder => {
                local(&[Esc], false, "close finder", 0);
                local(&[Enter], false, "open", 1);
                local(&[Up, Down], false, "select", 2);
                local(
                    &[Char('n'), Char('p'), Char('j'), Char('k')],
                    true,
                    "select",
                    2,
                );
                local(&[PageUp, PageDown], false, "preview", 3);
                local(&[Char('u')], true, "clear search", 10);
            }
        }
    }
    hints.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.description.cmp(&right.description))
    });
    hints
}

fn title(mode: Option<Mode<'_>>, context: Context) -> String {
    let Some(mode) = mode else {
        return "… Connecting".to_owned();
    };
    let icon = if mode.input == InputMode::Locked {
        "\u{f023}"
    } else {
        "\u{f11c}"
    };
    let mut title = format!("{icon} {}", mode_name(mode.input));
    if is_sidebar_mode(mode.input) {
        title.push_str(match context {
            Context::Application => "",
            Context::Sidebar { .. } => " · Sidebar",
            Context::Edit { .. } => " · Sidebar edit",
            Context::Create {
                multiple_fields: true,
            } => " · New workspace",
            Context::Create {
                multiple_fields: false,
            } => " · New terminal",
            Context::Confirm => " · Sidebar confirm",
            Context::Finder => " · Finder",
        });
    }
    title
}

fn ordered_keys(hint: &Hint) -> Vec<&str> {
    let mut keys: Vec<&str> = hint.keys.iter().map(String::as_str).collect();
    keys.sort_by_key(|key| {
        let arrow = key.chars().last().is_some_and(|c| "←↓↑→".contains(c));
        let uppercase_letter = key.len() == 1 && key.as_bytes()[0].is_ascii_uppercase();
        (!arrow, ui::width(key), uppercase_letter, *key)
    });
    keys
}

fn short_modifier(modifier: &str) -> Option<&'static str> {
    match modifier {
        "Ctrl" => Some("C-"),
        "Alt" => Some("A-"),
        "Shift" => Some("S-"),
        "Super" => Some("Super-"),
        _ => None,
    }
}

fn compact_key_width(key: &str) -> usize {
    let mut width = 0;
    let mut suffix = key;
    while let Some((modifier, rest)) = suffix.split_once('+') {
        let Some(abbreviation) = short_modifier(modifier) else {
            break;
        };
        width += abbreviation.len();
        suffix = rest;
    }
    width + ui::width(suffix)
}

fn compact_key(key: &str) -> String {
    let mut compact = String::with_capacity(key.len());
    let mut suffix = key;
    while let Some((modifier, rest)) = suffix.split_once('+') {
        let Some(abbreviation) = short_modifier(modifier) else {
            break;
        };
        compact.push_str(abbreviation);
        suffix = rest;
    }
    compact.push_str(suffix);
    compact
}

fn key_prefix(key: &str) -> Option<&str> {
    key.rfind('+').map(|end| &key[..=end])
}

fn alias_width(keys: &[&str]) -> usize {
    let mut previous_prefix = None;
    let mut width = 0;
    for (index, key) in keys.iter().enumerate() {
        let prefix = key_prefix(key);
        let label = prefix
            .filter(|prefix| Some(*prefix) == previous_prefix)
            .map_or(*key, |prefix| &key[prefix.len()..]);
        width += usize::from(index > 0) + ui::width(label);
        previous_prefix = prefix;
    }
    width
}

fn alias_rows(keys: &[&str], budget: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut line = String::new();
    let mut width = 0;
    let mut previous_prefix = None;
    for key in keys {
        let full = if ui::width(key) > budget {
            std::borrow::Cow::Owned(compact_key(key))
        } else {
            std::borrow::Cow::Borrowed(*key)
        };
        if ui::width(&full) > budget {
            return Vec::new();
        }
        let prefix = matches!(full, std::borrow::Cow::Borrowed(_))
            .then(|| key_prefix(key))
            .flatten();
        let mut label = prefix
            .filter(|prefix| Some(*prefix) == previous_prefix)
            .map_or(full.as_ref(), |prefix| &key[prefix.len()..]);
        if !line.is_empty() && width + 1 + ui::width(label) > budget {
            rows.push(std::mem::take(&mut line));
            width = 0;
            label = &full;
        }
        if !line.is_empty() {
            line.push('/');
            width += 1;
        }
        line.push_str(label);
        width += ui::width(label);
        previous_prefix = prefix;
    }
    if !line.is_empty() {
        rows.push(line);
    }
    rows
}

fn alias_keys(hint: &Hint, budget: usize) -> String {
    let keys = ordered_keys(hint);
    if let Some(key) = keys.first()
        && ui::width(key) > budget
    {
        let compact = compact_key(key);
        return if ui::width(&compact) <= budget {
            compact
        } else {
            String::new()
        };
    }
    let mut joined = String::new();
    let prefix = keys
        .first()
        .and_then(|key| key.rfind('+').map(|end| &key[..=end]));
    let common_prefix = prefix.filter(|prefix| keys.iter().all(|key| key.starts_with(prefix)));
    for (index, key) in keys.iter().enumerate() {
        let suffix = if index > 0 {
            common_prefix
                .and_then(|prefix| key.strip_prefix(prefix))
                .unwrap_or(key)
        } else {
            key
        };
        let added = ui::width(suffix) + usize::from(index > 0);
        if index > 0 && ui::width(&joined) + added > budget {
            break;
        }
        if index > 0 {
            joined.push('/');
        }
        joined.push_str(suffix);
    }
    joined
}

#[derive(Default)]
struct Line {
    ansi: String,
    cells: usize,
}

impl Line {
    fn push(&mut self, text: &str, color: &str) {
        self.ansi.push_str(color);
        self.ansi.push_str(text);
        self.cells += ui::width(text);
    }

    fn push_group(&mut self, text: &str, color: &str, theme: &ui::Theme) {
        for (index, key) in text.split('/').enumerate() {
            if index > 0 {
                self.push("/", &theme.muted);
            }
            self.push(key, color);
        }
    }

    fn pad_to(&mut self, cells: usize) {
        self.ansi
            .extend(std::iter::repeat_n(' ', cells.saturating_sub(self.cells)));
        self.cells = self.cells.max(cells);
    }

    fn finish(mut self, columns: usize, theme: &ui::Theme) -> String {
        self.ansi.push_str(&theme.base);
        self.ansi
            .extend(std::iter::repeat_n(' ', columns.saturating_sub(self.cells)));
        self.ansi
    }
}

struct Body<'a> {
    entries: Vec<(Vec<&'a str>, &'a str)>,
    single_key_width: usize,
    two_row_key_width: usize,
    full_key_width: usize,
    description_width: usize,
}

impl<'a> Body<'a> {
    fn new(hints: &'a [Hint]) -> Self {
        let mut body = Self {
            entries: Vec::with_capacity(hints.len()),
            single_key_width: 0,
            two_row_key_width: 0,
            full_key_width: 0,
            description_width: 0,
        };
        for hint in hints {
            let keys = ordered_keys(hint);
            if keys.is_empty() {
                continue;
            }
            body.single_key_width = body.single_key_width.max(
                keys.iter()
                    .map(|key| compact_key_width(key))
                    .max()
                    .unwrap_or(0),
            );
            let aliases = alias_width(&keys);
            body.two_row_key_width = body.two_row_key_width.max(aliases.div_ceil(2));
            body.full_key_width = body.full_key_width.max(aliases);
            body.description_width = body.description_width.max(ui::width(&hint.description));
            body.entries.push((keys, hint.description.as_str()));
        }
        body
    }

    fn unwrapped_columns(&self) -> usize {
        self.single_key_width.max(self.two_row_key_width) + 2 + self.description_width
    }

    fn full_columns(&self) -> usize {
        self.full_key_width + 2 + self.description_width
    }

    fn render(&self, columns: usize, max_rows: usize, theme: &ui::Theme) -> Vec<String> {
        if columns < 4 || max_rows == 0 {
            return Vec::new();
        }
        let full_fit = self.full_columns() <= columns;
        let minimum_key_width = if full_fit {
            self.full_key_width
        } else {
            self.single_key_width.max(self.two_row_key_width)
        };
        let centered_key_width = (columns - 2) / 2;
        let key_width = if self.unwrapped_columns() <= columns {
            centered_key_width.clamp(minimum_key_width, columns - 2 - self.description_width)
        } else {
            centered_key_width.max(self.single_key_width)
        }
        .clamp(1, columns - 3);
        let description_start = key_width + 2;
        let key_line = |keys: Option<String>| {
            let mut line = Line::default();
            line.ansi.push_str(&theme.base);
            if let Some(keys) = keys {
                line.pad_to(key_width - ui::width(&keys));
                line.push_group(&keys, &theme.accents[4], theme);
            }
            line.pad_to(description_start);
            line
        };
        let mut rows = Vec::new();
        'hints: for (keys, description) in &self.entries {
            if rows.len() == max_rows {
                break;
            }
            let key_rows = alias_rows(keys, key_width);
            if key_rows.is_empty() {
                continue;
            }
            let hint_start = rows.len();
            let mut key_rows = key_rows.into_iter();
            let mut line = key_line(key_rows.next());
            for word in description.split_whitespace() {
                let space = usize::from(line.cells > description_start);
                if line.cells + space + ui::width(word) > columns && space > 0 {
                    rows.push(line.finish(columns, theme));
                    if rows.len() == max_rows {
                        rows.truncate(hint_start);
                        continue 'hints;
                    }
                    line = key_line(key_rows.next());
                } else if space > 0 {
                    line.push(" ", &theme.muted);
                }
                let word = ui::truncate(word, columns.saturating_sub(line.cells));
                line.push(&word, &theme.muted);
            }
            rows.push(line.finish(columns, theme));
            for keys in key_rows {
                if rows.len() == max_rows {
                    rows.truncate(hint_start);
                    continue 'hints;
                }
                rows.push(key_line(Some(keys)).finish(columns, theme));
            }
        }
        rows
    }
}

#[cfg(test)]
fn body(hints: &[Hint], columns: usize, max_rows: usize) -> Vec<String> {
    Body::new(hints).render(columns, max_rows, &ui::Theme::default())
}

/// A footer only: callers reserve its returned height and anchor it within the sidebar viewport.
/// Styling is applied after plain-text fitting, so truncation never splits terminal escapes.
pub fn render(
    mode: Option<Mode<'_>>,
    context: Context,
    columns: usize,
    max_rows: usize,
    theme: &ui::Theme,
) -> Vec<String> {
    if columns == 0 || max_rows == 0 {
        return Vec::new();
    }
    let hints = available_hints(mode, context);
    let layout = Body::new(&hints);
    render_frame(
        &title(mode, context),
        mode,
        &hints,
        &layout,
        columns,
        max_rows,
        theme,
    )
}

pub(crate) struct ExpandedGuide {
    pub columns: usize,
    pub lines: Vec<String>,
}

/// A bordered guide sized within the available display space, including its border cells.
/// Prefer 48–72 columns and the sidebar width; smaller displays take precedence.
pub(crate) fn render_expanded(
    mode: Mode<'_>,
    context: Context,
    sidebar_columns: usize,
    max_columns: usize,
    max_rows: usize,
    theme: &ui::Theme,
) -> ExpandedGuide {
    if max_columns == 0 || max_rows == 0 {
        return ExpandedGuide {
            columns: 0,
            lines: Vec::new(),
        };
    }
    let mode = Some(mode);
    let hints = available_hints(mode, context);
    let layout = Body::new(&hints);
    let title = title(mode, context);
    let needed = (layout.full_columns() + 4).max(ui::width(&title) + 5);
    let columns = if needed <= sidebar_columns || max_columns <= sidebar_columns {
        sidebar_columns.min(max_columns)
    } else {
        needed.clamp(48, 72).min(max_columns)
    };
    let lines = render_frame(&title, mode, &hints, &layout, columns, max_rows, theme);
    ExpandedGuide { columns, lines }
}

fn available_hints(mode: Option<Mode<'_>>, context: Context) -> Vec<Hint> {
    mode.filter(|mode| mode.keybinds.is_some())
        .map(|mode| hints(mode, context))
        .unwrap_or_default()
}

fn render_frame(
    title: &str,
    mode: Option<Mode<'_>>,
    hints: &[Hint],
    layout: &Body<'_>,
    columns: usize,
    max_rows: usize,
    theme: &ui::Theme,
) -> Vec<String> {
    let waiting = mode.is_none_or(|mode| mode.keybinds.is_none());
    let unlocked = mode.is_some_and(|mode| mode.input != InputMode::Locked);
    let border = if unlocked {
        theme.accents[11].as_str()
    } else {
        theme.muted.as_str()
    };
    let border_weight = if unlocked { "\x1b[1m" } else { "" };
    if columns == 0 || max_rows == 0 {
        return Vec::new();
    }
    if columns < 8 || max_rows < 3 {
        let mut compact_title = title.to_owned();
        if max_rows == 1
            && let Some(hint) = hints.first()
            && ui::width(&compact_title) + 4 < columns
        {
            compact_title.push_str(" · ");
            compact_title.push_str(&alias_keys(hint, columns - ui::width(&compact_title)));
            compact_title.push(' ');
            compact_title.push_str(&hint.description);
        }
        let mut title_line = Line::default();
        title_line.ansi.push_str(&theme.base);
        title_line.push_group(&ui::fit(&compact_title, columns), &theme.accents[5], theme);
        let mut rows = vec![title_line.finish(columns, theme)];
        if max_rows > 1 {
            if hints.is_empty() {
                let message = if waiting {
                    "Waiting for keys"
                } else {
                    "No shortcuts"
                };
                rows.push(format!(
                    "{}{MUTED}{}{base}",
                    theme.base,
                    ui::fit(message, columns),
                    MUTED = theme.muted,
                    base = theme.base
                ));
            } else {
                rows.extend(layout.render(columns, max_rows - 1, theme));
            }
        }
        return rows;
    }
    let inner = columns - 4;
    let mut content = layout.render(inner, max_rows - 2, theme);
    if content.is_empty() {
        let message = if waiting {
            "Waiting for native keymap"
        } else if !hints.is_empty() {
            "Widen for shortcuts"
        } else {
            "No configured shortcuts"
        };
        content.push(format!("{}{}", theme.muted, ui::fit(message, inner)));
    }
    let heading = ui::truncate(title, columns - 5);
    let trailing = columns - 5 - ui::width(&heading);
    let mut rows = Vec::with_capacity(content.len() + 2);
    rows.push(format!(
        "{}{border}{border_weight}╭─ {title_color}{heading}{border}{border_weight} {}╮{}",
        theme.base,
        "─".repeat(trailing),
        theme.base,
        title_color = theme.accents[5],
    ));
    for line in content {
        rows.push(format!(
            "{}{border}{border_weight}│ {}{} {border}{border_weight}│{}",
            theme.base, line, theme.base, theme.base,
        ));
    }
    rows.push(format!(
        "{}{border}{border_weight}╰{}╯{}",
        theme.base,
        "─".repeat(columns - 2),
        theme.base,
    ));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_tile::prelude::ModeInfo;

    fn view(info: &ModeInfo) -> Mode<'_> {
        Mode {
            input: info.mode,
            keybinds: Some(&info.keybinds),
        }
    }

    fn mode(mode: InputMode, keys: &[(&str, Vec<Action>)]) -> ModeInfo {
        ModeInfo {
            mode,
            keybinds: vec![(
                mode,
                keys.iter()
                    .map(|(key, actions)| (key.parse().unwrap(), actions.clone()))
                    .collect(),
            )],
            ..Default::default()
        }
    }

    fn switch(input_mode: InputMode) -> Action {
        Action::SwitchToMode { input_mode }
    }

    fn toggle() -> Action {
        Action::KeybindPipe {
            name: Some(crate::CONTROL_PIPE.to_owned()),
            payload: Some("toggle".to_owned()),
            args: None,
            plugin: None,
            plugin_id: None,
            configuration: None,
            launch_new: false,
            skip_cache: false,
            floating: None,
            in_place: None,
            cwd: None,
            pane_title: None,
        }
    }

    fn plain(line: &str) -> String {
        let mut output = String::new();
        let mut escape = false;
        for character in line.chars() {
            if character == '\x1b' {
                assert!(!escape, "nested or fragmented escape");
                escape = true;
            } else if escape {
                if character == 'm' {
                    escape = false;
                } else {
                    assert!(character == '[' || character == ';' || character.is_ascii_digit());
                }
            } else {
                output.push(character);
            }
        }
        assert!(!escape, "unterminated escape");
        output
    }

    #[test]
    fn guide_borders_warn_in_every_known_unlocked_mode_without_changing_content() {
        let theme = ui::Theme::default();
        let locked = mode(InputMode::Locked, &[]);
        let subdued = render(Some(view(&locked)), Context::Application, 48, 8, &theme);
        let unknown = render(None, Context::Application, 48, 8, &theme);
        assert_eq!(unknown.last(), subdued.last());
        let mut warning_border: Option<String> = None;
        for input in [
            InputMode::Normal,
            InputMode::Pane,
            InputMode::Tab,
            InputMode::Resize,
            InputMode::Move,
            InputMode::Scroll,
            InputMode::Search,
            InputMode::EnterSearch,
            InputMode::RenamePane,
            InputMode::RenameTab,
            InputMode::Session,
            InputMode::Prompt,
            InputMode::Tmux,
        ] {
            let info = mode(input, &[]);
            for context in [Context::Application, Context::Sidebar { selected: true }] {
                let inline = render(Some(view(&info)), context, 48, 8, &theme);
                let expanded = render_expanded(view(&info), context, 48, 72, 8, &theme);
                for rows in [&inline, &expanded.lines] {
                    let bottom = rows.last().unwrap();
                    assert_ne!(Some(bottom), subdued.last(), "{input:?} {context:?}");
                    assert_eq!(plain(bottom), plain(subdued.last().unwrap()));
                    let border = bottom.split('╰').next().unwrap();
                    assert_eq!(
                        rows[0].split('╭').next().unwrap(),
                        border,
                        "top and bottom borders must share the attention style",
                    );
                    for row in &rows[1..rows.len() - 1] {
                        assert_eq!(row.split('│').next().unwrap(), border);
                        let before_right = row.rsplit_once('│').unwrap().0;
                        assert!(before_right.ends_with(&border[theme.base.len()..]));
                    }
                    if let Some(expected) = &warning_border {
                        assert_eq!(border, expected.as_str());
                    } else {
                        warning_border = Some(border.to_owned());
                    }
                }
            }
            // Empty application keymaps keep identical body text and geometry across modes.
            let inline = render(Some(view(&info)), Context::Application, 48, 8, &theme);
            assert_eq!(plain(&inline[1]), plain(&subdued[1]));
        }
    }

    #[test]
    fn effective_mode_and_final_transition_determine_hints() {
        let mut info = mode(
            InputMode::Locked,
            &[(
                "Alt u",
                vec![switch(InputMode::Pane), switch(InputMode::Normal)],
            )],
        );
        info.keybinds.push((
            InputMode::Normal,
            vec![("Ctrl g".parse().unwrap(), vec![switch(InputMode::Locked)])],
        ));
        let found = hints(view(&info), Context::Application);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].keys, ["Alt+u"]);
        assert_eq!(
            native_hint(
                &[switch(InputMode::Pane), switch(InputMode::Normal)],
                InputMode::Locked,
                Context::Application
            ),
            native_hint(
                &[switch(InputMode::Normal)],
                InputMode::Locked,
                Context::Application
            )
        );
        assert!(
            native_hint(
                &[Action::Quit, switch(InputMode::Normal)],
                InputMode::Locked,
                Context::Application
            )
            .is_none()
        );
    }

    #[test]
    fn compound_operations_merge_only_when_the_effective_destination_matches() {
        let create = || Action::NewPane {
            direction: Some(Direction::Right),
            pane_name: None,
            start_suppressed: false,
        };
        let info = mode(
            InputMode::Pane,
            &[
                ("a", vec![create(), switch(InputMode::Normal)]),
                (
                    "b",
                    vec![
                        create(),
                        switch(InputMode::Resize),
                        switch(InputMode::Normal),
                    ],
                ),
                ("c", vec![create(), switch(InputMode::Locked)]),
            ],
        );
        let found = hints(view(&info), Context::Application);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|hint| hint.keys == ["a", "b"]));
        assert!(found.iter().any(|hint| hint.keys == ["c"]));
    }

    #[test]
    fn disabled_bindings_and_local_interception_are_not_advertised() {
        let info = mode(
            InputMode::Locked,
            &[
                ("Ctrl Space", vec![Action::NoOp]),
                ("n", vec![Action::NoOp]),
                ("Enter", vec![Action::NoOp]),
            ],
        );
        assert!(hints(view(&info), Context::Application).is_empty());
        let found = hints(view(&info), Context::Sidebar { selected: true });
        assert!(
            !found
                .iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "n" || key == "Enter" || key == "Ctrl+Space")
        );
        let confirmation = hints(view(&info), Context::Confirm);
        assert!(
            confirmation
                .iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "y")
        );
        assert!(
            !confirmation
                .iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "Enter")
        );
    }

    #[test]
    fn sidebar_toggle_requires_an_active_supporting_binding() {
        let mut info = mode(InputMode::Locked, &[("Alt s", vec![toggle()])]);
        let found = hints(view(&info), Context::Application);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].keys, ["Alt+s"]);
        assert_ne!(
            found[0].description,
            hints(view(&info), Context::Sidebar { selected: false })[0].description
        );
        info.mode = InputMode::Resize;
        assert!(hints(view(&info), Context::Application).is_empty());
        // Compound bindings are not mistaken for a plain mode transition or a no-op.
        let compound = native_hint(
            &[toggle(), switch(InputMode::Locked)],
            InputMode::Normal,
            Context::Sidebar { selected: true },
        )
        .unwrap();
        assert!(compound.1 < 10);
        assert!(
            native_hint(
                &[toggle(), Action::Quit],
                InputMode::Normal,
                Context::Application
            )
            .is_none()
        );
    }

    #[test]
    fn native_submode_precedes_local_context_and_merges_aliases() {
        let info = mode(
            InputMode::Resize,
            &[
                (
                    "h",
                    vec![Action::Resize {
                        resize: Resize::Increase,
                        direction: Some(Direction::Left),
                    }],
                ),
                (
                    "Left",
                    vec![Action::Resize {
                        resize: Resize::Increase,
                        direction: Some(Direction::Left),
                    }],
                ),
                ("Esc", vec![switch(InputMode::Normal)]),
            ],
        );
        let app = hints(view(&info), Context::Application);
        let edit = hints(
            view(&info),
            Context::Edit {
                multiple_fields: true,
            },
        );
        assert_eq!(app.len(), 2);
        assert_eq!(
            app.iter()
                .map(|hint| (&hint.keys, &hint.description))
                .collect::<Vec<_>>(),
            edit.iter()
                .map(|hint| (&hint.keys, &hint.description))
                .collect::<Vec<_>>()
        );
        assert_eq!(app[1].keys.len(), 2);
        assert!(app[0].priority < app[1].priority);
    }

    #[test]
    fn selection_and_dialog_fields_change_only_available_actions() {
        let info = mode(InputMode::Locked, &[]);
        let empty = hints(view(&info), Context::Sidebar { selected: false });
        let selected = hints(view(&info), Context::Sidebar { selected: true });
        assert!(
            !empty
                .iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "Enter" || key == "N" || key == "r")
        );
        assert!(
            selected
                .iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "Enter")
        );
        let one = hints(
            view(&info),
            Context::Edit {
                multiple_fields: false,
            },
        );
        let many = hints(
            view(&info),
            Context::Edit {
                multiple_fields: true,
            },
        );
        assert!(
            !one.iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "Tab")
        );
        assert!(
            many.iter()
                .flat_map(|hint| &hint.keys)
                .any(|key| key == "Tab")
        );
    }

    #[test]
    fn constrained_height_keeps_exits_before_creation() {
        let info = mode(
            InputMode::Pane,
            &[
                (
                    "F9",
                    vec![Action::NewPane {
                        direction: None,
                        pane_name: None,
                        start_suppressed: false,
                    }],
                ),
                ("F12", vec![switch(InputMode::Normal)]),
            ],
        );
        let rendered = render(
            Some(view(&info)),
            Context::Application,
            20,
            3,
            &ui::Theme::default(),
        );
        assert_eq!(rendered.len(), 3);
        let content = plain(&rendered[1]);
        assert!(content.contains("F12"));
        assert!(!content.contains("F9"));
    }

    #[test]
    fn narrow_help_keeps_whole_keys_and_omits_incomplete_operations() {
        let hint = Hint {
            keys: vec!["Ctrl+Space".to_owned()],
            description: "sidebar".to_owned(),
            priority: 0,
        };
        let compact = alias_keys(&hint, 7);
        assert!(compact.ends_with("Space"));
        assert!(!compact.contains('…'));
        assert!(ui::width(&compact) <= 7);
        assert!(alias_keys(&hint, 2).is_empty());

        let rows = body(
            &[
                Hint {
                    keys: vec!["F1".to_owned()],
                    description: "back".to_owned(),
                    priority: 0,
                },
                Hint {
                    keys: vec!["F2".to_owned()],
                    description: "another wordy operation".to_owned(),
                    priority: 1,
                },
            ],
            12,
            2,
        );
        assert!(rows.iter().any(|row| plain(row).contains("F1")));
        assert!(!rows.iter().any(|row| plain(row).contains("F2")));
    }

    #[test]
    fn shortcut_grid_aligns_key_ends_and_wrapped_descriptions() {
        let hints = [
            Hint {
                keys: vec!["Ctrl+Space".to_owned()],
                description: "open sidebar".to_owned(),
                priority: 0,
            },
            Hint {
                keys: vec!["Esc".to_owned()],
                description: "close".to_owned(),
                priority: 1,
            },
            Hint {
                keys: vec!["↑".to_owned(), "↓".to_owned()],
                description: "move focus between panes".to_owned(),
                priority: 2,
            },
        ];
        let rows: Vec<_> = body(&hints, 28, 8).iter().map(|row| plain(row)).collect();
        let description_start = rows[0].find("open sidebar").unwrap();
        for (keys, description) in [
            ("Ctrl+Space", "open sidebar"),
            ("Esc", "close"),
            ("↑/↓", "move focus"),
        ] {
            let row = rows.iter().find(|row| row.contains(description)).unwrap();
            let key_start = row.find(keys).unwrap();
            assert_eq!(
                ui::width(&row[..key_start]) + ui::width(keys),
                description_start - 2
            );
            assert_eq!(
                ui::width(&row[..row.find(description).unwrap()]),
                description_start
            );
        }
        let continuation = rows
            .iter()
            .find(|row| row.contains("between panes"))
            .unwrap();
        assert!(continuation[..description_start].chars().all(|c| c == ' '));
        assert_eq!(continuation.find("between panes"), Some(description_start));
        assert!(rows.iter().all(|row| ui::width(row) == 28));
    }

    #[test]
    fn focus_aliases_wrap_completely_around_a_centered_gutter() {
        let hints = [Hint {
            keys: ["←", "↑", "→", "↓", "Alt+↑", "Alt+↓", "h", "j", "k", "l"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            description: "focus panes".to_owned(),
            priority: 0,
        }];
        let columns = 33;
        let rows: Vec<_> = body(&hints, columns, 8)
            .iter()
            .map(|row| plain(row))
            .collect();
        assert_eq!(rows.len(), 2);
        let key_width = (columns - 2) / 2;
        let key_rows: Vec<String> = rows
            .iter()
            .map(|row| row.chars().take(key_width).collect())
            .collect();
        let aliases: Vec<_> = key_rows
            .iter()
            .flat_map(|row| row.trim().split('/'))
            .collect();
        for key in ["←", "↑", "→", "↓", "h", "j", "k", "l"] {
            assert!(aliases.contains(&key), "missing {key}: {key_rows:?}");
        }
        assert!(key_rows[0].contains("Alt+↑/↓"));
        let description_start = ui::width(&rows[0][..rows[0].find("focus panes").unwrap()]);
        assert!((key_width + description_start).abs_diff(columns) <= 1);
        assert!(rows.iter().all(|row| ui::width(row) == columns));
    }

    #[test]
    fn expanded_width_fits_long_operations_without_description_wrapping() {
        let info = mode(
            InputMode::Tmux,
            &[(
                "F1",
                vec![
                    Action::ToggleFocusFullscreen,
                    Action::ToggleFloatingPanes,
                    Action::TogglePaneEmbedOrFloating,
                ],
            )],
        );
        let expanded = render_expanded(
            view(&info),
            Context::Application,
            38,
            120,
            12,
            &ui::Theme::default(),
        );
        assert!(expanded.columns > 48 && expanded.columns <= 72);
        assert_eq!(expanded.lines.len(), 3);
        assert!(
            render(
                Some(view(&info)),
                Context::Application,
                38,
                12,
                &ui::Theme::default()
            )
            .len()
                > expanded.lines.len()
        );
        assert!(
            render_expanded(
                view(&info),
                Context::Application,
                64,
                120,
                12,
                &ui::Theme::default()
            )
            .columns
                >= 64
        );
        assert_eq!(
            render_expanded(
                view(&info),
                Context::Application,
                100,
                120,
                12,
                &ui::Theme::default()
            )
            .columns,
            100
        );
        assert_eq!(
            render_expanded(
                view(&info),
                Context::Application,
                120,
                120,
                12,
                &ui::Theme::default()
            )
            .columns,
            120
        );
    }

    #[test]
    fn expanded_layout_respects_display_edges_and_complete_ansi_rows() {
        let info = mode(
            InputMode::Normal,
            &[
                ("Ctrl Space", vec![toggle()]),
                ("Esc", vec![switch(InputMode::Locked)]),
                ("Ctrl p", vec![switch(InputMode::Pane)]),
            ],
        );
        for columns in [0, 1, 3, 7, 8, 38, 47, 48, 60, 71, 72, 96] {
            for rows in [0, 1, 2, 3, 4, 12] {
                for sidebar in [0, 38, 64, 100] {
                    let guide = render_expanded(
                        view(&info),
                        Context::Sidebar { selected: true },
                        sidebar,
                        columns,
                        rows,
                        &ui::Theme::default(),
                    );
                    assert!(guide.columns <= columns);
                    assert!(guide.lines.len() <= rows);
                    if columns == 0 || rows == 0 {
                        assert_eq!(guide.columns, 0);
                        assert!(guide.lines.is_empty());
                    } else {
                        assert!(guide.columns >= sidebar.min(columns));
                        assert!(!guide.lines.is_empty());
                    }
                    for line in guide.lines {
                        assert_eq!(ui::width(&plain(&line)), guide.columns);
                    }
                }
            }
        }
    }

    #[test]
    fn all_layouts_fit_without_fragmenting_ansi() {
        let info = mode(
            InputMode::Normal,
            &[
                ("Ctrl Space", vec![toggle()]),
                ("Esc", vec![switch(InputMode::Locked)]),
                ("Ctrl p", vec![switch(InputMode::Pane)]),
                ("Ctrl t", vec![switch(InputMode::Tab)]),
                ("Ctrl n", vec![switch(InputMode::Resize)]),
            ],
        );
        for columns in 0..65 {
            for rows in 0..16 {
                for context in [
                    Context::Application,
                    Context::Sidebar { selected: true },
                    Context::Edit {
                        multiple_fields: true,
                    },
                    Context::Create {
                        multiple_fields: true,
                    },
                    Context::Create {
                        multiple_fields: false,
                    },
                    Context::Confirm,
                    Context::Finder,
                ] {
                    for mode in [None, Some(view(&info))] {
                        let rendered = render(mode, context, columns, rows, &ui::Theme::default());
                        assert!(rendered.len() <= rows);
                        if columns == 0 || rows == 0 {
                            assert!(rendered.is_empty());
                        }
                        for line in rendered {
                            assert_eq!(
                                ui::width(&plain(&line)),
                                columns,
                                "columns={columns}, rows={rows}"
                            );
                        }
                    }
                }
            }
        }
    }
}
