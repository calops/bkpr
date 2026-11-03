//! Retained trusted UI text scenes; physical output remains with SceneRenderer.
use crate::{Rect, Role, Theme, blank, clear, text, window};
use bkpr_protocol::{ColorSource, Cursor, Frame, Size};

/// Ordered surface-local semantic regions. Later spans take precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleSpan {
    pub rect: Rect,
    pub role: Role,
}

/// Ordered surface-local background overrides. Selection and hover retain their
/// semantic backgrounds; later spans win for ordinary cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundSpan {
    pub rect: Rect,
    pub background: ColorSource,
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
    backgrounds: Vec<BackgroundSpan>,
    source: Vec<Intent>,
    applied: Vec<Intent>,
    roles: Vec<Role>,
}

/// Text changes clear old bounds and repaint widget lines, not a fresh screen.
/// Decorations contain geometry only, never application layout or pane identities.
pub struct UiSurface {
    pub frame: Frame,
    lines: Vec<window::TextLine>,
    theme: Option<ThemeCache>,
}
impl UiSurface {
    pub fn new(size: Size) -> Self {
        Self {
            frame: blank(size),
            lines: Vec::new(),
            theme: None,
        }
    }
    pub fn paint(&mut self, size: Size, lines: Vec<window::TextLine>, cursor: Option<Cursor>) {
        let resized = self.frame.size != size;
        let repaint = resized || self.lines != lines;
        if repaint {
            self.restore_theme();
        }
        if self.frame.size != size {
            self.frame = blank(size);
            self.lines.clear();
        }
        if self.lines != lines {
            for (rect, _, _) in self.lines.iter().chain(&lines) {
                clear(&mut self.frame, *rect);
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
        self.apply_theme_with_backgrounds(theme, base, spans, &[])
    }

    /// Apply background-only materials after resolving semantic roles. Identical
    /// theme, role and background inputs reuse the retained cell styling.
    pub fn apply_theme_with_backgrounds(
        &mut self,
        theme: &Theme,
        base: Role,
        spans: &[RoleSpan],
        backgrounds: &[BackgroundSpan],
    ) -> bool {
        if self.theme.as_ref().is_some_and(|cache| {
            !cache.source.is_empty()
                && cache.revision == theme.revision()
                && cache.base == base
                && cache.spans == spans
                && cache.backgrounds == backgrounds
        }) {
            return false;
        }
        let mut cache = self.theme.take().unwrap_or_else(|| ThemeCache {
            revision: theme.revision(),
            base,
            spans: Vec::new(),
            backgrounds: Vec::new(),
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
        for span in backgrounds {
            let rect = span.rect;
            let color = theme.resolve_background(span.background);
            for y in rect.y..rect.y.saturating_add(rect.height).min(self.frame.size.rows) {
                let start = usize::from(y) * usize::from(self.frame.size.cols);
                let cells = &mut self.frame.rows[usize::from(y)].cells;
                for x in rect.x..rect.x.saturating_add(rect.width).min(self.frame.size.cols) {
                    let index = start + usize::from(x);
                    if cache.source[index].inverse
                        || matches!(cache.roles[index], Role::Selected | Role::Hover)
                    {
                        continue;
                    }
                    let cell = &mut cells[usize::from(x)];
                    cell.style.background = color;
                    cell.style.background_source = span.background;
                }
            }
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
        cache.backgrounds.clear();
        cache.backgrounds.extend_from_slice(backgrounds);
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
    // Deliberately tiny caller policy: these tests exercise retention, not an
    // application's palette/contrast recipe.
    fn policy(host: &ColorProbe) -> crate::ThemeStyles {
        let mut base = std::array::from_fn(|_| crate::default_style());
        base[Role::Error as usize].foreground_source = ColorSource::Palette(1);
        base[Role::Muted as usize].faint = true;
        let mut selected = base.clone();
        for style in &mut selected {
            style.inverse = !host.truecolor;
            if host.truecolor {
                style.background = host.background.unwrap();
                style.background_source = ColorSource::Rgb(style.background);
            }
        }
        crate::ThemeStyles {
            panel: base.clone(),
            base,
            selected,
        }
    }
    #[test]
    fn switching_between_independent_themes_restyles_an_unchanged_surface() {
        let make_theme = |background| {
            let mut theme = Theme::default();
            theme.update(&ColorProbe::default(), 0, |_| {
                let mut style = crate::default_style();
                style.background = background;
                style.background_source = ColorSource::Rgb(background);
                crate::ThemeStyles {
                    base: std::array::from_fn(|_| style.clone()),
                    selected: std::array::from_fn(|_| style.clone()),
                    panel: std::array::from_fn(|_| style.clone()),
                }
            });
            theme
        };
        let normal = make_theme([20, 30, 40]);
        let raised = make_theme([70, 80, 90]);
        let mut surface = UiSurface::new(size());
        surface.paint(
            size(),
            vec![(
                Rect {
                    width: 5,
                    height: 1,
                    ..Rect::default()
                },
                "draft".into(),
                false,
            )],
            None,
        );
        for theme in [&normal, &raised, &normal] {
            surface.apply_theme(theme, Role::Panel, &[]);
            assert_eq!(
                surface.frame.rows[0].cells[0].style.background_source,
                theme.style(Role::Panel).background_source
            );
        }
    }
    #[test]
    fn background_spans_preserve_selection_roles_ink_attributes_and_retained_styling() {
        let mut surface = UiSurface::new(size());
        surface.paint(
            size(),
            vec![(
                Rect {
                    x: 0,
                    y: 0,
                    width: 5,
                    height: 1,
                },
                "abcde".into(),
                false,
            )],
            None,
        );
        surface.frame.rows[0].cells[0].style.bold = true;
        surface.frame.rows[0].cells[0].style.underline = 1;
        surface.frame.rows[0].cells[1].style.inverse = true;
        let mut host = ColorProbe {
            truecolor: true,
            foreground: Some([230; 3]),
            background: Some([20; 3]),
            ..ColorProbe::default()
        };
        host.palette.insert(6, [30, 90, 120]);
        let mut theme = Theme::default();
        theme.update(&host, 0, |host| {
            let mut styles = policy(host);
            for table in [&mut styles.base, &mut styles.panel] {
                for (role, color) in [(Role::Selected, [40; 3]), (Role::Hover, [60; 3])] {
                    table[role as usize].background = color;
                    table[role as usize].background_source = ColorSource::Rgb(color);
                }
            }
            styles
        });
        let roles = [
            RoleSpan {
                rect: Rect {
                    x: 2,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                role: Role::Selected,
            },
            RoleSpan {
                rect: Rect {
                    x: 3,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                role: Role::Hover,
            },
            RoleSpan {
                rect: Rect {
                    x: 4,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                role: Role::Error,
            },
        ];
        let mut backgrounds = [BackgroundSpan {
            rect: Rect {
                x: 0,
                y: 0,
                width: u16::MAX,
                height: u16::MAX,
            },
            background: ColorSource::Palette(6),
        }];
        assert!(surface.apply_theme_with_backgrounds(&theme, Role::Panel, &roles, &backgrounds));
        let row = &surface.frame.rows[0].cells;
        assert_eq!(row[0].style.background, [30, 90, 120]);
        assert_eq!(row[0].style.background_source, ColorSource::Palette(6));
        assert!(row[0].style.bold);
        assert_eq!(row[0].style.underline, 1);
        assert_eq!(row[1].style.background, [20; 3]);
        assert_eq!(row[2].style.background, [40; 3]);
        assert_eq!(row[3].style.background, [60; 3]);
        assert_eq!(row[4].style.foreground_source, ColorSource::Palette(1));
        assert_eq!(row[4].style.background_source, ColorSource::Palette(6));
        assert!(!surface.apply_theme_with_backgrounds(&theme, Role::Panel, &roles, &backgrounds));

        backgrounds[0].background = ColorSource::Rgb([80, 70, 60]);
        assert!(surface.apply_theme_with_backgrounds(&theme, Role::Panel, &roles, &backgrounds));
        assert_eq!(
            surface.frame.rows[0].cells[0].style.background,
            [80, 70, 60]
        );
        assert_eq!(surface.frame.rows[0].cells[1].style.background, [20; 3]);
        backgrounds[0].background = ColorSource::Default;
        assert!(surface.apply_theme_with_backgrounds(&theme, Role::Panel, &roles, &backgrounds));
        assert_eq!(surface.frame.rows[0].cells[0].style.background, [20; 3]);
        assert_eq!(
            surface.frame.rows[0].cells[0].style.background_source,
            ColorSource::Default
        );
        assert!(surface.apply_theme(&theme, Role::Panel, &roles));
        assert!(!surface.apply_theme(&theme, Role::Panel, &roles));
    }

    #[test]
    fn selection_and_faint_intent_survive_repeated_theme_changes_and_loss() {
        let mut surface = UiSurface::new(size());
        surface.paint(size(), selected(), None);
        surface.frame.rows[2].cells[0].style.faint = true;
        let mut theme = Theme::default();
        theme.update(&ColorProbe::default(), 0, policy);
        surface.apply_theme(&theme, Role::Base, &[]);
        assert!(surface.frame.rows[1].cells[0].style.inverse);
        assert!(surface.frame.rows[2].cells[0].style.faint);
        let mut host = ColorProbe {
            truecolor: true,
            foreground: Some([235, 235, 235]),
            background: Some([20, 20, 20]),
            ..ColorProbe::default()
        };
        theme.update(&host, 0, policy);
        surface.apply_theme(&theme, Role::Base, &[]);
        assert!(!surface.frame.rows[1].cells[0].style.inverse);
        let first = surface.frame.rows[1].cells[0].style.background;
        host.background = Some([40, 25, 70]);
        theme.update(&host, 0, policy);
        surface.apply_theme(&theme, Role::Base, &[]);
        assert_ne!(surface.frame.rows[1].cells[0].style.background, first);
        host.truecolor = false;
        theme.update(&host, 0, policy);
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
        let mut theme = Theme::default();
        theme.update(&ColorProbe::default(), 0, policy);
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
