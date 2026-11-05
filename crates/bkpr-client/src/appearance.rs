//! Bkpr's terminal-derived visual policy. The framework stores resolved styles only.
use bkpr_protocol::{ColorSource, Size, Style};
use bkpr_tui_framework::{
    ColorProbe, Role, Theme, ThemeStyles, decoration::DecorationStyle, default_style,
};

/// Native PTYs and pointer coordinates use the same gutter in rich and plain modes.
pub(crate) const PANE_BORDER: u16 = 1;

/// Ordinary local dialogs dim lower surfaces; palette previews and passive hints do not.
pub(crate) const MODAL_BACKGROUND_FAINT: bool = true;

/// Material policy from main 9ad93bd and retained fork 18890b23:
/// compact frames anchor the opening at the native content boundary;
/// their placement translates horizontal edges to equalize physical spacing.
/// depth.rs uses sigma=.30cw, offset=.35cw, opacity=.72 and a .12cw facet.
/// Raised surfaces use the inverse shadow mask at .45 opacity and the same bevel.
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
    let surface = surface(fg, bg);
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
            kind: DecorationKind::RaisedSurface,
            // The supplied rect is the popup opening, not its shadow bounds.
            inset_px: padding,
            outset_px: padding,
            radius_px: popup_radius,
            shadow_opacity_milli: 450,
            ..recessed
        },
    ])
}

/// Physical passive-card attachment from retained fork 18890b23 joined.rs.
/// The bridge overlaps by one radius before the generic contour removes seams.
pub(crate) fn joined_surface(
    size: Size,
    popup: bkpr_tui_framework::Rect,
    item: bkpr_tui_framework::Rect,
    style: DecorationStyle,
) -> Option<bkpr_tui_framework::joined::JoinedSurface> {
    let cw = i32::try_from(size.cell_width).ok()?;
    let ch = i32::try_from(size.cell_height).ok()?;
    if !(2..=512).contains(&cw) || !(2..=512).contains(&ch) {
        return None;
    }
    let margin_x = cw - (cw + 1) / 2;
    let vertical_pad = ((cw + 3) / 4).min(ch / 8);
    let radius = i32::try_from(style.radius_px).ok()?;
    let popup_px = [
        i32::from(popup.x) * cw + margin_x,
        i32::from(popup.y) * ch + ch - vertical_pad,
        (i32::from(popup.x) + i32::from(popup.width)) * cw - margin_x,
        (i32::from(popup.y) + i32::from(popup.height)) * ch - ch + vertical_pad,
    ];
    if popup_px[2] - popup_px[0] < 2 * radius
        || popup_px[3] - popup_px[1] < 2 * radius
        || item.width == 0
        || item.height == 0
    {
        return None;
    }
    let mut item_px = [
        i32::from(item.x) * cw + margin_x,
        (i32::from(item.y) * ch - vertical_pad).max(0),
        (i32::from(item.x) + i32::from(item.width)) * cw - margin_x,
        (i32::from(item.y) + i32::from(item.height)) * ch + vertical_pad,
    ];
    if u32::from(item.x) + u32::from(item.width) <= u32::from(popup.x) + 1 {
        item_px[2] = popup_px[0] + radius;
    } else if u32::from(popup.x) + u32::from(popup.width) <= u32::from(item.x) + 1 {
        item_px[0] = popup_px[2] - radius;
    } else {
        return None;
    }
    Some(bkpr_tui_framework::joined::JoinedSurface {
        rects_px: [popup_px, item_px],
        style: DecorationStyle {
            inset_px: [0, 0],
            outset_px: [0, 0],
            ..style
        },
    })
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
            let surface = surface(fg, bg);
            for style in &mut self.styles {
                // UI palettes may differ from (or outlive missing) host defaults.
                style.foreground_source = ColorSource::Rgb(fg);
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

pub(crate) fn inactive_tree_guide_ink(theme: &Theme) -> ColorSource {
    match (
        theme.ink(Role::Base, false),
        theme.background(Role::Base, false),
    ) {
        (ColorSource::Rgb(foreground), ColorSource::Rgb(background)) => {
            ColorSource::Rgb(blend(foreground, background, 35))
        }
        _ => ColorSource::Palette(8),
    }
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

pub(crate) fn update_region_theme(
    theme: &mut Theme,
    host: &ColorProbe,
    accent: Option<u8>,
) -> bool {
    theme.update(
        host,
        accent.map_or(256, |index| 257 + u64::from(index)),
        |evidence| {
            let mut styles = theme_styles(evidence);
            if let Some(background) = region_background(evidence, accent) {
                for table in [&mut styles.base, &mut styles.panel] {
                    for role in ROLES {
                        if !matches!(role, Role::Selected | Role::Hover) {
                            let style = &mut table[role as usize];
                            style.background = background;
                            style.background_source = ColorSource::Rgb(background);
                        }
                    }
                }
            }
            styles
        },
    )
}

/// Raised region centers follow the fork's linear-light lift and stronger tint.
pub(crate) fn region_background(host: &ColorProbe, accent: Option<u8>) -> Option<[u8; 3]> {
    if !host.truecolor {
        return None;
    }
    let background = host.background?;
    let foreground = host.foreground?;
    let lifted = std::array::from_fn(|channel| {
        encoded(linear(background[channel]) * 0.94 + linear(foreground[channel]) * 0.06)
    });
    Some(
        accent
            .and_then(|index| host.palette.get(&usize::from(index)))
            .map_or(lifted, |&color| mode_tint(lifted, color, 0.65)),
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
            .map_or(background, |&color| mode_tint(background, color, 0.40)),
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

// Fork 18890b23 output/recessed_frames/colors.rs:24–56.
// Zellij's MIT notice is retained in the framework decoration module.
fn mode_tint(background: [u8; 3], accent: [u8; 3], mut amount: f64) -> [u8; 3] {
    let base = background.map(linear);
    let accent = accent.map(linear);
    let light = linear_luminance(base);
    let accent_light = linear_luminance(accent);
    if light == 0.0 || accent_light == 0.0 {
        return background;
    }
    let target = accent.map(|channel| channel * light / accent_light);
    for channel in 0..3 {
        let delta = target[channel] - base[channel];
        if delta > 0.0 {
            amount = amount.min((1.0 - base[channel]) / delta);
        }
    }
    std::array::from_fn(|channel| {
        encoded(base[channel] + amount * (target[channel] - base[channel]))
    })
}
fn encoded(value: f64) -> u8 {
    let value = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}
pub(crate) fn blend(fg: [u8; 3], bg: [u8; 3], percent: u16) -> [u8; 3] {
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

pub(crate) fn surface(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    // MAIN's signed six-percent shift moves every channel away from the
    // observed ink, without imposing a dark/light/chromatic classification.
    let offset = ((luminance(fg) - luminance(bg)) * 255.0 * 0.06).round() as i16;
    bg.map(|channel| (i16::from(channel) - offset).clamp(0, 255) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn passive_join_uses_main_pixel_margins_and_bridges_either_side() {
        use bkpr_tui_framework::Rect;
        let size = Size {
            cols: 100,
            rows: 30,
            cell_width: 10,
            cell_height: 20,
        };
        let host = ColorProbe {
            foreground: Some([230; 3]),
            background: Some([20; 3]),
            truecolor: true,
            ..ColorProbe::default()
        };
        let style = decoration_styles(&host, size).unwrap()[1];
        let popup = Rect {
            x: 20,
            y: 3,
            width: 30,
            height: 12,
        };
        let item = Rect {
            x: 0,
            y: 7,
            width: 20,
            height: 2,
        };
        let joined = joined_surface(size, popup, item, style).unwrap();
        assert_eq!(joined.rects_px, [[205, 78, 495, 282], [5, 138, 220, 182]]);
        assert_eq!(joined.style.inset_px, [0, 0]);
        assert_eq!(joined.style.outset_px, [0, 0]);
        let right_item = Rect { x: 50, ..item };
        let joined = joined_surface(size, popup, right_item, style).unwrap();
        assert_eq!(joined.rects_px[1], [480, 138, 695, 182]);
        assert!(joined_surface(size, popup, Rect { x: 25, ..item }, style).is_none());
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
