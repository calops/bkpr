//! Terminal-derived UI styles, adapted from bkpr-zellij/src/palette.rs on main.
//! Keep native color provenance until explicit RGB capability and color evidence
//! permit derived surfaces. Application pane frames never pass through this API.
use crate::{ColorProbe, default_style};
use bkpr_protocol::{ColorSource, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Role {
    Base,
    Panel,
    Border,
    Muted,
    Title,
    Selected,
    Hover,
    Accent,
    Success,
    Warning,
    Error,
    Info,
    Secondary,
    BrightAccent,
    BrightInfo,
}
const ROLES: [Role; 15] = [
    Role::Base,
    Role::Panel,
    Role::Border,
    Role::Muted,
    Role::Title,
    Role::Selected,
    Role::Hover,
    Role::Accent,
    Role::Success,
    Role::Warning,
    Role::Error,
    Role::Info,
    Role::Secondary,
    Role::BrightAccent,
    Role::BrightInfo,
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Evidence {
    truecolor: bool,
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    ansi: [Option<[u8; 3]>; 16],
}

/// Cached semantic styles. No environment variable or terminal-name heuristics.
/// Updating cursor/graphics metadata does not invalidate UI colors.
pub struct Theme {
    evidence: Evidence,
    styles: [Style; 15],
    selected: [Style; 15],
    panels: [Style; 15],
    revision: u64,
}
impl Default for Theme {
    fn default() -> Self {
        let mut theme = Self {
            evidence: Evidence::default(),
            styles: std::array::from_fn(|_| default_style()),
            selected: std::array::from_fn(|_| default_style()),
            panels: std::array::from_fn(|_| default_style()),
            revision: 0,
        };
        theme.rebuild();
        theme
    }
}
impl Theme {
    pub fn update(&mut self, probe: &ColorProbe) -> bool {
        let evidence = Evidence {
            truecolor: probe.truecolor,
            foreground: probe.foreground,
            background: probe.background,
            ansi: std::array::from_fn(|index| probe.palette.get(&index).copied()),
        };
        if self.evidence == evidence {
            return false;
        }
        self.evidence = evidence;
        self.rebuild();
        self.revision = self.revision.wrapping_add(1);
        true
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn style(&self, role: Role) -> &Style {
        &self.styles[role as usize]
    }
    pub fn ink(&self, role: Role, selected: bool) -> ColorSource {
        let style = self.resolved(role, selected);
        if style.inverse {
            if style.background_source == ColorSource::Default {
                self.evidence
                    .background
                    .map_or(ColorSource::Default, ColorSource::Rgb)
            } else {
                style.background_source
            }
        } else {
            style.foreground_source
        }
    }
    pub fn background(&self, role: Role, selected: bool) -> ColorSource {
        let style = self.resolved(role, selected);
        if style.inverse {
            if style.foreground_source == ColorSource::Default {
                self.evidence
                    .foreground
                    .map_or(ColorSource::Default, ColorSource::Rgb)
            } else {
                style.foreground_source
            }
        } else {
            style.background_source
        }
    }
    /// Resolve the actual rendered marker ink, including native inverse colors.
    /// Unknown native background ink is intentionally unavailable to SVG callers.
    pub fn effective_ink(&self, style: &Style) -> Option<ColorSource> {
        let source = if style.inverse {
            style.background_source
        } else {
            style.foreground_source
        };
        if style.inverse && source == ColorSource::Default {
            self.evidence.background.map(ColorSource::Rgb)
        } else {
            Some(source)
        }
    }
    pub(crate) fn resolved(&self, role: Role, selected: bool) -> &Style {
        if selected {
            &self.selected[role as usize]
        } else {
            self.style(role)
        }
    }
    pub(crate) fn on_surface(&self, role: Role, selected: bool, base: Role) -> &Style {
        if selected {
            &self.selected[role as usize]
        } else if base == Role::Panel {
            &self.panels[role as usize]
        } else {
            self.style(role)
        }
    }
    pub(crate) fn defaults(&self) -> ([u8; 3], [u8; 3]) {
        let fallback = default_style();
        (
            self.evidence.foreground.unwrap_or(fallback.foreground),
            self.evidence.background.unwrap_or(fallback.background),
        )
    }
    fn rebuild(&mut self) {
        let (fg, bg) = self.defaults();
        self.styles = std::array::from_fn(|_| {
            let mut style = default_style();
            style.foreground = fg;
            style.background = bg;
            style.underline_color = fg;
            style
        });
        self.styles[Role::Border as usize].faint = true;
        self.styles[Role::Muted as usize].faint = true;
        self.styles[Role::Title as usize].bold = true;
        self.styles[Role::Selected as usize].inverse = true;
        self.styles[Role::Hover as usize].underline = 1;
        for (role, index) in [
            (Role::Accent, 4),
            (Role::Success, 2),
            (Role::Warning, 3),
            (Role::Error, 1),
            (Role::Info, 6),
            (Role::Secondary, 5),
            (Role::BrightAccent, 12),
            (Role::BrightInfo, 14),
        ] {
            let style = &mut self.styles[role as usize];
            style.foreground_source = ColorSource::Palette(index);
            // RGB is a resolution payload, not evidence about an unknown ANSI entry.
            style.foreground = self.evidence.ansi[index as usize].unwrap_or(fg);
        }
        if let (true, Some(fg), Some(bg)) = (
            self.evidence.truecolor,
            self.evidence.foreground,
            self.evidence.background,
        ) {
            // MAIN's signed six-percent shift moves every channel away from the
            // observed ink, without imposing a dark/light/chromatic classification.
            let offset = ((luminance(fg) - luminance(bg)) * 255.0 * 0.06).round() as i16;
            let surface = bg.map(|channel| (i16::from(channel) - offset).clamp(0, 255) as u8);
            let target = contrast(fg, surface).min(4.5);
            for style in &mut self.styles {
                style.background = surface;
                style.background_source = ColorSource::Rgb(surface);
            }
            self.styles[Role::Panel as usize].background = bg;
            self.styles[Role::Panel as usize].background_source = ColorSource::Rgb(bg);
            for (role, percent, minimum) in [
                (Role::Muted, 65, 3.0_f64),
                (Role::Border, 40, 1.8_f64),
                (Role::Title, 88, 4.5_f64),
            ] {
                let color = readable(
                    blend(fg, surface, percent),
                    surface,
                    fg,
                    minimum.min(target),
                );
                let style = &mut self.styles[role as usize];
                style.foreground = color;
                style.foreground_source = ColorSource::Rgb(color);
                style.faint = false;
            }
            for (role, percent) in [(Role::Selected, 12), (Role::Hover, 6)] {
                let mut color = blend(fg, surface, percent);
                if contrast(fg, color) < target {
                    color = readable_background(color, surface, fg, target);
                }
                let style = &mut self.styles[role as usize];
                style.background = color;
                style.background_source = ColorSource::Rgb(color);
                style.inverse = false;
                style.underline = 0;
            }
            for (role, index) in [
                (Role::Accent, 4),
                (Role::Success, 2),
                (Role::Warning, 3),
                (Role::Error, 1),
                (Role::Info, 6),
                (Role::Secondary, 5),
                (Role::BrightAccent, 12),
                (Role::BrightInfo, 14),
            ] {
                if let Some(color) = self.evidence.ansi[index] {
                    let color = readable(blend(color, surface, 75), surface, fg, target);
                    let style = &mut self.styles[role as usize];
                    style.foreground = color;
                    style.foreground_source = ColorSource::Rgb(color);
                }
            }
        }
        self.selected = self.styles.clone();
        for role in ROLES {
            let selection = &self.styles[Role::Selected as usize];
            let style = &mut self.selected[role as usize];
            style.background = selection.background;
            style.background_source = selection.background_source;
            style.inverse = selection.inverse;
            style.faint = false;
            if !selection.inverse {
                let fg = readable(
                    style.foreground,
                    selection.background,
                    fg,
                    contrast(fg, selection.background).min(4.5),
                );
                // Unknown ANSI colors cannot be contrast-checked against RGB.
                if matches!(style.foreground_source, ColorSource::Palette(_)) {
                    style.foreground = selection.foreground;
                    style.foreground_source = selection.foreground_source;
                } else if fg != style.foreground {
                    style.foreground = fg;
                    style.foreground_source = ColorSource::Rgb(fg);
                }
            }
        }
        self.panels = self.styles.clone();
        for role in ROLES {
            if matches!(role, Role::Selected | Role::Hover | Role::Panel) {
                continue;
            }
            let style = &mut self.panels[role as usize];
            style.background = self.styles[Role::Panel as usize].background;
            style.background_source = self.styles[Role::Panel as usize].background_source;
            if self.evidence.truecolor
                && self.evidence.foreground.is_some()
                && self.evidence.background.is_some()
                && !matches!(style.foreground_source, ColorSource::Palette(_))
            {
                let minimum = match role {
                    Role::Border => 1.8_f64,
                    Role::Muted => 3.0,
                    _ => 4.5,
                };
                let color = readable(
                    style.foreground,
                    style.background,
                    fg,
                    minimum.min(contrast(fg, style.background)),
                );
                if color != style.foreground {
                    style.foreground = color;
                    style.foreground_source = ColorSource::Rgb(color);
                }
            }
        }
    }
}
fn blend(fg: [u8; 3], bg: [u8; 3], percent: u16) -> [u8; 3] {
    std::array::from_fn(|index| {
        ((u16::from(fg[index]) * percent + u16::from(bg[index]) * (100 - percent) + 50) / 100) as u8
    })
}
fn luminance(color: [u8; 3]) -> f64 {
    let linear = color.map(|channel| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
}
fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
fn readable(color: [u8; 3], bg: [u8; 3], ink: [u8; 3], minimum: f64) -> [u8; 3] {
    if contrast(color, bg) >= minimum {
        return color;
    }
    for percent in (5..=100).step_by(5) {
        let next = blend(ink, color, percent);
        if contrast(next, bg) >= minimum {
            return next;
        }
    }
    ink
}
fn readable_background(color: [u8; 3], base: [u8; 3], ink: [u8; 3], minimum: f64) -> [u8; 3] {
    for percent in (5..=100).step_by(5) {
        let next = blend(base, color, percent);
        if contrast(ink, next) >= minimum {
            return next;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_evidence_and_capability_loss_retain_native_sources() {
        let mut probe = ColorProbe::default();
        let mut theme = Theme::default();
        probe.feed(b"10;rgb:eeee/eeee/eeee");
        probe.feed(b"11;rgb:1818/1818/1818");
        theme.update(&probe);
        assert_eq!(
            theme.style(Role::Base).background_source,
            ColorSource::Default
        );
        assert!(theme.style(Role::Selected).inverse);
        probe.feed(b"1+r5463");
        assert!(theme.update(&probe));
        assert!(matches!(
            theme.style(Role::Base).background_source,
            ColorSource::Rgb(_)
        ));
        assert!(!theme.style(Role::Selected).inverse);
        assert!(!theme.update(&probe));
        probe.truecolor = false;
        assert!(theme.update(&probe));
        assert_eq!(
            theme.style(Role::Base).background_source,
            ColorSource::Default
        );
        assert_eq!(
            theme.style(Role::Error).foreground_source,
            ColorSource::Palette(1)
        );
        assert!(theme.style(Role::Selected).inverse);
    }
    #[test]
    fn observed_light_dark_and_chromatic_themes_remain_readable() {
        for (fg, bg) in [
            ([235, 235, 235], [20, 20, 20]),
            ([25, 25, 25], [245, 245, 245]),
            ([245, 224, 192], [42, 18, 70]),
        ] {
            let mut probe = ColorProbe {
                truecolor: true,
                foreground: Some(fg),
                background: Some(bg),
                ..ColorProbe::default()
            };
            probe.palette.insert(1, bg);
            let mut theme = Theme::default();
            theme.update(&probe);
            for role in [
                Role::Base,
                Role::Panel,
                Role::Title,
                Role::Selected,
                Role::Error,
            ] {
                let style = theme.style(role);
                assert!(contrast(style.foreground, style.background) >= 4.5);
                let selected = theme.resolved(role, true);
                assert!(contrast(selected.foreground, selected.background) >= 4.5);
                let panel = theme.on_surface(role, false, Role::Panel);
                assert!(contrast(panel.foreground, panel.background) >= 4.5);
            }
            let muted = theme.style(Role::Muted);
            assert!(contrast(muted.foreground, muted.background) >= 3.0);
        }
    }
    #[test]
    fn svg_ink_tracks_native_inverse_and_derived_selection() {
        let mut theme = Theme::default();
        assert_eq!(theme.effective_ink(theme.style(Role::Selected)), None);
        let mut host = ColorProbe {
            foreground: Some([230, 220, 210]),
            background: Some([30, 40, 50]),
            ..ColorProbe::default()
        };
        theme.update(&host);
        assert_eq!(
            theme.effective_ink(theme.style(Role::Selected)),
            Some(ColorSource::Rgb([30, 40, 50]))
        );
        host.truecolor = true;
        theme.update(&host);
        let selected = theme.style(Role::Selected);
        assert!(!selected.inverse);
        assert_eq!(
            theme.effective_ink(selected),
            Some(selected.foreground_source)
        );
    }
}
