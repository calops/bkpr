//! Local overlay ownership for the existing scene compositor.
//!
//! Layers own geometry and input/focus lifecycle, never product content or IO.
//! Submit their frames through `Scene` in `layers()` order after base surfaces.
use crate::{Rect, window};
use bkpr_protocol::Size;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub modal: bool,
    pub pointer: bool,
    pub dismiss_on_escape: bool,
    pub dismiss_on_outside: bool,
}
impl Policy {
    pub const PASSIVE: Self = Self {
        modal: false,
        pointer: true,
        dismiss_on_escape: false,
        dismiss_on_outside: false,
    };
    pub const MODAL: Self = Self {
        modal: true,
        pointer: true,
        dismiss_on_escape: true,
        dismiss_on_outside: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub outer: Rect,
    pub content: Rect,
}
impl Placement {
    pub fn centered(screen: Size, width: u16, height: u16) -> Self {
        Self::framed(window::centered(screen, width, height), screen)
    }
    pub fn beside(screen: Size, sidebar: u16, row: u16, width: u16, height: u16) -> Option<Self> {
        window::beside(screen, sidebar, row, width, height).map(|outer| Self::framed(outer, screen))
    }
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

pub struct Layer<F> {
    pub id: &'static str,
    pub placement: Placement,
    pub policy: Policy,
    return_focus: Option<F>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Press,
    Motion,
    Release,
    Wheel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerRoute {
    Pass,
    Block,
    Dismiss(&'static str),
    Target { id: &'static str, content: bool },
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
        if let Some(layer) = self.layers.iter_mut().find(|layer| layer.id == id) {
            layer.placement = placement;
            layer.policy = policy;
            return;
        }
        // Passive layers always remain below modal layers, regardless of update order.
        let index = if policy.modal {
            self.layers.len()
        } else {
            self.layers
                .iter()
                .position(|layer| layer.policy.modal)
                .unwrap_or(self.layers.len())
        };
        self.layers.insert(
            index,
            Layer {
                id,
                placement,
                policy,
                return_focus: focus,
            },
        );
    }
    pub fn remove(&mut self, id: &str) -> Option<F> {
        let index = self.layers.iter().position(|layer| layer.id == id)?;
        // Preserve a removed owner's gesture tombstone until release. Never hand
        // the remainder of a dismissed overlay's click to the underlying app.
        if matches!(self.captured, Some(Capture::Layer(owner)) if owner == id) {
            self.captured = Some(Capture::Retired);
        }
        self.layers.remove(index).return_focus
    }
    pub fn layer(&self, id: &str) -> Option<&Layer<F>> {
        self.layers.iter().find(|layer| layer.id == id)
    }
    pub fn layers(&self) -> impl Iterator<Item = &Layer<F>> {
        self.layers.iter()
    }
    pub fn keyboard_owner(&self) -> Option<&'static str> {
        self.layers
            .iter()
            .rev()
            .find(|layer| layer.policy.modal)
            .map(|layer| layer.id)
    }
    pub fn escape_target(&self) -> Option<&'static str> {
        self.layers
            .iter()
            .rev()
            .find(|layer| layer.policy.modal)
            .filter(|layer| layer.policy.dismiss_on_escape)
            .map(|layer| layer.id)
    }
    pub fn resize(&mut self, screen: Size) {
        for layer in &mut self.layers {
            layer.placement.outer = clip(layer.placement.outer, screen);
            layer.placement.content =
                intersect(clip(layer.placement.content, screen), layer.placement.outer);
        }
    }
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
