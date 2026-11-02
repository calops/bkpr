//! Retained trusted UI text and divider scenes; physical output remains with SceneRenderer.
use crate::{Rect, Role, Theme, blank, clear, text, window};
use bkpr_protocol::{Cursor, Frame, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divider {
    pub rect: Rect,
    pub vertical: bool,
}

/// Ordered surface-local semantic regions. Later spans take precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleSpan {
    pub rect: Rect,
    pub role: Role,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Intent {
    inverse: bool,
    faint: bool,
    bold: bool,
    underline: u8,
}
impl Intent {
    fn read(style: &bkpr_protocol::Style) -> Self {
        Self {
            inverse: style.inverse,
            faint: style.faint,
            bold: style.bold,
            underline: style.underline,
        }
    }
    fn restore(self, style: &mut bkpr_protocol::Style) {
        style.inverse = self.inverse;
        style.faint = self.faint;
        style.bold = self.bold;
        style.underline = self.underline;
    }
}

struct ThemeCache {
    revision: u64,
    base: Role,
    spans: Vec<RoleSpan>,
    source: Vec<Intent>,
    applied: Vec<Intent>,
    roles: Vec<Role>,
}

/// Text changes clear old bounds and repaint widget lines, not a fresh screen.
/// Decorations contain geometry only, never application layout or pane identities.
pub struct UiSurface {
    pub frame: Frame,
    lines: Vec<window::TextLine>,
    dividers: Vec<Divider>,
    theme: Option<ThemeCache>,
}
impl UiSurface {
    pub fn new(size: Size) -> Self {
        Self {
            frame: blank(size),
            lines: Vec::new(),
            dividers: Vec::new(),
            theme: None,
        }
    }
    pub fn paint(&mut self, size: Size, lines: Vec<window::TextLine>, cursor: Option<Cursor>) {
        self.paint_parts(size, lines, Vec::new(), cursor);
    }
    pub fn paint_parts(
        &mut self,
        size: Size,
        lines: Vec<window::TextLine>,
        dividers: Vec<Divider>,
        cursor: Option<Cursor>,
    ) {
        let resized = self.frame.size != size;
        let repaint = resized || self.lines != lines || self.dividers != dividers;
        if repaint {
            self.restore_theme();
        }
        if self.frame.size != size {
            self.frame = blank(size);
            self.lines.clear();
            self.dividers.clear();
        }
        if self.lines != lines || self.dividers != dividers {
            for (rect, _, _) in self.lines.iter().chain(&lines) {
                clear(&mut self.frame, *rect);
            }
            for divider in self.dividers.iter().chain(&dividers) {
                clear(&mut self.frame, divider.rect);
            }
            for (rect, label, selected) in &lines {
                let width = rect.width.min(size.cols.saturating_sub(rect.x));
                text(
                    &mut self.frame,
                    rect.x,
                    rect.y,
                    &window::fit(label, width),
                    *selected,
                );
            }
            for divider in &dividers {
                let rect = divider.rect;
                let width = rect.width.min(size.cols.saturating_sub(rect.x));
                let label = if divider.vertical { "│" } else { "─" }.repeat(usize::from(width));
                for y in rect.y..rect.y.saturating_add(rect.height).min(size.rows) {
                    text(&mut self.frame, rect.x, y, &label, false);
                    for cell in self.frame.rows[usize::from(y)]
                        .cells
                        .iter_mut()
                        .skip(usize::from(rect.x))
                        .take(usize::from(width))
                    {
                        cell.style.faint = true;
                    }
                }
            }
            self.dividers = dividers;
            self.lines = lines;
        }
        self.frame.cursor.visible = false;
        if let Some(cursor) = cursor {
            self.frame.cursor = cursor;
        }
    }
    /// Apply a terminal-derived style only to this trusted UI surface. Original
    /// selection/faint intent survives RGB styling and subsequent theme changes.
    /// Work is retained across identical paint/theme/role inputs; no scene copy
    /// or per-cell color derivation occurs on an unchanged frame.
    pub fn apply_theme(&mut self, theme: &Theme, base: Role, spans: &[RoleSpan]) -> bool {
        if self.theme.as_ref().is_some_and(|cache| {
            !cache.source.is_empty()
                && cache.revision == theme.revision()
                && cache.base == base
                && cache.spans == spans
        }) {
            return false;
        }
        let mut cache = self.theme.take().unwrap_or_else(|| ThemeCache {
            revision: theme.revision(),
            base,
            spans: Vec::new(),
            source: Vec::new(),
            applied: Vec::new(),
            roles: Vec::new(),
        });
        if cache.source.is_empty() {
            cache.source.extend(
                self.frame
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .map(|cell| Intent::read(&cell.style)),
            );
        }
        let count = usize::from(self.frame.size.cols) * usize::from(self.frame.size.rows);
        cache.roles.resize(count, base);
        cache.roles.fill(base);
        for span in spans {
            let rect = span.rect;
            for y in rect.y..rect.y.saturating_add(rect.height).min(self.frame.size.rows) {
                let start = usize::from(y) * usize::from(self.frame.size.cols);
                for x in rect.x..rect.x.saturating_add(rect.width).min(self.frame.size.cols) {
                    cache.roles[start + usize::from(x)] = span.role;
                }
            }
        }
        let (foreground, background) = theme.defaults();
        self.frame.foreground = foreground;
        self.frame.background = background;
        // Unknown native ANSI entries are not guessed RGB. Empty UI palette
        // preserves native sources through virtual-palette scene projection.
        self.frame.palette.clear();
        for (index, cell) in self
            .frame
            .rows
            .iter_mut()
            .flat_map(|row| &mut row.cells)
            .enumerate()
        {
            let source = &mut cache.source[index];
            if let Some(previous) = cache.applied.get(index) {
                // Match highlighting may alter attributes after a retained paint.
                // Keep such explicit widget attributes, not our previous styling.
                if cell.style.bold != previous.bold {
                    source.bold = cell.style.bold;
                }
                if cell.style.underline != previous.underline {
                    source.underline = cell.style.underline;
                }
            }
            let mut role = cache.roles[index];
            if source.faint && matches!(role, Role::Base | Role::Panel) {
                role = Role::Muted;
            }
            let style = theme.on_surface(role, source.inverse, base);
            cell.style.foreground = style.foreground;
            cell.style.background = style.background;
            cell.style.foreground_source = style.foreground_source;
            cell.style.background_source = style.background_source;
            cell.style.inverse = style.inverse;
            cell.style.faint = style.faint;
            cell.style.bold = source.bold || style.bold;
            cell.style.underline = source.underline.max(style.underline);
        }
        cache.applied.clear();
        cache.applied.extend(
            self.frame
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .map(|cell| Intent::read(&cell.style)),
        );
        cache.revision = theme.revision();
        cache.base = base;
        cache.spans.clear();
        cache.spans.extend_from_slice(spans);
        self.theme = Some(cache);
        true
    }

    fn restore_theme(&mut self) {
        if let Some(cache) = &mut self.theme {
            for (cell, source) in self
                .frame
                .rows
                .iter_mut()
                .flat_map(|row| &mut row.cells)
                .zip(&cache.source)
            {
                source.restore(&mut cell.style);
            }
            cache.source.clear();
            cache.applied.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColorProbe;
    use bkpr_protocol::ColorSource;
    fn size() -> Size {
        Size {
            cols: 5,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
        }
    }
    fn selected() -> Vec<window::TextLine> {
        vec![(
            Rect {
                x: 0,
                y: 1,
                width: 5,
                height: 1,
            },
            "row".into(),
            true,
        )]
    }
    #[test]
    fn selection_and_faint_intent_survive_repeated_theme_changes_and_loss() {
        let mut surface = UiSurface::new(size());
        surface.paint_parts(
            size(),
            selected(),
            vec![Divider {
                rect: Rect {
                    x: 0,
                    y: 2,
                    width: 5,
                    height: 1,
                },
                vertical: false,
            }],
            None,
        );
        let mut theme = Theme::default();
        surface.apply_theme(&theme, Role::Base, &[]);
        assert!(surface.frame.rows[1].cells[0].style.inverse);
        assert!(surface.frame.rows[2].cells[0].style.faint);
        let mut host = ColorProbe {
            truecolor: true,
            foreground: Some([235, 235, 235]),
            background: Some([20, 20, 20]),
            ..ColorProbe::default()
        };
        theme.update(&host);
        surface.apply_theme(&theme, Role::Base, &[]);
        assert!(!surface.frame.rows[1].cells[0].style.inverse);
        let first = surface.frame.rows[1].cells[0].style.background;
        host.background = Some([40, 25, 70]);
        theme.update(&host);
        surface.apply_theme(&theme, Role::Base, &[]);
        assert_ne!(surface.frame.rows[1].cells[0].style.background, first);
        host.truecolor = false;
        theme.update(&host);
        surface.apply_theme(&theme, Role::Base, &[]);
        assert!(surface.frame.rows[1].cells[0].style.inverse);
        assert!(surface.frame.rows[2].cells[0].style.faint);
        assert_eq!(
            surface.frame.rows[1].cells[0].style.background_source,
            ColorSource::Default
        );
        assert!(!surface.apply_theme(&theme, Role::Base, &[]));
    }
    #[test]
    fn repaint_clears_selection_and_clipped_roles_cover_blank_panel_cells() {
        let mut surface = UiSurface::new(size());
        let theme = Theme::default();
        surface.paint(size(), selected(), None);
        surface.apply_theme(&theme, Role::Panel, &[]);
        surface.paint(
            size(),
            vec![(
                Rect {
                    x: 0,
                    y: 1,
                    width: 5,
                    height: 1,
                },
                "new".into(),
                false,
            )],
            None,
        );
        let spans = [RoleSpan {
            rect: Rect {
                x: 3,
                y: 0,
                width: u16::MAX,
                height: u16::MAX,
            },
            role: Role::Error,
        }];
        surface.apply_theme(&theme, Role::Panel, &spans);
        assert!(!surface.frame.rows[1].cells[0].style.inverse);
        assert_eq!(
            surface.frame.rows[2].cells[4].style.foreground_source,
            ColorSource::Palette(1)
        );
        surface.frame.rows[1].cells[0].style.bold = true;
        let changed = [RoleSpan {
            rect: spans[0].rect,
            role: Role::Success,
        }];
        surface.apply_theme(&theme, Role::Panel, &changed);
        assert!(surface.frame.rows[1].cells[0].style.bold);
    }
}
