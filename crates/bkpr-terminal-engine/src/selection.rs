use crate::{Error, Result, Terminal, sdk};
use bkpr_protocol::{
    MAX_INPUT_BYTES, SelectionAction, SelectionEvent, SelectionSnapshot, SelectionSpan,
};
use sdk::{
    screen::{Screen, TrackedGridRef},
    selection::{
        FormatOptions, Selection,
        gesture::{
            Autoscroll, AutoscrollTickEvent, DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent,
        },
    },
    terminal::{Point, PointCoordinate, PointSpace, ScrollViewport},
};
use std::{collections::HashMap, time::Duration};

// A stable virtual surface avoids tying inspection to the current size owner's
// physical pixels, and preserves the native gesture's half-cell precision.
const CELL_WIDTH: u32 = 8;
const CELL_HEIGHT: u32 = 16;

struct Endpoints {
    start: TrackedGridRef,
    end: TrackedGridRef,
    rectangle: bool,
}
impl Endpoints {
    fn snapshot<'t>(&self, native: &'t sdk::Terminal<'_, '_>) -> Result<Option<Selection<'t>>> {
        Ok(
            match (self.start.snapshot(native)?, self.end.snapshot(native)?) {
                (Some(start), Some(end)) => Some(Selection::new(start, end, self.rectangle)),
                _ => None,
            },
        )
    }
}
struct View {
    gesture: Gesture<'static>,
    endpoints: Option<Endpoints>,
    dragging: bool,
    position: (f32, f32),
    time_ms: u64,
}
impl View {
    fn new() -> Result<Self> {
        Ok(Self {
            gesture: Gesture::new()?,
            endpoints: None,
            dragging: false,
            position: (0.0, 0.0),
            time_ms: 0,
        })
    }
    fn reset(&mut self, native: &sdk::Terminal<'_, '_>) {
        self.gesture.reset(native);
        self.endpoints = None;
        self.dragging = false;
    }
    fn valid(&self) -> bool {
        self.endpoints
            .as_ref()
            .is_none_or(|ends| ends.start.has_value() && ends.end.has_value())
    }
    fn remember(
        &mut self,
        native: &mut sdk::Terminal<'_, '_>,
        points: Option<(PointCoordinate, PointCoordinate, bool)>,
    ) -> Result<()> {
        match points {
            Some((start, end, rectangle)) => {
                if let Some(ends) = &mut self.endpoints {
                    ends.start.set(native, Point::Screen(start))?;
                    ends.end.set(native, Point::Screen(end))?;
                    ends.rectangle = rectangle;
                } else {
                    self.endpoints = Some(Endpoints {
                        start: native.track_grid_ref(Point::Screen(start))?,
                        end: native.track_grid_ref(Point::Screen(end))?,
                        rectangle,
                    });
                }
            }
            None => self.endpoints = None,
        }
        Ok(())
    }
}

fn coordinates(
    native: &sdk::Terminal<'_, '_>,
    selection: Option<Selection<'_>>,
) -> Result<Option<(PointCoordinate, PointCoordinate, bool)>> {
    let Some(selection) = selection else {
        return Ok(None);
    };
    Ok(
        match (
            native.point_from_grid_ref(&selection.start(), PointSpace::Screen)?,
            native.point_from_grid_ref(&selection.end(), PointSpace::Screen)?,
        ) {
            (Some(start), Some(end)) => Some((start, end, selection.is_rectangle())),
            _ => None,
        },
    )
}

pub(crate) struct Selections {
    views: HashMap<String, View>,
    screen: Screen,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    tick: AutoscrollTickEvent<'static>,
}
impl Selections {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            views: HashMap::new(),
            screen: Screen::Primary,
            press: PressEvent::new()?,
            drag: DragEvent::new()?,
            release: ReleaseEvent::new()?,
            tick: AutoscrollTickEvent::new()?,
        })
    }
    pub(crate) fn retain(
        &mut self,
        native: &sdk::Terminal<'_, '_>,
        mut keep: impl FnMut(&str) -> bool,
    ) {
        self.views.retain(|id, view| {
            if !keep(id) {
                view.reset(native);
                false
            } else {
                true
            }
        });
    }
    pub(crate) fn reconcile(&mut self, native: &sdk::Terminal<'_, '_>) -> Result<()> {
        if self.views.is_empty() {
            return Ok(());
        }
        let screen = native.active_screen()?;
        if self.screen != screen {
            self.retain(native, |_| false);
            self.screen = screen;
        } else {
            self.views.retain(|_, view| {
                if !view.valid() {
                    view.reset(native);
                    false
                } else {
                    true
                }
            });
        }
        Ok(())
    }
    fn apply(
        &mut self,
        native: &mut sdk::Terminal<'_, '_>,
        size: bkpr_protocol::Size,
        id: &str,
        event: SelectionEvent,
    ) -> Result<()> {
        if event.action == SelectionAction::Clear {
            if let Some(mut view) = self.views.remove(id) {
                view.reset(native);
            }
            return Ok(());
        }
        if !self.views.contains_key(id) {
            if event.action != SelectionAction::Press {
                return Ok(());
            }
            self.views.insert(id.to_owned(), View::new()?);
        }
        let view = self.views.get_mut(id).expect("selection view inserted");
        if !view.valid() {
            view.reset(native);
        }
        let geometry = Geometry {
            columns: u32::from(size.cols),
            cell_width: CELL_WIDTH,
            padding_left: 0,
            screen_height: u32::from(size.rows) * CELL_HEIGHT,
        };
        let x = f64::from(event.x) * f64::from(CELL_WIDTH);
        let y = f64::from(event.y) * f64::from(CELL_HEIGHT);
        let point = PointCoordinate {
            x: event.x.floor().clamp(0.0, f32::from(size.cols - 1)) as u16,
            y: event.y.floor().clamp(0.0, f32::from(size.rows - 1)) as u32,
        };
        match event.action {
            SelectionAction::Press => {
                if event.time_ms < view.time_ms {
                    view.reset(native);
                }
                view.time_ms = event.time_ms;
                self.press
                    .set_position(x, y)?
                    .set_time(Duration::from_millis(event.time_ms))?
                    .set_repeat_distance(f64::from(CELL_WIDTH) / 2.0)?
                    .set_repeat_interval(Duration::from_millis(500))?;
                let selection = self.press.apply(
                    &mut view.gesture,
                    native,
                    native.grid_ref(Point::Viewport(point))?,
                )?;
                let points = coordinates(native, selection)?;
                view.remember(native, points)?;
                view.dragging = true;
            }
            SelectionAction::Drag | SelectionAction::Release if view.dragging => {
                // A click-only release must not turn into a one-cell drag. A
                // release which moved since the last event still gets its final edge.
                if event.action == SelectionAction::Drag || view.position != (event.x, event.y) {
                    self.drag
                        .set_position(x, y)?
                        .set_rectangle(event.modifiers & 4 != 0)?;
                    let selection = self.drag.apply(
                        &mut view.gesture,
                        native,
                        native.grid_ref(Point::Viewport(point))?,
                        geometry,
                    )?;
                    let points = coordinates(native, selection)?;
                    view.remember(native, points)?;
                }
                if event.action == SelectionAction::Release {
                    self.release.apply(
                        &mut view.gesture,
                        native,
                        Some(native.grid_ref(Point::Viewport(point))?),
                    )?;
                    view.dragging = false;
                }
            }
            SelectionAction::Tick if view.dragging => {
                self.tick
                    .set_position(x, y)?
                    .set_rectangle(event.modifiers & 4 != 0)?;
                let selection = self
                    .tick
                    .apply(&mut view.gesture, native, point, geometry)?;
                let points = coordinates(native, selection)?;
                view.remember(native, points)?;
            }
            _ => {}
        }
        view.position = (event.x, event.y);
        Ok(())
    }
    fn direction(&self, native: &sdk::Terminal<'_, '_>, id: &str) -> Result<i8> {
        let Some(view) = self.views.get(id).filter(|view| view.dragging) else {
            return Ok(0);
        };
        Ok(match view.gesture.autoscroll(native)? {
            Autoscroll::Up => -1,
            Autoscroll::Down => 1,
            _ => 0,
        })
    }
}
impl Terminal {
    /// Apply a native gesture to one view. Returns whether its history viewport moved.
    pub fn select_view(&mut self, view_id: &str, event: SelectionEvent) -> Result<bool> {
        if !event.x.is_finite() || !event.y.is_finite() || event.modifiers & !15 != 0 {
            return Err(Error("invalid selection coordinates or modifiers".into()));
        }
        self.selections.reconcile(&self.native)?;
        // Empty-state output/resize paths deliberately avoid native screen
        // queries. The first real press establishes its own current screen.
        if event.action == SelectionAction::Press && self.selections.views.is_empty() {
            self.selections.screen = self.native.active_screen()?;
        }
        let before = self.view_scrollback_offset(view_id)?;
        if event.action == SelectionAction::Tick
            && self.selections.direction(&self.native, view_id)? == 0
        {
            return Ok(false);
        }
        let scrollbar = self.native.scrollbar()?;
        let row = scrollbar
            .total
            .saturating_sub(scrollbar.len)
            .saturating_sub(before);
        let projected = row != scrollbar.offset;
        if projected {
            self.native
                .scroll_viewport(ScrollViewport::Row(row as usize));
        }
        let result = self
            .selections
            .apply(&mut self.native, self.size, view_id, event);
        // Native autoscroll moves exactly one row itself. Capture that position
        // as this view's existing history anchor before restoring canonical live.
        let after = if event.action == SelectionAction::Tick {
            self.native.scrollbar().map(|bar| {
                (
                    bar.total.saturating_sub(bar.len).saturating_sub(bar.offset),
                    bar.offset,
                )
            })
        } else {
            Ok((before, row))
        };
        if !after
            .as_ref()
            .is_ok_and(|(_, offset)| *offset == scrollbar.offset)
        {
            self.native
                .scroll_viewport(if scrollbar.offset + scrollbar.len >= scrollbar.total {
                    ScrollViewport::Bottom
                } else {
                    ScrollViewport::Row(scrollbar.offset as usize)
                });
        }
        result?;
        let (after, _) = after?;
        if event.action == SelectionAction::Tick && self.native.active_screen()? == Screen::Primary
        {
            let delta = i64::try_from(after)
                .map_err(|_| Error("selection history offset overflow".into()))?
                - i64::try_from(before)
                    .map_err(|_| Error("selection history offset overflow".into()))?;
            self.scroll_view(view_id, delta)?;
        }
        Ok(before != self.view_scrollback_offset(view_id)?)
    }
    /// Project native endpoints into bounded row spans without installing a
    /// terminal-global selection or touching canonical render resources.
    pub fn view_selection(&mut self, view_id: &str) -> Result<Option<SelectionSnapshot>> {
        if !self.selections.views.contains_key(view_id) {
            return Ok(None);
        }
        self.selections.reconcile(&self.native)?;
        let Some(view) = self.selections.views.get(view_id) else {
            return Ok(None);
        };
        let Some(ends) = &view.endpoints else {
            return Ok(Some(SelectionSnapshot {
                active: false,
                dragging: view.dragging,
                rectangle: false,
                spans: Vec::new(),
                autoscroll: self.selections.direction(&self.native, view_id)?,
            }));
        };
        let Some(selection) = ends.snapshot(&self.native)? else {
            return Ok(None);
        };
        let ordered = selection.to_ordered(&self.native, sdk::selection::Order::Forward)?;
        let Some(start) = self
            .native
            .point_from_grid_ref(&ordered.start(), PointSpace::Screen)?
        else {
            return Ok(None);
        };
        let Some(end) = self
            .native
            .point_from_grid_ref(&ordered.end(), PointSpace::Screen)?
        else {
            return Ok(None);
        };
        let scrollbar = self.native.scrollbar()?;
        let top = scrollbar
            .total
            .saturating_sub(scrollbar.len)
            .saturating_sub(self.view_scrollback_offset(view_id)?);
        let mut spans = Vec::with_capacity(usize::from(self.size.rows));
        for row in 0..self.size.rows {
            let screen_y = top + u64::from(row);
            if screen_y < u64::from(start.y) || screen_y > u64::from(end.y) {
                continue;
            }
            let (left, right) = if ends.rectangle {
                (start.x.min(end.x), start.x.max(end.x).saturating_add(1))
            } else {
                (
                    if screen_y == u64::from(start.y) {
                        start.x
                    } else {
                        0
                    },
                    if screen_y == u64::from(end.y) {
                        end.x.saturating_add(1)
                    } else {
                        self.size.cols
                    },
                )
            };
            let left = left.min(self.size.cols);
            let right = right.min(self.size.cols);
            if left < right {
                spans.push(SelectionSpan {
                    row,
                    start: left,
                    end: right,
                });
            }
        }
        Ok(Some(SelectionSnapshot {
            active: true,
            dragging: view.dragging,
            rectangle: ends.rectangle,
            spans,
            autoscroll: self.selections.direction(&self.native, view_id)?,
        }))
    }
    /// Format the explicit native selection. Query the required length before
    /// allocating any terminal text, so oversized copies are errors, not truncation.
    pub fn copy_view_selection(&mut self, view_id: &str) -> Result<Option<Vec<u8>>> {
        self.selections.reconcile(&self.native)?;
        let Some(ends) = self
            .selections
            .views
            .get(view_id)
            .and_then(|view| view.endpoints.as_ref())
        else {
            return Ok(None);
        };
        let Some(selection) = ends.snapshot(&self.native)? else {
            return Ok(None);
        };
        let options = || {
            FormatOptions::new()
                .with_emit_format(sdk::fmt::Format::Plain)
                .with_unwrap(true)
                .with_trim(true)
                .with_selection(&selection)
        };
        let len = match self.native.format_selection_buf(options(), &mut []) {
            Ok(len) => len.unwrap_or(0),
            Err(sdk::Error::OutOfSpace { required }) => required,
            Err(error) => return Err(error.into()),
        };
        if len > MAX_INPUT_BYTES {
            return Err(Error("selection copy payload limit".into()));
        }
        if len == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0; len];
        let written = self
            .native
            .format_selection_buf(options(), &mut bytes)?
            .unwrap_or(0);
        bytes.truncate(written);
        Ok((!bytes.is_empty()).then_some(bytes))
    }
    /// Whether any view owns native gesture/endpoints; no native query or allocation.
    pub fn has_view_selections(&self) -> bool {
        !self.selections.views.is_empty()
    }
    pub fn retain_selection_views(&mut self, attached_visible: impl FnMut(&str) -> bool) {
        self.selections.retain(&self.native, attached_visible);
    }
    /// Reclaim gesture-owned native pins before the native terminal is dropped.
    pub(crate) fn clear_view_selections(&mut self) {
        self.selections.retain(&self.native, |_| false);
    }
}
