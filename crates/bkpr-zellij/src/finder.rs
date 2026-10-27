use crate::{
    focused::{Input, KeyAction, paint},
    overlay, pane_search, ui,
};
use bkpr::Snapshot;
use zellij_tile::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Rect {
    fn contains(self, row: isize, column: usize) -> bool {
        row >= 0
            && (self.y..self.y + self.height).contains(&(row as usize))
            && (self.x..self.x + self.width).contains(&column)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Compact {
    #[default]
    Input,
    List,
    Preview,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Layout {
    input: Rect,
    list: Rect,
    preview: Rect,
    search: Option<Rect>,
    vertical: Option<Rect>,
    horizontal: Option<Rect>,
    border: bool,
}

impl Layout {
    fn new(columns: usize, rows: usize, compact: Compact) -> Self {
        if columns == 0 || rows == 0 {
            return Self::default();
        }
        if rows <= 2 {
            let mut result = Self::default();
            if rows == 2 || compact == Compact::Input {
                result.input = Rect {
                    x: 0,
                    y: 0,
                    width: columns,
                    height: 1,
                };
            }
            let body = Rect {
                x: 0,
                y: rows - 1,
                width: columns,
                height: 1,
            };
            match compact {
                Compact::Preview => result.preview = body,
                _ if rows == 2 || compact == Compact::List => result.list = body,
                _ => {}
            }
            return result;
        }
        let border = rows >= 7 && columns >= 4;
        let margin = usize::from(border);
        let width = columns.saturating_sub(margin * 2);
        let input = Rect {
            x: margin,
            y: margin,
            width,
            height: 1,
        };
        // Keep input, list and preview reachable in three rows; larger views can
        // spend one row on a search rule without stealing either body surface.
        let search = (rows >= 4).then_some(Rect {
            x: 0,
            y: margin + 1,
            width: columns,
            height: 1,
        });
        let top = margin + 1 + usize::from(search.is_some());
        let bottom = rows - margin;
        let body = bottom.saturating_sub(top);
        let mut result = Self {
            input,
            search,
            border,
            ..Self::default()
        };
        if width >= 84 && body >= 2 {
            let list_width = width * 2 / 5;
            result.list = Rect {
                x: margin,
                y: top,
                width: list_width,
                height: body,
            };
            result.vertical = Some(Rect {
                x: margin + list_width,
                y: top,
                width: 1,
                height: body,
            });
            result.preview = Rect {
                x: margin + list_width + 1,
                y: top,
                width: width - list_width - 1,
                height: body,
            };
        } else {
            let separator = usize::from(body >= 3);
            let list_height = ((body - separator) * 2 / 5).max(1).min(body);
            result.list = Rect {
                x: margin,
                y: top,
                width,
                height: list_height,
            };
            if separator != 0 {
                result.horizontal = Some(Rect {
                    x: 0,
                    y: top + list_height,
                    width: columns,
                    height: 1,
                });
            }
            result.preview = Rect {
                x: margin,
                y: top + list_height + separator,
                width,
                height: body.saturating_sub(list_height + separator),
            };
        }
        result
    }

    fn rule_line(self, separator: Rect) -> String {
        let junction = (self.search == Some(separator))
            .then_some(self.vertical)
            .flatten()
            .map(|vertical| vertical.x);
        let mut line = String::with_capacity(separator.width * 3);
        for column in separator.x..separator.x + separator.width {
            line.push(if self.border && column == separator.x {
                '├'
            } else if self.border && column + 1 == separator.x + separator.width {
                '┤'
            } else if junction == Some(column) {
                '┬'
            } else {
                '─'
            });
        }
        line
    }

    fn bottom_line(self, columns: usize) -> String {
        let mut line = String::with_capacity(columns * 3);
        for column in 0..columns {
            line.push(if column == 0 {
                '╰'
            } else if column + 1 == columns {
                '╯'
            } else if self.vertical.is_some_and(|vertical| vertical.x == column) {
                '┴'
            } else {
                '─'
            });
        }
        line
    }
}

#[derive(Default)]
pub struct Finder {
    index: pane_search::Index,
    input: ui::Input,
    selected: Option<String>,
    selection: usize,
    scroll: usize,
    preview: ui::PopupContent,
    preview_lines: Vec<overlay::PopupLine>,
    preview_columns: Option<usize>,
    preview_scroll: usize,
    pointer_preview: bool,
    compact: Compact,
    layout: Layout,
}
impl Finder {
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub(crate) fn prepare(&mut self, rows: usize, columns: usize) -> Option<(usize, usize)> {
        self.layout = Layout::new(columns, rows, self.compact);
        self.ensure_selection_visible();
        self.prepare_preview();
        let input = self.layout.input;
        let prefix = if input.width >= 3 { 2 } else { 0 };
        let width = input.width.saturating_sub(prefix);
        let (_, cursor) = self.input.visible(width);
        (input.height > 0 && width > 0).then_some((input.x + prefix + cursor, input.y))
    }

    pub(crate) fn update(
        &mut self,
        snapshot: &Snapshot,
        opening: bool,
        changed: bool,
        coordinates: &FloatingPaneCoordinates,
    ) {
        if opening {
            *self = Self::default();
            self.selected = snapshot.focused_pane_id.clone();
        }
        if changed || opening {
            self.index.rebuild(snapshot);
        }
        if let (Some(columns), Some(rows)) = (coordinates.width, coordinates.height) {
            self.layout = Layout::new(columns.to_fixed(0), rows.to_fixed(0), self.compact);
        }
        if changed || opening {
            self.search(snapshot);
        }
    }
    pub(crate) fn input_event(&mut self, snapshot: &Snapshot, input: Input) -> KeyAction {
        match input {
            Input::Key(key) => self.key(snapshot, key),
            Input::Paste(text) => {
                let before = self.input.value.len();
                self.input.insert(&text);
                self.compact = Compact::Input;
                if self.input.value.len() != before {
                    self.selection = 0;
                    self.selected = None;
                    self.scroll = 0;
                    self.search(snapshot);
                }
                KeyAction::None
            }
        }
    }
    fn search(&mut self, snapshot: &Snapshot) {
        self.index.search(&self.input.value);

        self.selection = self
            .selected
            .as_ref()
            .and_then(|id| {
                self.index
                    .results()
                    .iter()
                    .position(|result| snapshot.panes[result.pane].id == *id)
            })
            .unwrap_or_else(|| {
                self.selection
                    .min(self.index.results().len().saturating_sub(1))
            });
        self.sync_selection(snapshot);
    }

    fn sync_selection(&mut self, snapshot: &Snapshot) {
        let selected = self
            .index
            .results()
            .get(self.selection)
            .map(|result| snapshot.panes[result.pane].id.clone());
        if selected != self.selected {
            self.preview_scroll = 0;
        }
        self.selected = selected;
        let content = self
            .selected
            .as_ref()
            .map(|id| ui::tooltip(&ui::Item::Pane(id.clone()), snapshot))
            .unwrap_or_default();
        if content != self.preview {
            self.preview = content;
            self.preview_columns = None;
        }
        self.ensure_selection_visible();
    }

    fn ensure_selection_visible(&mut self) {
        let height = self.layout.list.height.max(1);
        if self.selection < self.scroll {
            self.scroll = self.selection;
        } else if self.selection >= self.scroll.saturating_add(height) {
            self.scroll = self.selection.saturating_add(1).saturating_sub(height);
        }
        self.scroll = self
            .scroll
            .min(self.index.results().len().saturating_sub(height));
    }

    pub(crate) fn navigate(&mut self, snapshot: &Snapshot, delta: isize) {
        self.selection = self
            .selection
            .saturating_add_signed(delta)
            .min(self.index.results().len().saturating_sub(1));
        if self.layout.input.height == 0 || self.layout.list.height == 0 {
            self.compact = Compact::List;
        }
        self.sync_selection(snapshot);
    }

    fn scroll_preview(&mut self, delta: isize) {
        self.prepare_preview();
        self.preview_scroll = self.preview_scroll.saturating_add_signed(delta).min(
            self.preview_lines
                .len()
                .saturating_sub(self.layout.preview.height.max(1)),
        );
    }

    fn prepare_preview(&mut self) {
        let preview = self.layout.preview;
        if self.preview_columns != Some(preview.width) {
            let mut rows = self.preview.rows();
            for row in &mut rows {
                if row.refresh.take().is_some() {
                    row.right.clear();
                }
            }
            self.preview_lines = overlay::wrapped_rows(&rows, preview.width);
            self.preview_columns = Some(preview.width);
        }
        self.preview_scroll = self
            .preview_scroll
            .min(self.preview_lines.len().saturating_sub(preview.height));
    }

    pub(crate) fn key(&mut self, snapshot: &Snapshot, key: KeyWithModifier) -> KeyAction {
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
        if key.key_modifiers.contains(&KeyModifier::Alt)
            || key.key_modifiers.contains(&KeyModifier::Super)
        {
            return KeyAction::None;
        }
        if key.bare_key == BareKey::Esc {
            return KeyAction::Cancel;
        }
        if key.bare_key == BareKey::Enter {
            return if self.selected.is_some() {
                KeyAction::Select
            } else {
                KeyAction::None
            };
        }
        match key.bare_key {
            BareKey::Up if !ctrl => self.navigate(snapshot, -1),
            BareKey::Down if !ctrl => self.navigate(snapshot, 1),
            BareKey::Char('p' | 'k') if ctrl => self.navigate(snapshot, -1),
            BareKey::Char('n' | 'j') if ctrl => self.navigate(snapshot, 1),
            BareKey::PageUp | BareKey::PageDown => {
                let page = self.layout.preview.height.max(1) as isize;
                self.scroll_preview(if key.bare_key == BareKey::PageUp {
                    -page
                } else {
                    page
                });
                if self.layout.preview.height == 0 {
                    self.compact = Compact::Preview;
                }
            }
            BareKey::Tab => {
                self.compact = match self.compact {
                    Compact::Input => Compact::List,
                    Compact::List => Compact::Preview,
                    Compact::Preview => Compact::Input,
                };
                self.pointer_preview = self.compact == Compact::Preview;
            }
            _ => {
                let before = self.input.value.len();
                if !self.input.key(&key) {
                    return KeyAction::None;
                }
                self.compact = Compact::Input;
                if self.input.value.len() != before {
                    self.selection = 0;
                    self.selected = None;
                    self.scroll = 0;
                    self.search(snapshot);
                }
            }
        }
        KeyAction::None
    }

    pub(crate) fn mouse(&mut self, snapshot: &Snapshot, mouse: Mouse) -> KeyAction {
        match mouse {
            Mouse::Hover(row, column) | Mouse::Hold(row, column) => {
                self.pointer_preview = self.layout.preview.contains(row, column);
            }
            Mouse::LeftClick(row, column) if self.layout.list.contains(row, column) => {
                let selected = self.scroll + row as usize - self.layout.list.y;
                if selected < self.index.results().len() {
                    self.selection = selected;
                    self.pointer_preview = false;
                    self.sync_selection(snapshot);
                }
            }
            Mouse::LeftClick(row, column) if self.layout.preview.contains(row, column) => {
                // OSC8 body glyphs retain their real links; the terminal owns opening them.
                self.pointer_preview = true;
            }
            Mouse::ScrollUp(lines) => {
                let delta = -(lines.min(isize::MAX as usize) as isize);
                if self.pointer_preview {
                    self.scroll_preview(delta)
                } else {
                    self.navigate(snapshot, delta)
                }
            }
            Mouse::ScrollDown(lines) => {
                let delta = lines.min(isize::MAX as usize) as isize;
                if self.pointer_preview {
                    self.scroll_preview(delta)
                } else {
                    self.navigate(snapshot, delta)
                }
            }
            _ => {}
        }
        KeyAction::None
    }
    pub(crate) fn working_clock_active(&self) -> bool {
        self.layout.preview.width > 0
            && self
                .preview_lines
                .iter()
                .skip(self.preview_scroll)
                .take(self.layout.preview.height)
                .any(|line| overlay::working_clock_visible(&line.row, self.layout.preview.width))
    }

    pub(crate) fn render(
        &mut self,
        snapshot: &Snapshot,
        theme: &ui::Theme,
        clock_frame: usize,
        rows: usize,
        columns: usize,
    ) {
        let preview = self.layout.preview;
        let input = self.layout.input;
        let prefix = if input.width >= 3 { "> " } else { "" };
        let input_width = input.width.saturating_sub(ui::width(prefix));
        let (query, _) = self.input.visible(input_width);
        // Host commands precede all render ANSI; the parent flushes this frame afterwards.

        let title = format!(
            " Pane finder · {}/{} ",
            self.index.results().len(),
            snapshot.panes.len()
        );
        crate::focused::frame(
            rows,
            columns,
            self.layout.border,
            &title,
            theme,
            &theme.accents[6],
        );
        if self.layout.border {
            paint(
                0,
                rows - 1,
                &format!("{}{}", theme.accents[6], self.layout.bottom_line(columns)),
                theme,
            );
        }
        if input.height > 0 {
            paint(
                input.x,
                input.y,
                &format!("{}{prefix}{query}", theme.bright),
                theme,
            );
        }
        if let Some(separator) = self.layout.search {
            paint(
                separator.x,
                separator.y,
                &format!("{}{}", theme.accents[6], self.layout.rule_line(separator)),
                theme,
            );
        }
        if let Some(separator) = self.layout.vertical {
            let line = format!("{}│", theme.accents[6]);
            for row in separator.y..separator.y + separator.height {
                paint(separator.x, row, &line, theme);
            }
        }
        if let Some(separator) = self.layout.horizontal {
            paint(
                separator.x,
                separator.y,
                &format!("{}{}", theme.accents[6], self.layout.rule_line(separator)),
                theme,
            );
        }
        let list = self.layout.list;
        for row in 0..list.height {
            let index = self.scroll + row;
            let text = self.result_line(snapshot, theme, index, list.width);
            let background = if index == self.selection && !self.index.results().is_empty() {
                theme.selected.as_str()
            } else {
                ""
            };
            paint(list.x, list.y + row, &format!("{background}{text}"), theme);
        }
        for row in 0..preview.height {
            let text = self
                .preview_lines
                .get(row + self.preview_scroll)
                .map(|line| {
                    overlay::render_action_line(&line.row, preview.width, false, theme, clock_frame)
                })
                .unwrap_or_else(|| {
                    ui::fit(
                        if row == 0 && self.selected.is_none() {
                            "No pane selected"
                        } else {
                            ""
                        },
                        preview.width,
                    )
                });
            paint(preview.x, preview.y + row, &text, theme);
        }
        print!("{}{}", overlay::OSC8_CLOSE, ui::RESET);
    }

    pub(crate) fn result_line(
        &mut self,
        snapshot: &Snapshot,
        theme: &ui::Theme,
        index: usize,
        columns: usize,
    ) -> String {
        if self.index.results().get(index).is_none() {
            return ui::fit(
                if index == 0 && self.index.results().is_empty() {
                    "No matching panes"
                } else {
                    ""
                },
                columns,
            );
        }
        crate::finder_entry::render(
            &mut self.index,
            snapshot,
            index,
            index == self.selection,
            columns,
            theme,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use bkpr::{Pane, Status, Workspace};
    fn snapshot() -> Snapshot {
        Snapshot {
            protocol: bkpr::PROTOCOL_VERSION,
            instance_id: "native:test".to_owned(),
            revision: 1,
            session: "test".to_owned(),
            connected: true,
            error: None,
            focused_pane_id: Some("pane-2".to_owned()),
            workspaces: vec![Workspace {
                id: "workspace".to_owned(),
                name: "Project workspace".to_owned(),
                active_tab_id: "tab".to_owned(),
                projects: Vec::new(),
                status: Status::Idle,
                attention: None,
            }],
            panes: (1..=3)
                .map(|index| Pane {
                    id: format!("pane-{index}"),
                    terminal_id: format!("terminal_{index}"),
                    workspace_id: "workspace".to_owned(),
                    tab_id: "tab".to_owned(),
                    name: format!("Terminal {index}"),
                    description: format!("Working on feature {index}"),
                    harness: None,
                    agent_id: None,
                    status: Status::Idle,
                    attention: None,
                    cwd: Some(format!("/project/{index}")),
                    branch: Some(format!("feature/{index}")),
                    branch_changes: None,
                    project: None,
                    ci: None,
                })
                .collect(),
        }
    }
    #[test]
    fn adaptive_surfaces_stay_disjoint_and_inside_every_small_viewport() {
        for columns in 0..=150 {
            for rows in 0..=40 {
                for compact in [Compact::Input, Compact::List, Compact::Preview] {
                    let layout = Layout::new(columns, rows, compact);
                    let rects = [
                        layout.input,
                        layout.list,
                        layout.preview,
                        layout.search.unwrap_or_default(),
                        layout.vertical.unwrap_or_default(),
                        layout.horizontal.unwrap_or_default(),
                    ];
                    for rect in rects {
                        assert!(rect.x + rect.width <= columns);
                        assert!(rect.y + rect.height <= rows);
                        if layout.border
                            && [layout.input, layout.list, layout.preview].contains(&rect)
                        {
                            assert!(rect.x >= 1 && rect.x + rect.width < columns);
                            assert!(rect.y >= 1 && rect.y + rect.height < rows);
                        }
                    }
                    for (index, left) in rects.iter().enumerate() {
                        for right in &rects[index + 1..] {
                            if left.width * left.height == 0 || right.width * right.height == 0 {
                                continue;
                            }
                            assert!(
                                left.x + left.width <= right.x
                                    || right.x + right.width <= left.x
                                    || left.y + left.height <= right.y
                                    || right.y + right.height <= left.y
                            );
                        }
                    }
                    if columns > 0 && rows >= 3 {
                        assert!(layout.input.height > 0);
                        assert!(layout.list.height > 0);
                        assert!(layout.preview.height > 0);
                    }
                    if let Some(rule) = layout.search {
                        assert_eq!(rule.y, layout.input.y + layout.input.height);
                        assert_eq!(rule.width, columns);
                        assert_eq!(ui::width(&layout.rule_line(rule)), columns);
                        assert_eq!(layout.list.y, rule.y + 1);
                    }
                    if let Some(rule) = layout.horizontal {
                        assert_eq!(rule.width, columns);
                        assert_eq!(ui::width(&layout.rule_line(rule)), columns);
                    }
                    if layout.border {
                        assert_eq!(ui::width(&layout.bottom_line(columns)), columns);
                        assert_eq!(layout.preview.y + layout.preview.height, rows - 1);
                    }
                }
            }
        }
        let wide = Layout::new(110, 25, Compact::Input);
        assert_eq!(wide.list.y, wide.preview.y);
        assert!(wide.list.x + wide.list.width < wide.preview.x);
        let narrow = Layout::new(60, 25, Compact::Input);
        assert!(narrow.list.y + narrow.list.height < narrow.preview.y);
    }

    #[test]
    fn wide_rules_join_the_frame_without_crossing_the_full_width_input() {
        for rows in [7, 8, 25, 40] {
            let columns = 110;
            let layout = Layout::new(columns, rows, Compact::Input);
            let search = layout.search.unwrap();
            let vertical = layout.vertical.unwrap();
            let top = layout.rule_line(search);
            let bottom = layout.bottom_line(columns);
            assert!(top.starts_with('├') && top.ends_with('┤'));
            assert!(bottom.starts_with('╰') && bottom.ends_with('╯'));
            assert_eq!(top.chars().nth(vertical.x), Some('┬'));
            assert_eq!(bottom.chars().nth(vertical.x), Some('┴'));
            assert_eq!(top.matches('┬').count(), 1);
            assert_eq!(bottom.matches('┴').count(), 1);
            assert_eq!(vertical.y, search.y + 1);
            assert_eq!(vertical.y + vertical.height, rows - 1);
            assert_eq!(layout.input.x, 1);
            assert_eq!(layout.input.width, columns - 2);
            assert!(layout.input.y + layout.input.height <= search.y);
            assert_eq!(layout.list.height, rows - 4);
            assert_eq!(layout.preview.height, rows - 4);
            assert!(layout.horizontal.is_none());
        }
    }

    #[test]
    fn narrow_rules_join_both_sides_and_body_uses_the_last_interior_row() {
        for rows in [7, 8, 25, 40] {
            let columns = 60;
            let layout = Layout::new(columns, rows, Compact::Input);
            assert!(layout.vertical.is_none());
            for separator in [layout.search.unwrap(), layout.horizontal.unwrap()] {
                let line = layout.rule_line(separator);
                assert_eq!(line, format!("├{}┤", "─".repeat(columns - 2)));
            }
            let middle = layout.horizontal.unwrap();
            assert_eq!(middle.y, layout.list.y + layout.list.height);
            assert_eq!(layout.preview.y, middle.y + 1);
            assert_eq!(layout.preview.y + layout.preview.height, rows - 1);
            assert_eq!(layout.list.height + layout.preview.height + 1, rows - 4);
            assert_eq!(
                layout.bottom_line(columns),
                format!("╰{}╯", "─".repeat(columns - 2))
            );
        }
    }
    fn coordinates() -> FloatingPaneCoordinates {
        FloatingPaneCoordinates::default()
            .with_width_fixed(100)
            .with_height_fixed(28)
    }
    fn finder(snapshot: &Snapshot) -> Finder {
        let mut finder = Finder::default();
        finder.update(snapshot, true, true, &coordinates());
        finder
    }

    #[test]
    fn query_editor_navigation_paste_and_identity_survive_refresh() {
        let mut state = snapshot();
        let mut finder = finder(&state);
        assert_eq!(finder.selected(), Some("pane-2"));
        finder.input_event(&state, Input::Paste("Terminal".to_owned()));
        finder.key(&state, KeyWithModifier::new(BareKey::Down));
        let selected = finder.selected.clone();
        state.panes.reverse();
        finder.update(&state, false, true, &coordinates());
        assert_eq!(finder.selected, selected);
        assert_eq!(finder.input.value, "Terminal");
        finder.key(
            &state,
            KeyWithModifier {
                bare_key: BareKey::Char('u'),
                key_modifiers: std::collections::BTreeSet::from([KeyModifier::Ctrl]),
            },
        );
        for character in "α界ß".chars() {
            finder.key(&state, KeyWithModifier::new(BareKey::Char(character)));
        }
        finder.key(&state, KeyWithModifier::new(BareKey::Left));
        finder.key(&state, KeyWithModifier::new(BareKey::Backspace));
        assert_eq!(finder.input.value, "αß");
        assert_eq!(finder.input.cursor, "α".len());
        assert_eq!(
            finder.key(&state, KeyWithModifier::new(BareKey::Esc)),
            KeyAction::Cancel
        );
    }

    #[test]
    fn small_viewport_cursor_and_mouse_hit_testing_stay_inside_content() {
        let state = snapshot();
        let mut finder = finder(&state);
        finder.input_event(&state, Input::Paste("Terminal".to_owned()));
        for columns in 0..=12 {
            for rows in 0..=8 {
                if let Some((x, y)) = finder.prepare(rows, columns) {
                    assert!(x < columns && y < rows);
                }
            }
        }
        finder.prepare(25, 110);
        let list = finder.layout.list;
        finder.mouse(&state, Mouse::LeftClick(list.y as isize, list.x));
        assert_eq!(finder.selected(), Some("pane-1"));
        let selected = finder.selected.clone();
        let rule = finder.layout.search.unwrap();
        finder.mouse(&state, Mouse::LeftClick(rule.y as isize, rule.x));
        assert_eq!(finder.selected, selected);
    }

    #[test]
    fn preview_scroll_and_working_badges_follow_selection_and_live_removals() {
        let mut state = snapshot();
        state.panes[0].harness = Some("omp".to_owned());
        state.panes[0].status = Status::Working;
        let mut finder = finder(&state);
        finder.navigate(&state, -1);
        finder.prepare(28, 100);
        assert!(finder.working_clock_active());
        let selected = finder.selected.clone();
        let preview = finder.layout.preview;
        finder.mouse(&state, Mouse::Hover(preview.y as isize, preview.x));
        finder.mouse(&state, Mouse::ScrollDown(usize::MAX));
        assert_eq!(finder.selected, selected);
        assert_eq!(
            finder.preview_scroll,
            finder.preview_lines.len().saturating_sub(preview.height)
        );
        finder.navigate(&state, 1);
        finder.prepare(28, 100);
        assert_eq!(finder.preview_scroll, 0);
        assert!(!finder.working_clock_active());
        state
            .panes
            .retain(|pane| Some(&pane.id) != finder.selected.as_ref());
        finder.update(&state, false, true, &coordinates());
        assert!(
            finder
                .selected
                .as_ref()
                .is_some_and(|id| state.panes.iter().any(|pane| &pane.id == id))
        );
        state.panes.clear();
        finder.update(&state, false, true, &coordinates());
        assert!(finder.selected.is_none());
        assert!(finder.preview.is_empty());
    }

    #[test]
    fn one_row_layout_can_cycle_every_surface_and_resume_unicode_input() {
        let state = snapshot();
        let mut finder = finder(&state);
        assert!(finder.prepare(1, 5).is_some());
        finder.key(&state, KeyWithModifier::new(BareKey::Tab));
        assert!(finder.prepare(1, 5).is_none());
        assert_eq!(finder.layout.list.height, 1);
        finder.key(&state, KeyWithModifier::new(BareKey::Tab));
        finder.prepare(1, 5);
        assert_eq!(finder.layout.preview.height, 1);
        finder.key(&state, KeyWithModifier::new(BareKey::Char('界')));
        assert!(finder.prepare(1, 5).is_some());
        assert_eq!(finder.compact, Compact::Input);
    }
}
