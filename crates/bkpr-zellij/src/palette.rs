//! Cached terminal-native styles, with surfaces shifted away from the reported foreground.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    pub truecolor: bool,
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub ansi: [Option<[u8; 3]>; 16],
}

#[derive(Clone, Debug)]
pub struct Theme {
    colors: TerminalColors,
    pub base: String,
    pub selected: String,
    pub hovered: String,
    pub muted: String,
    pub bright: String,
    pub added: String,
    pub deleted: String,
    pub accents: [String; 16],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: TerminalColors::default(),
            base: "\x1b[22;39;49m".into(),
            // Attribute-only highlights retain terminal-native colors on limited terminals.
            selected: "\x1b[7m".into(),
            hovered: "\x1b[4m".into(),
            muted: "\x1b[22;39m".into(),
            bright: "\x1b[22;39m".into(),
            added: "\x1b[22;32m".into(),
            deleted: "\x1b[22;31m".into(),
            accents: std::array::from_fn(|index| {
                let code = if index < 8 { 30 + index } else { 90 + index - 8 };
                format!("\x1b[22;{code}m")
            }),
        }
    }
}

fn blend(foreground: [u8; 3], background: [u8; 3], foreground_percent: u16) -> [u8; 3] {
    std::array::from_fn(|index| {
        ((u16::from(foreground[index]) * foreground_percent
            + u16::from(background[index]) * (100 - foreground_percent)
            + 50)
            / 100) as u8
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

fn surface(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    // One signed offset keeps every channel moving in the same luminance direction,
    // including chromatic palettes, without classifying the terminal as dark or light.
    let offset = ((luminance(fg) - luminance(bg)) * 255.0 * 0.06).round() as i16;
    bg.map(|channel| (i16::from(channel) - offset).clamp(0, 255) as u8)
}

fn foreground([r, g, b]: [u8; 3]) -> String {
    format!("\x1b[22;38;2;{r};{g};{b}m")
}

fn background([r, g, b]: [u8; 3]) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

fn enables_rgb(colors: &TerminalColors) -> bool {
    colors.truecolor
        && colors.background.is_some()
        && (colors.foreground.is_some() || colors.ansi.iter().any(Option::is_some))
}

impl Theme {
    /// Recompute escapes only when terminal metadata changes, never while painting rows.
    pub fn update(&mut self, colors: &TerminalColors) -> bool {
        if self.colors == *colors {
            return false;
        }
        if !enables_rgb(&self.colors) && !enables_rgb(colors) {
            self.colors.clone_from(colors);
            return false;
        }
        let mut next = Self::default();
        if colors.truecolor {
            let surface_bg = match (colors.foreground, colors.background) {
                (Some(fg), Some(bg)) => {
                    let surface_bg = surface(fg, bg);
                    let [r, g, b] = surface_bg;
                    next.base = format!("\x1b[22;39;48;2;{r};{g};{b}m");
                    next.muted = foreground(blend(fg, surface_bg, 65));
                    next.bright = foreground(blend(fg, surface_bg, 88));
                    next.selected = background(blend(fg, surface_bg, 12));
                    next.hovered = background(blend(fg, surface_bg, 6));
                    Some(surface_bg)
                }
                _ => colors.background,
            };
            if let Some(bg) = surface_bg {
                for (index, color) in colors.ansi.iter().enumerate() {
                    if let Some(color) = color {
                        next.accents[index] = foreground(blend(*color, bg, 75));
                    }
                }
                next.added.clone_from(&next.accents[2]);
                next.deleted.clone_from(&next.accents[1]);
            }
        }
        next.colors = colors.clone();
        *self = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_limited_colors_never_emit_rgb() {
        for colors in [TerminalColors::default(), TerminalColors {
            foreground: Some([240, 240, 240]),
            background: Some([12, 12, 12]),
            ansi: [Some([100, 100, 100]); 16],
            ..TerminalColors::default()
        }, TerminalColors { truecolor: true, ..TerminalColors::default() }, TerminalColors {
            truecolor: true,
            foreground: Some([240, 240, 240]),
            ansi: [Some([100, 100, 100]); 16],
            ..TerminalColors::default()
        }] {
            let mut theme = Theme::default();
            theme.update(&colors);
            assert_eq!(theme.base, "\x1b[22;39;49m");
            for style in [&theme.base, &theme.selected, &theme.hovered, &theme.muted,
                &theme.bright, &theme.added, &theme.deleted].into_iter().chain(theme.accents.iter()) {
                assert!(!style.contains(";2;"));
            }
        }
    }

    fn rgb(style: &str) -> [u8; 3] {
        let (_, channels) = style.trim_end_matches('m').split_once(";2;").unwrap();
        let mut channels = channels.split(';').map(|channel| channel.parse().unwrap());
        std::array::from_fn(|_| channels.next().unwrap())
    }

    fn distance(left: [u8; 3], right: [u8; 3]) -> u16 {
        left.into_iter().zip(right).map(|(l, r)| u16::from(l.abs_diff(r))).sum()
    }

    fn contrast_ratio(left: [u8; 3], right: [u8; 3]) -> f64 {
        let left = luminance(left);
        let right = luminance(right);
        (left.max(right) + 0.05) / (left.min(right) + 0.05)
    }

    #[test]
    fn surfaces_move_symmetrically_away_from_the_foreground() {
        let mut previous_offset: Option<i16> = None;
        for (fg, bg) in [([230, 220, 210], [20, 30, 40]), ([20, 30, 40], [230, 220, 210])] {
            let mut theme = Theme::default();
            assert!(theme.update(&TerminalColors {
                truecolor: true,
                foreground: Some(fg),
                background: Some(bg),
                ..TerminalColors::default()
            }));
            let surface = rgb(&theme.base);
            let offset = i16::from(bg[0]) - i16::from(surface[0]);
            assert_ne!(offset, 0);
            for channel in 0..3 {
                assert_eq!(i16::from(bg[channel]) - i16::from(surface[channel]), offset);
            }
            assert!(contrast_ratio(fg, surface) > contrast_ratio(fg, bg));
            if let Some(previous) = previous_offset {
                assert_eq!(offset, -previous);
            }
            previous_offset = Some(offset);
        }
    }

    #[test]
    fn chromatic_surfaces_follow_actual_luminance_and_saturate_safely() {
        for (fg, bg) in [
            ([0, 180, 0], [230, 0, 0]),
            ([230, 0, 0], [0, 180, 0]),
            ([255, 255, 255], [2, 20, 40]),
            ([0, 0, 0], [253, 235, 215]),
        ] {
            let adjusted = surface(fg, bg);
            let direction = luminance(bg) - luminance(fg);
            assert!((luminance(adjusted) - luminance(bg)) * direction > 0.0);
            for channel in 0..3 {
                assert!((f64::from(adjusted[channel]) - f64::from(bg[channel])) * direction >= 0.0);
            }
            assert!(contrast_ratio(fg, adjusted) > contrast_ratio(fg, bg));
        }
        assert_eq!(surface([255; 3], [2, 20, 40])[0], 0);
        assert_eq!(surface([0; 3], [253, 235, 215])[0], 255);
        assert_eq!(surface([255; 3], [0; 3]), [0; 3]);
        assert_eq!(surface([0; 3], [255; 3]), [255; 3]);
        assert_eq!(surface([12, 80, 140], [12, 80, 140]), [12, 80, 140]);
    }

    #[test]
    fn muted_and_shades_follow_both_light_and_dark_terminals() {
        for (fg, bg) in [([230, 220, 210], [20, 30, 40]), ([20, 30, 40], [230, 220, 210])] {
            let mut theme = Theme::default();
            let colors = TerminalColors { truecolor: true, foreground: Some(fg),
                background: Some(bg), ..TerminalColors::default() };
            assert!(theme.update(&colors));
            let surface = rgb(&theme.base);
            let muted = rgb(&theme.muted);
            let bright = rgb(&theme.bright);
            let selected = rgb(&theme.selected);
            let hovered = rgb(&theme.hovered);
            for color in [muted, bright, selected, hovered] {
                for channel in 0..3 {
                    assert!((fg[channel].min(surface[channel])..=fg[channel].max(surface[channel]))
                        .contains(&color[channel]));
                }
            }
            assert!(distance(hovered, surface) < distance(selected, surface));
            assert!(distance(selected, surface) < distance(muted, surface));
            assert!(distance(muted, surface) < distance(bright, surface));
            assert!(distance(bright, surface) < distance(fg, surface));
            assert!(u32::from(distance(muted, surface)) * 100
                < u32::from(distance(fg, surface)) * 75);
            assert!(contrast_ratio(muted, surface) >= 4.5);
            assert!(contrast_ratio(bright, selected) >= 4.5);
            assert!(!theme.update(&colors));
            assert!(theme.update(&TerminalColors::default()));
            assert_eq!(theme.base, "\x1b[22;39;49m");
            assert_eq!(theme.muted, "\x1b[22;39m");
            assert_eq!(theme.selected, "\x1b[7m");
            assert_eq!(theme.hovered, "\x1b[4m");
        }
    }

    #[test]
    fn partial_and_limited_metadata_does_not_invalidate_native_styles() {
        let mut theme = Theme::default();
        let mut colors = TerminalColors { foreground: Some([240, 240, 240]),
            background: Some([20, 20, 20]), ..TerminalColors::default() };
        assert!(!theme.update(&colors));
        assert_eq!(theme.base, "\x1b[22;39;49m");
        colors.background = None;
        colors.truecolor = true;
        assert!(!theme.update(&colors));
        assert_eq!(theme.base, "\x1b[22;39;49m");
        colors.background = Some([20, 20, 20]);
        assert!(theme.update(&colors));
        assert!(theme.base.contains("48;2;"));
        colors.foreground = None;
        assert!(theme.update(&colors));
        assert_eq!(theme.base, "\x1b[22;39;49m");
        colors.foreground = Some([240, 240, 240]);
        assert!(theme.update(&colors));
        colors.background = None;
        assert!(theme.update(&colors));
        assert_eq!(theme.base, "\x1b[22;39;49m");
    }

    #[test]
    fn ansi_accents_follow_actual_hues_and_missing_entries_stay_native() {
        for (fg, bg) in [
            (None, [255, 255, 255]),
            (None, [0, 0, 0]),
            (Some([230, 220, 210]), [20, 30, 40]),
            (Some([20, 30, 40]), [230, 220, 210]),
        ] {
            let mut colors = TerminalColors { truecolor: true, foreground: fg, background: Some(bg),
                ..TerminalColors::default() };
            let mut theme = Theme::default();
            assert_eq!(theme.update(&colors), fg.is_some());
            let surface = if fg.is_some() {
                rgb(&theme.base)
            } else {
                assert_eq!(theme.base, "\x1b[22;39;49m");
                bg
            };
            for index in 0..16 {
                let actual = [(index * 11) as u8, 64, 192];
                colors.ansi[index] = Some(actual);
                assert!(theme.update(&colors));
                if fg.is_none() {
                    assert_eq!(theme.base, "\x1b[22;39;49m");
                    assert_eq!(theme.selected, "\x1b[7m");
                    assert_eq!(theme.hovered, "\x1b[4m");
                    assert_eq!(theme.muted, "\x1b[22;39m");
                }
                let accent = rgb(&theme.accents[index]);
                assert!(u32::from(distance(accent, surface)) * 100
                    < u32::from(distance(actual, surface)) * 85);
                for channel in 0..3 {
                    assert!((actual[channel].min(surface[channel])..=actual[channel].max(surface[channel]))
                        .contains(&accent[channel]));
                }
            }
            assert_eq!(theme.accents[2], theme.added);
            assert_eq!(theme.accents[1], theme.deleted);
            colors.ansi[4] = None;
            assert!(theme.update(&colors));
            assert_eq!(theme.accents[4], "\x1b[22;34m");
            colors.truecolor = false;
            assert!(theme.update(&colors));
            assert!(theme.accents.iter().all(|style| !style.contains(";2;")));
            assert_eq!(theme.base, "\x1b[22;39;49m");
        }
    }
}
