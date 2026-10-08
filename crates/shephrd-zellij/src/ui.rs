use shephrd::{Snapshot, Status};
use unicode_width::UnicodeWidthStr;

pub const RESET: &str = "\u{1b}[0m";
pub const MUTED: &str = "\u{1b}[2;39m";
pub const BRIGHT: &str = "\u{1b}[22;39m";

pub struct Theme {
    pub selected: String,
    pub hovered: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selected: "\u{1b}[48;5;237m".to_owned(),
            hovered: "\u{1b}[48;5;235m".to_owned(),
        }
    }
}

impl Theme {
    pub fn update(&mut self, style: zellij_tile::prelude::Style) {
        use zellij_tile::prelude::PaletteColor;
        fn background(color: PaletteColor) -> String {
            match color {
                PaletteColor::Rgb((r, g, b)) => format!("\u{1b}[48;2;{r};{g};{b}m"),
                PaletteColor::EightBit(index) => format!("\u{1b}[48;5;{index}m"),
            }
        }
        fn highlight(
            base: PaletteColor,
            foreground: PaletteColor,
            preferred: PaletteColor,
            percent: u16,
        ) -> PaletteColor {
            if preferred != base {
                return preferred;
            }
            match (base, foreground) {
                (PaletteColor::Rgb((br, bg, bb)), PaletteColor::Rgb((fr, fg, fb))) => {
                    let mix = |b: u8, f: u8| {
                        ((u16::from(b) * (100 - percent) + u16::from(f) * percent) / 100) as u8
                    };
                    PaletteColor::Rgb((mix(br, fr), mix(bg, fg), mix(bb, fb)))
                }
                _ => PaletteColor::EightBit(if percent > 10 { 237 } else { 235 }),
            }
        }
        let normal = style.colors.text_unselected.background;
        let foreground = style.colors.text_unselected.base;
        self.selected = background(highlight(
            normal,
            foreground,
            style.colors.list_selected.background,
            20,
        ));
        self.hovered = background(highlight(
            normal,
            foreground,
            style.colors.list_unselected.background,
            10,
        ));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Workspace(String),
    Pane(String),
}

impl Item {
    pub fn workspace_id<'a>(&'a self, state: &'a Snapshot) -> Option<&'a str> {
        match self {
            Self::Workspace(id) => Some(id),
            Self::Pane(id) => state
                .panes
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.workspace_id.as_str()),
        }
    }
}

#[derive(Clone)]
pub struct TreeRow {
    pub item: Option<Item>,
    pub workspace_id: Option<String>,
    pub kind: RowKind,
}

#[derive(Clone)]
pub enum RowKind {
    Workspace { index: usize, collapsed: bool },
    Projects { index: usize, has_children: bool },
    Pane { index: usize, last: bool },
    Detail { index: usize, last: bool },
    Blank,
}

pub fn tree(state: &Snapshot, collapsed: &std::collections::BTreeSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for (index, workspace) in state.workspaces.iter().enumerate() {
        if index != 0 {
            rows.push(TreeRow {
                item: None,
                workspace_id: None,
                kind: RowKind::Blank,
            });
        }
        let folded = collapsed.contains(&workspace.id);
        let children: Vec<_> = state
            .panes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.workspace_id == workspace.id)
            .collect();
        let item = Some(Item::Workspace(workspace.id.clone()));
        let group = Some(workspace.id.clone());
        rows.push(TreeRow {
            item: item.clone(),
            workspace_id: group.clone(),
            kind: RowKind::Workspace {
                index,
                collapsed: folded,
            },
        });
        if folded {
            continue;
        }
        rows.push(TreeRow {
            item,
            workspace_id: group.clone(),
            kind: RowKind::Projects {
                index,
                has_children: !children.is_empty(),
            },
        });
        for (child_index, (pane_index, pane)) in children.iter().enumerate() {
            let item = Some(Item::Pane(pane.id.clone()));
            let last = child_index + 1 == children.len();
            rows.push(TreeRow {
                item: item.clone(),
                workspace_id: group.clone(),
                kind: RowKind::Pane {
                    index: *pane_index,
                    last,
                },
            });
            rows.push(TreeRow {
                item,
                workspace_id: group.clone(),
                kind: RowKind::Detail {
                    index: *pane_index,
                    last,
                },
            });
        }
    }
    rows
}

pub fn items(rows: &[TreeRow]) -> Vec<Item> {
    let mut items = Vec::new();
    for row in rows {
        if let Some(item) = &row.item
            && items.last() != Some(item)
        {
            items.push(item.clone());
        }
    }
    items
}

/// Metadata is untrusted terminal text. Never allow it to emit control sequences.
pub fn clean(text: &str) -> std::borrow::Cow<'_, str> {
    for (index, character) in text.chars().enumerate() {
        if index == 1024 || character.is_control() {
            return std::borrow::Cow::Owned(
                text.chars()
                    .filter(|c| !c.is_control())
                    .take(1024)
                    .collect(),
            );
        }
    }
    std::borrow::Cow::Borrowed(text)
}

pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate(text: &str, columns: usize) -> String {
    let text = clean(text);
    if width(&text) <= columns {
        return text.into_owned();
    }
    if columns == 0 {
        return String::new();
    }
    let budget = columns - 1;
    let mut end = 0;
    for (offset, character) in text.char_indices() {
        let next = offset + character.len_utf8();
        if width(&text[..next]) > budget {
            break;
        }
        end = next;
    }
    format!("{}…", &text[..end])
}

pub fn fit(text: &str, columns: usize) -> String {
    let mut text = truncate(text, columns);
    let padding = columns.saturating_sub(width(&text));
    text.extend(std::iter::repeat_n(' ', padding));
    text
}

pub fn status_color(status: Status) -> &'static str {
    match status {
        Status::Idle => "\u{1b}[38;5;245m",
        Status::Working => "\u{1b}[38;5;75m",
        Status::Blocked => "\u{1b}[38;5;214m",
        Status::Done => "\u{1b}[38;5;114m",
        Status::Unknown => "\u{1b}[38;5;245m",
    }
}

pub fn render_row(
    row: &TreeRow,
    state: &Snapshot,
    theme: &Theme,
    columns: usize,
    selected: bool,
    hovered: bool,
    selected_pane_group: bool,
) -> String {
    let background = if selected {
        theme.selected.as_str()
    } else if hovered {
        theme.hovered.as_str()
    } else {
        ""
    };
    let guides = if selected || selected_pane_group {
        BRIGHT
    } else {
        MUTED
    };
    let body = match &row.kind {
        RowKind::Workspace { index, collapsed } => {
            let workspace = &state.workspaces[*index];
            let dot = truncate(" ● ", columns);
            let available = columns.saturating_sub(width(&dot));
            let name = truncate(&workspace.name, available);
            let fill = columns.saturating_sub(width(&dot) + width(&name));
            let tail = if *collapsed {
                "·".repeat(fill)
            } else {
                " ".repeat(fill)
            };
            format!(
                "{}{}\u{1b}[22;39;1m{name}\u{1b}[22m{MUTED}{tail}",
                status_color(workspace.status),
                dot
            )
        }
        RowKind::Projects {
            index,
            has_children,
        } => {
            let projects = &state.workspaces[*index].projects;
            let lead = truncate(if *has_children { " │ " } else { "   " }, columns);
            format!(
                "{guides}{lead}{MUTED}{}",
                fit(&projects.join(", "), columns.saturating_sub(width(&lead)))
            )
        }
        RowKind::Pane { index, last } => {
            let pane = &state.panes[*index];
            let lead = truncate(if *last { " ╰─ " } else { " ├─ " }, columns);
            let available = columns.saturating_sub(width(&lead));
            let harness = pane.harness.as_deref().map(clean).unwrap_or_default();
            // Preserve a useful name before allocating space for the right-aligned harness.
            let harness = if available > 8 {
                truncate(&harness, available / 2)
            } else {
                String::new()
            };
            let separator = usize::from(!harness.is_empty());
            let name_columns = available.saturating_sub(width(&harness) + separator);
            let name = fit(&pane.name, name_columns);
            format!(
                "{guides}{lead}\u{1b}[22;39m{name}{}{MUTED}{harness}",
                " ".repeat(separator)
            )
        }
        RowKind::Detail { index, last } => {
            let pane = &state.panes[*index];
            let lead = truncate(if *last { "    " } else { " │  " }, columns);
            let description = if pane.description.trim().is_empty() {
                pane.status.label().to_owned()
            } else {
                format!("{} · {}", clean(&pane.description), pane.status.label())
            };
            format!(
                "{guides}{lead}{MUTED}{}",
                fit(&description, columns.saturating_sub(width(&lead)))
            )
        }
        RowKind::Blank => " ".repeat(columns),
    };
    format!("{RESET}{background}{body}{RESET}")
}

pub fn tooltip(item: &Item, state: &Snapshot) -> Vec<String> {
    match item {
        Item::Workspace(id) => state
            .workspaces
            .iter()
            .find(|w| &w.id == id)
            .map(|workspace| {
                let mut lines = vec![
                    workspace.name.clone(),
                    format!("Status: {}", workspace.status.label()),
                ];
                if !workspace.projects.is_empty() {
                    lines.push(format!("Projects: {}", workspace.projects.join(", ")));
                }
                lines.push(format!(
                    "{} native attachments",
                    state.panes.iter().filter(|p| &p.workspace_id == id).count()
                ));
                lines
            })
            .unwrap_or_default(),
        Item::Pane(id) => state
            .panes
            .iter()
            .find(|p| &p.id == id)
            .map(|pane| {
                let mut lines = vec![
                    pane.name.clone(),
                    format!("Status: {}", pane.status.label()),
                ];
                if let Some(harness) = &pane.harness {
                    lines.push(format!("Harness: {harness}"));
                }
                if !pane.description.is_empty() {
                    lines.push(pane.description.clone());
                }
                if let Some(cwd) = &pane.cwd {
                    lines.push(format!("Directory: {cwd}"));
                }
                if let Some(workspace) = state.workspaces.iter().find(|w| w.id == pane.workspace_id)
                {
                    lines.push(format!("Workspace: {}", workspace.name));
                }
                lines
            })
            .unwrap_or_default(),
    }
}

#[derive(Default)]
pub struct Input {
    pub value: String,
    pub cursor: usize,
}

impl Input {
    pub fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }

    pub fn insert(&mut self, text: &str) {
        let text = clean(text);
        // Keep requests and the interactive editor bounded independently of the backend.
        if self.value.len() + text.len() <= 4096 {
            self.value.insert_str(self.cursor, &text);
            self.cursor += text.len();
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.value[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    pub fn right(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.value.replace_range(self.cursor..end, "");
    }

    pub fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn visible(&self, columns: usize) -> (String, usize) {
        if columns == 0 {
            return (String::new(), 0);
        }
        let mut start = 0;
        while width(&self.value[start..self.cursor]) >= columns {
            start += self.value[start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
        }
        let cursor_column = width(&self.value[start..self.cursor]);
        (fit(&self.value[start..], columns), cursor_column)
    }
}

pub enum Modal {
    Create {
        name: Input,
        cwd: Input,
        cwd_active: bool,
    },
    CreatePane {
        workspace_id: String,
        cwd: Input,
    },
    Rename {
        item: Item,
        input: Input,
    },
    Close {
        item: Item,
        name: String,
    },
}

impl Modal {
    pub fn input_mut(&mut self) -> Option<&mut Input> {
        match self {
            Self::Create {
                name,
                cwd,
                cwd_active,
            } => Some(if *cwd_active { cwd } else { name }),
            Self::CreatePane { cwd, .. } => Some(cwd),
            Self::Rename { input, .. } => Some(input),
            Self::Close { .. } => None,
        }
    }

    pub fn lines(&self, columns: usize) -> (Vec<String>, Option<(usize, usize)>) {
        match self {
            Self::Create {
                name,
                cwd,
                cwd_active,
            } => {
                let (name_line, name_cursor) = name.visible(columns.saturating_sub(2));
                let (cwd_line, cwd_cursor) = cwd.visible(columns.saturating_sub(2));
                (
                    vec![
                        "Create workspace".to_owned(),
                        "Name (optional):".to_owned(),
                        format!("  {name_line}"),
                        "Directory (optional):".to_owned(),
                        format!("  {cwd_line}"),
                        String::new(),
                        "Tab field · Enter create".to_owned(),
                        "Esc cancel".to_owned(),
                    ],
                    Some(if *cwd_active {
                        (2 + cwd_cursor, 4)
                    } else {
                        (2 + name_cursor, 2)
                    }),
                )
            }
            Self::CreatePane { cwd, .. } => {
                let (line, cursor) = cwd.visible(columns.saturating_sub(2));
                (
                    vec![
                        "Create terminal".to_owned(),
                        "Directory (optional):".to_owned(),
                        format!("  {line}"),
                        String::new(),
                        "Enter create · Esc cancel".to_owned(),
                    ],
                    Some((2 + cursor, 2)),
                )
            }
            Self::Rename { input, .. } => {
                let (line, cursor) = input.visible(columns.saturating_sub(2));
                (
                    vec![
                        "Rename".to_owned(),
                        String::new(),
                        format!("  {line}"),
                        String::new(),
                        "Enter save · Esc cancel".to_owned(),
                    ],
                    Some((2 + cursor, 2)),
                )
            }
            Self::Close { name, .. } => (
                vec![
                    "Close permanently?".to_owned(),
                    name.clone(),
                    String::new(),
                    "This terminates Herdr PTYs.".to_owned(),
                    "Enter / y confirm".to_owned(),
                    "Esc / n cancel".to_owned(),
                ],
                None,
            ),
        }
    }
}
