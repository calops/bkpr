//! Cached application-supplied semantic styles and native color evidence.
//! Palette recipes and visual policy belong to the caller, not the framework.
use crate::{ColorProbe, default_style};
use bkpr_protocol::{ColorSource, Style};
use std::sync::atomic::{AtomicU64, Ordering};

// A retained surface can switch between independently updated theme instances.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// Semantic style-table index; colors and attributes are supplied by application policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Role {
    /// Ordinary text and unaccented surface content.
    Base,
    /// Panel surface content.
    Panel,
    /// Structural borders and separators.
    Border,
    /// Deemphasized text.
    Muted,
    /// Headings and titles.
    Title,
    /// Explicitly selected content.
    Selected,
    /// Pointer-hovered content.
    Hover,
    /// Primary accent.
    Accent,
    /// Successful state.
    Success,
    /// Warning state.
    Warning,
    /// Error state.
    Error,
    /// Informational state.
    Info,
    /// Secondary accent.
    Secondary,
    /// Emphasized primary accent.
    BrightAccent,
    /// Emphasized informational accent.
    BrightInfo,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Evidence {
    truecolor: bool,
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    ansi: [Option<[u8; 3]>; 16],
}

/// Complete resolved style tables, supplied by the application policy.
pub struct ThemeStyles {
    /// Ordinary-surface styles, indexed by `Role as usize`.
    pub base: [Style; 15],
    /// Selection styles, indexed by `Role as usize`.
    pub selected: [Style; 15],
    /// Panel-surface styles, indexed by `Role as usize`.
    pub panel: [Style; 15],
}

/// Retained application-supplied styles keyed by observed color evidence and policy.
/// Defaults to terminal-default styles until updated; graphics and cursor evidence are separate.
pub struct Theme {
    evidence: Evidence,
    policy_key: u64,
    styles: [Style; 15],
    selected: [Style; 15],
    panels: [Style; 15],
    revision: u64,
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            evidence: Evidence::default(),
            policy_key: 0,
            styles: std::array::from_fn(|_| default_style()),
            selected: std::array::from_fn(|_| default_style()),
            panels: std::array::from_fn(|_| default_style()),
            revision: 0,
        }
    }
}
impl Theme {
    /// Rebuild styles and return `true` when color evidence or `policy_key` changes.
    /// The key must distinguish policy inputs other than truecolor, default colors
    /// and ANSI entries 0–15; unchanged inputs do not invoke `policy`.
    pub fn update(
        &mut self,
        probe: &ColorProbe,
        policy_key: u64,
        policy: impl FnOnce(&ColorProbe) -> ThemeStyles,
    ) -> bool {
        let evidence = Evidence {
            truecolor: probe.truecolor,
            foreground: probe.foreground,
            background: probe.background,
            ansi: std::array::from_fn(|index| probe.palette.get(&index).copied()),
        };
        if self.revision != 0 && self.evidence == evidence && self.policy_key == policy_key {
            return false;
        }
        self.evidence = evidence;
        self.policy_key = policy_key;
        let styles = policy(probe);
        self.styles = styles.base;
        self.selected = styles.selected;
        self.panels = styles.panel;
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        true
    }
    /// Process-local styling cache token; zero until the first update.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Ordinary-surface style for `role`.
    pub fn style(&self, role: Role) -> &Style {
        &self.styles[role as usize]
    }
    /// Selection style for `role`.
    pub fn selected_style(&self, role: Role) -> &Style {
        &self.selected[role as usize]
    }
    /// Panel-surface style for `role`.
    pub fn panel_style(&self, role: Role) -> &Style {
        &self.panels[role as usize]
    }
    /// Visible ink from the ordinary or selected style, accounting for inverse.
    /// Inverse terminal-default ink resolves to observed background RGB when known.
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
    /// Displayed background from the ordinary or selected style, accounting for inverse.
    /// Inverse terminal-default background resolves to observed foreground RGB when known.
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
    /// Visible ink for a style; unknown inverse terminal-default ink returns `None`.
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
    /// Ordinary or selected style for the requested role.
    pub(crate) fn resolved(&self, role: Role, selected: bool) -> &Style {
        if selected {
            &self.selected[role as usize]
        } else {
            self.style(role)
        }
    }
    /// Select selection styles first, otherwise panel styles only for a panel base.
    pub(crate) fn on_surface(&self, role: Role, selected: bool, base: Role) -> &Style {
        if selected {
            &self.selected[role as usize]
        } else if base == Role::Panel {
            &self.panels[role as usize]
        } else {
            self.style(role)
        }
    }
    /// Resolve a foreground source using observed defaults and ANSI entries.
    pub(crate) fn resolve_foreground(&self, source: ColorSource) -> [u8; 3] {
        self.resolve_color(source, self.defaults().0)
    }
    /// Resolve a background source using observed defaults and ANSI entries.
    pub(crate) fn resolve_background(&self, source: ColorSource) -> [u8; 3] {
        self.resolve_color(source, self.defaults().1)
    }
    fn resolve_color(&self, source: ColorSource, default: [u8; 3]) -> [u8; 3] {
        match source {
            ColorSource::Default => default,
            ColorSource::Palette(index) => self
                .evidence
                .ansi
                .get(usize::from(index))
                .copied()
                .flatten()
                .unwrap_or(default),
            ColorSource::Rgb(color) => color,
        }
    }
    /// Observed default RGB colors, or scene fallback RGB for unresolved channels.
    pub(crate) fn defaults(&self) -> ([u8; 3], [u8; 3]) {
        let fallback = default_style();
        (
            self.evidence.foreground.unwrap_or(fallback.foreground),
            self.evidence.background.unwrap_or(fallback.background),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_is_called_once_per_color_evidence_not_graphics_metadata() {
        let mut theme = Theme::default();
        let mut host = ColorProbe::default();
        let calls = std::cell::Cell::new(0);
        let policy = |_: &ColorProbe| {
            calls.set(calls.get() + 1);
            let mut base = std::array::from_fn(|_| default_style());
            base[Role::Title as usize].foreground_source = ColorSource::Palette(6);
            ThemeStyles {
                selected: base.clone(),
                panel: base.clone(),
                base,
            }
        };
        assert!(theme.update(&host, 0, policy));
        assert_eq!(
            theme.style(Role::Title).foreground_source,
            ColorSource::Palette(6)
        );
        host.revision += 1;
        assert!(!theme.update(&host, 0, policy));
        assert_eq!(calls.get(), 1);
        host.background = Some([12, 23, 34]);
        assert!(theme.update(&host, 0, policy));
        assert_eq!(calls.get(), 2);
    }
}
