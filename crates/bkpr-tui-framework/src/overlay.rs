//! Local overlay ownership for the existing scene compositor.
//!
//! Layers own geometry and input/focus lifecycle, never product content or IO.
//! Submit their frames through `Scene` in `layers()` order after base surfaces.
use crate::{Rect, window};
use bkpr_protocol::Size;

/// Input ownership and owner-handled dismissal rules for a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Claims keyboard input and blocks pointer input to lower layers.
    pub modal: bool,
    /// Enables pointer targeting within the outer rectangle.
    pub pointer: bool,
    /// Allows Escape to request dismissal when this is the top modal layer.
    pub dismiss_on_escape: bool,
    /// Allows an outside press to request dismissal of a modal layer.
    pub dismiss_on_outside: bool,
}
impl Policy {
    /// Pointer-targetable layer without keyboard ownership or dismissal.
    pub const PASSIVE: Self = Self {
        modal: false,
        pointer: true,
        dismiss_on_escape: false,
        dismiss_on_outside: false,
    };
    /// Modal layer dismissible by Escape or an outside press.
    pub const MODAL: Self = Self {
        modal: true,
        pointer: true,
        dismiss_on_escape: true,
        dismiss_on_outside: true,
    };
}

/// Screen-coordinate outer bounds and content hit region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Full layer bounds, including any frame.
    pub outer: Rect,
    /// Content bounds within the outer rectangle.
    pub content: Rect,
}
impl Placement {
    /// Centers and clips a framed layer to the screen.
    pub fn centered(screen: Size, width: u16, height: u16) -> Self {
        Self::framed(window::centered(screen, width, height), screen)
    }
    /// Places a framed layer beside a sidebar, or returns no usable placement.
    pub fn beside(screen: Size, sidebar: u16, row: u16, width: u16, height: u16) -> Option<Self> {
        window::beside(screen, sidebar, row, width, height).map(|outer| Self::framed(outer, screen))
    }
    /// Clips outer bounds and derives the standard window content inset.
    pub fn framed(outer: Rect, screen: Size) -> Self {
        let outer = clip(outer, screen);
        let inset = window::content(outer.size(screen));
        Self {
            outer,
            content: Rect {
                x: outer.x.saturating_add(inset.x),
                y: outer.y.saturating_add(inset.y),
                width: inset.width.min(outer.width),
                height: inset.height.min(outer.height),
            },
        }
    }
}
fn clip(rect: Rect, screen: Size) -> Rect {
    Rect {
        x: rect.x.min(screen.cols),
        y: rect.y.min(screen.rows),
        width: rect.width.min(screen.cols.saturating_sub(rect.x)),
        height: rect.height.min(screen.rows.saturating_sub(rect.y)),
    }
}
fn intersect(rect: Rect, bounds: Rect) -> Rect {
    let x = rect.x.max(bounds.x);
    let y = rect.y.max(bounds.y);
    Rect {
        x,
        y,
        width: rect
            .x
            .saturating_add(rect.width)
            .min(bounds.x.saturating_add(bounds.width))
            .saturating_sub(x),
        height: rect
            .y
            .saturating_add(rect.height)
            .min(bounds.y.saturating_add(bounds.height))
            .saturating_sub(y),
    }
}

/// Retained layer geometry, policy, and opaque cancellation focus token.
pub struct Layer<F> {
    /// Stable identity used for updates, removal, and input routing.
    pub id: &'static str,
    /// Screen-coordinate layer and content bounds.
    pub placement: Placement,
    /// Input ownership and dismissal rules.
    pub policy: Policy,
    return_focus: Option<F>,
}
/// Pointer lifecycle phase supplied by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    /// Begins a captured gesture when a layer is targeted.
    Press,
    /// Updates pointer position, retaining any gesture owner.
    Motion,
    /// Ends the captured gesture.
    Release,
    /// Routes wheel input independently of gesture capture.
    Wheel,
}
/// Routing result; the owner performs any requested dismissal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerRoute {
    /// Deliver to the underlying application.
    Pass,
    /// Consume without delivering to a layer.
    Block,
    /// Request dismissal of the identified modal layer.
    Dismiss(&'static str),
    /// Deliver to the identified layer.
    Target {
        /// Stable layer identity.
        id: &'static str,
        /// Whether the pointer lies within the content region.
        content: bool,
    },
}
#[derive(Clone, Copy)]
enum Capture {
    Layer(&'static str),
    Retired,
}

/// Focus tokens are opaque consumer values. Updating/reflowing a layer preserves
/// its original token; removing it returns that token for cancellation only.
/// Successful product actions discard it instead of undoing the new selection.
pub struct OverlayStack<F> {
    layers: Vec<Layer<F>>,
    captured: Option<Capture>,
}
impl<F> Default for OverlayStack<F> {
    fn default() -> Self {
        Self {
            layers: Vec::new(),
            captured: None,
        }
    }
}
impl<F> OverlayStack<F> {
    /// Inserts a layer or updates its geometry and policy without replacing
    /// its focus token. Passive layers remain below modal layers after policy changes.
    pub fn present(
        &mut self,
        id: &'static str,
        placement: Placement,
        policy: Policy,
        focus: Option<F>,
    ) {
        let placement = Placement {
            content: intersect(placement.content, placement.outer),
            ..placement
        };
        let layer = if let Some(index) = self.layers.iter().position(|layer| layer.id == id) {
            let layer = &mut self.layers[index];
            let modal_changed = layer.policy.modal != policy.modal;
            layer.placement = placement;
            layer.policy = policy;
            if !modal_changed {
                return;
            }
            self.layers.remove(index)
        } else {
            Layer {
                id,
                placement,
                policy,
                return_focus: focus,
            }
        };
        let index = if policy.modal {
            self.layers.len()
        } else {
            self.layers
                .iter()
                .position(|layer| layer.policy.modal)
                .unwrap_or(self.layers.len())
        };
        self.layers.insert(index, layer);
    }
    /// Removes a layer and returns its cancellation focus token, if any.
    pub fn remove(&mut self, id: &str) -> Option<F> {
        let index = self.layers.iter().position(|layer| layer.id == id)?;
        // Preserve a removed owner's gesture tombstone until release. Never hand
        // the remainder of a dismissed overlay's click to the underlying app.
        if matches!(self.captured, Some(Capture::Layer(owner)) if owner == id) {
            self.captured = Some(Capture::Retired);
        }
        self.layers.remove(index).return_focus
    }
    /// Looks up a retained layer by identity.
    pub fn layer(&self, id: &str) -> Option<&Layer<F>> {
        self.layers.iter().find(|layer| layer.id == id)
    }
    /// Iterates layers in back-to-front composition order.
    pub fn layers(&self) -> impl Iterator<Item = &Layer<F>> {
        self.layers.iter()
    }
    /// Returns the top modal layer, which owns keyboard input.
    pub fn keyboard_owner(&self) -> Option<&'static str> {
        self.layers
            .iter()
            .rev()
            .find(|layer| layer.policy.modal)
            .map(|layer| layer.id)
    }
    /// Returns the top modal layer only if it permits Escape dismissal.
    pub fn escape_target(&self) -> Option<&'static str> {
        self.layers
            .iter()
            .rev()
            .find(|layer| layer.policy.modal)
            .filter(|layer| layer.policy.dismiss_on_escape)
            .map(|layer| layer.id)
    }
    /// Clips existing bounds without restoring space lost to earlier resizes.
    pub fn resize(&mut self, screen: Size) {
        for layer in &mut self.layers {
            layer.placement.outer = clip(layer.placement.outer, screen);
            layer.placement.content =
                intersect(clip(layer.placement.content, screen), layer.placement.outer);
        }
    }
    /// Routes screen-cell coordinates, capturing presses through release.
    /// Removed gesture owners continue blocking until release; wheels route afresh.
    pub fn route_pointer(&mut self, x: u16, y: u16, phase: PointerPhase) -> PointerRoute {
        if phase != PointerPhase::Wheel
            && let Some(captured) = self.captured
        {
            if phase == PointerPhase::Release {
                self.captured = None;
            }
            let Capture::Layer(id) = captured else {
                return PointerRoute::Block;
            };
            return self
                .layer(id)
                .map_or(PointerRoute::Block, |layer| PointerRoute::Target {
                    id,
                    content: layer.placement.content.contains(x, y),
                });
        }
        for layer in self.layers.iter().rev() {
            if layer.policy.pointer && layer.placement.outer.contains(x, y) {
                if phase == PointerPhase::Press {
                    self.captured = Some(Capture::Layer(layer.id));
                }
                return PointerRoute::Target {
                    id: layer.id,
                    content: layer.placement.content.contains(x, y),
                };
            }
            if layer.policy.modal {
                if phase == PointerPhase::Press && layer.policy.dismiss_on_outside {
                    self.captured = Some(Capture::Layer(layer.id));
                    return PointerRoute::Dismiss(layer.id);
                }
                return PointerRoute::Block;
            }
        }
        PointerRoute::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn placement() -> Placement {
        Placement {
            outer: Rect {
                x: 2,
                y: 2,
                width: 8,
                height: 6,
            },
            content: Rect {
                x: 3,
                y: 3,
                width: 6,
                height: 4,
            },
        }
    }
    #[test]
    fn passive_never_steals_keyboard_and_modal_restores_original_focus() {
        let mut stack = OverlayStack::default();
        stack.present("hint", placement(), Policy::PASSIVE, None);
        assert_eq!(stack.keyboard_owner(), None);
        stack.present("form", placement(), Policy::MODAL, Some("terminal"));
        stack.present("form", placement(), Policy::MODAL, Some("wrong"));
        stack.present("guide", placement(), Policy::PASSIVE, None);
        assert_eq!(stack.layers().last().unwrap().id, "form");
        assert_eq!(stack.keyboard_owner(), Some("form"));
        assert_eq!(stack.remove("form"), Some("terminal"));
        assert_eq!(stack.keyboard_owner(), None);
    }
    #[test]
    fn policy_transitions_keep_pointer_and_keyboard_modal_ownership_aligned() {
        let mut stack = OverlayStack::default();
        stack.present("changing", placement(), Policy::MODAL, Some("original"));
        stack.present("modal", placement(), Policy::MODAL, Some("other"));
        stack.present(
            "changing",
            placement(),
            Policy::PASSIVE,
            Some("replacement"),
        );
        assert_eq!(stack.keyboard_owner(), Some("modal"));
        assert_eq!(
            stack.route_pointer(3, 3, PointerPhase::Wheel),
            PointerRoute::Target {
                id: "modal",
                content: true
            }
        );
        stack.remove("modal");
        stack.present("passive", placement(), Policy::PASSIVE, None);
        stack.present("changing", placement(), Policy::MODAL, None);
        assert_eq!(stack.keyboard_owner(), Some("changing"));
        assert_eq!(
            stack.route_pointer(3, 3, PointerPhase::Wheel),
            PointerRoute::Target {
                id: "changing",
                content: true
            }
        );
        assert_eq!(stack.remove("changing"), Some("original"));
    }
    #[test]
    fn dismissal_eats_entire_gesture_even_after_layer_is_removed() {
        let mut stack = OverlayStack::<()>::default();
        stack.present("form", placement(), Policy::MODAL, None);
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Press),
            PointerRoute::Dismiss("form")
        );
        stack.remove("form");
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Motion),
            PointerRoute::Block
        );
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Release),
            PointerRoute::Block
        );
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Press),
            PointerRoute::Pass
        );
    }
    #[test]
    fn replaced_owner_does_not_receive_a_retired_gestures_release() {
        let mut stack = OverlayStack::<()>::default();
        stack.present("form", placement(), Policy::MODAL, None);
        stack.route_pointer(3, 3, PointerPhase::Press);
        stack.remove("form");
        stack.present("form", placement(), Policy::MODAL, None);
        assert_eq!(
            stack.route_pointer(3, 3, PointerPhase::Release),
            PointerRoute::Block
        );
        assert_eq!(
            stack.route_pointer(3, 3, PointerPhase::Press),
            PointerRoute::Target {
                id: "form",
                content: true
            }
        );
    }
    #[test]
    fn pointer_capture_and_wheel_use_visible_owner_without_leaking() {
        let mut stack = OverlayStack::<()>::default();
        stack.present("card", placement(), Policy::PASSIVE, None);
        assert_eq!(
            stack.route_pointer(3, 3, PointerPhase::Press),
            PointerRoute::Target {
                id: "card",
                content: true
            }
        );
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Motion),
            PointerRoute::Target {
                id: "card",
                content: false
            }
        );
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Wheel),
            PointerRoute::Pass
        );
        assert_eq!(
            stack.route_pointer(0, 0, PointerPhase::Release),
            PointerRoute::Target {
                id: "card",
                content: false
            }
        );
    }
    #[test]
    fn resize_clips_hit_regions_and_preserves_focus_token() {
        let mut stack = OverlayStack::default();
        stack.present("form", placement(), Policy::MODAL, Some(7));
        stack.resize(Size {
            cols: 4,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        });
        assert_eq!(stack.layer("form").unwrap().placement.outer.width, 2);
        assert_eq!(stack.layer("form").unwrap().placement.content.height, 1);
        assert_eq!(stack.remove("form"), Some(7));
    }
}
