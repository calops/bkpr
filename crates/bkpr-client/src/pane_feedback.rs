use super::*;

/// What a pane's centered notice says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Notice {
    /// The view is attaching; shown with a spinner until its first frame arrives.
    Opening,
    /// The frame places images the terminal cannot show.
    NeedsGraphics,
}

/// Centered pane notice: attach progress, or images the terminal cannot show.
pub(super) struct Feedback {
    notice: Notice,
    loading: loading::LoadingIndicator,
    pub(super) surface: UiSurface,
    pub(super) rect: Option<Rect>,
}

impl Feedback {
    fn new(viewport: Size, now: Instant, notice: Notice) -> Self {
        Self {
            notice,
            loading: loading::LoadingIndicator::new(
                now,
                Duration::from_millis(200),
                &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'],
            ),
            surface: UiSurface::new(Size {
                cols: 1,
                rows: 1,
                ..viewport
            }),
            rect: None,
        }
    }
}

impl App {
    /// Show "Loading pane…" until the view's first frame arrives.
    pub(super) fn begin_pane_attach(&mut self, pane: &str, viewport: Size) {
        let now = Instant::now();
        let mut feedback = Feedback::new(viewport, now, Notice::Opening);
        feedback.loading.update(true, false, now);
        self.pane_feedback.insert(pane.to_owned(), feedback);
    }

    pub(super) fn retire_pane_feedback(&mut self, pane: &str) {
        self.pane_feedback.remove(pane);
    }

    /// Resolve attach progress and the graphics-support notice from current frames.
    pub(super) fn sync_pane_feedback(&mut self, size: Size, rects: &[(String, Rect)]) {
        let now = Instant::now();
        self.colors.graphics.expire(now);
        let unsupported = !self.colors.graphics.supported() && self.colors.graphics.resolved();
        for (pane, outer) in rects {
            let Some(frame) = self.views.get(pane).and_then(|view| view.frame.as_ref()) else {
                continue;
            };
            let needs_graphics = unsupported && !frame.placements.is_empty();
            if !needs_graphics {
                // Inline frames arrive complete with their images: nothing is left to load.
                self.pane_feedback.remove(pane);
                continue;
            }
            let viewport = content(*outer).size(size);
            self.pane_feedback
                .entry(pane.clone())
                .or_insert_with(|| Feedback::new(viewport, now, Notice::NeedsGraphics))
                .notice = Notice::NeedsGraphics;
        }
    }

    pub(super) fn paint_pane_feedback(
        &mut self,
        size: Size,
        rects: &[(String, Rect)],
        theme: &Theme,
    ) {
        let now = Instant::now();
        for (pane, outer) in rects {
            let Some(feedback) = self.pane_feedback.get_mut(pane) else {
                continue;
            };
            feedback.rect = None;
            let area = content(*outer);
            let pending = feedback.notice == Notice::Opening;
            let label = match feedback.notice {
                Notice::Opening => "Loading pane…",
                Notice::NeedsGraphics => "Images require Kitty graphics support",
            };
            let width =
                (text_width(label).saturating_add(2).min(u16::MAX as usize) as u16).min(area.width);
            let rect = Rect {
                x: area.x + (area.width - width) / 2,
                y: area.y + area.height / 2,
                width,
                height: u16::from(area.height != 0),
            };
            let visible = !self.backgrounded
                && width != 0
                && rect.height != 0
                && !self.overlays.layers().any(|layer| {
                    layer.policy.modal
                        && layer.placement.outer.contains(rect.x, rect.y)
                        && layer.placement.outer.contains(rect.x + width - 1, rect.y)
                });
            feedback.loading.update(pending, visible, now);
            if !visible {
                continue;
            }
            let local = Rect {
                x: 0,
                y: 0,
                width,
                height: 1,
            };
            let local_size = Size {
                cols: width,
                rows: 1,
                ..size
            };
            if feedback.surface.frame.size != local_size {
                feedback.surface.paint(local_size, Vec::new(), None);
            }
            feedback.surface.frame.foreground = theme.style(Role::Base).foreground;
            feedback.surface.frame.background = theme.style(Role::Panel).background;
            let painted = if pending {
                feedback
                    .loading
                    .paint(&mut feedback.surface, local, label, Role::Base, theme)
                    .is_some()
            } else {
                feedback.surface.paint(
                    local_size,
                    vec![(local, window::fit(label, width), false)],
                    None,
                );
                true
            };
            if painted {
                for cell in &mut feedback.surface.frame.rows[0].cells {
                    cell.style.foreground_source = bkpr_protocol::ColorSource::Default;
                    cell.style.background_source = bkpr_protocol::ColorSource::Default;
                }
                if pending {
                    let warning = theme.style(Role::Warning);
                    let spinner = &mut feedback.surface.frame.rows[0].cells[0];
                    spinner.style.foreground = warning.foreground;
                    spinner.style.foreground_source = warning.foreground_source;
                }
                feedback.rect = Some(rect);
            }
        }
    }

    pub(super) fn pane_feedback_timeout(&self, timeout: i32, now: Instant) -> i32 {
        self.colors
            .graphics
            .next_deadline()
            .into_iter()
            .chain(
                self.pane_feedback
                    .values()
                    .filter_map(|feedback| feedback.loading.next_deadline()),
            )
            .fold(timeout, |timeout, deadline| {
                let remaining = deadline.saturating_duration_since(now);
                let millis =
                    remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
                timeout.min(millis.min(i32::MAX as u128) as i32)
            })
    }
}
