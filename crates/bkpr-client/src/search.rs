use super::*;

const MAX_NAVIGATION: usize = 64;

struct Editor {
    pane: String,
    view: String,
    workspace: Option<String>,
    epoch: u64,
    query: String,
    revision: u64,
    dirty: bool,
    generation: Option<u64>,
    error: Option<String>,
    navigation: VecDeque<SearchAction>,
    repeating: HashSet<String>,
}
struct Flight {
    pane: String,
    view: String,
    epoch: u64,
    query: Option<u64>,
    receipt: updates::ResponseReceipt,
}
#[derive(Default)]
pub(super) struct State {
    epoch: u64,
    editor: Option<Editor>,
    flights: HashMap<String, Flight>,
    clears: VecDeque<(String, String)>,
}

impl App {
    pub(super) fn search_open(&self) -> bool {
        self.search.editor.is_some()
    }

    pub(super) fn begin_search(&mut self) -> Result<()> {
        let pane = self
            .state
            .focused
            .clone()
            .context("no focused pane to search")?;
        let attached = self.views.get(&pane).context("pane is not attached")?;
        if !attached.spec.visible || attached.retirement_requested || attached.frame.is_none() {
            bail!("pane view is not available for search");
        }
        let view = attached.view.clone();
        self.close_search();
        self.retire_resize("search opened");
        self.selection_focus_lost();
        self.search.epoch = self.search.epoch.wrapping_add(1);
        self.search.editor = Some(Editor {
            pane,
            view,
            workspace: self.state.workspace.clone(),
            epoch: self.search.epoch,
            query: String::new(),
            revision: 0,
            dirty: true,
            generation: None,
            error: None,
            navigation: VecDeque::new(),
            repeating: HashSet::new(),
        });
        Ok(())
    }

    pub(super) fn close_search(&mut self) {
        if let Some(editor) = self.search.editor.take() {
            let target = (editor.pane, editor.view);
            if !self.search.clears.contains(&target) {
                self.search.clears.push_back(target);
            }
            self.search.epoch = self.search.epoch.wrapping_add(1);
        }
    }

    fn search_valid(&self, editor: &Editor) -> bool {
        self.state.workspace == editor.workspace
            && self.state.focused.as_ref() == Some(&editor.pane)
            && self.state.form.is_none()
            && !self.resize_capturing()
            && self.views.get(&editor.pane).is_some_and(|view| {
                view.view == editor.view && view.spec.visible && !view.retirement_requested
            })
            && !self.view_failures.contains_key(&editor.pane)
    }

    pub(super) fn validate_search(&mut self) {
        if self
            .search
            .editor
            .as_ref()
            .is_some_and(|editor| !self.search_valid(editor))
        {
            self.close_search();
        }
    }

    pub(super) fn search_spans(&self, pane: &str) -> &[SearchSpan] {
        self.search_snapshot()
            .filter(|_| {
                self.search
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.pane == pane)
            })
            .map_or(&[], |search| search.spans.as_slice())
    }

    fn search_snapshot(&self) -> Option<&SearchSnapshot> {
        let editor = self.search.editor.as_ref()?;
        if !self.search_valid(editor) || editor.dirty || editor.error.is_some() {
            return None;
        }
        let generation = editor.generation?;
        self.views
            .get(&editor.pane)?
            .metadata
            .search
            .as_ref()
            .filter(|search| search.query == editor.query && search.generation == generation)
    }

    pub(super) fn search_repeat(&self, key: &str) -> bool {
        !self.state.prefix
            && !self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
            && self
                .search
                .editor
                .as_ref()
                .is_some_and(|editor| self.search_valid(editor) && editor.repeating.contains(key))
    }

    pub(super) fn search_release(&mut self, key: &str) {
        if let Some(editor) = self.search.editor.as_mut() {
            editor.repeating.remove(key);
        }
    }

    pub(super) fn search_key(&mut self, event: &KeyEvent) -> bool {
        if !self.search_open() {
            return false;
        }
        let editor = self.search.editor.as_mut().unwrap();
        if event.action == KeyAction::Repeat && !editor.repeating.contains(&event.key) {
            return true;
        }
        if event.action == KeyAction::Press && !self.held.contains_key(&event.key) {
            editor.repeating.insert(event.key.clone());
        }
        if !self.held.contains_key(&event.key) {
            self.state.suppressed.insert(event.key.clone());
        }
        use crate::keyguide::{self, Action, Context};
        use bkpr_tui_framework::EditAction;
        match keyguide::lookup(Context::Search, event) {
            Some(Action::Cancel) => self.close_search(),
            Some(action @ (Action::NextMatch | Action::PreviousMatch)) => {
                let editor = self.search.editor.as_mut().unwrap();
                if editor.navigation.len() < MAX_NAVIGATION {
                    editor
                        .navigation
                        .push_back(if action == Action::PreviousMatch {
                            SearchAction::Previous
                        } else {
                            SearchAction::Next
                        });
                } else {
                    self.notify("Search navigation queue full; key not admitted".into());
                }
            }
            Some(Action::Edit(EditAction::Backspace)) => {
                let editor = self.search.editor.as_mut().unwrap();
                if editor.query.pop().is_some() {
                    editor.dirty = true;
                    editor.revision = editor.revision.wrapping_add(1);
                    editor.generation = None;
                    editor.error = None;
                    editor.navigation.clear();
                }
            }
            Some(Action::Edit(EditAction::Clear)) => {
                let editor = self.search.editor.as_mut().unwrap();
                editor.query.clear();
                editor.dirty = true;
                editor.revision = editor.revision.wrapping_add(1);
                editor.generation = None;
                editor.error = None;
                editor.navigation.clear();
            }
            None if event.modifiers & 14 == 0 => self.search_text(&event.text),
            _ => {}
        }
        true
    }

    pub(super) fn search_text(&mut self, text: &str) {
        let Some(editor) = self.search.editor.as_mut() else {
            return;
        };
        if editor.query.len() + text.len() > MAX_SEARCH_BYTES {
            self.notify(format!(
                "Search query full ({MAX_SEARCH_BYTES} UTF-8 bytes); text not admitted"
            ));
            return;
        }
        let text = safe_text(text);
        if text.is_empty() {
            return;
        }
        editor.query.push_str(&text);
        editor.dirty = true;
        editor.revision = editor.revision.wrapping_add(1);
        editor.generation = None;
        editor.error = None;
        editor.navigation.clear();
    }

    // One operation per view and one latest editor query are retained. A slow
    // retiring pane cannot delay another view. Existing input wakes drive all
    // completions; idle searches add no timer or transport request.
    pub(super) fn flush_search(&mut self) -> Result<()> {
        self.validate_search();
        let editor = &mut self.search.editor;
        self.search.flights.retain(|_, flight| {
            let outcome = flight.receipt.try_recv();
            if matches!(outcome, Err(mpsc::TryRecvError::Empty)) {
                return true;
            }
            if let Some(editor) = editor.as_mut().filter(|editor| {
                editor.epoch == flight.epoch
                    && editor.pane == flight.pane
                    && editor.view == flight.view
                    && !editor.dirty
                    && flight
                        .query
                        .is_none_or(|revision| revision == editor.revision)
            }) {
                match outcome {
                    Ok(Ok(response)) if flight.query.is_some() => {
                        editor.generation = response
                            .get("generation")
                            .and_then(serde_json::Value::as_u64);
                        editor.error = editor
                            .generation
                            .is_none()
                            .then(|| "Search response omitted its generation".into());
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => editor.error = Some(format!("Search rejected: {error}")),
                    Err(_) => {
                        editor.error = Some(
                            "Search completion disconnected; outcome unknown; not replayed".into(),
                        )
                    }
                }
            }
            false
        });
        let mut index = 0;
        while index < self.search.clears.len() {
            let (pane, view) = &self.search.clears[index];
            if self
                .views
                .get(pane)
                .is_none_or(|attached| attached.view != *view || attached.retirement_requested)
            {
                self.search.clears.remove(index);
                continue;
            }
            if self.search.flights.contains_key(view) {
                index += 1;
                continue;
            }
            let receipt = self.input.response(Operation::Pane {
                pane_id: pane.clone(),
                request: PaneRequest::Search {
                    view_id: view.clone(),
                    action: SearchAction::Clear,
                },
            })?;
            let (pane, view) = self.search.clears.remove(index).unwrap();
            self.search.flights.insert(
                view.clone(),
                Flight {
                    pane,
                    view,
                    epoch: 0,
                    query: None,
                    receipt,
                },
            );
        }
        let Some(editor) = self.search.editor.as_mut() else {
            return Ok(());
        };
        if self.search.flights.contains_key(&editor.view) {
            return Ok(());
        }
        let (action, query) = if editor.dirty {
            (
                SearchAction::Query {
                    text: editor.query.clone(),
                },
                Some(editor.revision),
            )
        } else if let Some(action) = editor.navigation.front() {
            (action.clone(), None)
        } else {
            return Ok(());
        };
        let receipt = self.input.response(Operation::Pane {
            pane_id: editor.pane.clone(),
            request: PaneRequest::Search {
                view_id: editor.view.clone(),
                action,
            },
        })?;
        if query.is_some() {
            editor.dirty = false;
        } else {
            editor.navigation.pop_front();
        }
        self.search.flights.insert(
            editor.view.clone(),
            Flight {
                pane: editor.pane.clone(),
                view: editor.view.clone(),
                epoch: editor.epoch,
                query,
                receipt,
            },
        );
        Ok(())
    }

    pub(super) fn search_overlay(&self, surface: &mut UiSurface, size: Size) -> bool {
        let Some(editor) = self.search.editor.as_ref() else {
            return false;
        };
        let status = if let Some(error) = &editor.error {
            error.clone()
        } else if editor.query.is_empty() {
            "type a literal query".into()
        } else if let Some(search) = self.search_snapshot() {
            let current = search
                .selected_index
                .map_or_else(|| "-".into(), |index| (index + 1).to_string());
            if search.running {
                format!("searching | {current}/{} found", search.total_matches)
            } else if search.total_matches == 0 {
                "no matches".into()
            } else {
                format!("{current}/{} newest→oldest", search.total_matches)
            }
        } else {
            "searching".into()
        };
        let mut width = 0;
        let mut start = editor.query.len();
        for (index, ch) in editor.query.char_indices().rev() {
            let columns = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + columns > usize::from(size.cols / 3) {
                break;
            }
            start = index;
            width += columns;
        }
        let prefix = format!("Search {status} | /");
        let value = format!(
            "{prefix}{} | Enter next  Shift-Enter previous  Esc clear",
            &editor.query[start..]
        );
        let local = Size { rows: 1, ..size };
        let cursor = Cursor {
            x: (unicode_width::UnicodeWidthStr::width(prefix.as_str()) + width)
                .min(usize::from(size.cols.saturating_sub(1))) as u16,
            y: 0,
            visible: true,
            blinking: false,
            shape: CursorShape::Bar,
            color: None,
        };
        surface.paint(
            local,
            vec![(
                Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: 1,
                },
                value,
                false,
            )],
            Some(cursor),
        );
        true
    }

    pub(super) fn theme_search(&self, surface: &mut UiSurface, theme: &Theme) {
        let Some(editor) = &self.search.editor else {
            return;
        };
        let size = surface.frame.size;
        let mut spans = vec![RoleSpan {
            rect: Rect {
                x: 0,
                y: 0,
                width: size.cols,
                height: 1,
            },
            role: if editor.error.is_some() {
                Role::Error
            } else {
                Role::Muted
            },
        }];
        let cursor = &surface.frame.cursor;
        if cursor.visible {
            let mut width = 0;
            for ch in editor.query.chars().rev() {
                let columns = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if width + columns > usize::from(size.cols / 3) {
                    break;
                }
                width += columns;
            }
            spans.push(RoleSpan {
                rect: Rect {
                    x: cursor.x.saturating_sub(width as u16),
                    y: 0,
                    width: (width as u16).saturating_add(1),
                    height: 1,
                },
                role: Role::Selected,
            });
        }
        surface.apply_theme(theme, Role::Panel, &spans);
    }
}
