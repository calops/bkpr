//! Bkpr's terminal-derived visual policy. The framework stores resolved styles only.
use bkpr_protocol::{ColorSource, Size, Style};
use bkpr_tui_framework::{
    ColorProbe, Role, Theme, ThemeStyles, decoration::DecorationStyle, default_style,
};

/// Native PTYs and pointer coordinates use the same gutter in rich and plain modes.
pub(crate) const PANE_BORDER: u16 = 1;

/// Focused local dialogs dim lower surfaces; passive hints never do.
pub(crate) const MODAL_BACKGROUND_FAINT: bool = true;

/// Material policy from main 9ad93bd and its retained fork d7b40a37:
/// default compact frames anchor the opening at the native content boundary;
/// their placement translates horizontal edges to equalize physical spacing.
/// depth.rs uses sigma=.30cw, offset=.35cw, opacity=.72 and a .12cw facet.
/// raised.rs uses the inverse mask at .45 opacity; forms remain shadow-only.
/// The sidebar itself is a plain matte surface, not a decorated window.
pub(crate) fn decoration_styles(host: &ColorProbe, size: Size) -> Option<[DecorationStyle; 2]> {
    use bkpr_tui_framework::decoration::DecorationKind;
    if !host.truecolor
        || !(2..=512).contains(&size.cell_width)
        || !(2..=512).contains(&size.cell_height)
    {
        return None;
    }
    let (fg, bg) = (host.foreground?, host.background?);
    let cw = size.cell_width;
    let ch = size.cell_height;
    let thickness = cw.div_ceil(2).min(ch - 1);
    let popup_radius = (cw * 2 - thickness).min(ch - thickness);
    let pane_radius = (cw * 3 / 2).min(2 * cw).min(2 * ch);
    let offset = ((luminance(fg) - luminance(bg)) * 255.0 * 0.06).round() as i16;
    let surface = bg.map(|channel| (i16::from(channel) - offset).clamp(0, 255) as u8);
    let sigma_milli = (cw * 300).max(750);
    let support = (cw * 350 + sigma_milli * 5 + 1000).div_ceil(1000);
    let padding = [support.div_ceil(cw) * cw, support.div_ceil(ch) * ch];
    let recessed = DecorationStyle {
        kind: DecorationKind::Recessed,
        background: bg,
        surface,
        foreground: fg,
        inset_px: [cw, ch],
        outset_px: [0, 0],
        radius_px: pane_radius,
        sigma_milli,
        offset_milli: [(cw * 350) as i32; 2],
        shadow_opacity_milli: 720,
        lip_width_milli: (cw * 120).clamp(750, 2000),
        lip_highlight_milli: 240,
        lip_shade_milli: 240,
    };
    Some([
        recessed,
        DecorationStyle {
            kind: DecorationKind::Raised,
            // The supplied rect is the popup opening, not its shadow bounds.
            inset_px: padding,
            outset_px: padding,
            radius_px: popup_radius,
            shadow_opacity_milli: 450,
            lip_width_milli: 0,
            lip_highlight_milli: 0,
            lip_shade_milli: 0,
            ..recessed
        },
    ])
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

struct Recipe<'a> {
    evidence: &'a ColorProbe,
    styles: [Style; 15],
    selected: [Style; 15],
    panels: [Style; 15],
}
impl Recipe<'_> {
    fn defaults(&self) -> ([u8; 3], [u8; 3]) {
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
            style.foreground = self
                .evidence
                .palette
                .get(&usize::from(index))
                .copied()
                .unwrap_or(fg);
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
            for style in &mut self.styles {
                style.background = surface;
                style.background_source = ColorSource::Rgb(surface);
            }
            self.styles[Role::Panel as usize].background = bg;
            self.styles[Role::Panel as usize].background_source = ColorSource::Rgb(bg);
            for (role, percent) in [(Role::Muted, 65), (Role::Border, 65), (Role::Title, 88)] {
                let color = blend(fg, surface, percent);
                let style = &mut self.styles[role as usize];
                style.foreground = color;
                style.foreground_source = ColorSource::Rgb(color);
                style.faint = false;
            }
            for (role, percent) in [(Role::Selected, 12), (Role::Hover, 6)] {
                let color = blend(fg, surface, percent);
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
                if let Some(color) = self.evidence.palette.get(&index).copied() {
                    let color = blend(color, surface, 75);
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
        }
        self.panels = self.styles.clone();
        for role in ROLES {
            if matches!(role, Role::Selected | Role::Hover | Role::Panel) {
                continue;
            }
            let style = &mut self.panels[role as usize];
            style.background = self.styles[Role::Panel as usize].background;
            style.background_source = self.styles[Role::Panel as usize].background_source;
        }
    }
}
pub(crate) fn update_theme(theme: &mut Theme, host: &ColorProbe) -> bool {
    theme.update(host, 0, theme_styles)
}

pub(crate) fn update_guide_theme(theme: &mut Theme, host: &ColorProbe, accent: Option<u8>) -> bool {
    theme.update(
        host,
        accent.map_or(1, |index| 2 + u64::from(index)),
        |evidence| {
            let mut styles = theme_styles(evidence);
            if let Some(background) = guide_background(evidence, accent) {
                for style in styles
                    .base
                    .iter_mut()
                    .chain(&mut styles.selected)
                    .chain(&mut styles.panel)
                {
                    style.background = background;
                    style.background_source = ColorSource::Rgb(background);
                }
            }
            styles
        },
    )
}

pub(crate) fn guide_background(host: &ColorProbe, accent: Option<u8>) -> Option<[u8; 3]> {
    if !host.truecolor {
        return None;
    }
    let background = host.background?;
    Some(
        accent
            .and_then(|index| host.palette.get(&usize::from(index)))
            .map_or(background, |&color| mode_tint(background, color)),
    )
}

fn theme_styles(evidence: &ColorProbe) -> ThemeStyles {
    let mut recipe = Recipe {
        evidence,
        styles: std::array::from_fn(|_| default_style()),
        selected: std::array::from_fn(|_| default_style()),
        panels: std::array::from_fn(|_| default_style()),
    };
    recipe.rebuild();
    ThemeStyles {
        base: recipe.styles,
        selected: recipe.selected,
        panel: recipe.panels,
    }
}

// Fork d7b40a37 output/recessed_frames/colors.rs:32–56.
// Zellij's MIT notice is retained in the framework decoration module.
fn mode_tint(background: [u8; 3], accent: [u8; 3]) -> [u8; 3] {
    let base = background.map(linear);
    let accent = accent.map(linear);
    let light = linear_luminance(base);
    let accent_light = linear_luminance(accent);
    if light == 0.0 || accent_light == 0.0 {
        return background;
    }
    let target = accent.map(|channel| channel * light / accent_light);
    let mut amount: f64 = 0.40;
    for channel in 0..3 {
        let delta = target[channel] - base[channel];
        if delta > 0.0 {
            amount = amount.min((1.0 - base[channel]) / delta);
        }
    }
    std::array::from_fn(|channel| {
        let value = base[channel] + amount * (target[channel] - base[channel]);
        let value = if value <= 0.0031308 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (value * 255.0).round().clamp(0.0, 255.0) as u8
    })
}
fn blend(fg: [u8; 3], bg: [u8; 3], percent: u16) -> [u8; 3] {
    std::array::from_fn(|index| {
        ((u16::from(fg[index]) * percent + u16::from(bg[index]) * (100 - percent) + 50) / 100) as u8
    })
}
fn linear(channel: u8) -> f64 {
    let value = f64::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn linear_luminance(rgb: [f64; 3]) -> f64 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}
fn luminance(color: [u8; 3]) -> f64 {
    linear_luminance(color.map(linear))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_panes_use_native_content_insets_without_changing_popup_material() {
        let host = ColorProbe {
            truecolor: true,
            foreground: Some([230; 3]),
            background: Some([30; 3]),
            ..ColorProbe::default()
        };
        for (cw, ch) in [(8, 16), (9, 20), (14, 9)] {
            let styles = decoration_styles(
                &host,
                Size {
                    cols: 80,
                    rows: 24,
                    cell_width: cw,
                    cell_height: ch,
                },
            )
            .unwrap();
            assert_eq!(styles[0].inset_px, [cw, ch]);
            assert_eq!(styles[0].radius_px, (cw * 3 / 2).min(2 * ch));
            let thickness = cw.div_ceil(2).min(ch - 1);
            assert_eq!(
                styles[1].radius_px,
                (cw * 2 - thickness).min(ch - thickness)
            );
            assert_eq!(styles[1].inset_px, styles[1].outset_px);
            assert_eq!(styles[1].shadow_opacity_milli, 450);
        }
    }

    #[test]
    fn static_material_requires_observed_defaults_and_truecolor() {
        let size = Size {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut host = ColorProbe::default();
        assert!(decoration_styles(&host, size).is_none());
        host.background = Some([20, 30, 40]);
        host.foreground = Some([230, 220, 210]);
        assert!(decoration_styles(&host, size).is_none());
        host.truecolor = true;
        let styles = decoration_styles(&host, size).unwrap();
        assert_ne!(styles[0], styles[1]);
        host.background = Some([235, 225, 215]);
        assert_ne!(styles, decoration_styles(&host, size).unwrap());
        host.background = None;
        assert!(decoration_styles(&host, size).is_none());
    }
    #[test]
    fn partial_evidence_and_capability_loss_retain_native_sources() {
        let mut probe = ColorProbe::default();
        let mut theme = Theme::default();
        probe.feed(b"10;rgb:eeee/eeee/eeee");
        probe.feed(b"11;rgb:1818/1818/1818");
        update_theme(&mut theme, &probe);
        assert_eq!(
            theme.style(Role::Base).background_source,
            ColorSource::Default
        );
        assert!(theme.style(Role::Selected).inverse);
        probe.feed(b"1+r5463");
        assert!(update_theme(&mut theme, &probe));
        assert!(matches!(
            theme.style(Role::Base).background_source,
            ColorSource::Rgb(_)
        ));
        assert!(!theme.style(Role::Selected).inverse);
        assert!(!update_theme(&mut theme, &probe));
        probe.truecolor = false;
        assert!(update_theme(&mut theme, &probe));
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
    fn svg_ink_tracks_native_inverse_and_derived_selection() {
        let mut theme = Theme::default();
        update_theme(&mut theme, &ColorProbe::default());
        assert_eq!(theme.effective_ink(theme.style(Role::Selected)), None);
        let mut host = ColorProbe {
            foreground: Some([230, 220, 210]),
            background: Some([30, 40, 50]),
            ..ColorProbe::default()
        };
        update_theme(&mut theme, &host);
        assert_eq!(
            theme.effective_ink(theme.style(Role::Selected)),
            Some(ColorSource::Rgb([30, 40, 50]))
        );
        host.truecolor = true;
        update_theme(&mut theme, &host);
        let selected = theme.style(Role::Selected);
        assert!(!selected.inverse);
        assert_eq!(
            theme.effective_ink(selected),
            Some(selected.foreground_source)
        );
    }
    #[test]
    fn guide_mode_background_changes_without_terminal_color_change_and_restores() {
        use bkpr_tui_framework::UiSurface;
        let mut host = ColorProbe {
            truecolor: true,
            foreground: Some([230; 3]),
            background: Some([32, 36, 44]),
            ..ColorProbe::default()
        };
        host.palette.insert(4, [45, 100, 210]);
        host.palette.insert(3, [220, 180, 60]);
        let mut theme = Theme::default();
        let mut surface = UiSurface::new(Size {
            cols: 12,
            rows: 2,
            cell_width: 9,
            cell_height: 18,
        });
        let base = host.background.unwrap();
        let mut colors = Vec::new();
        for accent in [Some(4), Some(3), None, Some(4)] {
            update_guide_theme(&mut theme, &host, accent);
            surface.apply_theme(&theme, Role::Panel, &[]);
            let style = &surface.frame.rows[0].cells[0].style;
            assert_eq!(style.background_source, ColorSource::Rgb(style.background));
            // Half an encoded eight-bit step times the maximum sRGB slope.
            assert!((luminance(style.background) - luminance(base)).abs() <= 1.2 / 255.0);
            colors.push(style.background);
        }
        assert!(colors[0][2] > colors[0][0]);
        assert!(colors[1][0] > colors[1][2]);
        assert_eq!(colors[2], base);
        assert_eq!(colors[3], colors[0]);
        host.palette.clear();
        update_guide_theme(&mut theme, &host, Some(4));
        surface.apply_theme(&theme, Role::Panel, &[]);
        assert_eq!(surface.frame.rows[0].cells[0].style.background, base);
        host.truecolor = false;
        update_guide_theme(&mut theme, &host, Some(3));
        surface.apply_theme(&theme, Role::Panel, &[]);
        assert_eq!(
            surface.frame.rows[0].cells[0].style.background_source,
            ColorSource::Default
        );
    }
}
