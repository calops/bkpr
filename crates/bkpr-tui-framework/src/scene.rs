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
    /// Opaque cells inside a source-local, half-open clip. The surface origin
    /// is unchanged; cells outside its frame or viewport are never projected.
    /// Partial wide glyphs become blank cells rather than split glyphs.
    CellsClipped(Rect),
    /// Source-local cells with the same bounds as `CellsClipped`, retaining
    /// earlier intersecting graphics below text instead of removing them.
    /// Later opaque surfaces still occlude both these cells and their underlay.
    CellsWithUnderlay(Rect),
    Graphics,
}
/// Borrowed, view-local terminal colors; canonical frames remain unchanged.
/// Entries outside this palette retain their canonical frame colors.
#[derive(Clone, Copy, Debug)]
pub struct PaletteOverride<'a> {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: &'a [[u8; 3]],
}
pub struct Surface<'a> {
    pub id: &'a str,
    pub rect: Rect,
    pub frame: &'a Frame,
    /// Presentation colors for defaults and indexed colors, never explicit RGB.
    pub palette: Option<PaletteOverride<'a>>,
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
impl Surface<'_> {
    fn cell_rect(&self) -> Option<Rect> {
        match self.content {
            SurfaceContent::Graphics => None,
            SurfaceContent::All | SurfaceContent::Cells => Some(self.rect),
            SurfaceContent::CellsClipped(clip) | SurfaceContent::CellsWithUnderlay(clip) => {
                let width = self.rect.width.min(self.frame.size.cols);
                let height = self.rect.height.min(self.frame.size.rows);
                let left = clip.x.min(width);
                let top = clip.y.min(height);
                let right = clip.x.saturating_add(clip.width).min(width);
                let bottom = clip.y.saturating_add(clip.height).min(height);
                (left < right && top < bottom).then(|| Rect {
                    x: self.rect.x.saturating_add(left),
                    y: self.rect.y.saturating_add(top),
                    width: right - left,
                    height: bottom - top,
                })
            }
        }
    }
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
            .find(|s| s.cell_rect().is_some_and(|rect| rect.contains(x, y)))
            .map(|s| s.id)
    }
    pub fn hit_map(&self) -> HitMap {
        HitMap {
            regions: self
                .surfaces
                .iter()
                .filter_map(|s| s.cell_rect().map(|rect| (s.id.to_owned(), rect)))
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
            "\x1b[?1049h\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1bP+q5463\x1b\\\x1bP+q524742\x1b\\\x1b[?u\x1b[?5522$p\x1b[?2031$p"
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
    clip_projections(placements, cover, size, false)
}

/// Retain the existing sampler and resources when moving covered material below
/// text. Both opaque clipping and text underlays use this one fragment path.
fn clip_projections(
    placements: &mut Vec<geometry::Projection>,
    cover: Rect,
    size: Size,
    underlay: bool,
) -> Result<()> {
    let cover = geometry::Rect {
        x: u32::from(cover.x) * size.cell_width,
        y: u32::from(cover.y) * size.cell_height,
        width: u32::from(cover.width) * size.cell_width,
        height: u32::from(cover.height) * size.cell_height,
    };
    let mut output = Vec::with_capacity(placements.len());
    for p in placements.drain(..) {
        if underlay && p.z < 0 {
            output.push(p);
            continue;
        }
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
        if underlay && left == x && top == y && right == x + width && bottom == y + height {
            output.push(geometry::Projection { z: p.z + i32::MIN / 2, ..p });
            continue;
        }
        let fragments = [
            (x, y, width, top - y),
            (x, bottom, width, y + height - bottom),
            (x, top, left - x, bottom - top),
            (right, top, x + width - right, bottom - top),
            (left, top, if underlay { right - left } else { 0 }, bottom - top),
        ];
        for (part, (fx, fy, fw, fh)) in fragments.into_iter().enumerate() {
            if fw == 0 || fh == 0 {
                continue;
            }
            let mut fragment = p;
            if part == 4 {
                fragment.z += i32::MIN / 2;
            }
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
fn effective_style(
    style: &Style,
    frame: &Frame,
    palette: PaletteOverride<'_>,
    native_palette: &[bool; 16],
    override_defaults: bool,
) -> Style {
    let entry = |index| {
        palette
            .palette
            .get(index)
            .or_else(|| frame.palette.get(index))
            .copied()
    };
    let resolve = |source: ColorSource, default: [u8; 3], fallback: [u8; 3]| match source {
        ColorSource::Default => default,
        ColorSource::Palette(index) => entry(usize::from(index)).unwrap_or(fallback),
        ColorSource::Rgb(rgb) => rgb,
    };
    let mut result = style.clone();
    result.foreground = resolve(style.foreground_source, palette.foreground, style.foreground);
    result.background = resolve(style.background_source, palette.background, style.background);
    result.underline_color = resolve(
        style.underline_source,
        result.foreground,
        style.underline_color,
    );
    // Resolve virtual pane palettes before mixing surfaces. Preserve native
    // ANSI encoding when the host entry matches; only virtual mismatches need
    // explicit RGB. Unresolved UI sources remain honest native ANSI indices.
    let projected_source = |source| match source {
        ColorSource::Palette(index) => match entry(usize::from(index)) {
            Some(rgb)
                if !native_palette
                    .get(usize::from(index))
                    .copied()
                    .unwrap_or(false) =>
            {
                ColorSource::Rgb(rgb)
            }
            _ => source,
        },
        _ => source,
    };
    // Unknown host defaults intentionally remain native for canonical frames.
    // An explicit view override must instead take effect even without a probe.
    result.foreground_source = if override_defaults && style.foreground_source == ColorSource::Default {
        ColorSource::Rgb(result.foreground)
    } else {
        projected_source(style.foreground_source)
    };
    result.background_source = if override_defaults && style.background_source == ColorSource::Default {
        ColorSource::Rgb(result.background)
    } else {
        projected_source(style.background_source)
    };
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
    let palette = surface.palette.unwrap_or(PaletteOverride {
        foreground: surface.frame.foreground,
        background: surface.frame.background,
        palette: &surface.frame.palette,
    });
    // Prepare host/virtual palette equality once per surface, not per cell.
    let native_palette = std::array::from_fn(|index| {
        palette
            .palette
            .get(index)
            .or_else(|| surface.frame.palette.get(index))
            .is_some_and(|rgb| host.palette.get(&index) == Some(rgb))
    });
    let blank = blank_cell();
    let mut blank_style = effective_style(
        &blank.style,
        surface.frame,
        palette,
        &native_palette,
        surface.palette.is_some(),
    );
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
                        let mut style = effective_style(
                            &cell.style,
                            surface.frame,
                            palette,
                            &native_palette,
                            surface.palette.is_some(),
                        );
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
    layout: Vec<(String, Rect, SurfaceContent, Option<Rect>)>,
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
        let changed = self.canvas.as_ref().is_none_or(|canvas| canvas.size != scene.size)
            || self.layout.len() != scene.surfaces.len()
            || self
                .layout
                .iter()
                .zip(&scene.surfaces)
                .any(|((id, rect, content, cells), surface)| {
                    id != surface.id
                        || *rect != surface.rect
                        || *content != surface.content
                        || *cells != surface.cell_rect()
                });
        if changed {
            self.layout = scene
                .surfaces
                .iter()
                .map(|s| (s.id.to_owned(), s.rect, s.content, s.cell_rect()))
                .collect();
            self.visible = scene
                .surfaces
                .iter()
                .enumerate()
                .map(|(index, surface)| {
                    let mut visible: Vec<_> = surface.cell_rect().into_iter().collect();
                    for upper in &scene.surfaces[index + 1..] {
                        let Some(cover) = upper.cell_rect() else {
                            continue;
                        };
                        visible = visible
                            .into_iter()
                            .flat_map(|rect| subtract(rect, cover))
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
        // No owner (or a clipped owner) must not reuse a previous surface's
        // hidden anchor. Out-of-bounds coordinates carry no physical position.
        canvas.cursor.x = scene.size.cols;
        canvas.cursor.y = scene.size.rows;
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
        // A common ordinal also preserves scene ordering when an above-text
        // fragment is moved into the below-text band by a later cell underlay.
        let mut depth = 0_i32;
        for (index, surface) in scene.surfaces.iter().enumerate() {
            let r = surface.rect;
            if r.width == 0 || r.height == 0 {
                continue;
            }
            if let Some(cover) = surface.cell_rect() {
                if matches!(surface.content, SurfaceContent::CellsWithUnderlay(_)) {
                    clip_projections(&mut prepared.placements, cover, scene.size, true)?;
                } else {
                    occlude(&mut prepared.placements, cover, scene.size)?;
                }
                compose_cells(canvas, surface, &self.visible[index], host);
            }
            if surface.focused && surface.content != SurfaceContent::Graphics {
                let cursor = &surface.frame.cursor;
                let x = cursor.x.saturating_add(r.x);
                let y = cursor.y.saturating_add(r.y);
                if cursor.x < r.width
                    && cursor.y < r.height
                    && surface.cell_rect().is_some_and(|rect| rect.contains(x, y))
                {
                    // A passive plate may cover the owner's cells without
                    // owning focus. Keep its logical position for hidden
                    // cursor trails; only the visible glyph is occluded.
                    canvas.cursor.clone_from(cursor);
                    canvas.cursor.x = x;
                    canvas.cursor.y = y;
                    canvas.cursor.visible &= self.visible[index]
                        .iter()
                        .any(|rect| rect.contains(x, y));
                } else {
                    canvas.cursor.visible = false;
                    canvas.cursor.x = scene.size.cols;
                    canvas.cursor.y = scene.size.rows;
                }
            }
            if matches!(
                surface.content,
                SurfaceContent::Cells
                    | SurfaceContent::CellsClipped(_)
                    | SurfaceContent::CellsWithUnderlay(_)
            ) {
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
                depth = depth.checked_add(1).context("scene image depth capacity exhausted")?;
                p.z = [i32::MIN, i32::MIN / 2, 0][band]
                    .checked_add(depth)
                    .context("scene image depth capacity exhausted")?;
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
        // Rebuild visible resource requirements after layer clipping. The renderer
        // may retain bounded inactive projections only while their source survives.
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
    fn canonical_style(style: &Style, frame: &Frame) -> Style {
        effective_style(
            style,
            frame,
            PaletteOverride {
                foreground: frame.foreground,
                background: frame.background,
                palette: &frame.palette,
            },
            &[false; 16],
            false,
        )
    }

    #[test]
    fn borrowed_palettes_recolor_independent_views_and_removal_restores_canonical_colors() {
        let size = Size { cols: 15, rows: 2, cell_width: 8, cell_height: 16 };
        let mut source = blank(Size { cols: 5, rows: 1, ..size });
        source.foreground = [201, 202, 203];
        source.background = [21, 22, 23];
        source.palette = vec![[60, 70, 80]; 17];
        source.palette[1] = [101, 102, 103];
        source.palette[2] = [111, 112, 113];
        source.palette[3] = [121, 122, 123];
        source.palette[16] = [131, 132, 133];
        text(&mut source, 0, 0, "DAREF", false);
        for cell in &mut source.rows[0].cells {
            cell.style.foreground_source = ColorSource::Default;
            cell.style.background_source = ColorSource::Default;
            cell.style.underline_source = ColorSource::Default;
            cell.style.underline = 1;
        }
        let ansi = &mut source.rows[0].cells[1].style;
        ansi.foreground_source = ColorSource::Palette(1);
        ansi.background_source = ColorSource::Palette(2);
        ansi.underline_source = ColorSource::Palette(3);
        let literal = &mut source.rows[0].cells[2].style;
        literal.foreground_source = ColorSource::Rgb([141, 142, 143]);
        literal.background_source = ColorSource::Rgb([151, 152, 153]);
        literal.underline_source = ColorSource::Rgb([161, 162, 163]);
        let extended = &mut source.rows[0].cells[3].style;
        extended.foreground_source = ColorSource::Palette(16);
        extended.background_source = ColorSource::Palette(16);
        extended.underline_source = ColorSource::Palette(16);
        source.rows[0].cells[4].style.foreground_source = ColorSource::Palette(1);
        let rows = source.rows.clone();
        let cells = source.rows[0].cells.as_ptr();
        let canonical_palette = source.palette.clone();
        let mut first = [[10, 20, 30]; 16];
        first[1] = [31, 32, 33];
        first[2] = [41, 42, 43];
        first[3] = [51, 52, 53];
        let mut second = [[90, 100, 110]; 16];
        second[1] = [61, 62, 63];
        second[2] = [71, 72, 73];
        second[3] = [81, 82, 83];
        let palettes = [
            Some(PaletteOverride {
                foreground: [11, 12, 13],
                background: [14, 15, 16],
                palette: &first,
            }),
            Some(PaletteOverride {
                foreground: [17, 18, 19],
                background: [24, 25, 26],
                palette: &second,
            }),
            None,
        ];
        let mut scene = Scene {
            size,
            surfaces: palettes.into_iter().enumerate().map(|(index, palette)| Surface {
                id: ["first", "second", "canonical"][index],
                rect: Rect { x: index as u16 * 5, y: 0, width: 5, height: 1 },
                frame: &source,
                palette,
                content: SurfaceContent::Cells,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
            }).collect(),
        };
        let mut host = ColorProbe {
            truecolor: true,
            foreground: None,
            background: None,
            ..ColorProbe::default()
        };
        for (index, rgb) in source.palette.iter().take(16).enumerate() {
            host.palette.insert(index, *rgb);
        }
        let mut physical_palette = [[60, 70, 80]; 256];
        physical_palette[..source.palette.len()].copy_from_slice(&source.palette);
        let mut physical = bkpr_terminal_engine::Terminal::new(size).unwrap();
        physical.set_default_colors(source.foreground, source.background, None).unwrap();
        physical.set_palette(&physical_palette).unwrap();
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let mut bytes = Vec::new();
        renderer.render(&mut bytes, &scene, &host, &mut terminal).unwrap();
        physical.feed(&bytes).unwrap();
        let observed = physical.snapshot().unwrap();
        for (index, (foreground, background, palette)) in [
            ([11, 12, 13], [14, 15, 16], first.as_slice()),
            ([17, 18, 19], [24, 25, 26], second.as_slice()),
            (source.foreground, source.background, source.palette.as_slice()),
        ].into_iter().enumerate() {
            let view = &observed.rows[0].cells[index * 5..index * 5 + 5];
            let expected = [
                (foreground, background, foreground),
                (palette[1], palette[2], palette[3]),
                ([141, 142, 143], [151, 152, 153], [161, 162, 163]),
                ([131, 132, 133], [131, 132, 133], [131, 132, 133]),
                (palette[1], background, palette[1]),
            ];
            for (cell, (fg, bg, underline)) in view.iter().zip(expected) {
                assert_eq!(cell.style.foreground, fg, "view {index}: {}", cell.text);
                assert_eq!(cell.style.background, bg, "view {index}: {}", cell.text);
                assert_eq!(cell.style.underline_color, underline, "view {index}: {}", cell.text);
                assert_eq!(cell.style.underline, 1);
            }
        }
        for surface in &mut scene.surfaces {
            surface.palette = None;
        }
        bytes.clear();
        renderer.render(&mut bytes, &scene, &host, &mut terminal).unwrap();
        physical.feed(&bytes).unwrap();
        let restored = physical.snapshot().unwrap();
        for index in 0..3 {
            assert_eq!(
                &restored.rows[0].cells[index * 5..index * 5 + 5],
                &observed.rows[0].cells[10..15],
            );
        }
        assert_eq!(source.rows, rows);
        assert_eq!(source.rows[0].cells.as_ptr(), cells);
        assert_eq!(source.palette, canonical_palette);
        assert_eq!(source.foreground, [201, 202, 203]);
        assert_eq!(source.background, [21, 22, 23]);
    }

    #[test]
    fn borrowed_palette_preserves_annotations_clipping_and_focused_cursor() {
        let size = Size { cols: 6, rows: 3, cell_width: 8, cell_height: 16 };
        let mut source = blank(Size { cols: 4, rows: 1, ..size });
        text(&mut source, 0, 0, "ABCD", false);
        source.cursor.x = 2;
        source.cursor.y = 0;
        source.cursor.visible = false;
        for cell in &mut source.rows[0].cells {
            cell.style.foreground_source = ColorSource::Palette(1);
            cell.style.background_source = ColorSource::Default;
            cell.style.underline_source = ColorSource::Default;
            cell.style.bold = true;
        }
        source.rows[0].cells[1].style.inverse = true;
        let palette = [[31, 32, 33]; 16];
        let selection = [SelectionSpan { row: 0, start: 1, end: 2 }];
        let search = [SearchSpan { row: 0, start: 2, end: 3, selected: true }];
        let scene = Scene {
            size,
            surfaces: vec![Surface {
                id: "clipped",
                rect: Rect { x: 1, y: 1, width: 4, height: 1 },
                frame: &source,
                palette: Some(PaletteOverride {
                    foreground: [11, 12, 13],
                    background: [21, 22, 23],
                    palette: &palette,
                }),
                content: SurfaceContent::CellsClipped(Rect { x: 1, y: 0, width: 2, height: 1 }),
                selection: &selection,
                search: &search,
                focused: true,
                faint: true,
            }],
        };
        let host = ColorProbe { truecolor: true, ..ColorProbe::default() };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let mut bytes = Vec::new();
        renderer.render(&mut bytes, &scene, &host, &mut terminal).unwrap();
        let mut physical = bkpr_terminal_engine::Terminal::new(size).unwrap();
        physical.feed(&bytes).unwrap();
        let observed = physical.snapshot().unwrap();
        assert_eq!(observed.rows[1].cells[1].text.trim(), "");
        assert_eq!(observed.rows[1].cells[2].text, "B");
        assert_eq!(observed.rows[1].cells[3].text, "C");
        assert_eq!(observed.rows[1].cells[4].text.trim(), "");
        let selected = &observed.rows[1].cells[2].style;
        assert!(!selected.inverse, "selection toggles the original inverse intent");
        assert!(selected.bold && selected.faint);
        assert_eq!(selected.foreground, [31, 32, 33]);
        assert_eq!(selected.background, [21, 22, 23]);
        let searched = &observed.rows[1].cells[3].style;
        assert!(searched.inverse && searched.bold && searched.faint);
        assert_eq!(searched.underline, 1);
        let cursor = &renderer.canvas.as_ref().unwrap().cursor;
        assert_eq!((cursor.x, cursor.y, cursor.visible), (3, 1, false));
        assert!(source.rows[0].cells[1].style.inverse);
        assert!(!source.rows[0].cells[2].style.inverse);
        assert!(!source.rows[0].cells[2].style.faint);
        assert_eq!(source.rows[0].cells[2].style.underline, 0);
    }
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
                    palette: None,
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
                    palette: None,
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
                    palette: None,
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
                    palette: None,
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
                palette: None,
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
    #[test]
    fn hidden_focus_anchor_finishes_physical_output_across_navigation_and_focus_changes() {
        let size = Size {
            cols: 16,
            rows: 6,
            cell_width: 8,
            cell_height: 16,
        };
        let mut native = blank(Size { cols: 12, ..size });
        text(&mut native, 0, 0, "native", false);
        native.cursor.x = 2;
        native.cursor.y = 1;
        native.cursor.visible = true;
        let mut sidebar = blank(Size { cols: 4, ..size });
        text(&mut sidebar, 1, 2, "one", true);
        sidebar.cursor.x = 1;
        sidebar.cursor.y = 2;
        let mut popup = blank(Size { cols: 7, rows: 3, ..size });
        text(&mut popup, 0, 0, "details", false);
        popup.cursor.x = 1;
        popup.cursor.y = 1;
        popup.cursor.visible = true;
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        let paint = |renderer: &mut Renderer,
                     terminal: &mut TerminalGuard,
                     sidebar: &Frame,
                     popup: &Frame,
                     focus: &str| {
            let scene = Scene {
                size,
                surfaces: vec![
                    Surface {
                        id: "native",
                        rect: Rect { x: 4, y: 0, width: 12, height: 6 },
                        frame: &native,
                        palette: None,
                        selection: &[],
                        search: &[],
                        focused: focus == "native",
                        faint: false,
                        content: SurfaceContent::All,
                    },
                    Surface {
                        id: "sidebar",
                        rect: Rect { x: 0, y: 0, width: 4, height: 6 },
                        frame: sidebar,
                        palette: None,
                        selection: &[],
                        search: &[],
                        focused: focus == "sidebar",
                        faint: false,
                        content: SurfaceContent::Cells,
                    },
                    Surface {
                        id: "popup",
                        rect: Rect { x: 7, y: 2, width: 7, height: 3 },
                        frame: popup,
                        palette: None,
                        selection: &[],
                        search: &[],
                        focused: focus == "modal",
                        faint: false,
                        content: SurfaceContent::Cells,
                    },
                ],
            };
            let mut bytes = Vec::new();
            renderer.render(&mut bytes, &scene, &ColorProbe::default(), terminal).unwrap();
            String::from_utf8(bytes).unwrap()
        };
        let first = paint(&mut renderer, &mut terminal, &sidebar, &popup, "sidebar");
        assert!(first.ends_with("\x1b[3;2H\x1b[?2026l"));
        assert!(!first.contains("\x1b[?25h"));
        assert!(!renderer.canvas.as_ref().unwrap().cursor.visible);
        assert!(paint(&mut renderer, &mut terminal, &sidebar, &popup, "sidebar").is_empty());

        // A keyboard selection can move without changing any painted cell.
        sidebar.cursor.y = 4;
        let moved = paint(&mut renderer, &mut terminal, &sidebar, &popup, "sidebar");
        assert!(moved.ends_with("\x1b[5;2H\x1b[?2026l"));
        assert!(!moved.contains("\x1b[?25h"));
        assert_eq!(moved.matches('H').count(), 1);
        text(&mut popup, 0, 2, "updated", false);
        let metadata = paint(&mut renderer, &mut terminal, &sidebar, &popup, "sidebar");
        assert!(metadata.ends_with("\x1b[5;2H\x1b[?2026l"));
        assert!(!metadata.contains("\x1b[?25h"));

        let modal = paint(&mut renderer, &mut terminal, &sidebar, &popup, "modal");
        assert!(modal.contains("\x1b[4;9H\x1b[?25h"));
        assert!(renderer.canvas.as_ref().unwrap().cursor.visible);
        let pane = paint(&mut renderer, &mut terminal, &sidebar, &popup, "native");
        assert!(pane.contains("\x1b[2;7H\x1b[?25h"));
        assert!(renderer.canvas.as_ref().unwrap().cursor.visible);
        let restored = paint(&mut renderer, &mut terminal, &sidebar, &popup, "sidebar");
        assert!(restored.ends_with("\x1b[5;2H\x1b[?2026l"));
        assert!(!restored.contains("\x1b[?25h"));
        assert!(!renderer.canvas.as_ref().unwrap().cursor.visible);
        assert!(paint(&mut renderer, &mut terminal, &sidebar, &popup, "sidebar").is_empty());
    }
    #[test]
    fn hidden_focus_anchor_respects_cell_clip_and_source_bounds() {
        let size = Size {
            cols: 10,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        };
        let mut source = blank(Size { cols: 6, rows: 2, ..size });
        text(&mut source, 0, 0, "A界BC", false);
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        for (x, expected) in [(3, Some(5)), (1, None), (3, Some(5)), (6, None)] {
            source.cursor.x = x;
            let scene = Scene {
                size,
                surfaces: vec![Surface {
                    id: "clipped",
                    rect: Rect { x: 2, y: 1, width: 6, height: 2 },
                    frame: &source,
                    palette: None,
                    content: SurfaceContent::CellsClipped(Rect {
                        x: 2, y: 0, width: 3, height: 1,
                    }),
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                }],
            };
            let mut bytes = Vec::new();
            renderer.render(&mut bytes, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            let output = String::from_utf8(bytes).unwrap();
            let canvas = renderer.canvas.as_ref().unwrap();
            assert!(!canvas.cursor.visible);
            assert!(!output.contains("\x1b[?25h"));
            // The half-visible wide glyph remains blank, independently of the
            // focused hidden position.
            assert_eq!(canvas.rows[1].cells[4].text, " ");
            assert_eq!(canvas.rows[1].cells[4].width, 1);
            if let Some(x) = expected {
                assert_eq!((canvas.cursor.x, canvas.cursor.y), (x, 1));
                assert!(output.ends_with("\x1b[2;6H\x1b[?2026l"));
            } else {
                assert_eq!((canvas.cursor.x, canvas.cursor.y), (size.cols, size.rows));
                assert!(!output.contains('H'));
            }
        }
    }
    #[test]
    fn passive_opaque_plate_preserves_hidden_anchor_but_occludes_visible_cursor() {
        let size = Size {
            cols: 10,
            rows: 6,
            cell_width: 8,
            cell_height: 16,
        };
        let mut sidebar = blank(size);
        text(&mut sidebar, 1, 2, "selected", true);
        sidebar.cursor.x = 1;
        sidebar.cursor.y = 2;
        let mut plate = blank(size);
        text(&mut plate, 1, 2, "selected", false);
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        for visible in [false, true] {
            sidebar.cursor.visible = visible;
            let scene = Scene {
                size,
                surfaces: vec![
                    Surface {
                        id: "sidebar",
                        rect: Rect { x: 0, y: 0, width: 10, height: 6 },
                        frame: &sidebar,
                        palette: None,
                        content: SurfaceContent::Cells,
                        selection: &[],
                        search: &[],
                        focused: true,
                        faint: false,
                    },
                    Surface {
                        id: "passive-joined-anchor",
                        rect: Rect { x: 0, y: 0, width: 10, height: 6 },
                        frame: &plate,
                        palette: None,
                        content: SurfaceContent::CellsClipped(Rect {
                            x: 1, y: 2, width: 8, height: 1,
                        }),
                        selection: &[],
                        search: &[],
                        focused: false,
                        faint: false,
                    },
                ],
            };
            let mut bytes = Vec::new();
            renderer.render(&mut bytes, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            let output = String::from_utf8(bytes).unwrap();
            let canvas = renderer.canvas.as_ref().unwrap();
            assert_eq!((canvas.cursor.x, canvas.cursor.y), (1, 2));
            assert!(!canvas.cursor.visible);
            assert!(!canvas.rows[2].cells[1].style.inverse);
            assert!(!output.contains("\x1b[?25h"));
            if !visible {
                assert!(output.ends_with("\x1b[3;2H\x1b[?2026l"));
            } else {
                // Revealing the source cursor under an unchanged passive
                // plate changes neither its hidden anchor nor physical output.
                assert!(output.is_empty());
            }
        }
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
    fn clipped_cells_keep_source_origin_and_bound_wide_glyphs_cursor_hits_and_images() {
        for content in [
            SurfaceContent::CellsClipped as fn(Rect) -> SurfaceContent,
            SurfaceContent::CellsWithUnderlay,
        ] {
        let size = Size {
            cols: 12,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        };
        let mut native = blank(size);
        text(&mut native, 0, 1, "............", false);
        let local = Size { cols: 6, rows: 2, ..size };
        let mut source = blank(local);
        text(&mut source, 0, 0, "A界BC", false);
        source.cursor.visible = true;
        source.cursor.x = 3;
        source.cursor.y = 0;
        let effects = effect_frame(local);
        source.images = effects.images;
        source.placements = effects.placements;
        let mut scene = Scene {
            size,
            surfaces: vec![
                Surface {
                    id: "native",
                    rect: Rect { x: 0, y: 0, width: 12, height: 4 },
                    frame: &native,
                    palette: None,
                    content: SurfaceContent::All,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                },
                Surface {
                    id: "clipped",
                    rect: Rect { x: 2, y: 1, width: 6, height: 2 },
                    frame: &source,
                    palette: None,
                    content: content(Rect {
                        x: 2, y: 0, width: 3, height: 1,
                    }),
                    selection: &[],
                    search: &[],
                    focused: true,
                    faint: false,
                },
            ],
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        renderer.render(&mut Vec::new(), &scene, &ColorProbe::default(), &mut terminal).unwrap();
        let canvas = renderer.canvas.as_ref().unwrap();
        assert_eq!(canvas.rows[1].cells[3].text, ".");
        assert_eq!(canvas.rows[1].cells[4].text, " ");
        assert_eq!(canvas.rows[1].cells[4].width, 1);
        assert_eq!(canvas.rows[1].cells[5].text, "B");
        assert_eq!(canvas.rows[1].cells[6].text, "C");
        assert_eq!(canvas.rows[1].cells[7].text, ".");
        assert!(canvas.cursor.visible);
        assert_eq!((canvas.cursor.x, canvas.cursor.y), (5, 1));
        assert!(terminal.graphics.image_ids.is_empty());
        let hits = scene.hit_map();
        for (x, expected) in [(3, "native"), (4, "clipped"), (6, "clipped"), (7, "native")] {
            assert_eq!(scene.hit_test(x, 1), Some(expected));
            assert_eq!(hits.hit_test(x, 1), Some(expected));
        }
        let mut idle = Vec::new();
        renderer.render(&mut idle, &scene, &ColorProbe::default(), &mut terminal).unwrap();
        assert!(idle.is_empty());
        scene.surfaces[1].content = content(Rect {
            x: 0, y: 0, width: 1, height: 1,
        });
        renderer.render(&mut Vec::new(), &scene, &ColorProbe::default(), &mut terminal).unwrap();
        let canvas = renderer.canvas.as_ref().unwrap();
        assert_eq!(canvas.rows[1].cells[2].text, "A");
        assert_eq!(canvas.rows[1].cells[5].text, ".");
        assert!(!canvas.cursor.visible);
        }
    }

    #[test]
    fn clipped_cells_intersect_overflowing_clips_with_source_and_viewport() {
        for content in [
            SurfaceContent::CellsClipped as fn(Rect) -> SurfaceContent,
            SurfaceContent::CellsWithUnderlay,
        ] {
        let size = Size { cols: 10, rows: 5, cell_width: 8, cell_height: 16 };
        let source = blank(Size { cols: 3, rows: 2, ..size });
        let mut surface = Surface {
            id: "clipped",
            rect: Rect { x: 2, y: 1, width: 6, height: 3 },
            frame: &source,
            palette: None,
            content: content(Rect {
                x: 1, y: 1, width: u16::MAX, height: u16::MAX,
            }),
            selection: &[],
            search: &[],
            focused: false,
            faint: false,
        };
        assert_eq!(surface.cell_rect(), Some(Rect { x: 3, y: 2, width: 2, height: 1 }));
        surface.content = content(Rect {
            x: u16::MAX, y: 0, width: u16::MAX, height: 1,
        });
        assert_eq!(surface.cell_rect(), None);
        let scene = Scene { size, surfaces: vec![surface] };
        assert_eq!(scene.hit_test(2, 1), None);
        assert_eq!(scene.hit_map().hit_test(2, 1), None);
        }
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
                    palette: None,
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
                    palette: None,
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
                    palette: None,
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
                    palette: None,
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

    /// Inspect emitted Kitty placements after decoding them with the native
    /// consumer, using the existing phase-preserving raster projection.
    fn native_material_pixels(frame: &Frame) -> Vec<Option<([u8; 4], bool)>> {
        let size = frame.size;
        let width = (u32::from(size.cols) * size.cell_width) as usize;
        let height = (u32::from(size.rows) * size.cell_height) as usize;
        let mut pixels = vec![None; width * height];
        let prepared = geometry::prepare(frame, size).unwrap();
        for p in prepared.placements {
            let (w, h) = if let Some(key) = p.derived {
                (key.visible.width, key.visible.height)
            } else if let Some((cols, rows)) = p.cells {
                (cols * size.cell_width - p.offset_x, rows * size.cell_height - p.offset_y)
            } else {
                (p.source.width, p.source.height)
            };
            let key = p.derived.unwrap_or(geometry::DerivedKey {
                image: p.image,
                source: p.source,
                width: w,
                height: h,
                visible: geometry::Rect { x: 0, y: 0, width: w, height: h },
            });
            let rgba = geometry::raster(prepared.images[&p.image], key).unwrap();
            let left = (p.col * size.cell_width + p.offset_x) as usize;
            let top = (p.row * size.cell_height + p.offset_y) as usize;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let destination = &mut pixels[(top + y) * width + left + x];
                    assert!(destination.is_none(), "feedback duplicated material/shadows");
                    *destination = Some((
                        rgba[(y * w as usize + x) * 4..][..4].try_into().unwrap(),
                        p.z >= 0,
                    ));
                }
            }
        }
        pixels
    }

    #[test]
    fn feedback_underlays_preserve_native_pixels_sampling_identity_and_popup_occlusion() {
        use bkpr_terminal_engine::Terminal;
        let size = Size { cols: 7, rows: 5, cell_width: 4, cell_height: 6 };
        let rect = Rect { x: 0, y: 0, width: 7, height: 5 };
        let vertical = Rect { x: 3, y: 0, width: 1, height: 5 };
        let horizontal = Rect { x: 0, y: 2, width: 7, height: 1 };
        let popup_rect = Rect { x: 3, y: 2, width: 2, height: 1 };
        for (image_width, image_height) in [(20, 24), (7, 5)] {
            let mut native = blank(size);
            text(&mut native, 0, 4, "native", false);
            native.cursor.visible = true;
            native.cursor.x = 1;
            native.cursor.y = 4;
            let mut material = graphics_frame(size);
            material.images.push(Arc::new(Image {
                id: 91,
                generation: 7,
                width: image_width,
                height: image_height,
                format: PixelFormat::Rgba,
                pixels: (0..image_width * image_height)
                    .flat_map(|i| [(i * 17) as u8, (i * 31) as u8, (i * 47) as u8, 128])
                    .collect(),
            }));
            material.placements.push(Placement {
                image_id: 91, generation: 7, placement_id: 1,
                col: 1, row: 0, offset_x: 1, offset_y: 2,
                width: 20, height: 24,
                source_x: 0, source_y: 0,
                source_width: image_width, source_height: image_height,
                layer: ImageLayer::AboveText, z: i32::MAX,
            });
            let source = Arc::clone(&material.images[0]);
            let canonical = material.placements.clone();
            let mut feedback = blank(size);
            let mut style = default_style();
            style.foreground_source = ColorSource::Palette(6);
            style.foreground = feedback.palette[6];
            crate::window::paint_edge_feedback(&mut feedback, &[vertical, horizontal], &style);
            let mut popup = blank(popup_rect.size(size));
            text(&mut popup, 0, 0, "PP", false);
            let mut scene = Scene {
                size,
                surfaces: vec![
                    Surface {
                        id: "native", rect, frame: &native, content: SurfaceContent::All,
                        palette: None,
                        selection: &[], search: &[], focused: true, faint: false,
                    },
                    Surface {
                        id: "material", rect, frame: &material, content: SurfaceContent::Graphics,
                        palette: None,
                        selection: &[], search: &[], focused: false, faint: false,
                    },
                ],
            };
            let mut renderer = Renderer::default();
            let mut terminal = terminal();
            let mut consumer = Terminal::new(Size { rows: size.rows + 1, ..size }).unwrap();
            let mut output = Vec::new();
            renderer.render(&mut output, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            consumer.feed(&output).unwrap();
            let baseline = native_material_pixels(&consumer.snapshot().unwrap());
            let alias = renderer.aliases[&("material".into(), source.key())];
            for (id, segment) in [("vertical", vertical), ("horizontal", horizontal)] {
                scene.surfaces.push(Surface {
                    id, rect, frame: &feedback, content: SurfaceContent::CellsWithUnderlay(segment),
                    palette: None,
                    selection: &[], search: &[], focused: false, faint: false,
                });
            }
            output.clear();
            renderer.render(&mut output, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            consumer.feed(&output).unwrap();
            let snapshot = consumer.snapshot().unwrap();
            let actual = native_material_pixels(&snapshot);
            assert_eq!(snapshot.rows[2].cells[3].text, "┼");
            let width = usize::from(size.cols) * size.cell_width as usize;
            for (index, (before, after)) in baseline.iter().zip(&actual).enumerate() {
                assert_eq!(before.map(|p| p.0), after.map(|p| p.0), "material pixel {index} changed");
                if let Some((_, above)) = after {
                    let x = (index % width) as u16 / size.cell_width as u16;
                    let y = (index / width) as u16 / size.cell_height as u16;
                    assert_eq!(*above, !vertical.contains(x, y) && !horizontal.contains(x, y));
                }
            }
            assert!(terminal.graphics.projections.iter().all(|p| p.image == alias));
            assert_eq!(renderer.aliases[&("material".into(), source.key())], alias);
            assert!(Arc::ptr_eq(&material.images[0], &source));
            assert_eq!(material.placements, canonical);
            assert!(renderer.canvas.as_ref().unwrap().cursor.visible);
            assert_eq!(scene.hit_test(3, 2), Some("horizontal"));
            output.clear();
            renderer.render(&mut output, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            assert!(output.is_empty(), "retained feedback should be idle");
            scene.surfaces.push(Surface {
                id: "popup", rect: popup_rect, frame: &popup, content: SurfaceContent::Cells,
                palette: None,
                selection: &[], search: &[], focused: false, faint: false,
            });
            renderer.render(&mut output, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            consumer.feed(&output).unwrap();
            let snapshot = consumer.snapshot().unwrap();
            let covered = native_material_pixels(&snapshot);
            for (index, (before, after)) in actual.iter().zip(&covered).enumerate() {
                let x = (index % width) as u16 / size.cell_width as u16;
                let y = (index / width) as u16 / size.cell_height as u16;
                assert_eq!(*after, if popup_rect.contains(x, y) { None } else { *before });
            }
            assert_eq!(snapshot.rows[2].cells[3].text, "P");
            assert_eq!(scene.hit_map().hit_test(3, 2), Some("popup"));
            // Normal opaque cells must still remove, not demote, images.
            scene.surfaces.pop();
            scene.surfaces[2].content = SurfaceContent::CellsClipped(vertical);
            scene.surfaces[3].content = SurfaceContent::CellsClipped(horizontal);
            output.clear();
            renderer.render(&mut output, &scene, &ColorProbe::default(), &mut terminal).unwrap();
            consumer.feed(&output).unwrap();
            let opaque = native_material_pixels(&consumer.snapshot().unwrap());
            for (index, (before, after)) in baseline.iter().zip(&opaque).enumerate() {
                let x = (index % width) as u16 / size.cell_width as u16;
                let y = (index / width) as u16 / size.cell_height as u16;
                assert_eq!(*after, if vertical.contains(x, y) || horizontal.contains(x, y) {
                    None
                } else {
                    *before
                });
            }
        }
    }

    #[test]
    fn feedback_demotes_only_earlier_above_text_material_in_original_scene_order() {
        let size = Size { cols: 2, rows: 2, cell_width: 4, cell_height: 6 };
        let rect = Rect { x: 0, y: 0, width: 2, height: 2 };
        let first = effect_frame(size);
        let mut second = effect_frame(size);
        second.placements[0].layer = ImageLayer::BelowText;
        let third = effect_frame(size);
        let later = effect_frame(size);
        let mut background = effect_frame(size);
        background.placements[0].layer = ImageLayer::BelowBackground;
        let feedback = blank(size);
        let image_surface = |id, frame| Surface {
            id, rect, frame, content: SurfaceContent::Graphics,
            palette: None,
            selection: &[], search: &[], focused: false, faint: false,
        };
        let scene = Scene {
            size,
            surfaces: vec![
                image_surface("background", &background),
                image_surface("first", &first),
                image_surface("second", &second),
                image_surface("third", &third),
                Surface {
                    id: "feedback", rect, frame: &feedback,
                    palette: None,
                    content: SurfaceContent::CellsWithUnderlay(rect),
                    selection: &[], search: &[], focused: false, faint: false,
                },
                image_surface("later", &later),
            ],
        };
        let mut renderer = Renderer::default();
        let mut terminal = terminal();
        renderer.render(&mut Vec::new(), &scene, &ColorProbe::default(), &mut terminal).unwrap();
        let depth = |id: &str| {
            let key = renderer.aliases[&(id.to_owned(), first.images[0].key())];
            let projections: Vec<_> = terminal.graphics.projections.iter()
                .filter(|p| p.image == key).collect();
            assert_eq!(projections.len(), 1, "fully covered images need no fragmentation");
            assert!(projections[0].derived.is_none(), "full demotion must retain resource identity");
            projections[0].z
        };
        assert!(depth("background") < i32::MIN / 2);
        assert!(depth("first") >= i32::MIN / 2);
        assert!(depth("first") < depth("second"));
        assert!(depth("second") < depth("third"));
        assert!(depth("third") < 0);
        assert!(depth("later") >= 0, "later graphics must not be demoted retroactively");
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
                    palette: None,
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
                    palette: None,
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
                    palette: None,
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
            palette: None,
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
                    palette: None,
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
                    palette: None,
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
                    palette: None,
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
                palette: None,
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
                    palette: None,
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
                    palette: None,
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
                palette: None,
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
                palette: None,
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
                    palette: None,
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
                    palette: None,
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
                canonical_style(&source.rows[0].cells[column].style, &source);
            expected.inverse = !expected.inverse;
            assert_eq!(canvas.rows[0].cells[column].style, expected);
            assert_eq!(
                canvas.rows[0].cells[column + 6].style,
                canonical_style(&source.rows[0].cells[column].style, &source)
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
            canonical_style(&source.rows[0].cells[0].style, &source)
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
        let mut selected = canonical_style(&source.rows[0].cells[0].style, &source);
        selected.inverse = !selected.inverse;
        assert_eq!(
            canvas.rows[0].cells[0].style, selected,
            "selection overrides both search styles"
        );
        assert_eq!(
            canvas.rows[0].cells[1].style, selected,
            "wide glyph is annotated atomically"
        );
        let mut chosen = canonical_style(&source.rows[0].cells[2].style, &source);
        chosen.inverse = !chosen.inverse;
        chosen.bold = true;
        chosen.underline = chosen.underline.max(1);
        assert_eq!(canvas.rows[0].cells[2].style, chosen);
        let mut ordinary = canonical_style(&source.rows[0].cells[3].style, &source);
        ordinary.underline = ordinary.underline.max(1);
        assert_eq!(canvas.rows[0].cells[3].style, ordinary);
        assert_eq!(canvas.rows[0].cells[2].text, "e\u{301}");
        assert_eq!(
            canvas.rows[0].cells[2 + 6].style,
            canonical_style(&source.rows[0].cells[2].style, &source)
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
                    palette: None,
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
                    palette: None,
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
