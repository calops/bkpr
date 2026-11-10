use crate::{Error, Result, Terminal, sdk};
use bkpr_protocol::{MAX_SEARCH_BYTES, SearchAction, SearchSnapshot, SearchSpan};
use sdk::{
    search::{MatchBuffer, Search, Status},
    terminal::{Point, PointCoordinate, PointSpace, ScrollViewport},
};
use std::collections::HashMap;

struct ViewSearch {
    native: Search<'static, 'static>,
    query: String,
    generation: u64,
    epoch: u64,
    dirty: bool,
    matches: MatchBuffer,
}
#[derive(Default)]
pub(crate) struct Searches {
    views: HashMap<String, ViewSearch>,
    generation: u64,
    cursor: usize,
}
impl Searches {
    pub(crate) fn invalidate(&mut self) {
        for view in self.views.values_mut() {
            view.dirty = true;
        }
    }
    pub(crate) fn retain(&mut self, mut epoch: impl FnMut(&str) -> Option<u64>) {
        self.views.retain(|id, view| epoch(id) == Some(view.epoch));
    }
}

fn restore(native: &mut sdk::Terminal<'_, '_>, bar: sdk::terminal::Scrollbar) {
    native.scroll_viewport(if bar.offset + bar.len >= bar.total {
        ScrollViewport::Bottom
    } else {
        ScrollViewport::Row(bar.offset as usize)
    });
}

impl Terminal {
    /// Create or navigate one inspection-local native search. Native matches are
    /// never installed as the terminal selection. No terminal text is copied.
    pub fn search_view(&mut self, id: &str, epoch: u64, action: SearchAction) -> Result<bool> {
        if let SearchAction::Query { text } = &action
            && text.len() > MAX_SEARCH_BYTES
        {
            return Err(Error("search query exceeds byte limit".into()));
        }
        if matches!(&action, SearchAction::Clear)
            || matches!(&action, SearchAction::Query { text } if text.is_empty())
        {
            self.searches.views.remove(id);
            return Ok(false);
        }
        if self
            .searches
            .views
            .get(id)
            .is_some_and(|v| v.epoch != epoch)
        {
            self.searches.views.remove(id);
        }
        if let SearchAction::Query { text } = action {
            if !self.searches.views.contains_key(id) {
                self.searches.views.insert(
                    id.to_owned(),
                    ViewSearch {
                        native: Search::new(&mut self.native)?,
                        query: String::new(),
                        generation: 0,
                        epoch,
                        dirty: true,
                        matches: MatchBuffer::new(),
                    },
                );
            }
            let view = self.searches.views.get_mut(id).expect("inserted search");
            if view.query != text {
                self.searches.generation = self
                    .searches
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| Error("search generation exhausted".into()))?;
                view.native.set_needle(&mut self.native, text.as_bytes())?;
                view.query = text;
                view.generation = self.searches.generation;
                view.dirty = true;
            }
            return Ok(false);
        }
        let before = self.view_scrollback_offset(id)?;
        self.navigate_search(id, matches!(action, SearchAction::Previous))?;
        Ok(before != self.view_scrollback_offset(id)?)
    }
    fn navigate_search(&mut self, id: &str, previous: bool) -> Result<()> {
        let before = self.view_scrollback_offset(id)?;
        let bar = self.native.scrollbar()?;
        self.native.scroll_viewport(ScrollViewport::Row(
            bar.total.saturating_sub(bar.len).saturating_sub(before) as usize,
        ));
        let result = (|| -> Result<Option<u64>> {
            let Some(view) = self.searches.views.get_mut(id) else {
                return Ok(None);
            };
            let selected = if previous {
                view.native.select_prev(&mut self.native)?
            } else {
                view.native.select_next(&mut self.native)?
            };
            if !selected {
                return Ok(None);
            }
            Ok(Some(self.native.scrollbar()?.offset))
        })();
        restore(&mut self.native, bar);
        if let Some(row) = result?
            && self.native.active_screen()? == sdk::screen::Screen::Primary
        {
            let point = Point::Screen(PointCoordinate {
                x: 0,
                y: row as u32,
            });
            if row + bar.len >= bar.total {
                self.history.remove(id);
            } else if let Some(anchor) = self.history.get_mut(id) {
                anchor.set(&mut self.native, point)?;
            } else {
                self.history
                    .insert(id.to_owned(), self.native.track_grid_ref(point)?);
            }
        }
        Ok(())
    }
    pub fn retain_search_views(&mut self, epoch: impl FnMut(&str) -> Option<u64>) {
        self.searches.retain(epoch);
    }
    pub fn has_view_searches(&self) -> bool {
        !self.searches.views.is_empty()
    }
    pub fn view_search_generation(&self, id: &str) -> Option<u64> {
        self.searches.views.get(id).map(|view| view.generation)
    }
    pub fn has_search_work(&self) -> Result<bool> {
        for view in self.searches.views.values() {
            if view.dirty || view.native.status()? != Status::Complete {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// One view, one bounded SDK feed/tick per worker turn. Round-robin fairness
    /// keeps unrelated views and PTY controls responsive under large histories.
    pub fn advance_search(&mut self) -> Result<Option<(String, u64, bool)>> {
        let len = self.searches.views.len();
        if len == 0 {
            return Ok(None);
        }
        let cursor = self.searches.cursor % len;
        let entries = self.searches.views.iter().enumerate();
        let mut chosen = None;
        for (index, (id, view)) in entries.clone().skip(cursor).chain(entries.take(cursor)) {
            if view.dirty || view.native.status()? != Status::Complete {
                chosen = Some((index, id.clone()));
                break;
            }
        }
        let Some((index, id)) = chosen else {
            return Ok(None);
        };
        let view = self.searches.views.get_mut(&id).expect("search exists");
        if view.dirty || view.native.status()? == Status::FeedRequired {
            view.native.feed(&mut self.native)?;
            view.dirty = false;
        }
        view.native.tick()?;
        let epoch = view.epoch;
        // Native resize/reflow can discard its selected match while retaining
        // the query. Recover an actual newest native result, never a fabricated
        // old index. A still-valid selection (including ASCII-equivalent query
        // edits) is left untouched.
        let select = view.native.selected_index()?.is_none() && view.native.total_matches()? > 0;
        self.searches.cursor = (index + 1) % len;
        let before = self.view_scrollback_offset(&id)?;
        if select {
            self.navigate_search(&id, false)?;
        }
        let moved = before != self.view_scrollback_offset(&id)?;
        Ok(Some((id, epoch, moved)))
    }
    /// Borrow native matches only within this call, project the bounded viewport
    /// list, and discard all grid references before allowing another VT write.
    pub fn view_search(&mut self, id: &str) -> Result<Option<SearchSnapshot>> {
        let offset = self.view_scrollback_offset(id)?;
        let Some(view) = self.searches.views.get_mut(id) else {
            return Ok(None);
        };
        let bar = self.native.scrollbar()?;
        let top = bar.total.saturating_sub(bar.len).saturating_sub(offset);
        self.native
            .scroll_viewport(ScrollViewport::Row(top as usize));
        let result = (|| -> Result<SearchSnapshot> {
            let snapshot = view.native.snapshot(&mut self.native)?;
            let terminal = snapshot.terminal();
            let points = |selection: sdk::selection::Selection<'_>| -> Result<Option<(PointCoordinate, PointCoordinate)>> {
                let ordered = selection.to_ordered(terminal, sdk::selection::Order::Forward)?;
                Ok(terminal.point_from_grid_ref(&ordered.start(), PointSpace::Screen)?
                    .zip(terminal.point_from_grid_ref(&ordered.end(), PointSpace::Screen)?))
            };
            let selected = snapshot
                .selected_match()?
                .map(points)
                .transpose()?
                .flatten();
            let mut spans = Vec::new();
            for selection in snapshot.viewport_matches(&mut view.matches)? {
                let Some((start, end)) = points(selection)? else {
                    continue;
                };
                let current = selected == Some((start, end));
                let first = u64::from(start.y)
                    .saturating_sub(top)
                    .min(u64::from(self.size.rows));
                let last = u64::from(end.y)
                    .saturating_add(1)
                    .saturating_sub(top)
                    .min(u64::from(self.size.rows));
                for row in first as u16..last as u16 {
                    let y = top + u64::from(row);
                    let left = if y == u64::from(start.y) { start.x } else { 0 };
                    let right = if y == u64::from(end.y) {
                        end.x.saturating_add(1)
                    } else {
                        self.size.cols
                    };
                    if left < right.min(self.size.cols) {
                        spans.push(SearchSpan {
                            row,
                            start: left,
                            end: right.min(self.size.cols),
                            selected: current,
                        });
                    }
                }
            }
            spans.sort_unstable_by_key(|span| (span.row, span.start, span.end, span.selected));
            Ok(SearchSnapshot {
                query: view.query.clone(),
                generation: view.generation,
                running: view.native.status()? != Status::Complete,
                total_matches: view.native.total_matches()? as u64,
                selected_index: view.native.selected_index()?.map(|i| i as u64),
                spans,
            })
        })();
        restore(&mut self.native, bar);
        result.map(Some)
    }
}
