//! Instance-local palettes for Bkpr chrome and native pane presentation.
use bkpr_tui_framework::{ColorProbe, PaletteOverride};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum ThemeId {
    #[default]
    Terminal,
    CatppuccinLatte,
    CatppuccinFrappe,
    CatppuccinMacchiato,
    CatppuccinMocha,
    SolarizedDark,
    SolarizedLight,
    TokyoNightNight,
    TokyoNightStorm,
    TokyoNightDay,
    PaperColorLight,
    PaperColorDark,
}

pub(crate) const THEMES: &[(ThemeId, &str)] = &[
    (ThemeId::Terminal, "Terminal (inferred)"),
    (ThemeId::CatppuccinLatte, "Catppuccin Latte"),
    (ThemeId::CatppuccinFrappe, "Catppuccin Frappe"),
    (ThemeId::CatppuccinMacchiato, "Catppuccin Macchiato"),
    (ThemeId::CatppuccinMocha, "Catppuccin Mocha"),
    (ThemeId::SolarizedDark, "Solarized Dark"),
    (ThemeId::SolarizedLight, "Solarized Light"),
    (ThemeId::TokyoNightNight, "Tokyo Night"),
    (ThemeId::TokyoNightStorm, "Tokyo Night Storm"),
    (ThemeId::TokyoNightDay, "Tokyo Night Day"),
    (ThemeId::PaperColorLight, "PaperColor Light"),
    (ThemeId::PaperColorDark, "PaperColor Dark"),
];

impl ThemeId {
    pub(crate) fn name(self) -> &'static str {
        THEMES[self as usize].1
    }

    /// Project named colors per client without mutating shared terminal state.
    pub(crate) fn pane_palette(self, truecolor: bool) -> Option<PaletteOverride<'static>> {
        let preset = truecolor.then(|| self.preset()).flatten()?;
        Some(PaletteOverride {
            foreground: preset.foreground,
            background: preset.background,
            palette: &preset.ansi,
        })
    }

    fn preset(self) -> Option<&'static Preset> {
        match self {
            Self::Terminal => None,
            Self::CatppuccinLatte => Some(&LATTE),
            Self::CatppuccinFrappe => Some(&FRAPPE),
            Self::CatppuccinMacchiato => Some(&MACCHIATO),
            Self::CatppuccinMocha => Some(&MOCHA),
            Self::SolarizedDark => Some(&SOLARIZED_DARK),
            Self::SolarizedLight => Some(&SOLARIZED_LIGHT),
            Self::TokyoNightNight => Some(&TOKYO_NIGHT),
            Self::TokyoNightStorm => Some(&TOKYO_STORM),
            Self::TokyoNightDay => Some(&TOKYO_DAY),
            Self::PaperColorLight => Some(&PAPERCOLOR_LIGHT),
            Self::PaperColorDark => Some(&PAPERCOLOR_DARK),
        }
    }
}

/// Effective input to the existing appearance policy, not new host evidence.
///
/// The caller retains the actual probe and updates this cache before styling.
/// Terminal restoration therefore uses the latest observation, not an opening
/// snapshot. Only fg/bg/ANSI16 are replaced, and only with positive RGB evidence.
#[derive(Default)]
pub(crate) struct Palette {
    probe: ColorProbe,
}

impl Palette {
    pub(crate) fn update(&mut self, observed: &ColorProbe, choice: ThemeId) {
        let preset = if observed.truecolor {
            choice.preset()
        } else {
            None
        };
        self.probe.truecolor = observed.truecolor;
        self.probe.foreground =
            preset.map_or(observed.foreground, |preset| Some(preset.foreground));
        self.probe.background =
            preset.map_or(observed.background, |preset| Some(preset.background));
        self.probe.cursor = observed.cursor;
        self.probe.revision = observed.revision;
        // Graphics replies do not increment ColorProbe::revision. Compare the
        // exact metadata, including pending query identity, independently.
        if self.probe.graphics != observed.graphics {
            self.probe.graphics.clone_from(&observed.graphics);
        }
        match preset {
            None => {
                if self.probe.palette != observed.palette {
                    self.probe.palette.clone_from(&observed.palette);
                }
            }
            Some(preset) => {
                // Public probe fields may change without a revision bump. This
                // allocation-free comparison also preserves non-ANSI entries.
                let missing = (0..16)
                    .filter(|index| !observed.palette.contains_key(index))
                    .count();
                let matches = self.probe.palette.len() == observed.palette.len() + missing
                    && observed.palette.iter().all(|(index, color)| {
                        *index < 16 || self.probe.palette.get(index) == Some(color)
                    })
                    && preset
                        .ansi
                        .iter()
                        .enumerate()
                        .all(|(index, color)| self.probe.palette.get(&index) == Some(color));
                if !matches {
                    self.probe
                        .palette
                        .retain(|index, _| *index < 16 || observed.palette.contains_key(index));
                    self.probe.palette.extend(
                        observed
                            .palette
                            .iter()
                            .filter(|(index, _)| **index >= 16)
                            .map(|(&index, &color)| (index, color)),
                    );
                    self.probe
                        .palette
                        .extend(preset.ansi.iter().copied().enumerate());
                }
            }
        }
    }

    pub(crate) fn probe(&self) -> &ColorProbe {
        &self.probe
    }

    /// False means every selection honestly uses the native terminal palette.
    pub(crate) fn override_supported(&self) -> bool {
        self.probe.truecolor
    }
}

struct Preset {
    foreground: [u8; 3],
    background: [u8; 3],
    ansi: [[u8; 3]; 16],
}

const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

const fn preset(foreground: u32, background: u32, ansi: [u32; 16]) -> Preset {
    let mut colors = [[0; 3]; 16];
    let mut index = 0;
    while index < 16 {
        colors[index] = rgb(ansi[index]);
        index += 1;
    }
    Preset {
        foreground: rgb(foreground),
        background: rgb(background),
        ansi: colors,
    }
}

// Small app-owned constants avoid a palette dependency and its runtime parsing.
// Values below retain upstream terminal fg/bg/ANSI16; all Bkpr-specific surface,
// selection, contrast and decoration derivation remains in appearance.rs.
//
// Catppuccin: https://github.com/catppuccin/kitty/tree/43098316202b84d6a71f71aaf8360f102f4d3f1a/themes
// Files: latte.conf, frappe.conf, macchiato.conf, mocha.conf (MIT).
// Copyright (c) 2021 Catppuccin
const LATTE: Preset = preset(
    0x4c4f69,
    0xeff1f5,
    [
        0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be, 0x6c6f85,
        0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
    ],
);
const FRAPPE: Preset = preset(
    0xc6d0f5,
    0x303446,
    [
        0x51576d, 0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xb5bfe2, 0x626880,
        0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xa5adce,
    ],
);
const MACCHIATO: Preset = preset(
    0xcad3f5,
    0x24273a,
    [
        0x494d64, 0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xb8c0e0, 0x5b6078,
        0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xa5adcb,
    ],
);
const MOCHA: Preset = preset(
    0xcdd6f4,
    0x1e1e2e,
    [
        0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70,
        0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
    ],
);

// Solarized: https://github.com/altercation/solarized/blob/62f656a02f93c5190a8753159e34b385588d5ff3/xresources/solarized
// Light uses the source's commented light base substitutions (MIT).
// Copyright (c) 2011 Ethan Schoonover
const SOLARIZED_DARK: Preset = preset(
    0x839496,
    0x002b36,
    [
        0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36,
        0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
    ],
);
const SOLARIZED_LIGHT: Preset = preset(
    0x657b83,
    0xfdf6e3,
    [
        0xeee8d5, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0x073642, 0xfdf6e3,
        0xcb4b16, 0x93a1a1, 0x839496, 0x657b83, 0x6c71c4, 0x586e75, 0x002b36,
    ],
);

// Tokyo Night: https://github.com/folke/tokyonight.nvim/tree/cdc07ac78467a233fd62c493de29a17e0cf2b2b6/extras/kitty
// Files: tokyonight_night.conf, tokyonight_storm.conf, tokyonight_day.conf.
// Author: Folke Lemaitre. Each source artifact explicitly declares MIT, although
// that repository's root LICENSE is Apache-2.0. These are the MIT-tagged generated
// terminal palette values, translated to constants, not the Lua implementation.
const TOKYO_NIGHT: Preset = preset(
    0xc0caf5,
    0x1a1b26,
    [
        0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
        0xff899d, 0x9fe044, 0xfaba4a, 0x8db0ff, 0xc7a9ff, 0xa4daff, 0xc0caf5,
    ],
);
const TOKYO_STORM: Preset = preset(
    0xc0caf5,
    0x24283b,
    [
        0x1d202f, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
        0xff899d, 0x9fe044, 0xfaba4a, 0x8db0ff, 0xc7a9ff, 0xa4daff, 0xc0caf5,
    ],
);
const TOKYO_DAY: Preset = preset(
    0x3760bf,
    0xe1e2e7,
    [
        0xb4b5b9, 0xf52a65, 0x587539, 0x8c6c3e, 0x2e7de9, 0x9854f1, 0x007197, 0x6172b0, 0xa1a6c5,
        0xff4774, 0x5c8524, 0xa27629, 0x358aff, 0xa463ff, 0x007ea8, 0x3760bf,
    ],
);

// PaperColor: https://github.com/NLKNguyen/papercolor-theme/blob/0cfe64ffb24c21a6101b5f994ca342a74c977aef/colors/PaperColor.vim
// Default light/dark color00–15; Normal uses color07 foreground and color00 background.
// Copyright (c) 2015-2020 Nikyle Nguyen (MIT).
const PAPERCOLOR_LIGHT: Preset = preset(
    0x444444,
    0xeeeeee,
    [
        0xeeeeee, 0xaf0000, 0x008700, 0x5f8700, 0x0087af, 0x878787, 0x005f87, 0x444444, 0xbcbcbc,
        0xd70000, 0xd70087, 0x8700af, 0xd75f00, 0xd75f00, 0x005faf, 0x005f87,
    ],
);
const PAPERCOLOR_DARK: Preset = preset(
    0xd0d0d0,
    0x1c1c1c,
    [
        0x1c1c1c, 0xaf005f, 0x5faf00, 0xd7af5f, 0x5fafd7, 0x808080, 0xd7875f, 0xd0d0d0, 0x585858,
        0x5faf5f, 0xafd700, 0xaf87d7, 0xffaf00, 0xff5faf, 0x00afaf, 0x5f8787,
    ],
);

// MIT permission notice for the attributed palette data above:
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> ColorProbe {
        let mut observed = ColorProbe::default();
        observed.feed(b"1+r5463");
        observed.feed(b"10;#eeeeee");
        observed.feed(b"11;#202830");
        observed.feed(b"12;#abcdef");
        observed.feed(b"4;4;#123456");
        observed.feed(b"4;200;#654321");
        observed
    }

    fn assert_probe_eq(actual: &ColorProbe, expected: &ColorProbe) {
        assert_eq!(actual.truecolor, expected.truecolor);
        assert_eq!(actual.foreground, expected.foreground);
        assert_eq!(actual.background, expected.background);
        assert_eq!(actual.cursor, expected.cursor);
        assert_eq!(actual.palette, expected.palette);
        assert_eq!(actual.revision, expected.revision);
        assert!(actual.graphics == expected.graphics);
    }

    #[test]
    fn overrides_only_ui_colors_and_preserves_actual_host_evidence() {
        let mut observed = host();
        observed.graphics.begin();
        let mut palette = Palette::default();
        for &(choice, _) in THEMES {
            palette.update(&observed, choice);
            let effective = palette.probe();
            assert_eq!(effective.truecolor, observed.truecolor);
            assert_eq!(effective.cursor, observed.cursor);
            assert_eq!(effective.revision, observed.revision);
            assert!(effective.graphics == observed.graphics);
            assert_eq!(effective.palette.get(&200), observed.palette.get(&200));
        }
    }

    #[test]
    fn terminal_selection_restores_latest_observed_colors() {
        let mut observed = host();
        let mut palette = Palette::default();
        palette.update(&observed, ThemeId::CatppuccinMocha);
        assert_eq!(palette.probe().background, Some(MOCHA.background));
        observed.theme_changed(b"theme-change;2");
        observed.feed(b"10;#343434");
        observed.feed(b"11;#fafafa");
        observed.feed(b"4;4;#abcdff");
        palette.update(&observed, ThemeId::CatppuccinMocha);
        assert_eq!(palette.probe().background, Some(MOCHA.background));
        assert!(!palette.probe().palette.contains_key(&200));
        palette.update(&observed, ThemeId::Terminal);
        assert_probe_eq(palette.probe(), &observed);
    }

    #[test]
    fn unknown_or_lost_rgb_capability_uses_actual_native_palette() {
        let mut observed = host();
        let mut palette = Palette::default();
        palette.update(&observed, ThemeId::SolarizedDark);
        assert!(palette.override_supported());
        observed.truecolor = false;
        for &(choice, _) in THEMES {
            palette.update(&observed, choice);
            assert!(!palette.override_supported());
            assert_probe_eq(palette.probe(), &observed);
        }
        palette.update(&ColorProbe::default(), ThemeId::TokyoNightDay);
        assert!(!palette.override_supported());
        assert_probe_eq(palette.probe(), &ColorProbe::default());
    }

    #[test]
    fn graphics_and_cursor_changes_are_not_hidden_by_palette_cache() {
        let mut observed = host();
        let mut palette = Palette::default();
        palette.update(&observed, ThemeId::TokyoNightStorm);
        let revision = observed.revision;
        let query = String::from_utf8(observed.graphics.begin()).unwrap();
        palette.update(&observed, ThemeId::TokyoNightStorm);
        assert!(palette.probe().graphics == observed.graphics);
        let id = query.split("i=").nth(1).unwrap().split(',').next().unwrap();
        observed.feed(format!("graphics;Gi={id};OK").as_bytes());
        assert_eq!(observed.revision, revision);
        palette.update(&observed, ThemeId::TokyoNightStorm);
        assert!(palette.probe().graphics.supported());
        assert!(palette.probe().graphics == observed.graphics);
        observed.cursor = None;
        observed.palette.insert(200, [5, 6, 7]);
        palette.update(&observed, ThemeId::TokyoNightStorm);
        assert_eq!(palette.probe().cursor, None);
        assert_eq!(palette.probe().palette.get(&200), Some(&[5, 6, 7]));
    }

    #[test]
    fn unchanged_palette_keeps_the_existing_theme_revision() {
        let observed = host();
        let mut palette = Palette::default();
        let mut theme = bkpr_tui_framework::Theme::default();
        for &(choice, _) in THEMES {
            palette.update(&observed, choice);
            crate::appearance::update_theme(&mut theme, palette.probe());
            let revision = theme.revision();
            palette.update(&observed, choice);
            assert!(!crate::appearance::update_theme(
                &mut theme,
                palette.probe()
            ));
            assert_eq!(theme.revision(), revision);
        }
    }
}
