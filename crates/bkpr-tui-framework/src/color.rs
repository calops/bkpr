//! Allocation-free, perceptually normalized sRGB color families.
//!
//! Resolve colors when the source palette changes and cache the resulting bytes;
//! frame rendering should only read the resolved variants.

use palette::{FromColor, Okhsl, Srgb};

/// Caller-selected perceptual Okhsl lightness and saturation in the sRGB gamut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    lightness: f32,
    saturation: f32,
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
            lightness,
            saturation,
        }
    }

    fn resolve(self, source: Okhsl<f64>, achromatic: bool) -> [u8; 3] {
        let normalized = Okhsl::new(
            source.hue,
            if achromatic {
                0.0
            } else {
                f64::from(self.saturation)
            },
            f64::from(self.lightness),
        );
        Srgb::from_color(normalized).into_format::<u8>().into()
    }
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
    /// Both variants retain the source hue and replace its lightness and
    /// saturation with the selected targets. Achromatic sources (equal RGB
    /// channels, including black and white) remain neutral regardless of the
    /// target saturation. The base bytes are preserved exactly.
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
    fn rejects_non_finite_and_out_of_range_targets() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.01, 1.01] {
            assert!(std::panic::catch_unwind(|| Tone::new(invalid, 0.5)).is_err());
            assert!(std::panic::catch_unwind(|| Tone::new(0.5, invalid)).is_err());
        }
    }
}
