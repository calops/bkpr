//! Retained row navigation shared by application-owned tree models.
//! Row identity and hierarchy remain caller policy; this module owns only
//! selection, viewport reveal, scrolling, hover and click sequencing.
use std::time::{Duration, Instant};

/// Stable selection identity supplied by an application-owned row.
pub trait NavigationRow<T> {
    fn target(&self) -> Option<&T>;
}
pub struct Navigation<T, R> {
    /// Caller-requested viewport width retained across temporary constraints.
    pub preferred_columns: Option<u16>,
    pub active: bool,
    pub cursor: Option<T>,
    pub scroll: usize,
    pub rows: Vec<R>,
    pub hover: Option<T>,
    pub pointer_row: Option<u16>,
    last_click: Option<(T, Instant)>,
    height: usize,
    reveal: bool,
}
impl<T, R> Default for Navigation<T, R> {
    fn default() -> Self {
        Self {
            preferred_columns: None,
            active: false,
            cursor: None,
            scroll: 0,
            rows: Vec::new(),
            hover: None,
            pointer_row: None,
            last_click: None,
            height: 0,
            reveal: false,
        }
    }
}
impl<T: Clone + Eq, R: NavigationRow<T>> Navigation<T, R> {
    pub fn request_reveal(&mut self) {
        self.reveal = true;
    }
    pub fn viewport(&mut self, height: usize) {
        self.reveal |= height != self.height;
        self.height = height;
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(height));
        if self.reveal && height > 0 {
            if let Some(first) = self
                .rows
                .iter()
                .position(|row| row.target() == self.cursor.as_ref() && row.target().is_some())
            {
                let last = self
                    .rows
                    .iter()
                    .rposition(|row| row.target() == self.cursor.as_ref())
                    .unwrap_or(first);
                if first < self.scroll {
                    self.scroll = first;
                } else if last >= self.scroll + height {
                    self.scroll = (last + 1).saturating_sub(height).min(first);
                }
            }
            self.reveal = false;
        }
    }
    pub fn viewport_rows(&self) -> usize {
        self.height
    }
    pub fn refresh_viewport(&mut self) {
        self.viewport(self.height);
    }
    pub fn enter(&mut self) {
        self.active = true;
        self.reveal = true;
    }
    pub fn select(&mut self, target: T) {
        self.cursor = Some(target);
        self.reveal = true;
    }
    pub fn click(&mut self, target: T, now: Instant) -> bool {
        let double = self.last_click.as_ref().is_some_and(|(previous, at)| {
            previous == &target && now.saturating_duration_since(*at) < Duration::from_millis(350)
        });
        self.select(target.clone());
        self.last_click = if double { None } else { Some((target, now)) };
        double
    }
    pub fn hover(&mut self, target: Option<&T>) {
        if self.hover.as_ref() != target {
            self.hover = target.cloned();
        }
    }
    pub fn navigate(&mut self, delta: isize) {
        self.hover = None;
        self.pointer_row = None;
        self.last_click = None;
        let mut items = self.rows.iter().filter_map(|row| row.target()).peekable();
        let mut index = 0isize;
        let mut selected = 0isize;
        let mut count = 0isize;
        while let Some(item) = items.next() {
            while items.peek().is_some_and(|next| *next == item) {
                items.next();
            }
            if Some(item) == self.cursor.as_ref() {
                selected = index;
            }
            index += 1;
            count += 1;
        }
        if count == 0 {
            return;
        }
        let target = selected
            .saturating_add(delta)
            .clamp(0, count.saturating_sub(1));
        let mut previous = None;
        let mut index = 0;
        for row in &self.rows {
            if let Some(item) = row.target() {
                if previous == Some(item) {
                    continue;
                }
                if index == target {
                    self.cursor = Some(item.clone());
                    self.reveal = true;
                    break;
                }
                previous = Some(item);
                index += 1;
            }
        }
        self.viewport(self.height);
    }
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(self.height));
        self.reveal = false;
    }
}
