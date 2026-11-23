use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct NoticeText<'a> {
    pub(super) title: &'static str,
    pub(super) session: Option<(&'static str, &'a str)>,
    pub(super) detail: &'a str,
    pub(super) hint: &'static str,
    pub(super) role: Role,
}

impl<'a> NoticeText<'a> {
    pub(super) fn pane(
        title: &'static str,
        detail: &'a str,
        hint: &'static str,
        role: Role,
    ) -> Self {
        Self {
            title,
            session: None,
            detail,
            hint,
            role,
        }
    }

    pub(super) fn session(group: &'a crate::controller::SessionGroup) -> Option<Self> {
        use crate::controller::SessionStatus;
        let title = match group.status {
            SessionStatus::Online => return None,
            SessionStatus::Offline => "Session offline",
            SessionStatus::Rejected => "Session identity changed",
            SessionStatus::Queued => "Connection queued",
            SessionStatus::Reconnecting => "Reconnecting",
            _ => "Connecting",
        };
        Some(Self {
            title,
            session: Some((group.status.badge(), &group.name)),
            detail: &group.detail,
            hint: "",
            role: group.status.role(),
        })
    }
}

/// Retains the cell layout independently of placement, theme and material readiness.
pub(super) struct NoticeCard {
    surface: UiSurface,
    layout_size: Option<Size>,
    outer: Rect,
    title_rect: Rect,
    title: &'static str,
    session: Option<(&'static str, String)>,
    detail: String,
    hint: &'static str,
    role: Role,
    graphical: bool,
}

impl NoticeCard {
    pub(super) fn new(size: Size) -> Self {
        Self {
            surface: UiSurface::new(Size {
                cols: 1,
                rows: 1,
                ..size
            }),
            layout_size: None,
            outer: Rect::default(),
            title_rect: Rect::default(),
            title: "",
            session: None,
            detail: String::new(),
            hint: "",
            role: Role::Muted,
            graphical: false,
        }
    }

    pub(super) fn layout(&mut self, area: Rect, size: Size, text: NoticeText<'_>) {
        let layout_size = area.size(size);
        if self.layout_size != Some(layout_size)
            || self.title != text.title
            || self
                .session
                .as_ref()
                .map(|(badge, name)| (*badge, name.as_str()))
                != text.session
            || self.detail != text.detail
            || self.hint != text.hint
            || (self.role == Role::Error) != (text.role == Role::Error)
        {
            self.layout_size = Some(layout_size);
            self.title = text.title;
            self.session = text.session.map(|(badge, name)| (badge, name.to_owned()));
            self.detail.clear();
            self.detail.push_str(text.detail);
            self.hint = text.hint;

            let title = text
                .session
                .map(|(badge, name)| format!("{badge} {} · {name}", text.title));
            let layout = window::notice_rows(
                Rect { x: 0, y: 0, ..area },
                size,
                title.as_deref().unwrap_or(text.title),
                [text.detail, text.hint]
                    .into_iter()
                    .filter(|line| !line.is_empty()),
                text.role == Role::Error,
            );
            self.outer = Rect::default();
            if let Some((outer, rows, title)) = layout {
                self.outer = outer;
                self.title_rect = title;
                self.surface.paint(outer.size(size), rows, None);
            }
        }
        self.role = text.role;
        self.outer.x = area.x + (area.width - self.outer.width) / 2;
        self.outer.y = area.y + (area.height - self.outer.height) / 2;
    }

    pub(super) fn decorate(
        &mut self,
        theme: &Theme,
        graphical_theme: &Theme,
        graphics: &crate::chrome_graphics::ChromeGraphics,
        effects: &mut Frame,
        base: u32,
    ) {
        let size = self.surface.frame.size;
        let framed = self.role == Role::Error && self.outer.width >= 3 && self.outer.height >= 3;
        let body = window::content(size);
        self.graphical = framed
            && graphics.decorate_region(
                effects,
                self.outer,
                Rect {
                    x: self.outer.x + body.x,
                    y: self.outer.y + body.y,
                    ..body
                },
                base,
                Some(1),
            );
        let mut spans = window::theme_spans(size);
        if !framed {
            for span in &mut spans {
                span.rect = Rect::default();
            }
        } else if !self.graphical {
            for span in &mut spans[1..] {
                span.role = Role::Error;
            }
        }
        let title = RoleSpan {
            rect: self.title_rect,
            role: if self.role == Role::Error {
                Role::Title
            } else {
                self.role
            },
        };
        let base = if framed { Role::Panel } else { Role::Muted };
        let theme = if self.graphical {
            graphical_theme
        } else {
            theme
        };
        self.surface.apply_theme(
            theme,
            base,
            &[spans[0], spans[1], spans[2], spans[3], spans[4], title],
        );
    }

    pub(super) fn layer(&self, id: &'static str, faint: bool) -> Surface<'_> {
        Surface {
            id,
            palette: None,
            content: SurfaceContent::Annotation(if self.graphical {
                window::content(self.surface.frame.size)
            } else {
                Rect {
                    x: 0,
                    y: 0,
                    ..self.outer
                }
            }),
            rect: self.outer,
            frame: &self.surface.frame,
            selection: &[],
            search: &[],
            focused: false,
            faint,
        }
    }
}

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
    painted: Option<(Notice, Option<char>)>,
    spinner_col: Option<u16>,
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
            painted: None,
            spinner_col: None,
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
            if let Some(feedback) = self.pane_feedback.get_mut(pane) {
                feedback.notice = Notice::NeedsGraphics;
            } else {
                let viewport = content(*outer).size(size);
                self.pane_feedback.insert(
                    pane.clone(),
                    Feedback::new(viewport, now, Notice::NeedsGraphics),
                );
            }
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
            let width = (text_width(label) as u16 + if pending { 2 } else { 0 }).min(area.width);
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
            let glyph = feedback.loading.glyph();
            if !visible || pending && glyph.is_none() {
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
            let key = (feedback.notice, glyph);
            if feedback.surface.frame.size != local_size || feedback.painted != Some(key) {
                if pending {
                    if feedback.surface.frame.size != local_size {
                        feedback.surface.paint(local_size, Vec::new(), None);
                    }
                    feedback.spinner_col = feedback
                        .loading
                        .paint(&mut feedback.surface, local, label, Role::Base, theme)
                        .map(|rect| rect.x);
                } else {
                    feedback.surface.paint(
                        local_size,
                        vec![(local, window::fit(label, width), false)],
                        None,
                    );
                    feedback.spinner_col = None;
                }
                for cell in &mut feedback.surface.frame.rows[0].cells {
                    cell.style.foreground_source = bkpr_protocol::ColorSource::Default;
                    cell.style.background_source = bkpr_protocol::ColorSource::Default;
                }
                feedback.painted = Some(key);
            }
            feedback.surface.frame.foreground = theme.style(Role::Base).foreground;
            feedback.surface.frame.background = theme.style(Role::Panel).background;
            if let Some(col) = feedback.spinner_col {
                let warning = theme.style(Role::Warning);
                let spinner = &mut feedback.surface.frame.rows[0].cells[usize::from(col)];
                spinner.style.foreground = warning.foreground;
                spinner.style.foreground_source = warning.foreground_source;
            }
            feedback.rect = Some(rect);
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
