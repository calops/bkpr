//! Allocation-free, perceptually normalized sRGB color families.
//!
//! Resolve colors when the source palette changes and cache the resulting bytes;
//! frame rendering should only read the resolved variants.

use palette::{
    FromColor, LinLuma, Okhsl, Okhsv, Oklch, Srgb, color_difference::Wcag21RelativeContrast,
    white_point::D65,
};

type Luminance = LinLuma<D65, f64>;

/// A fixed perceptual tone or the most colorful readable tone at the source hue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Fixed {
        lightness: f32,
        saturation: f32,
    },
    Vivid {
        background: Luminance,
        minimum_contrast: f64,
    },
}

impl Tone {
    /// Creates an Okhsl target, with both components in the inclusive unit range.
    ///
    /// # Panics
    ///
    /// Panics if either component is non-finite or outside `0.0..=1.0`.
    pub fn new(lightness: f32, saturation: f32) -> Self {
        assert!(
            lightness.is_finite() && (0.0..=1.0).contains(&lightness),
            "tone lightness must be finite and in 0.0..=1.0"
        );
        assert!(
            saturation.is_finite() && (0.0..=1.0).contains(&saturation),
            "tone saturation must be finite and in 0.0..=1.0"
        );
        Self {
            target: Target::Fixed {
                lightness,
                saturation,
            },
        }
    }

    /// Maximizes in-gamut Oklch chroma at the source hue while meeting a WCAG
    /// contrast ratio against `background`, including final RGB quantization.
    ///
    /// Neutral sources use the highest-contrast neutral. Resolution is bounded
    /// and allocation-free; cache its result rather than resolving per frame.
    ///
    /// # Panics
    ///
    /// Panics if the ratio is non-finite, below one, or unattainable against
    /// `background` even with black or white.
    pub fn vivid(background: [u8; 3], minimum_contrast: f64) -> Self {
        let background = luminance(background);
        let maximum = luminance([0; 3])
            .relative_contrast(background)
            .max(luminance([255; 3]).relative_contrast(background));
        assert!(
            minimum_contrast.is_finite() && (1.0..=maximum).contains(&minimum_contrast),
            "tone contrast must be finite, at least one, and achievable"
        );
        Self {
            target: Target::Vivid {
                background,
                minimum_contrast,
            },
        }
    }

    fn resolve(self, source: Okhsl<f64>, achromatic: bool) -> [u8; 3] {
        match self.target {
            Target::Fixed {
                lightness,
                saturation,
            } => rgb_at(
                source,
                if achromatic {
                    0.0
                } else {
                    f64::from(saturation)
                },
                f64::from(lightness),
            ),
            Target::Vivid {
                background,
                minimum_contrast,
            } => vivid(source, achromatic, background, minimum_contrast),
        }
    }
}

pub(crate) fn luminance(rgb: [u8; 3]) -> Luminance {
    Srgb::from(rgb).into_format::<f64>().relative_luminance()
}

fn rgb_at(source: Okhsl<f64>, saturation: f64, lightness: f64) -> [u8; 3] {
    Srgb::from_color(Okhsl::new(source.hue, saturation, lightness))
        .into_format::<u8>()
        .into()
}

fn chroma(rgb: [u8; 3]) -> f64 {
    Oklch::from_color(Srgb::from(rgb).into_format::<f64>()).chroma
}

fn vivid(
    source: Okhsl<f64>,
    achromatic: bool,
    background: Luminance,
    minimum_contrast: f64,
) -> [u8; 3] {
    let endpoints = [[0; 3], [255; 3]];
    let mut best = if luminance(endpoints[0]).relative_contrast(background)
        >= luminance(endpoints[1]).relative_contrast(background)
    {
        endpoints[0]
    } else {
        endpoints[1]
    };
    if achromatic {
        return best;
    }
    let cusp = Okhsl::from_color(Okhsv::new(source.hue, 1.0_f64, 1.0));
    let peak = rgb_at(source, 1.0, cusp.lightness);
    if luminance(peak).relative_contrast(background) >= minimum_contrast {
        return peak;
    }
    let mut best_chroma = chroma(best);
    for (endpoint, lightness) in endpoints.into_iter().zip([0.0, 1.0]) {
        if luminance(endpoint).relative_contrast(background) < minimum_contrast {
            continue;
        }
        let mut feasible = lightness;
        let mut infeasible = cusp.lightness;
        let mut candidate = endpoint;
        for _ in 0..20 {
            let middle = (feasible + infeasible) * 0.5;
            let rgb = rgb_at(source, 1.0, middle);
            if luminance(rgb).relative_contrast(background) >= minimum_contrast {
                feasible = middle;
                candidate = rgb;
            } else {
                infeasible = middle;
            }
        }
        let candidate_chroma = chroma(candidate);
        if candidate_chroma > best_chroma {
            best = candidate;
            best_chroma = candidate_chroma;
        }
    }
    best
}

/// Application-selected targets for the two perceptually normalized variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorTones {
    /// Target used for the bright variant; no ordering of targets is enforced.
    pub bright: Tone,
    /// Target used for the muted variant; no ordering of targets is enforced.
    pub muted: Tone,
}

impl ColorTones {
    /// Resolves bounded sRGB variants using gamut-aware perceptual Okhsl.
    ///
    /// Both variants retain the source hue and apply the selected tone policy.
    /// Achromatic sources (equal RGB channels, including black and white) remain
    /// neutral. The base bytes are preserved exactly.
    ///
    /// This allocates nothing, but performs color conversions: cache the result
    /// when the source palette changes rather than resolving on every frame.
    pub fn resolve(self, rgb: [u8; 3]) -> ColorVariants {
        let source = Okhsl::from_color(Srgb::from(rgb).into_format::<f64>());
        let achromatic = rgb[0] == rgb[1] && rgb[1] == rgb[2];
        ColorVariants {
            base: rgb,
            bright: self.bright.resolve(source, achromatic),
            muted: self.muted.resolve(source, achromatic),
        }
    }
}

/// Cached sRGB bytes for a source color and its normalized perceptual variants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorVariants {
    base: [u8; 3],
    bright: [u8; 3],
    muted: [u8; 3],
}

impl ColorVariants {
    /// Returns the unchanged source sRGB bytes.
    pub fn base(&self) -> [u8; 3] {
        self.base
    }

    /// Returns sRGB bytes normalized to the caller's bright Okhsl target.
    pub fn bright(&self) -> [u8; 3] {
        self.bright
    }

    /// Returns sRGB bytes normalized to the caller's muted Okhsl target.
    pub fn muted(&self) -> [u8; 3] {
        self.muted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perceptual(rgb: [u8; 3]) -> Okhsl<f64> {
        Okhsl::from_color(Srgb::from(rgb).into_format::<f64>())
    }

    fn rgb(hue: f64, saturation: f64, lightness: f64) -> [u8; 3] {
        Srgb::from_color(Okhsl::new(hue, saturation, lightness))
            .into_format::<u8>()
            .into()
    }

    fn assert_target(rgb: [u8; 3], lightness: f64, saturation: f64) {
        let actual = perceptual(rgb);
        assert!(
            (actual.lightness - lightness).abs() < 0.015,
            "unexpected lightness for {rgb:?}: {actual:?}"
        );
        assert!(
            (actual.saturation - saturation).abs() < 0.025,
            "unexpected saturation for {rgb:?}: {actual:?}"
        );
    }

    #[test]
    fn dark_and_pale_sources_normalize_to_the_same_family() {
        let tones = ColorTones {
            bright: Tone::new(0.72, 0.9),
            muted: Tone::new(0.45, 0.5),
        };
        for hue in [30.0, 145.0, 265.0] {
            let dark = tones.resolve(rgb(hue, 1.0, 0.2));
            let pale = tones.resolve(rgb(hue, 0.4, 0.85));
            for (dark_rgb, pale_rgb, lightness, saturation) in [
                (dark.bright(), pale.bright(), 0.72, 0.9),
                (dark.muted(), pale.muted(), 0.45, 0.5),
            ] {
                assert_target(dark_rgb, lightness, saturation);
                assert_target(pale_rgb, lightness, saturation);
                for (dark_channel, pale_channel) in dark_rgb.into_iter().zip(pale_rgb) {
                    assert!(dark_channel.abs_diff(pale_channel) <= 5);
                }
            }
        }
    }

    #[test]
    fn saturated_gamut_edges_keep_their_perceptual_hues() {
        let tones = ColorTones {
            bright: Tone::new(0.7, 0.98),
            muted: Tone::new(0.45, 0.8),
        };
        for source in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [0, 255, 255],
            [255, 0, 255],
        ] {
            let variants = tones.resolve(source);
            assert_eq!(variants.base(), source);
            for (result, lightness, saturation) in [
                (variants.bright(), 0.7, 0.98),
                (variants.muted(), 0.45, 0.8),
            ] {
                assert_target(result, lightness, saturation);
                let delta = (perceptual(result).hue - perceptual(source).hue)
                    .into_degrees()
                    .abs();
                assert!(delta < 2.0, "hue changed for {source:?}: {result:?}");
            }
        }
    }

    #[test]
    fn achromatic_sources_remain_neutral_including_black_and_white() {
        let tones = ColorTones {
            bright: Tone::new(0.8, 1.0),
            muted: Tone::new(0.35, 0.9),
        };
        let expected = tones.resolve([128; 3]);
        for channel in 0..=255 {
            let variants = tones.resolve([channel; 3]);
            assert_eq!(variants.base(), [channel; 3]);
            assert_eq!(variants.bright(), expected.bright());
            assert_eq!(variants.muted(), expected.muted());
            for result in [variants.bright(), variants.muted()] {
                assert_eq!(result[0], result[1]);
                assert_eq!(result[1], result[2]);
            }
        }
        assert!(expected.bright()[0] > expected.muted()[0]);
        assert!(expected.muted()[0] > 0);
        assert!(expected.bright()[0] < 255);
    }

    #[test]
    fn unit_endpoints_produce_black_white_and_neutral_targets() {
        let endpoints = ColorTones {
            bright: Tone::new(1.0, 1.0),
            muted: Tone::new(0.0, 1.0),
        };
        let neutral = ColorTones {
            bright: Tone::new(0.65, 0.0),
            muted: Tone::new(0.3, 0.0),
        };
        for source in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [17, 128, 243]] {
            let variants = endpoints.resolve(source);
            assert_eq!(variants.bright(), [255; 3]);
            assert_eq!(variants.muted(), [0; 3]);
            let variants = neutral.resolve(source);
            for result in [variants.bright(), variants.muted()] {
                assert_eq!(result[0], result[1]);
                assert_eq!(result[1], result[2]);
                assert!(result[0] > 0 && result[0] < 255);
            }
        }
    }

    #[test]
    fn vivid_hues_remain_readable_across_the_gamut_and_backgrounds() {
        for background in [[18, 18, 32], [253, 246, 227], [118; 3]] {
            let tone = Tone::vivid(background, 4.5);
            for hue in (0..360).step_by(5) {
                let source = perceptual(rgb(f64::from(hue), 0.8, 0.6));
                let result = tone.resolve(source, false);
                assert!(
                    luminance(result).relative_contrast(luminance(background)) >= 4.5,
                    "{background:?}, hue {hue}: {result:?}"
                );
                let delta = (perceptual(result).hue - source.hue).into_radians();
                let hue_error = 2.0 * chroma(result) * (delta * 0.5).sin().abs();
                assert!(
                    hue_error < 0.005,
                    "hue {hue}: {result:?}, Oklab hue displacement {hue_error}"
                );
            }
        }
    }

    #[test]
    fn vivid_chroma_matches_a_full_lightness_search_without_hue_exceptions() {
        for background in [[18, 18, 32], [253, 246, 227], [118; 3], [0; 3]] {
            let tone = Tone::vivid(background, 4.5);
            for hue in (0..360).step_by(15) {
                let source = perceptual(rgb(f64::from(hue), 0.8, 0.6));
                let result = tone.resolve(source, false);
                let actual_chroma = chroma(result);
                for step in 0..=400 {
                    let alternative = rgb_at(source, 1.0, f64::from(step) / 400.0);
                    if luminance(alternative).relative_contrast(luminance(background)) >= 4.5 {
                        assert!(
                            actual_chroma + 0.005 >= chroma(alternative),
                            "{background:?}, hue {hue}: {result:?} less vivid than {alternative:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn vivid_neutrals_stay_neutral_and_readable_for_every_source_gray() {
        for background in [[18, 18, 32], [253, 246, 227], [118; 3]] {
            let tones = ColorTones {
                bright: Tone::vivid(background, 4.5),
                muted: Tone::new(0.5, 0.2),
            };
            let expected = tones.resolve([128; 3]).bright();
            for gray in 0..=255 {
                let result = tones.resolve([gray; 3]);
                assert_eq!(result.base(), [gray; 3]);
                assert_eq!(result.bright(), expected);
                assert_eq!(expected[0], expected[1]);
                assert_eq!(expected[1], expected[2]);
                assert!(luminance(expected).relative_contrast(luminance(background)) >= 4.5);
            }
        }
    }

    #[test]
    fn vivid_rejects_impossible_or_invalid_contrast() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.99, 22.0] {
            assert!(std::panic::catch_unwind(|| Tone::vivid([0; 3], invalid)).is_err());
        }
        assert!(std::panic::catch_unwind(|| Tone::vivid([118; 3], 7.0)).is_err());
        assert_eq!(
            Tone::vivid([0; 3], 21.0).resolve(perceptual([137, 180, 250]), false),
            [255; 3]
        );
    }

    #[test]
    fn rejects_non_finite_and_out_of_range_targets() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.01, 1.01] {
            assert!(std::panic::catch_unwind(|| Tone::new(invalid, 0.5)).is_err());
            assert!(std::panic::catch_unwind(|| Tone::new(0.5, invalid)).is_err());
        }
    }
}
