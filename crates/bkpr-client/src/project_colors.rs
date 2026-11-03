//! Client-owned finite project accents, derived only from observed terminal colours.
//! Ported from Bkpr main 9ad93bd809f8fb0dbd0c28bcbc267a682a21430b,
//! crates/bkpr-zellij/src/palette.rs (project palette and terminal-native fallback).

use crate::appearance::{blend, surface};
use bkpr_protocol::ColorSource;
use bkpr_tui_framework::{ColorProbe, Role};
use std::collections::BTreeSet;

// Black, gray and white can disappear against terminal-native surfaces.
const ACCENTS: [u8; 12] = [4, 5, 6, 3, 13, 12, 2, 1, 9, 10, 11, 14];
const SHADE_PASSES: usize = 3 + (100 - 35 + 1);
const SHADE_CANDIDATES: usize = ACCENTS.len() * SHADE_PASSES;
// Includes the initial 75% accents, even though their RGBs recur in the shade passes.
const MAX_CANDIDATES: u32 = (ACCENTS.len() + SHADE_CANDIDATES) as u32;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Evidence {
    truecolor: bool,
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    ansi: [Option<[u8; 3]>; 16],
}
impl Evidence {
    fn from_probe(probe: &ColorProbe) -> Self {
        Self {
            truecolor: probe.truecolor,
            foreground: probe.foreground,
            background: probe.background,
            ansi: std::array::from_fn(|index| probe.palette.get(&index).copied()),
        }
    }

    fn surface(self) -> Option<[u8; 3]> {
        self.background
            .filter(|_| self.truecolor)
            .map(|bg| self.foreground.map_or(bg, |fg| surface(fg, bg)))
    }
}

#[derive(Default)]
pub(crate) struct ProjectPalette {
    evidence: Option<Evidence>,
    capacity: u32,
    colours: Vec<ColorSource>,
    seen: BTreeSet<[u8; 3]>,
    background: Option<[u8; 3]>,
    next_shade: usize,
    exhausted: bool,
}
impl ProjectPalette {
    /// Cache colour evidence and monotonically grow finite demand. Returns whether
    /// any slot's colour/availability changed, not whether unrelated probe state did.
    pub(crate) fn update(&mut self, probe: &ColorProbe, capacity: u32) -> bool {
        let evidence = Evidence::from_probe(probe);
        self.capacity = self.capacity.max(capacity.min(MAX_CANDIDATES));
        let mut changed = false;
        let mut length = self.colours.len();
        if self.evidence != Some(evidence) {
            self.evidence = Some(evidence);
            self.background = evidence.surface();
            self.seen.clear();
            self.next_shade = 0;
            self.exhausted = self.background.is_none();
            length = 0;
            // Main always offers all base accents, including before demand arrives.
            for index in ACCENTS {
                let colour = match evidence.ansi[usize::from(index)].zip(self.background) {
                    Some((colour, bg)) => {
                        let colour = blend(colour, bg, 75);
                        if colour == bg {
                            continue;
                        }
                        ColorSource::Rgb(colour)
                    }
                    None => ColorSource::Palette(index),
                };
                changed |= self.write_colour(colour, &mut length);
            }
        }
        // Continue the finite candidate stream instead of rebuilding earlier slots
        // on every demand increase. Theme changes restart it with the retained demand.
        if !self.exhausted {
            let bg = self
                .background
                .expect("RGB shades require an observed background");
            while length < self.capacity as usize && self.next_shade < SHADE_CANDIDATES {
                let pass = self.next_shade / ACCENTS.len();
                let index = ACCENTS[self.next_shade % ACCENTS.len()];
                self.next_shade += 1;
                let percent = match pass {
                    0 => 90,
                    1 => 60,
                    2 => 45,
                    _ => 103 - pass as u16,
                };
                let Some(colour) = evidence.ansi[usize::from(index)] else {
                    continue;
                };
                let colour = blend(colour, bg, percent);
                if colour != bg {
                    changed |= self.write_colour(ColorSource::Rgb(colour), &mut length);
                }
            }
            self.exhausted = self.next_shade == SHADE_CANDIDATES;
        }
        changed |= length != self.colours.len();
        self.colours.truncate(length);
        changed
    }

    /// Unknown and exhausted ordinals use the caller's current muted style.
    fn colour(&self, slot: Option<u32>, muted: ColorSource) -> ColorSource {
        slot.and_then(|slot| self.colours.get(slot as usize))
            .copied()
            .unwrap_or(muted)
    }

    /// Main's project escapes include SGR 22: a known accent must not inherit
    /// dim/bold from its surrounding metadata row. Unknown/exhausted stays muted.
    pub(crate) fn ink(&self, slot: Option<u32>, muted: ColorSource) -> (Role, ColorSource) {
        let foreground = self.colour(slot, muted);
        (
            if foreground == muted {
                Role::Muted
            } else {
                Role::Base
            },
            foreground,
        )
    }

    fn write_colour(&mut self, colour: ColorSource, length: &mut usize) -> bool {
        // Native sources stay native even when the reported RGBs alias: unlike
        // confirmed derived RGBs, different ANSI sources are not the same style.
        if let ColorSource::Rgb(rgb) = colour
            && !self.seen.insert(rgb)
        {
            return false;
        }
        let changed = if let Some(previous) = self.colours.get_mut(*length) {
            let changed = *previous != colour;
            *previous = colour;
            changed
        } else {
            self.colours.push(colour);
            true
        };
        *length += 1;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MUTED: ColorSource = ColorSource::Default;

    fn host() -> ColorProbe {
        ColorProbe {
            truecolor: true,
            foreground: Some([230, 230, 230]),
            background: Some([12, 18, 24]),
            palette: (0..16)
                .map(|index| {
                    (
                        index,
                        [
                            40 + index as u8 * 11,
                            220 - index as u8 * 9,
                            70 + index as u8 * 7,
                        ],
                    )
                })
                .collect(),
            ..ColorProbe::default()
        }
    }

    fn assigned(palette: &ProjectPalette, count: u32) -> Vec<ColorSource> {
        (0..count)
            .map(|slot| palette.colour(Some(slot), MUTED))
            .collect()
    }

    #[test]
    fn aliases_and_invisible_accents_do_not_take_rgb_slots() {
        let mut probe = host();
        probe.foreground = Some([0; 3]);
        probe.background = Some([0; 3]);
        probe.palette.insert(4, [80, 40, 20]);
        probe.palette.insert(5, [80, 40, 20]);
        probe.palette.insert(6, [0; 3]);
        probe.palette.insert(3, [20, 80, 40]);
        let mut palette = ProjectPalette::default();
        palette.update(&probe, 40);
        assert_eq!(
            palette.colour(Some(0), MUTED),
            ColorSource::Rgb([60, 30, 15])
        );
        assert_eq!(
            palette.colour(Some(1), MUTED),
            ColorSource::Rgb([15, 60, 30])
        );
        let colours = assigned(&palette, 40);
        for (index, colour) in colours.iter().enumerate() {
            assert_ne!(*colour, MUTED);
            assert_ne!(*colour, ColorSource::Rgb([0; 3]));
            assert!(!colours[..index].contains(colour));
        }
    }

    #[test]
    fn demand_growth_preserves_existing_slots_and_never_shrinks() {
        let probe = host();
        let mut palette = ProjectPalette::default();
        assert!(palette.update(&probe, 20));
        let first = assigned(&palette, 20);
        assert!(palette.update(&probe, 100));
        assert_eq!(assigned(&palette, 20), first);
        assert_ne!(palette.colour(Some(99), MUTED), MUTED);
        let expanded = assigned(&palette, 100);
        let allocation = palette.colours.as_ptr();
        assert!(!palette.update(&probe, 3));
        assert!(!palette.update(&probe, 100));
        assert_eq!(assigned(&palette, 100), expanded);
        assert_eq!(palette.colours.as_ptr(), allocation);
    }

    #[test]
    fn theme_and_capability_changes_rebuild_with_retained_demand() {
        let mut probe = host();
        let mut palette = ProjectPalette::default();
        palette.update(&probe, 100);
        let before = palette.colour(Some(0), MUTED);
        probe.background = Some([240, 235, 230]);
        assert!(palette.update(&probe, 0));
        assert_ne!(palette.colour(Some(0), MUTED), before);
        let before = palette.colour(Some(0), MUTED);
        probe.foreground = Some([20, 25, 30]);
        assert!(palette.update(&probe, 0));
        assert_ne!(palette.colour(Some(0), MUTED), before);
        let before = palette.colour(Some(0), MUTED);
        probe.palette.insert(4, [15, 180, 190]);
        assert!(palette.update(&probe, 0));
        assert_ne!(palette.colour(Some(0), MUTED), before);
        let rgb = assigned(&palette, 100);
        probe.truecolor = false;
        assert!(palette.update(&probe, 0));
        assert_eq!(palette.colour(Some(0), MUTED), ColorSource::Palette(4));
        assert_eq!(palette.colour(Some(12), MUTED), MUTED);
        probe.truecolor = true;
        assert!(palette.update(&probe, 0));
        assert_eq!(assigned(&palette, 100), rgb);
    }

    #[test]
    fn preferred_shades_then_dense_shades_extend_the_same_slot_sequence() {
        let mut probe = host();
        probe.foreground = Some([0; 3]);
        probe.background = Some([0; 3]);
        probe
            .palette
            .values_mut()
            .for_each(|rgb| *rgb = [100, 80, 40]);
        let mut palette = ProjectPalette::default();
        palette.update(&probe, 1);
        let mut previous = assigned(&palette, 1);
        for expected in [
            [90, 72, 36],
            [60, 48, 24],
            [45, 36, 18],
            [100, 80, 40],
            [99, 79, 40],
        ] {
            let slot = previous.len() as u32;
            assert!(palette.update(&probe, slot + 1));
            assert_eq!(assigned(&palette, slot), previous);
            assert_eq!(
                palette.colour(Some(slot), MUTED),
                ColorSource::Rgb(expected)
            );
            previous.push(ColorSource::Rgb(expected));
        }
    }

    #[test]
    fn native_and_incomplete_hosts_never_guess_rgb_or_reuse_exhausted_slots() {
        let mut probe = host();
        // Even complete aliased RGB reports do not authorize RGB output.
        probe.truecolor = false;
        probe.palette.values_mut().for_each(|rgb| *rgb = [100; 3]);
        let mut palette = ProjectPalette::default();
        palette.update(&probe, u32::MAX);
        let native = assigned(&palette, 12);
        for (index, colour) in native.iter().enumerate() {
            assert!(matches!(colour, ColorSource::Palette(_)));
            assert!(!native[..index].contains(colour));
        }
        assert_eq!(palette.colour(Some(12), MUTED), MUTED);
        probe.truecolor = true;
        probe.background = None;
        assert!(!palette.update(&probe, u32::MAX));
        assert_eq!(assigned(&palette, 12), native);
        assert_eq!(palette.colour(Some(u32::MAX), MUTED), MUTED);
        let other_muted = ColorSource::Palette(8);
        assert_eq!(palette.colour(None, other_muted), other_muted);
        assert_eq!(palette.colour(Some(12), other_muted), other_muted);
    }

    #[test]
    fn ansi_and_background_are_sufficient_without_a_foreground_guess() {
        let mut probe = ColorProbe {
            truecolor: true,
            background: Some([20; 3]),
            ..ColorProbe::default()
        };
        probe.palette.insert(4, [100, 60, 40]);
        let mut palette = ProjectPalette::default();
        palette.update(&probe, 13);
        assert_eq!(
            palette.colour(Some(0), MUTED),
            ColorSource::Rgb([80, 50, 35])
        );
        // The next unknown ANSI entry remains native rather than being fabricated.
        assert_eq!(palette.colour(Some(1), MUTED), ColorSource::Palette(5));
        assert!(matches!(
            palette.colour(Some(12), MUTED),
            ColorSource::Rgb(_)
        ));
    }

    #[test]
    fn quantized_gamut_exhaustion_is_finite_and_does_not_cycle() {
        let mut probe = host();
        probe.foreground = Some([0; 3]);
        probe.background = Some([0; 3]);
        probe.palette.values_mut().for_each(|rgb| *rgb = [1; 3]);
        let mut palette = ProjectPalette::default();
        palette.update(&probe, u32::MAX);
        assert_eq!(palette.colour(Some(0), MUTED), ColorSource::Rgb([1; 3]));
        for slot in [1, 12, 100, MAX_CANDIDATES, u32::MAX] {
            assert_eq!(palette.colour(Some(slot), MUTED), MUTED);
        }
        let allocation = palette.colours.as_ptr();
        assert!(!palette.update(&probe, u32::MAX));
        assert_eq!(palette.colours.as_ptr(), allocation);
        // Recolouring can make even the only remaining shade invisible.
        probe.palette.values_mut().for_each(|rgb| *rgb = [0; 3]);
        assert!(palette.update(&probe, u32::MAX));
        assert_eq!(palette.colour(Some(0), MUTED), MUTED);
        assert!(!palette.update(&probe, u32::MAX));
    }

    #[test]
    fn graphics_cursor_revision_and_non_ansi_entries_do_not_invalidate_colours() {
        let mut probe = host();
        let mut palette = ProjectPalette::default();
        palette.update(&probe, 40);
        let colours = assigned(&palette, 40);
        let allocation = palette.colours.as_ptr();
        probe.cursor = Some([17, 33, 65]);
        probe.revision += 1;
        probe.palette.insert(200, [4, 8, 16]);
        let _query = probe.graphics.begin();
        assert!(!palette.update(&probe, 40));
        assert_eq!(assigned(&palette, 40), colours);
        assert_eq!(palette.colours.as_ptr(), allocation);
    }
}
