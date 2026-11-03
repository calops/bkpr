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
/// A surface can project opaque cells, transparent graphics, or both.
/// Resource namespaces are stable pane IDs, never native image IDs alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceContent {
    #[default]
    All,
    Cells,
    Graphics,
}
pub struct Surface<'a> {
    pub id: &'a str,
    pub rect: Rect,
    pub frame: &'a Frame,
    pub content: SurfaceContent,
    /// Sorted, one-span-per-row view-local highlighting, outside the canonical delta baseline.
    pub selection: &'a [SelectionSpan],
    /// Sorted by row/start. Selection wins over selected search, then ordinary
    /// search. Annotations borrow canonical glyphs/styles/resources unchanged.
    pub search: &'a [SearchSpan],
    pub focused: bool,
    /// Client-selected presentation only; canonical styles and image resources stay unchanged.
    pub faint: bool,
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
            .find(|s| s.content != SurfaceContent::Graphics && s.rect.contains(x, y))
            .map(|s| s.id)
    }
    pub fn hit_map(&self) -> HitMap {
        HitMap {
            regions: self
                .surfaces
                .iter()
                .filter(|s| s.content != SurfaceContent::Graphics)
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
    let mut frame = graphics_frame(size);
    let style = default_style();
    frame.rows = (0..size.rows)
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
        .collect();
    frame
}

/// Resource-only layers borrow this metadata without allocating a cell canvas.
pub fn graphics_frame(size: Size) -> Frame {
    let style = default_style();
    Frame {
        size,
        rows: Vec::new(),
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
        mouse_reporting: false,
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

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let mut guard = Self {
            graphics: GraphicsState::new(
                ((uuid::Uuid::new_v4().as_u128() as u32) & 0x3ffff800) | 0x80000000,
            ),
            kitty_keyboard: false,
            clipboard_mode: None,
            requested_clipboard_mode: None,
            theme_mode: None,
            background: background::Background::default(),
        };
        let mut out = std::io::stdout();
        write!(
            out,
            "\x1b[?1049h\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1bP+q5463\x1b\\\x1bP+q524742\x1b\\\x1b[?u\x1b[?5522$p\x1b[?2031$p"
        )?;
        guard.prepare_background_query();
        out.write_all(&ColorProbe::query())?;
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
            geometry::reuse_constant_axes(&mut fragment, size);
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
fn effective_style(style: &Style, frame: &Frame, native_palette: &[bool; 16]) -> Style {
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
    // Resolve virtual pane palettes before mixing surfaces. Preserve native
    // ANSI encoding when the host entry matches; only virtual mismatches need
    // explicit RGB. Unresolved UI sources remain honest native ANSI indices.
    let projected_source = |source| match source {
        ColorSource::Palette(index) => match frame.palette.get(usize::from(index)) {
            Some(rgb)
                if !native_palette
                    .get(usize::from(index))
                    .copied()
                    .unwrap_or(false) =>
            {
                ColorSource::Rgb(*rgb)
            }
            _ => source,
        },
        _ => source,
    };
    result.foreground_source = projected_source(style.foreground_source);
    result.background_source = projected_source(style.background_source);
    result.underline_source = projected_source(style.underline_source);
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
fn compose_cells(canvas: &mut Frame, surface: &Surface<'_>, visible: &[Rect], host: &ColorProbe) {
    // Prepare host/virtual palette equality once per surface, not per cell.
    let native_palette = std::array::from_fn(|index| {
        surface
            .frame
            .palette
            .get(index)
            .is_some_and(|rgb| host.palette.get(&index) == Some(rgb))
    });
    let blank = blank_cell();
    let mut blank_style = effective_style(&blank.style, surface.frame, &native_palette);
    blank_style.faint |= surface.faint;
    let r = surface.rect;
    for clip in visible {
        let left = clip.x - r.x;
        let right = left + clip.width;
        for y in clip.y..clip.y + clip.height {
            let selection = surface
                .selection
                .binary_search_by_key(&(y - r.y), |span| span.row)
                .ok()
                .map(|index| &surface.selection[index]);
            let row = y - r.y;
            let begin = surface.search.partition_point(|span| span.row < row);
            let end = surface.search.partition_point(|span| span.row <= row);
            let search = &surface.search[begin..end];
            let mut search_index = 0;
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
                        let mut style =
                            effective_style(&cell.style, surface.frame, &native_palette);
                        style.faint |= surface.faint;
                        while search_index < search.len() && search[search_index].end <= x {
                            search_index += 1;
                        }
                        annotate(&mut style, selection, &search[search_index..], x, end);
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
                let mut style = blank_style.clone();
                while search_index < search.len() && search[search_index].end <= column {
                    search_index += 1;
                }
                annotate(
                    &mut style,
                    selection,
                    &search[search_index..],
                    column,
                    column + 1,
                );
                copy_cell(&mut target[(r.x + column) as usize], blank, &style);
            }
        }
    }
}

fn annotate(
    style: &mut Style,
    selection: Option<&SelectionSpan>,
    search: &[SearchSpan],
    start: u16,
    end: u16,
) {
    if selection.is_some_and(|span| span.start < end && span.end > start) {
        style.inverse = !style.inverse;
        return;
    }
    let mut matched = false;
    let mut selected = false;
    for span in search.iter().take_while(|span| span.start < end) {
        if span.end > start {
            matched = true;
            selected |= span.selected;
        }
    }
    if matched {
        // Preserve source colors, glyphs, hyperlinks and attributes. Underline
        // marks all results; inversion additionally identifies the chosen one.
        style.underline = style.underline.max(1);
        if selected {
            style.inverse = !style.inverse;
            style.bold = true;
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
    layout: Vec<(String, Rect, SurfaceContent)>,
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
                .any(|((id, rect, content), surface)| {
                    id != surface.id || *rect != surface.rect || *content != surface.content
                });
        if changed {
            self.layout = scene
                .surfaces
                .iter()
                .map(|s| (s.id.to_owned(), s.rect, s.content))
                .collect();
            self.visible = scene
                .surfaces
                .iter()
                .enumerate()
                .map(|(index, surface)| {
                    let mut visible = vec![surface.rect];
                    for upper in &scene.surfaces[index + 1..] {
                        if upper.content == SurfaceContent::Graphics {
                            continue;
                        }
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
        // Native image depth is pane-local. Preserve each Kitty text/background
        // band, but rank images by scene layer before their local depth so a
        // pane cannot paint over a later transparent effect with an extreme z.
        let mut depths = [i32::MIN, i32::MIN / 2, -1];
        for (index, surface) in scene.surfaces.iter().enumerate() {
            let r = surface.rect;
            if r.width == 0 || r.height == 0 {
                continue;
            }
            if surface.content != SurfaceContent::Graphics {
                occlude(&mut prepared.placements, r, scene.size)?;
                compose_cells(canvas, surface, &self.visible[index], host);
            }
            if surface.focused && surface.content != SurfaceContent::Graphics {
                canvas.cursor.clone_from(&surface.frame.cursor);
                canvas.cursor.visible &= canvas.cursor.x < r.width && canvas.cursor.y < r.height;
                canvas.cursor.x += r.x;
                canvas.cursor.y += r.y;
                canvas.cursor.visible &= self.visible[index]
                    .iter()
                    .any(|rect| rect.contains(canvas.cursor.x, canvas.cursor.y));
            }
            if surface.content == SurfaceContent::Cells {
                continue;
            }
            let mut local = geometry::prepare(surface.frame, r.size(scene.size))?;
            if !local.placements.is_sorted_by_key(|p| p.z) {
                local.placements.sort_by_key(|p| p.z);
            }
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
                let band = if p.z < i32::MIN / 2 {
                    0
                } else if p.z < 0 {
                    1
                } else {
                    2
                };
                depths[band] = depths[band]
                    .checked_add(1)
                    .context("scene image depth capacity exhausted")?;
                p.z = depths[band];
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
            PhysicalTarget {
                background: terminal.background.surface(),
                graphics: &mut terminal.graphics,
            },
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
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                    content: SurfaceContent::All,
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
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
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
            requested_clipboard_mode: None,
            theme_mode: None,
            background: background::Background::default(),
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
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
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
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                    content: SurfaceContent::All,
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
                selection: &[],
                search: &[],
                focused: true,
                faint: false,
                content: SurfaceContent::All,
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
    fn effect_frame(size: Size) -> Frame {
        let mut frame = graphics_frame(size);
        frame.images.push(Arc::new(Image {
            id: 9,
            generation: 1,
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
            pixels: vec![0, 0, 0, 64],
        }));
        frame.placements.push(Placement {
            image_id: 9,
            generation: 1,
            placement_id: 1,
            col: 0,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            width: size.cell_width,
            height: size.cell_height,
            source_x: 0,
            source_y: 0,
            source_width: 1,
            source_height: 1,
            layer: ImageLayer::AboveText,
            z: 0,
        });
        frame
    }
    #[test]
    fn transparent_effects_preserve_native_cells_cursor_hits_and_idle_damage() {
        let size = Size {
            cols: 8,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        };
        let mut native = blank(size);
        text(&mut native, 0, 0, "native", false);
        native.cursor.visible = true;
        native.cursor.x = 3;
        native.cursor.y = 2;
        let original = native.clone();
        let mut effects = effect_frame(size);
        effects.cursor.visible = true;
        let mut scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "native",
                    rect,
                    frame: &native,
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                    content: SurfaceContent::All,
                },
                Surface {
                    id: "effects",
                    rect,
                    frame: &effects,
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                    content: SurfaceContent::Graphics,
                },
            ],
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
        assert_eq!(terminal.graphics.image_ids.len(), 1);
        let canvas = renderer.canvas.as_ref().unwrap();
        assert_eq!(canvas.rows[0].cells[0], native.rows[0].cells[0]);
        assert!(canvas.cursor.visible);
        assert_eq!((canvas.cursor.x, canvas.cursor.y), (3, 2));
        assert_eq!(scene.hit_test(0, 0), Some("native"));
        assert_eq!(scene.hit_map().hit_test(0, 0), Some("native"));
        let mut idle = Vec::new();
        renderer
            .render(&mut idle, &scene, &ColorProbe::default(), &mut terminal)
            .unwrap();
        assert!(idle.is_empty());

        // A mode change, without moving the layer, must invalidate opaque
        // coverage and release resources excluded from a cells-only surface.
        scene.surfaces[1].content = SurfaceContent::Cells;
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        assert_eq!(renderer.canvas.as_ref().unwrap().rows[0].cells[0].text, " ");
        assert!(terminal.graphics.image_ids.is_empty());
        assert_eq!(scene.hit_test(0, 0), Some("effects"));
        scene.surfaces[1].content = SurfaceContent::Graphics;
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        assert_eq!(
            renderer.canvas.as_ref().unwrap().rows[0].cells[0],
            native.rows[0].cells[0]
        );
        assert!(renderer.canvas.as_ref().unwrap().cursor.visible);
        assert_eq!(native, original);
    }
    #[test]
    fn transparent_layers_stay_above_native_images_regardless_of_pane_local_depth() {
        let size = Size {
            cols: 8,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        };
        let mut native = effect_frame(size);
        native.placements[0].z = i32::MAX;
        let effects = effect_frame(size);
        let scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "native",
                    rect,
                    frame: &native,
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                    content: SurfaceContent::All,
                },
                Surface {
                    id: "effects",
                    rect,
                    frame: &effects,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::Graphics,
                },
            ],
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
        let native_key = renderer.aliases[&("native".into(), native.images[0].key())];
        let effect_key = renderer.aliases[&("effects".into(), effects.images[0].key())];
        let depth = |image| {
            terminal
                .graphics
                .projections
                .iter()
                .find(|p| p.image == image)
                .unwrap()
                .z
        };
        assert!(
            depth(native_key) < depth(effect_key),
            "pane-local depth escaped its scene layer"
        );
    }

    fn composite_rgba_projections(
        size: Size,
        projections: &[geometry::Projection],
        image: &Image,
        background: [u8; 3],
    ) -> Vec<[u8; 3]> {
        assert_eq!(image.format, PixelFormat::Rgba);
        let width = size.cols as usize * size.cell_width as usize;
        let height = size.rows as usize * size.cell_height as usize;
        let mut pixels = vec![background; width * height];
        let mut ordered: Vec<_> = projections.iter().collect();
        ordered.sort_by_key(|p| p.z);
        for p in ordered {
            let (w, h) = if let Some(key) = p.derived {
                (key.visible.width, key.visible.height)
            } else if let Some((cols, rows)) = p.cells {
                (
                    cols * size.cell_width - p.offset_x,
                    rows * size.cell_height - p.offset_y,
                )
            } else {
                (p.source.width, p.source.height)
            };
            let key = p.derived.unwrap_or(geometry::DerivedKey {
                image: p.image,
                source: p.source,
                width: w,
                height: h,
                visible: geometry::Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                },
            });
            let rgba = geometry::raster(image, key).unwrap();
            let left = (p.col * size.cell_width + p.offset_x) as usize;
            let top = (p.row * size.cell_height + p.offset_y) as usize;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let source = &rgba[(y * w as usize + x) * 4..][..4];
                    let destination = &mut pixels[(top + y) * width + left + x];
                    for c in 0..3 {
                        destination[c] = ((u32::from(source[c]) * u32::from(source[3])
                            + u32::from(destination[c]) * (255 - u32::from(source[3]))
                            + 127)
                            / 255) as u8;
                    }
                }
            }
        }
        pixels
    }

    #[test]
    fn shared_recessed_frames_preserve_both_contours_independent_of_scene_order() {
        use crate::decoration::{
            DecorationGeometry, DecorationKind, DecorationStyle, PreparedDecoration, atlas_size,
            rasterize,
        };
        let size = Size {
            cols: 20,
            rows: 11,
            cell_width: 8,
            cell_height: 16,
        };
        let screen = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        let upper_rect = Rect {
            height: 6,
            ..screen
        };
        let lower_rect = Rect {
            y: 5,
            height: 6,
            ..screen
        };
        let style = DecorationStyle {
            kind: DecorationKind::Recessed,
            background: [30; 3],
            surface: [20; 3],
            foreground: [200; 3],
            inset_px: [8, 16],
            outset_px: [0, 0],
            radius_px: 12,
            sigma_milli: 2400,
            offset_milli: [2800, 2800],
            shadow_opacity_milli: 720,
            lip_width_milli: 960,
            lip_highlight_milli: 240,
            lip_shade_milli: 240,
        };
        let cell = [size.cell_width, size.cell_height];
        let [width, height] = atlas_size(style, cell).unwrap();
        let decoration = PreparedDecoration {
            style,
            cell,
            image: Arc::new(Image {
                id: 11,
                generation: 1,
                width,
                height,
                format: PixelFormat::Rgba,
                pixels: rasterize(style, cell).unwrap(),
            }),
        };
        // Match the fork's physical translation: exterior edges move by the
        // row/column excess; shared edges move by complementary half-excess.
        // The shared row is [80,96), with independent ownership at pixel 88.
        let upper_bounds = [0, -8, 160, 100];
        let lower_bounds = [0, 76, 160, 184];
        let full_clip = [0, 0, 160, 176];
        let place = |rect, bounds_px, clip_px| {
            let mut frame = graphics_frame(size);
            assert!(decoration.place_with_geometry(
                &mut frame,
                rect,
                1,
                &[],
                DecorationGeometry { bounds_px, clip_px },
            ));
            frame
        };
        let upper = place(upper_rect, upper_bounds, [0, 0, 160, 88]);
        let lower = place(lower_rect, lower_bounds, [0, 88, 160, 176]);
        let isolated_upper = place(upper_rect, upper_bounds, full_clip);
        let isolated_lower = place(lower_rect, lower_bounds, full_clip);
        let reference = |frame: &Frame| {
            composite_rgba_projections(
                size,
                &geometry::prepare(frame, size).unwrap().placements,
                &decoration.image,
                style.background,
            )
        };
        let upper_pixels = reference(&isolated_upper);
        let lower_pixels = reference(&isolated_lower);
        let mut expected = upper_pixels[..88 * 160].to_vec();
        expected.extend_from_slice(&lower_pixels[88 * 160..]);
        // Each content-facing half contains a real curved rim and shadow, not
        // a flat separator that could accidentally satisfy the ownership test.
        for (pixels, y) in [(&upper_pixels, 83), (&lower_pixels, 95)] {
            assert_ne!(pixels[y * 160 + 8], pixels[y * 160 + 80]);
            assert_ne!(pixels[y * 160 + 151], pixels[y * 160 + 80]);
            assert_ne!(pixels[y * 160 + 80], style.background);
        }
        let mut native = blank(size);
        text(&mut native, 1, 1, "upper native", false);
        text(&mut native, 1, 7, "lower native", false);
        native.cursor.x = 2;
        native.cursor.y = 7;
        native.cursor.visible = true;
        let mut scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "native",
                    rect: screen,
                    frame: &native,
                    content: SurfaceContent::All,
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                },
                Surface {
                    id: "upper",
                    rect: screen,
                    frame: &upper,
                    content: SurfaceContent::Graphics,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                },
                Surface {
                    id: "lower",
                    rect: screen,
                    frame: &lower,
                    content: SurfaceContent::Graphics,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                },
            ],
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        for _ in 0..2 {
            renderer
                .render(
                    &mut Vec::new(),
                    &scene,
                    &ColorProbe::default(),
                    &mut terminal,
                )
                .unwrap();
            assert_eq!(
                composite_rgba_projections(
                    size,
                    &terminal.graphics.projections,
                    &decoration.image,
                    style.background,
                ),
                expected,
                "one frame's opaque gutter erased its neighbor's contour",
            );
            let canvas = renderer.canvas.as_ref().unwrap();
            assert_eq!(canvas.rows, native.rows);
            assert_eq!(canvas.cursor, native.cursor);
            assert_eq!(scene.hit_test(1, 5), Some("native"));
            scene.surfaces.swap(1, 2);
        }
        let mut popup = blank(Size {
            cols: 2,
            rows: 1,
            ..size
        });
        text(&mut popup, 0, 0, "PP", false);
        scene.surfaces.push(Surface {
            id: "popup",
            rect: Rect {
                x: 9,
                y: 5,
                width: 2,
                height: 1,
            },
            frame: &popup,
            content: SurfaceContent::Cells,
            selection: &[],
            search: &[],
            focused: false,
            faint: false,
        });
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        let covered = composite_rgba_projections(
            size,
            &terminal.graphics.projections,
            &decoration.image,
            style.background,
        );
        for y in 80..96 {
            assert!(
                covered[y * 160 + 72..y * 160 + 88]
                    .iter()
                    .all(|pixel| *pixel == style.background)
            );
        }
        assert_eq!(renderer.canvas.as_ref().unwrap().rows[5].cells[9].text, "P");
        assert_eq!(scene.hit_test(9, 5), Some("popup"));
    }
    #[test]
    fn opaque_popup_clips_transparent_effects_without_stealing_uncovered_hits() {
        let size = Size {
            cols: 8,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        };
        let native = blank(size);
        let effects = effect_frame(size);
        let mut popup = blank(Size {
            cols: 1,
            rows: 1,
            ..size
        });
        text(&mut popup, 0, 0, "P", false);
        let mut scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "native",
                    rect,
                    frame: &native,
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                    content: SurfaceContent::All,
                },
                Surface {
                    id: "effects",
                    rect,
                    frame: &effects,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::Graphics,
                },
                Surface {
                    id: "popup",
                    rect: Rect {
                        width: 1,
                        height: 1,
                        ..rect
                    },
                    frame: &popup,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::Cells,
                },
            ],
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
        assert!(terminal.graphics.image_ids.is_empty());
        assert_eq!(renderer.canvas.as_ref().unwrap().rows[0].cells[0].text, "P");
        assert_eq!(scene.hit_map().hit_test(0, 0), Some("popup"));
        assert_eq!(scene.hit_map().hit_test(1, 0), Some("native"));
        scene.surfaces.pop();
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        assert_eq!(terminal.graphics.image_ids.len(), 1);
        assert_eq!(renderer.canvas.as_ref().unwrap().rows[0].cells[0].text, " ");
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
    fn physical_canvas_keeps_native_default_local_osc11_and_explicit_backgrounds() {
        let size = Size { cols: 6, rows: 1, cell_width: 8, cell_height: 16 };
        let local = Size { cols: 2, ..size };
        let original = [17, 34, 51];
        let matte = [8, 9, 10];
        let mut panes = Vec::new();
        for (index, input) in [
            b"AA".as_slice(),
            b"\x1b]11;rgb:4444/5555/6666\x1b\\BB",
            b"\x1b[48;2;119;136;153mCC",
        ].into_iter().enumerate() {
            let mut native = bkpr_terminal_engine::Terminal::new(local).unwrap();
            native.set_default_colors([220, 220, 220], original, None).unwrap();
            native.feed(input).unwrap();
            let frame = native.snapshot().unwrap();
            if index == 1 {
                assert_eq!(frame.background, [68, 85, 102]);
                // Native engine owns pane-local OSC 11 queries; they do not
                // travel to the physical terminal or require another SDK API.
                let effects = native.feed(b"\x1b]11;?\x1b\\").unwrap();
                let mut demux = ReplyDemux::default();
                let mut local_probe = ColorProbe::default();
                for effect in effects {
                    if let Effect::PtyWrite(bytes) = effect {
                        for part in demux.feed(&bytes) {
                            if let InputPart::Reply(reply) = part {
                                local_probe.feed(&reply);
                            }
                        }
                    }
                }
                assert_eq!(local_probe.background, Some([68, 85, 102]));
            }
            panes.push(frame);
        }
        let scene = Scene {
            size,
            surfaces: panes.iter().enumerate().map(|(index, frame)| Surface {
                id: ["default", "local", "explicit"][index],
                rect: Rect { x: index as u16 * 2, y: 0, width: 2, height: 1 },
                frame,
                content: SurfaceContent::All,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
            }).collect(),
        };
        let mut host = ColorProbe {
            truecolor: true,
            foreground: Some([220, 220, 220]),
            background: Some(original),
            ..ColorProbe::default()
        };
        let mut terminal = terminal();
        let mut renderer = Renderer::default();
        let mut physical = bkpr_terminal_engine::Terminal::new(size).unwrap();
        physical.set_default_colors([220, 220, 220], original, None).unwrap();
        let mut bytes = Vec::new();
        terminal.set_background(&mut bytes, Some(TerminalBackground { original, matte })).unwrap();
        renderer.render(&mut bytes, &scene, &host, &mut terminal).unwrap();
        let output = String::from_utf8_lossy(&bytes);
        for sgr in ["48;2;17;34;51m", "48;2;68;85;102m", "48;2;119;136;153m"] {
            assert!(output.contains(sgr), "{sgr}");
        }
        assert!(!output.contains("\x1b[49m"));
        physical.feed(&bytes).unwrap();
        let frame = physical.snapshot().unwrap();
        assert_eq!(frame.background, matte);
        for (index, rgb) in [original, [68, 85, 102], [119, 136, 153]].into_iter().enumerate() {
            assert_eq!(frame.rows[0].cells[index * 2].style.background, rgb);
        }
        assert_eq!(host.background, Some(original));
        // Theme invalidation may clear logical observations before the next
        // ready/fallback decision. Physical ownership is independent evidence.
        host.background = None;
        host.revision += 1;
        bytes.clear();
        renderer.render(&mut bytes, &scene, &host, &mut terminal).unwrap();
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("48;2;17;34;51m"));
        assert!(!output.contains("\x1b[49m"));
        physical.feed(&bytes).unwrap();
        assert_eq!(physical.snapshot().unwrap().rows[0].cells[0].style.background, original);
        host.background = Some(original);
        host.revision += 1;
        // Even unchanged scene cells must repaint when the physical default
        // changes back. Native defaults regain SGR 49; pane-local colors do not.
        bytes.clear();
        terminal.set_background(&mut bytes, None).unwrap();
        renderer.render(&mut bytes, &scene, &host, &mut terminal).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\x1b[49m"));
        physical.feed(&bytes).unwrap();
        let frame = physical.snapshot().unwrap();
        assert_eq!(frame.background, original);
        for (index, rgb) in [original, [68, 85, 102], [119, 136, 153]].into_iter().enumerate() {
            assert_eq!(frame.rows[0].cells[index * 2].style.background, rgb);
        }
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
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
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
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
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

    #[test]
    fn native_ui_ansi_and_virtual_pane_palettes_keep_distinct_output_semantics() {
        let size = Size {
            cols: 2,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        };
        let mut ui = crate::UiSurface::new(size);
        ui.paint(
            size,
            vec![(
                Rect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                "UI".into(),
                false,
            )],
            None,
        );
        let mut theme = crate::Theme::default();
        theme.update(&ColorProbe::default(), 0, |_| {
            let mut base = std::array::from_fn(|_| default_style());
            base[crate::Role::Error as usize].foreground_source = ColorSource::Palette(1);
            crate::ThemeStyles {
                selected: base.clone(),
                panel: base.clone(),
                base,
            }
        });
        ui.apply_theme(&theme, crate::Role::Error, &[]);
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let scene = Scene {
            size,
            surfaces: vec![Surface {
                id: "ui",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                frame: &ui.frame,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
                content: SurfaceContent::All,
            }],
        };
        let mut bytes = Vec::new();
        renderer
            .render(&mut bytes, &scene, &ColorProbe::default(), &mut terminal)
            .unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("\x1b[31m"));
        assert!(!output.contains("38;2;") && !output.contains("38;5;"));

        let mut pane = blank(size);
        text(&mut pane, 0, 0, "AP", false);
        pane.palette[1] = [210, 45, 60];
        pane.rows[0].cells[0].style.foreground_source = ColorSource::Palette(1);
        let canonical = pane.rows.clone();
        let scene = Scene {
            size,
            surfaces: vec![Surface {
                id: "app",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                frame: &pane,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
                content: SurfaceContent::All,
            }],
        };
        let mut host = ColorProbe {
            truecolor: true,
            ..ColorProbe::default()
        };
        host.feed(b"4;1;rgb:d2/2d/3c");
        let mut bytes = Vec::new();
        renderer
            .render(&mut bytes, &scene, &host, &mut terminal)
            .unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("\x1b[31m"));
        host.feed(b"4;1;rgb:10/20/30");
        let mut bytes = Vec::new();
        renderer
            .render(&mut bytes, &scene, &host, &mut terminal)
            .unwrap();
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("38;2;210;45;60m")
        );
        assert_eq!(pane.rows, canonical);
    }

    #[test]
    fn selection_overlay_preserves_canonical_wide_combining_styles_images_and_other_views() {
        let size = Size {
            cols: 12,
            rows: 2,
            cell_width: 8,
            cell_height: 16,
        };
        let local = Size { cols: 6, ..size };
        let mut source = blank(local);
        text(&mut source, 0, 0, "界e\u{301}!", false);
        source.foreground = [15, 25, 35];
        source.background = [45, 55, 65];
        source.palette[1] = [200, 20, 10];
        source.rows[0].cells[0].style.foreground_source = ColorSource::Palette(1);
        source.rows[0].cells[0].style.bold = true;
        source.rows[0].cells[0].hyperlink = Some("https://example.invalid/".into());
        source.rows[0].cells[2].style.inverse = true;
        source.rows[0].cells[2].style.italic = true;
        source.rows[0].cells[2].style.underline = 2;
        source.rows[0].cells[2].style.strikethrough = true;
        source.images.push(Arc::new(Image {
            id: 7,
            generation: 1,
            width: 8,
            height: 16,
            format: PixelFormat::Rgba,
            pixels: [255, 0, 0, 255].repeat(8 * 16),
        }));
        source.placements.push(Placement {
            image_id: 7,
            generation: 1,
            placement_id: 1,
            col: 4,
            row: 1,
            offset_x: 0,
            offset_y: 0,
            width: 8,
            height: 16,
            source_x: 0,
            source_y: 0,
            source_width: 8,
            source_height: 16,
            layer: ImageLayer::BelowText,
            z: 0,
        });
        let canonical_rows = source.rows.clone();
        let canonical_cells = source.rows[0].cells.as_ptr();
        let pixels = source.images[0].pixels.as_ptr();
        let mut scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "selected-view",
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 6,
                        height: 2,
                    },
                    frame: &source,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
                },
                Surface {
                    id: "other-view",
                    rect: Rect {
                        x: 6,
                        y: 0,
                        width: 6,
                        height: 2,
                    },
                    frame: &source,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
                },
            ],
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
        let canvas_cells = renderer.canvas.as_ref().unwrap().rows[0].cells.as_ptr();
        let spans = [SelectionSpan {
            row: 0,
            start: 1,
            end: 3,
        }];
        scene.surfaces[0].selection = &spans;
        let mut damage = Vec::new();
        renderer
            .render(&mut damage, &scene, &ColorProbe::default(), &mut terminal)
            .unwrap();
        assert!(!damage.is_empty());
        let canvas = renderer.canvas.as_ref().unwrap();
        assert_eq!(canvas.rows[0].cells.as_ptr(), canvas_cells);
        assert_eq!(canvas.rows[0].cells[0].text, "界");
        assert_eq!(canvas.rows[0].cells[0].width, 2);
        assert_eq!(canvas.rows[0].cells[1].width, 0);
        assert_eq!(canvas.rows[0].cells[2].text, "e\u{301}");
        for column in [0, 2] {
            let mut expected =
                effective_style(&source.rows[0].cells[column].style, &source, &[false; 16]);
            expected.inverse = !expected.inverse;
            assert_eq!(canvas.rows[0].cells[column].style, expected);
            assert_eq!(
                canvas.rows[0].cells[column + 6].style,
                effective_style(&source.rows[0].cells[column].style, &source, &[false; 16])
            );
        }
        assert_eq!(canvas.rows[0].cells[0].style, canvas.rows[0].cells[1].style);
        assert_eq!(
            canvas.rows[0].cells[0].hyperlink,
            source.rows[0].cells[0].hyperlink
        );
        assert_eq!(source.rows, canonical_rows);
        assert_eq!(source.rows[0].cells.as_ptr(), canonical_cells);
        assert_eq!(source.images[0].pixels.as_ptr(), pixels);
        assert_eq!(renderer.aliases.len(), 2);
        assert_eq!(terminal.graphics.image_ids.len(), 2);
        let mut unchanged = Vec::new();
        renderer
            .render(
                &mut unchanged,
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        assert!(unchanged.is_empty());
        scene.surfaces[0].selection = &[];
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        assert_eq!(
            renderer.canvas.as_ref().unwrap().rows[0].cells[0].style,
            effective_style(&source.rows[0].cells[0].style, &source, &[false; 16])
        );
        let matches = [
            SearchSpan {
                row: 0,
                start: 0,
                end: 4,
                selected: false,
            },
            SearchSpan {
                row: 0,
                start: 1,
                end: 3,
                selected: true,
            },
        ];
        let selection = [SelectionSpan {
            row: 0,
            start: 0,
            end: 1,
        }];
        scene.surfaces[0].search = &matches;
        scene.surfaces[0].selection = &selection;
        renderer
            .render(
                &mut Vec::new(),
                &scene,
                &ColorProbe::default(),
                &mut terminal,
            )
            .unwrap();
        let canvas = renderer.canvas.as_ref().unwrap();
        let mut selected = effective_style(&source.rows[0].cells[0].style, &source, &[false; 16]);
        selected.inverse = !selected.inverse;
        assert_eq!(
            canvas.rows[0].cells[0].style, selected,
            "selection overrides both search styles"
        );
        assert_eq!(
            canvas.rows[0].cells[1].style, selected,
            "wide glyph is annotated atomically"
        );
        let mut chosen = effective_style(&source.rows[0].cells[2].style, &source, &[false; 16]);
        chosen.inverse = !chosen.inverse;
        chosen.bold = true;
        chosen.underline = chosen.underline.max(1);
        assert_eq!(canvas.rows[0].cells[2].style, chosen);
        let mut ordinary = effective_style(&source.rows[0].cells[3].style, &source, &[false; 16]);
        ordinary.underline = ordinary.underline.max(1);
        assert_eq!(canvas.rows[0].cells[3].style, ordinary);
        assert_eq!(canvas.rows[0].cells[2].text, "e\u{301}");
        assert_eq!(
            canvas.rows[0].cells[2 + 6].style,
            effective_style(&source.rows[0].cells[2].style, &source, &[false; 16])
        );
        assert_eq!(source.rows, canonical_rows);
        assert_eq!(source.rows[0].cells.as_ptr(), canonical_cells);
        assert_eq!(source.images[0].pixels.as_ptr(), pixels);
        assert_eq!(renderer.aliases.len(), 2);
        let mut idle = Vec::new();
        renderer
            .render(&mut idle, &scene, &ColorProbe::default(), &mut terminal)
            .unwrap();
        assert!(
            idle.is_empty(),
            "static search annotations must produce no damage"
        );
    }

    #[test]
    fn view_local_faint_preserves_other_view_and_canonical_styles_through_dismissal() {
        let size = Size {
            cols: 4,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        };
        let mut source = blank(Size { cols: 2, ..size });
        text(&mut source, 0, 0, "AB", false);
        source.rows[0].cells[0].style.bold = true;
        source.rows[0].cells[0].style.italic = true;
        source.rows[0].cells[1].style.faint = true;
        let canonical = source.rows.clone();
        let mut scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "first",
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    frame: &source,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
                },
                Surface {
                    id: "other",
                    rect: Rect {
                        x: 2,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    frame: &source,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                    content: SurfaceContent::All,
                },
            ],
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let host = ColorProbe::default();
        renderer
            .render(&mut Vec::new(), &scene, &host, &mut terminal)
            .unwrap();
        let original = renderer.canvas.as_ref().unwrap().rows.clone();
        scene.surfaces[0].faint = true;
        renderer
            .render(&mut Vec::new(), &scene, &host, &mut terminal)
            .unwrap();
        let canvas = renderer.canvas.as_ref().unwrap();
        let mut expected = original[0].cells[0].style.clone();
        expected.faint = true;
        assert_eq!(canvas.rows[0].cells[0].style, expected);
        assert_eq!(canvas.rows[0].cells[1], original[0].cells[1]);
        assert_eq!(canvas.rows[0].cells[2..], original[0].cells[2..]);
        assert_eq!(source.rows, canonical);
        let mut idle = Vec::new();
        renderer
            .render(&mut idle, &scene, &host, &mut terminal)
            .unwrap();
        assert!(idle.is_empty(), "unchanged modal backdrop must not repaint");
        scene.surfaces[0].faint = false;
        renderer
            .render(&mut Vec::new(), &scene, &host, &mut terminal)
            .unwrap();
        assert_eq!(renderer.canvas.as_ref().unwrap().rows, original);
        assert_eq!(source.rows, canonical);
    }
}
