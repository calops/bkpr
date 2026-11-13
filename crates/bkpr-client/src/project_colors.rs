//! Client-owned finite project accents, derived only from observed terminal colours.
//! Ported from Bkpr main 9ad93bd809f8fb0dbd0c28bcbc267a682a21430b,
//! crates/bkpr-zellij/src/palette.rs (project palette and terminal-native fallback).

use crate::appearance::{blend, surface};
use bkpr_protocol::{ColorSource, ProjectSnapshot, RegistrySnapshot};
use bkpr_tui_framework::{ColorProbe, Role};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

// Black, gray and white can disappear against terminal-native surfaces.
const ACCENTS: [u8; 12] = [4, 5, 6, 3, 13, 12, 2, 1, 9, 10, 11, 14];
const SHADE_PASSES: usize = 3 + (100 - 35 + 1);
const SHADE_CANDIDATES: usize = ACCENTS.len() * SHADE_PASSES;
// Includes the initial 75% accents, even though their RGBs recur in the shade passes.
const MAX_CANDIDATES: u32 = (ACCENTS.len() + SHADE_CANDIDATES) as u32;

/// Filesystem identity is local to a host/SSH route, never a session or admission
/// generation. Separate local daemons can observe the same repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum ProjectHost {
    /// Shared filesystem namespace of local daemon attachments.
    #[default]
    Local,
    /// A remote filesystem reached through the selected SSH route.
    Remote {
        destination: String,
        ssh_config: Option<PathBuf>,
    },
}

struct Assignment {
    slot: u32,
    seen: u64,
}

/// One host's client-assigned canonical repository ordinals.
#[derive(Default)]
pub(crate) struct ProjectSlots {
    repositories: HashMap<PathBuf, Assignment>,
}
impl ProjectSlots {
    /// Resolve an observed repository; unknown identity has no accent.
    pub(crate) fn slot(&self, project: &ProjectSnapshot) -> Option<u32> {
        self.repositories
            .get(&project.repository.as_ref()?.common_dir)
            .map(|assignment| assignment.slot)
    }
}

// Keep recent disconnected/retired identities without allowing session churn to
// retain an unbounded registry. Live identities are never evicted or renumbered.
const RETIRED_PROJECTS: usize = 256;

/// Client-wide stable assignments with bounded retention of retired identities.
#[derive(Default)]
pub(crate) struct ProjectColors {
    hosts: HashMap<ProjectHost, ProjectSlots>,
    empty: ProjectSlots,
    free: BTreeSet<u32>,
    next: u32,
    revision: u64,
}
impl ProjectColors {
    /// Project-color projection for an attachment's filesystem namespace.
    pub(crate) fn slots(&self, host: &ProjectHost) -> &ProjectSlots {
        self.hosts.get(host).unwrap_or(&self.empty)
    }
    /// Empty projection for an unknown attachment owner.
    pub(crate) fn empty(&self) -> &ProjectSlots {
        &self.empty
    }

    /// Ordinal capacity needed by the finite terminal-derived palette.
    pub(crate) fn capacity(&self) -> u32 {
        self.next
    }

    /// Called only when immutable registry publications or host membership change.
    /// Canonical snapshots stay untouched; palette/theme changes do not enter here.
    pub(crate) fn reconcile<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = (&'a ProjectHost, &'a Arc<RegistrySnapshot>)>,
    ) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("project revision exhausted");
        for (host, registry) in sessions {
            let mut repositories = registry
                .panes
                .iter()
                .filter_map(|pane| pane.project.as_ref()?.repository.as_ref())
                .peekable();
            if repositories.peek().is_none() {
                continue;
            }
            if !self.hosts.contains_key(host) {
                self.hosts.insert(host.clone(), ProjectSlots::default());
            }
            let slots = self.hosts.get_mut(host).unwrap();
            for repository in repositories {
                if let Some(assignment) = slots.repositories.get_mut(&repository.common_dir) {
                    assignment.seen = self.revision;
                    continue;
                }
                let slot = self.free.pop_first().unwrap_or_else(|| {
                    let slot = self.next;
                    self.next = self.next.checked_add(1).expect("project slots exhausted");
                    slot
                });
                slots.repositories.insert(
                    repository.common_dir.clone(),
                    Assignment {
                        slot,
                        seen: self.revision,
                    },
                );
            }
        }
        let revision = self.revision;
        let mut retired: Vec<_> = self
            .hosts
            .iter()
            .flat_map(|(host, slots)| {
                slots
                    .repositories
                    .iter()
                    .filter_map(move |(identity, assignment)| {
                        (assignment.seen != revision).then_some((
                            assignment.seen,
                            assignment.slot,
                            host,
                            identity,
                        ))
                    })
            })
            .collect();
        let surplus = retired.len().saturating_sub(RETIRED_PROJECTS);
        if surplus == 0 {
            drop(retired);
            self.hosts.retain(|_, slots| !slots.repositories.is_empty());
            return;
        }
        retired.sort_unstable_by_key(|(seen, slot, _, _)| (*seen, *slot));
        let evicted: Vec<_> = retired
            .into_iter()
            .take(surplus)
            .map(|(_, slot, host, identity)| (slot, host.clone(), identity.clone()))
            .collect();
        for (slot, host, identity) in evicted {
            self.hosts
                .get_mut(&host)
                .unwrap()
                .repositories
                .remove(&identity);
            self.free.insert(slot);
        }
        self.hosts.retain(|_, slots| !slots.repositories.is_empty());
    }
}

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

    fn registry(projects: impl IntoIterator<Item = ProjectSnapshot>) -> Arc<RegistrySnapshot> {
        Arc::new(RegistrySnapshot {
            revision: 1,
            workspaces: Vec::new(),
            panes: projects
                .into_iter()
                .enumerate()
                .map(|(index, project)| bkpr_protocol::PaneInfo {
                    id: index.to_string(),
                    workspace_id: None,
                    window_id: None,
                    title: String::new(),
                    status: bkpr_protocol::PaneStatus::Running,
                    exit_code: None,
                    cwd: project.cwd.clone(),
                    foreground_cwd: None,
                    harness: None,
                    harness_status: None,
                    project: Some(project),
                    home: "/fixture".into(),
                    isolated_environment: true,
                    harness_status_changed_at_ms: None,
                })
                .collect(),
        })
    }

    #[test]
    fn one_client_allocates_across_independent_sessions_and_host_filesystems() {
        let local = ProjectHost::Local;
        let remote = ProjectHost::Remote {
            destination: "user@builder".into(),
            ssh_config: None,
        };
        let other_route = ProjectHost::Remote {
            destination: "user@builder".into(),
            ssh_config: Some("/fixture/other-ssh-config".into()),
        };
        let project = crate::ci::tests::project();
        let mut worktree = project.clone();
        let repository = worktree.repository.as_mut().unwrap();
        repository.root = "/fixture/worktree".into();
        repository.git_dir = "/fixture/repo/.git/worktrees/linked".into();
        let mut distinct = project.clone();
        distinct.repository.as_mut().unwrap().common_dir = "/fixture/other/.git".into();
        let first = registry([project.clone(), worktree.clone()]);
        let second = registry([project.clone(), distinct.clone()]);
        let remote_registry = registry([project.clone()]);
        let mut colors = ProjectColors::default();
        // Each independent backend formerly called its first repository slot 0.
        // No backend presentation ordinal participates in this client allocation.
        colors.reconcile([
            (&local, &first),
            (&local, &second),
            (&remote, &remote_registry),
            (&other_route, &remote_registry),
        ]);
        let slot = colors.slots(&local).slot(&project);
        assert!(slot.is_some());
        assert_eq!(colors.slots(&local).slot(&worktree), slot);
        let slots = [
            slot,
            colors.slots(&local).slot(&distinct),
            colors.slots(&remote).slot(&project),
            colors.slots(&other_route).slot(&project),
        ];
        for (index, slot) in slots.iter().enumerate() {
            assert!(slot.is_some());
            assert!(!slots[..index].contains(slot));
        }
        let mut unknown = project.clone();
        unknown.repository = None;
        assert_eq!(colors.slots(&local).slot(&unknown), None);

        let mut palette = ProjectPalette::default();
        let mut probe = host();
        palette.update(&probe, colors.capacity());
        let before = assigned(&palette, 4);
        for (index, color) in before.iter().enumerate() {
            assert!(!before[..index].contains(color));
        }
        probe.background = Some([240, 235, 230]);
        palette.update(&probe, colors.capacity());
        assert_ne!(assigned(&palette, 4), before);
        assert_eq!(colors.slots(&local).slot(&project), slot);
    }

    #[test]
    fn reconnect_restores_recent_identity_and_retirement_is_bounded() {
        let local = ProjectHost::Local;
        let project = crate::ci::tests::project();
        let live = registry([project.clone()]);
        let mut colors = ProjectColors::default();
        colors.reconcile([(&local, &live)]);
        let original = colors.slots(&local).slot(&project);
        colors.reconcile([]);
        colors.reconcile([(&local, &live)]);
        assert_eq!(colors.slots(&local).slot(&project), original);
        let mut recent = project.clone();
        for index in 0..RETIRED_PROJECTS * 2 {
            recent.repository.as_mut().unwrap().common_dir =
                format!("/fixture/retired-{index}/.git").into();
            let transient = registry([recent.clone()]);
            colors.reconcile([(&local, &live), (&local, &transient)]);
            assert_eq!(colors.slots(&local).slot(&project), original);
            assert!(colors.slots(&local).repositories.len() <= RETIRED_PROJECTS + 2);
        }
        let recent_slot = colors.slots(&local).slot(&recent);
        assert!(recent_slot.is_some());
        colors.reconcile([(&local, &live)]);
        let returning = registry([recent.clone()]);
        colors.reconcile([(&local, &live), (&local, &returning)]);
        assert_eq!(colors.slots(&local).slot(&recent), recent_slot);
        assert!(colors.capacity() <= RETIRED_PROJECTS as u32 + 3);
        let mut oldest = project.clone();
        oldest.repository.as_mut().unwrap().common_dir = "/fixture/retired-0/.git".into();
        assert_eq!(colors.slots(&local).slot(&oldest), None);
        colors.reconcile([]);
        assert!(colors.slots(&local).repositories.len() <= RETIRED_PROJECTS);
    }

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
