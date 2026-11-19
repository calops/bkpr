use super::*;
use crate::resources::ResourcePhase;

pub(super) struct Feedback {
    view: String,
    revision: Option<u64>,
    viewport: Size,
    supported: bool,
    opening: bool,
    keys: Vec<ImageKey>,
    missing: bool,
    pending: bool,
    failed: bool,
    reported: bool,
    disabled: bool,
    started: Instant,
    loading: loading::LoadingIndicator,
    pub(super) diagnostic: Option<diagnostics::DiagnosticId>,
    pub(super) surface: UiSurface,
    pub(super) rect: Option<Rect>,
}

impl Feedback {
    fn new(view: &str, viewport: Size, now: Instant) -> Self {
        Self {
            view: view.to_owned(),
            revision: None,
            viewport,
            supported: false,
            opening: false,
            keys: Vec::new(),
            pending: false,
            failed: false,
            reported: false,
            missing: false,
            disabled: false,
            started: now,
            loading: loading::LoadingIndicator::new(
                now,
                Duration::from_millis(200),
                &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'],
            ),
            diagnostic: None,
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
    pub(super) fn begin_image_attach(&mut self, pane: &str, viewport: Size) {
        let now = Instant::now();
        let mut feedback = Feedback::new("", viewport, now);
        feedback.opening = true;
        feedback.loading.update(true, false, now);
        self.image_feedback.insert(pane.to_owned(), feedback);
    }

    pub(super) fn retire_image_resources(&mut self, pane: &str) {
        if let Some(attached) = self.views.get(pane) {
            self.resources.retire_view(pane, &attached.view);
        }
        self.image_feedback.remove(pane);
    }

    pub(super) fn sync_image_resources(&mut self, size: Size, rects: &[(String, Rect)]) {
        let now = Instant::now();
        self.colors.graphics.expire(now);
        let dirty = if self.image_runtime_failed {
            false
        } else {
            match self.resources.poll() {
                Ok(dirty) => dirty,
                Err(error) => {
                    self.image_runtime_failed = true;
                    self.report_failure(
                        "Couldn't receive new images. Existing content is still available.",
                        &error,
                    );
                    for (pane, attached) in &self.views {
                        self.resources.retire_view(pane, &attached.view);
                    }
                    true
                }
            }
        };
        let supported = self.colors.graphics.supported();
        let mut incidents = Vec::new();
        for (pane, outer) in rects {
            let viewport = content(*outer).size(size);
            let Some(attached) = self.views.get_mut(pane) else {
                continue;
            };
            let (Some(frame), Some(manifest)) = (&attached.frame, &attached.image_manifest) else {
                continue;
            };
            let feedback = self
                .image_feedback
                .entry(pane.clone())
                .or_insert_with(|| Feedback::new(&attached.view, viewport, now));
            if feedback.opening {
                feedback.opening = false;
                feedback.view.clone_from(&attached.view);
            }
            if feedback.view != attached.view {
                self.resources.retire_view(pane, &feedback.view);
                *feedback = Feedback::new(&attached.view, viewport, now);
            }
            let changed = feedback.revision != Some(attached.revision)
                || feedback.viewport != viewport
                || feedback.supported != supported;
            if changed {
                feedback.revision = Some(attached.revision);
                feedback.viewport = viewport;
                feedback.supported = supported;
                let desired = visible_image_keys(frame, viewport, manifest).and_then(|keys| {
                    feedback.keys = keys;
                    let descriptors =
                        if supported && !feedback.disabled && !self.image_runtime_failed {
                            feedback
                                .keys
                                .iter()
                                .map(|key| {
                                    *manifest
                                        .descriptor(*key)
                                        .expect("visible keys belong to the validated manifest")
                                })
                                .collect::<Vec<_>>()
                        } else {
                            Vec::new()
                        };
                    self.resources
                        .sync_view(pane, &attached.view, &descriptors, |key| {
                            attached.images.get(&key).cloned()
                        })
                });
                if let Err(error) = desired {
                    if !feedback.disabled {
                        incidents.push((
                            pane.clone(),
                            format!(
                                "Stage: image demand\nPane: {pane}\nView: {}\n{error:#}",
                                attached.view
                            ),
                        ));
                    }
                    feedback.disabled = true;
                    self.resources.retire_view(pane, &attached.view);
                }
            }
            if dirty || changed {
                if let Err(error) = attached.sync_resources(&self.resources) {
                    if !feedback.disabled {
                        incidents.push((
                            pane.clone(),
                            format!(
                                "Stage: image installation\nPane: {pane}\nView: {}\n{error:#}",
                                attached.view
                            ),
                        ));
                    }
                    feedback.disabled = true;
                    self.resources.retire_view(pane, &attached.view);
                }
                feedback.missing = feedback
                    .keys
                    .iter()
                    .any(|key| !attached.images.contains_key(key));
                let statuses = self.resources.statuses(pane, &attached.view);
                feedback.pending = statuses.iter().any(|status| {
                    !matches!(status.phase, ResourcePhase::Ready | ResourcePhase::Failed)
                });
                feedback.failed = statuses
                    .iter()
                    .any(|status| status.phase == ResourcePhase::Failed);
                if !feedback.failed {
                    feedback.reported = false;
                }
                if !feedback.reported
                    && let Some(failure) = statuses
                        .iter()
                        .find(|status| status.phase == ResourcePhase::Failed)
                {
                    feedback.reported = true;
                    let descriptor = attached
                        .image_manifest
                        .as_ref()
                        .and_then(|manifest| manifest.descriptor(failure.key));
                    incidents.push((pane.clone(), format!(
                        "Stage: image resource {:?}\nPane: {pane}\nView: {}\nResource: {:?}\nDescriptor: {descriptor:?}\nReceived encoded bytes: {}\nDeclared encoded bytes: {:?}\nElapsed: {:?}\n{}",
                        failure.phase, attached.view, failure.key, failure.received, failure.total,
                        now.saturating_duration_since(feedback.started),
                        failure.error.as_ref().map_or_else(|| "No further details were provided.".to_owned(), |error| format!("{error:#}")),
                    )));
                }
            }
        }
        for (pane, details) in incidents {
            let id = self.report_details(
                "An image couldn't load. You can keep using this pane.",
                &details,
                diagnostics::Outcome::NotApplicable,
            );
            if let Some(feedback) = self.image_feedback.get_mut(&pane) {
                feedback.diagnostic = Some(id);
            }
        }
    }

    pub(super) fn paint_image_feedback(
        &mut self,
        size: Size,
        rects: &[(String, Rect)],
        theme: &Theme,
    ) {
        let now = Instant::now();
        let supported = self.colors.graphics.supported();
        let unresolved = !self.colors.graphics.resolved();
        for (pane, outer) in rects {
            let Some(feedback) = self.image_feedback.get_mut(pane) else {
                continue;
            };
            feedback.rect = None;
            let area = content(*outer);
            let failed = feedback.missing
                && (feedback.failed || feedback.disabled || self.image_runtime_failed);
            let pending = feedback.opening
                || (feedback.missing
                    && !feedback.disabled
                    && !self.image_runtime_failed
                    && (supported && feedback.pending || unresolved));
            let label = if feedback.opening {
                "Loading pane…"
            } else if pending {
                if failed {
                    "Loading images… · some unavailable"
                } else {
                    "Loading images…"
                }
            } else if failed {
                "Images unavailable · Ctrl-b !"
            } else if !supported && !unresolved && !feedback.keys.is_empty() {
                "Images require Kitty graphics support"
            } else {
                ""
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
            if !visible || label.is_empty() {
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

    pub(super) fn image_timeout(&self, timeout: i32, now: Instant) -> i32 {
        self.colors
            .graphics
            .next_deadline()
            .into_iter()
            .chain(
                self.image_feedback
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
