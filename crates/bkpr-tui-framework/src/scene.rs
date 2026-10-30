#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Rect {
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
    pub fn size(self, screen: Size) -> Size {
        Size {
            cols: self.width.max(1),
            rows: self.height.max(1),
            ..screen
        }
    }
}
/// Ordered opaque surfaces share clipping, hit testing and the sole physical output pass.
/// Resource namespaces are stable pane IDs, never native image IDs alone.
pub struct Surface<'a> {
    pub id: &'a str,
    pub rect: Rect,
    pub frame: &'a Frame,
    pub focused: bool,
}
pub struct Scene<'a> {
    pub size: Size,
    pub surfaces: Vec<Surface<'a>>,
}
impl Scene<'_> {
    pub fn hit_test(&self, x: u16, y: u16) -> Option<&str> {
        self.surfaces
            .iter()
            .rev()
            .find(|s| s.rect.contains(x, y))
            .map(|s| s.id)
    }
    pub fn hit_map(&self) -> HitMap {
        HitMap {
            regions: self
                .surfaces
                .iter()
                .map(|s| (s.id.to_owned(), s.rect))
                .collect(),
        }
    }
}
/// Owned event geometry from the exact scene submitted to the renderer.
pub struct HitMap {
    regions: Vec<(String, Rect)>,
}
impl HitMap {
    pub fn hit_test(&self, x: u16, y: u16) -> Option<&str> {
        self.regions
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(id, _)| id.as_str())
    }
}
pub fn default_style() -> Style {
    Style {
        foreground: [220, 220, 220],
        background: [24, 24, 24],
        underline_color: [220, 220, 220],
        foreground_source: ColorSource::Default,
        background_source: ColorSource::Default,
        underline_source: ColorSource::Default,
        bold: false,
        italic: false,
        faint: false,
        blink: false,
        inverse: false,
        invisible: false,
        strikethrough: false,
        overline: false,
        underline: 0,
    }
}
pub fn blank(size: Size) -> Frame {
    let style = default_style();
    Frame {
        size,
        rows: (0..size.rows)
            .map(|_| Row {
                cells: (0..size.cols)
                    .map(|_| Cell {
                        text: " ".into(),
                        width: 1,
                        style: style.clone(),
                        hyperlink: None,
                    })
                    .collect(),
            })
            .collect(),
        cursor: Cursor {
            x: 0,
            y: 0,
            visible: false,
            blinking: false,
            shape: CursorShape::Block,
            color: None,
        },
        foreground: style.foreground,
        background: style.background,
        palette: vec![
            [0, 0, 0],
            [205, 49, 49],
            [13, 188, 121],
            [229, 229, 16],
            [36, 114, 200],
            [188, 63, 188],
            [17, 168, 205],
            [229, 229, 229],
            [102, 102, 102],
            [241, 76, 76],
            [35, 209, 139],
            [245, 245, 67],
            [59, 142, 234],
            [214, 112, 214],
            [41, 184, 219],
            [255, 255, 255],
        ],
        images: Vec::new(),
        placements: Vec::new(),
        alternate_screen: false,
        scrollback_total: 0,
        scrollback_offset: 0,
    }
}
/// Trusted UI text still passes the same scene validation and damage renderer.
pub fn text(frame: &mut Frame, x: u16, y: u16, value: &str, selected: bool) {
    let foreground = frame.foreground;
    let background = frame.background;
    let Some(row) = frame.rows.get_mut(y as usize) else {
        return;
    };
    let mut column = x as usize;
    for ch in value.chars().filter(|c| !is_scene_protocol_char(*c)) {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            if column > x as usize {
                let mut lead = column - 1;
                while lead > x as usize && row.cells[lead].width == 0 {
                    lead -= 1;
                }
                row.cells[lead].text.push(ch);
            }
            continue;
        }
        if column + width > row.cells.len() {
            break;
        }
        let mut style = default_style();
        style.foreground = foreground;
        style.background = background;
        style.inverse = selected;
        let cell = &mut row.cells[column];
        cell.text.clear();
        cell.text.push(ch);
        cell.width = width as u8;
        cell.style.clone_from(&style);
        cell.hyperlink = None;
        for offset in 1..width {
            let cell = &mut row.cells[column + offset];
            cell.text.clear();
            cell.width = 0;
            cell.style.clone_from(&style);
            cell.hyperlink = None;
        }
        column += width;
    }
}
/// Apply confirmed physical defaults to trusted widget scenes only. Application
/// scenes retain their engine's own default-color provenance and values.
pub fn native_colors(frame: &mut Frame, host: &ColorProbe) {
    let foreground = host.foreground.unwrap_or(frame.foreground);
    let background = host.background.unwrap_or(frame.background);
    if foreground == frame.foreground && background == frame.background {
        return;
    }
    for cell in frame.rows.iter_mut().flat_map(|row| &mut row.cells) {
        if cell.style.foreground_source == ColorSource::Default {
            cell.style.foreground = foreground;
        }
        if cell.style.background_source == ColorSource::Default {
            cell.style.background = background;
        }
    }
    frame.foreground = foreground;
    frame.background = background;
}

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let guard = Self {
            graphics: GraphicsState::new(
                ((uuid::Uuid::new_v4().as_u128() as u32) & 0x3ffff800) | 0x80000000,
            ),
            kitty_keyboard: false,
            clipboard_mode: None,
        };
        let mut out = std::io::stdout();
        write!(
            out,
            "\x1b[?1049h\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1bP+q5463\x1b\\\x1bP+q524742\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b[?u\x1b[?5522$p"
        )?;
        for index in 0..256 {
            write!(out, "\x1b]4;{index};?\x1b\\")?;
        }
        out.flush()?;
        Ok(guard)
    }
}
/// Subtract an opaque surface from physical image projections. Only newly
/// clipped/rescaled visible fragments allocate raster bytes, with the original
/// sampler phase retained even across several overlapping UI layers.
fn occlude(placements: &mut Vec<geometry::Projection>, cover: Rect, size: Size) -> Result<()> {
    let cover = geometry::Rect {
        x: u32::from(cover.x) * size.cell_width,
        y: u32::from(cover.y) * size.cell_height,
        width: u32::from(cover.width) * size.cell_width,
        height: u32::from(cover.height) * size.cell_height,
    };
    let mut output = Vec::with_capacity(placements.len());
    for p in placements.drain(..) {
        let x = p.col * size.cell_width + p.offset_x;
        let y = p.row * size.cell_height + p.offset_y;
        let (width, height) = if let Some(k) = p.derived {
            (k.visible.width, k.visible.height)
        } else if let Some((c, r)) = p.cells {
            (
                c * size.cell_width - p.offset_x,
                r * size.cell_height - p.offset_y,
            )
        } else {
            (p.source.width, p.source.height)
        };
        let left = x.max(cover.x);
        let top = y.max(cover.y);
        let right = (x + width).min(cover.x + cover.width);
        let bottom = (y + height).min(cover.y + cover.height);
        if right <= left || bottom <= top {
            output.push(p);
            continue;
        }
        let fragments = [
            (x, y, width, top - y),
            (x, bottom, width, y + height - bottom),
            (x, top, left - x, bottom - top),
            (right, top, x + width - right, bottom - top),
        ];
        for (fx, fy, fw, fh) in fragments {
            if fw == 0 || fh == 0 {
                continue;
            }
            let mut fragment = p;
            fragment.col = fx / size.cell_width;
            fragment.row = fy / size.cell_height;
            fragment.offset_x = fx % size.cell_width;
            fragment.offset_y = fy % size.cell_height;
            fragment.cells = None;
            if p.derived.is_none() && p.cells.is_none() {
                fragment.source = geometry::Rect {
                    x: p.source.x + fx - x,
                    y: p.source.y + fy - y,
                    width: fw,
                    height: fh,
                };
            } else {
                let base = p.derived.unwrap_or(geometry::DerivedKey {
                    image: p.image,
                    source: p.source,
                    width,
                    height,
                    visible: geometry::Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                });
                fragment.derived = Some(geometry::DerivedKey {
                    visible: geometry::Rect {
                        x: base.visible.x + fx - x,
                        y: base.visible.y + fy - y,
                        width: fw,
                        height: fh,
                    },
                    ..base
                });
                fragment.source = geometry::Rect {
                    x: 0,
                    y: 0,
                    width: fw,
                    height: fh,
                };
            }
            output.push(fragment);
            if output.len() > 4096 {
                bail!("composed projection limit");
            }
        }
    }
    *placements = output;
    Ok(())
}

fn subtract(rect: Rect, cover: Rect) -> Vec<Rect> {
    let left = rect.x.max(cover.x);
    let top = rect.y.max(cover.y);
    let right = (rect.x + rect.width).min(cover.x + cover.width);
    let bottom = (rect.y + rect.height).min(cover.y + cover.height);
    if right <= left || bottom <= top {
        return vec![rect];
    }
    [
        (rect.x, rect.y, rect.width, top - rect.y),
        (rect.x, bottom, rect.width, rect.y + rect.height - bottom),
        (rect.x, top, left - rect.x, bottom - top),
        (right, top, rect.x + rect.width - right, bottom - top),
    ]
    .into_iter()
    .filter(|(_, _, w, h)| *w > 0 && *h > 0)
    .map(|(x, y, width, height)| Rect {
        x,
        y,
        width,
        height,
    })
    .collect()
}
fn blank_cell() -> &'static Cell {
    static BLANK: std::sync::LazyLock<Cell> = std::sync::LazyLock::new(|| Cell {
        text: " ".into(),
        width: 1,
        style: default_style(),
        hyperlink: None,
    });
    &BLANK
}
/// Clear trusted UI geometry in place, retaining each cell's text allocation.
pub fn clear(frame: &mut Frame, rect: Rect) {
    let mut style = default_style();
    style.foreground = frame.foreground;
    style.background = frame.background;
    for row in frame
        .rows
        .iter_mut()
        .skip(rect.y as usize)
        .take(rect.height as usize)
    {
        for cell in row
            .cells
            .iter_mut()
            .skip(rect.x as usize)
            .take(rect.width as usize)
        {
            if cell.text != " "
                || cell.width != 1
                || cell.style != style
                || cell.hyperlink.is_some()
            {
                cell.text.clear();
                cell.text.push(' ');
                cell.width = 1;
                cell.style.clone_from(&style);
                cell.hyperlink = None;
            }
        }
    }
}
fn effective_style(style: &Style, frame: &Frame) -> Style {
    let resolve = |source: ColorSource, default: [u8; 3], fallback: [u8; 3]| match source {
        ColorSource::Default => default,
        ColorSource::Palette(index) => frame
            .palette
            .get(index as usize)
            .copied()
            .unwrap_or(fallback),
        ColorSource::Rgb(rgb) => rgb,
    };
    let mut result = style.clone();
    result.foreground = resolve(style.foreground_source, frame.foreground, style.foreground);
    result.background = resolve(style.background_source, frame.background, style.background);
    result.underline_color = resolve(
        style.underline_source,
        result.foreground,
        style.underline_color,
    );
    result
}
fn copy_cell(destination: &mut Cell, source: &Cell, style: &Style) {
    if destination.text != source.text
        || destination.width != source.width
        || destination.style != *style
        || destination.hyperlink != source.hyperlink
    {
        destination.text.clone_from(&source.text);
        destination.width = source.width;
        destination.style.clone_from(style);
        destination.hyperlink.clone_from(&source.hyperlink);
    }
}
fn compose_cells(canvas: &mut Frame, surface: &Surface<'_>, visible: &[Rect]) {
    let blank = blank_cell();
    let blank_style = effective_style(&blank.style, surface.frame);
    let r = surface.rect;
    for clip in visible {
        let left = clip.x - r.x;
        let right = left + clip.width;
        for y in clip.y..clip.y + clip.height {
            let target = &mut canvas.rows[y as usize].cells;
            let mut x = 0u16;
            for cell in surface
                .frame
                .rows
                .get((y - r.y) as usize)
                .into_iter()
                .flat_map(|row| &row.cells)
            {
                if cell.width == 0 {
                    continue;
                }
                let end = x.saturating_add(u16::from(cell.width));
                if x >= right {
                    break;
                }
                if end > left && x < right {
                    if x >= left && end <= right {
                        let style = effective_style(&cell.style, surface.frame);
                        copy_cell(&mut target[(r.x + x) as usize], cell, &style);
                        for tail in x + 1..end {
                            let destination = &mut target[(r.x + tail) as usize];
                            if destination.width != 0
                                || !destination.text.is_empty()
                                || destination.style != style
                                || destination.hyperlink.is_some()
                            {
                                destination.text.clear();
                                destination.width = 0;
                                destination.style.clone_from(&style);
                                destination.hyperlink = None;
                            }
                        }
                    } else {
                        for column in x.max(left)..end.min(right) {
                            copy_cell(&mut target[(r.x + column) as usize], blank, &blank_style);
                        }
                    }
                }
                x = end;
            }
            for column in x.max(left)..right {
                copy_cell(&mut target[(r.x + column) as usize], blank, &blank_style);
            }
        }
    }
}

/// A client owns one renderer and physical resource allocator. Borrowed immutable
/// pixels are never cloned when a pane-local generation is given a physical name.
#[derive(Default)]
pub struct Renderer {
    aliases: HashMap<(String, ImageKey), ImageKey>,
    next_alias: u32,
    canvas: Option<Frame>,
    layout: Vec<(String, Rect)>,
    visible: Vec<Vec<Rect>>,
}
impl Renderer {
    pub fn render(
        &mut self,
        out: &mut impl Write,
        scene: &Scene<'_>,
        host: &ColorProbe,
        terminal: &mut TerminalGuard,
    ) -> Result<()> {
        for surface in &scene.surfaces {
            let r = surface.rect;
            if u32::from(r.x) + u32::from(r.width) > u32::from(scene.size.cols)
                || u32::from(r.y) + u32::from(r.height) > u32::from(scene.size.rows)
            {
                bail!("surface outside scene");
            }
        }
        let changed = self.layout.len() != scene.surfaces.len()
            || self
                .layout
                .iter()
                .zip(&scene.surfaces)
                .any(|((id, rect), surface)| id != surface.id || *rect != surface.rect);
        if changed {
            self.layout = scene
                .surfaces
                .iter()
                .map(|s| (s.id.to_owned(), s.rect))
                .collect();
            self.visible = scene
                .surfaces
                .iter()
                .enumerate()
                .map(|(index, surface)| {
                    let mut visible = vec![surface.rect];
                    for upper in &scene.surfaces[index + 1..] {
                        visible = visible
                            .into_iter()
                            .flat_map(|rect| subtract(rect, upper.rect))
                            .collect();
                    }
                    visible
                })
                .collect();
        }
        let canvas = self.canvas.get_or_insert_with(|| blank(scene.size));
        if canvas.size != scene.size {
            *canvas = blank(scene.size);
        }
        if changed {
            clear(
                canvas,
                Rect {
                    x: 0,
                    y: 0,
                    width: scene.size.cols,
                    height: scene.size.rows,
                },
            );
        }
        canvas.cursor.visible = false;
        let mut prepared = geometry::Prepared {
            images: HashMap::new(),
            original: HashSet::new(),
            derived: HashSet::new(),
            placements: Vec::new(),
        };
        let mut retained = HashSet::new();
        for (index, surface) in scene.surfaces.iter().enumerate() {
            let r = surface.rect;
            if r.width == 0 || r.height == 0 {
                continue;
            }
            occlude(&mut prepared.placements, r, scene.size)?;
            compose_cells(canvas, surface, &self.visible[index]);
            if surface.focused {
                canvas.cursor.clone_from(&surface.frame.cursor);
                canvas.cursor.visible &= canvas.cursor.x < r.width && canvas.cursor.y < r.height;
                canvas.cursor.x += r.x;
                canvas.cursor.y += r.y;
                canvas.cursor.visible &= self.visible[index]
                    .iter()
                    .any(|rect| rect.contains(canvas.cursor.x, canvas.cursor.y));
            }
            let local = geometry::prepare(surface.frame, r.size(scene.size))?;
            let mut mapping = HashMap::new();
            for (key, image) in local.images {
                let scoped = (surface.id.to_owned(), key);
                retained.insert(scoped.clone());
                let alias = if let Some(alias) = self.aliases.get(&scoped) {
                    *alias
                } else {
                    self.next_alias = self
                        .next_alias
                        .checked_add(1)
                        .context("scene resource namespace exhausted")?;
                    let alias = ImageKey {
                        id: self.next_alias,
                        generation: 0,
                    };
                    self.aliases.insert(scoped, alias);
                    alias
                };
                mapping.insert(key, alias);
                prepared.images.insert(alias, image);
            }
            for mut p in local.placements {
                p.image = mapping[&p.image];
                if let Some(key) = p.derived {
                    p.derived = Some(geometry::DerivedKey {
                        image: mapping[&key.image],
                        ..key
                    });
                }
                p.col += u32::from(r.x);
                p.row += u32::from(r.y);
                p.placement_id = prepared.placements.len() + 1;
                prepared.placements.push(p);
            }
        }
        self.aliases.retain(|key, _| retained.contains(key));
        // Rebuild live resource receipts after layer clipping, including newly
        // derived fragments, so hidden images cannot outlive their placements.
        prepared.original.clear();
        prepared.derived.clear();
        for (index, p) in prepared.placements.iter_mut().enumerate() {
            p.placement_id = index + 1;
            if let Some(key) = p.derived {
                prepared.derived.insert(key);
            } else {
                prepared.original.insert(p.image);
            }
        }
        if prepared.images.len() > 1024 || prepared.placements.len() > 4096 {
            bail!("composed scene resource limit");
        }
        draw_prepared(
            out,
            canvas,
            scene.size,
            "",
            host,
            &mut terminal.graphics,
            prepared,
        )
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    #[test]
    fn hit_testing_uses_topmost_geometry() {
        let size = Size {
            cols: 20,
            rows: 10,
            cell_width: 8,
            cell_height: 16,
        };
        let frame = blank(size);
        let scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "pane",
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 20,
                        height: 10,
                    },
                    frame: &frame,
                    focused: true,
                },
                Surface {
                    id: "popup",
                    rect: Rect {
                        x: 4,
                        y: 2,
                        width: 8,
                        height: 4,
                    },
                    frame: &frame,
                    focused: false,
                },
            ],
        };
        assert_eq!(scene.hit_test(5, 3), Some("popup"));
        assert_eq!(scene.hit_test(0, 0), Some("pane"));
        assert_eq!(scene.hit_test(20, 0), None);
    }
    #[test]
    fn trusted_text_clips_wide_cells() {
        let mut f = blank(Size {
            cols: 3,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        });
        text(&mut f, 0, 0, "界x界", false);
        assert_eq!(f.rows[0].cells[0].width, 2);
        assert_eq!(f.rows[0].cells[1].width, 0);
        assert_eq!(f.rows[0].cells[2].text, "x");
    }
    fn terminal() -> std::mem::ManuallyDrop<TerminalGuard> {
        std::mem::ManuallyDrop::new(TerminalGuard {
            graphics: GraphicsState::new(1000),
            kitty_keyboard: false,
            clipboard_mode: None,
        })
    }
    #[test]
    fn same_native_image_keys_are_scoped_by_pane_without_copying_pixels() {
        let size = Size {
            cols: 8,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
        };
        let local = Size { cols: 4, ..size };
        let mut left = blank(local);
        let mut right = blank(local);
        for (frame, color) in [
            (&mut left, [255, 0, 0, 255]),
            (&mut right, [0, 255, 0, 255]),
        ] {
            frame.images.push(Arc::new(Image {
                id: 7,
                generation: 1,
                width: 8,
                height: 16,
                format: PixelFormat::Rgba,
                pixels: color.repeat(8 * 16),
            }));
            frame.placements.push(Placement {
                image_id: 7,
                generation: 1,
                placement_id: 1,
                col: 0,
                row: 0,
                offset_x: 0,
                offset_y: 0,
                width: 8,
                height: 16,
                source_x: 0,
                source_y: 0,
                source_width: 8,
                source_height: 16,
                layer: ImageLayer::AboveText,
                z: 0,
            });
            frame.cursor.visible = true;
            frame.cursor.x = 1;
            frame.cursor.y = 1;
        }
        let pixels = left.images[0].pixels.as_ptr();
        let scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "left",
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 4,
                        height: 3,
                    },
                    frame: &left,
                    focused: false,
                },
                Surface {
                    id: "right",
                    rect: Rect {
                        x: 4,
                        y: 0,
                        width: 4,
                        height: 3,
                    },
                    frame: &right,
                    focused: true,
                },
            ],
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let mut bytes = Vec::new();
        renderer
            .render(&mut bytes, &scene, &ColorProbe::default(), &mut terminal)
            .unwrap();
        assert_eq!(renderer.aliases.len(), 2);
        assert_eq!(terminal.graphics.image_ids.len(), 2);
        assert_eq!(left.images[0].pixels.as_ptr(), pixels);
        assert_eq!(Arc::strong_count(&left.images[0]), 1);
        let cursor = &renderer.canvas.as_ref().unwrap().cursor;
        assert!(cursor.visible);
        assert_eq!((cursor.x, cursor.y), (5, 1));
    }
    #[test]
    fn unchanged_scene_reuses_grid_storage_and_emits_no_output() {
        let size = Size {
            cols: 10,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
        };
        let mut frame = blank(size);
        text(&mut frame, 0, 0, "stationary", false);
        let scene = Scene {
            size,
            surfaces: vec![Surface {
                id: "pane",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 3,
                },
                frame: &frame,
                focused: true,
            }],
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        let rows = renderer.canvas.as_ref().unwrap().rows.as_ptr();
        let cells = renderer.canvas.as_ref().unwrap().rows[0].cells.as_ptr();
        let text = renderer.canvas.as_ref().unwrap().rows[0].cells[0]
            .text
            .as_ptr();
        let mut repeated = Vec::new();
        renderer
            .render(&mut repeated, &scene, &ColorProbe::default(), &mut terminal)
            .unwrap();
        assert!(repeated.is_empty());
        assert_eq!(renderer.canvas.as_ref().unwrap().rows.as_ptr(), rows);
        assert_eq!(
            renderer.canvas.as_ref().unwrap().rows[0].cells.as_ptr(),
            cells
        );
        assert_eq!(
            renderer.canvas.as_ref().unwrap().rows[0].cells[0]
                .text
                .as_ptr(),
            text
        );
    }
    #[test]
    fn popup_clipping_keeps_uncovered_image_fragments_and_sampler_phase() {
        let image = ImageKey {
            id: 1,
            generation: 0,
        };
        let source = geometry::Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let derived = geometry::DerivedKey {
            image,
            source,
            width: 37,
            height: 23,
            visible: geometry::Rect {
                x: 3,
                y: 4,
                width: 24,
                height: 16,
            },
        };
        let mut projections = vec![geometry::Projection {
            image,
            derived: Some(derived),
            source: geometry::Rect {
                x: 0,
                y: 0,
                width: 24,
                height: 16,
            },
            col: 0,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            cells: None,
            z: 1,
            placement_id: 1,
        }];
        occlude(
            &mut projections,
            Rect {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            },
            Size {
                cols: 3,
                rows: 1,
                cell_width: 8,
                cell_height: 16,
            },
        )
        .unwrap();
        assert_eq!(projections.len(), 2);
        let mut fragments = projections
            .iter()
            .map(|p| p.derived.unwrap().visible)
            .collect::<Vec<_>>();
        fragments.sort_by_key(|r| r.x);
        assert_eq!(
            fragments,
            vec![
                geometry::Rect {
                    x: 3,
                    y: 4,
                    width: 8,
                    height: 16
                },
                geometry::Rect {
                    x: 19,
                    y: 4,
                    width: 8,
                    height: 16
                }
            ]
        );
        assert!(
            projections
                .iter()
                .all(|p| p.derived.unwrap().source == source)
        );
    }
    #[test]
    fn pane_palettes_and_default_backgrounds_are_resolved_before_composition() {
        let size = Size {
            cols: 4,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        };
        let local = Size { cols: 2, ..size };
        let mut a = blank(local);
        let mut b = blank(local);
        a.palette[1] = [255, 0, 0];
        b.palette[1] = [0, 255, 0];
        a.background = [1, 2, 3];
        b.background = [4, 5, 6];
        a.rows[0].cells[0].style.foreground_source = ColorSource::Palette(1);
        b.rows[0].cells[0].style.foreground_source = ColorSource::Palette(1);
        let scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "a",
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    frame: &a,
                    focused: false,
                },
                Surface {
                    id: "b",
                    rect: Rect {
                        x: 2,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    frame: &b,
                    focused: false,
                },
            ],
        };
        let host = ColorProbe {
            truecolor: true,
            foreground: Some([220, 220, 220]),
            background: Some([24, 24, 24]),
            ..ColorProbe::default()
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let mut bytes = Vec::new();
        renderer
            .render(&mut bytes, &scene, &host, &mut terminal)
            .unwrap();
        let frame = renderer.canvas.as_ref().unwrap();
        assert_eq!(frame.rows[0].cells[0].style.foreground, [255, 0, 0]);
        assert_eq!(frame.rows[0].cells[2].style.foreground, [0, 255, 0]);
        assert_eq!(frame.rows[0].cells[0].style.background, [1, 2, 3]);
        assert_eq!(frame.rows[0].cells[2].style.background, [4, 5, 6]);
        assert_eq!(
            frame.rows[0].cells[0].style.background_source,
            ColorSource::Default
        );
        let bytes = String::from_utf8(bytes).unwrap();
        for sgr in [
            "38;2;255;0;0m",
            "38;2;0;255;0m",
            "48;2;1;2;3m",
            "48;2;4;5;6m",
        ] {
            assert!(bytes.contains(sgr), "{sgr}");
        }
    }
}
