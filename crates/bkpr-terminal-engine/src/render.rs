use super::*;
use sdk::{
    render as native,
    screen::{CellContentTag, CellWide},
    style::{RgbColor, StyleColor},
};
use std::collections::HashSet;

pub(super) fn rgb(color: RgbColor) -> [u8; 3] {
    [color.r, color.g, color.b]
}
pub(super) fn native_rgb([r, g, b]: [u8; 3]) -> RgbColor {
    RgbColor { r, g, b }
}
fn source(color: StyleColor) -> ColorSource {
    match color {
        StyleColor::None => ColorSource::Default,
        StyleColor::Palette(index) => ColorSource::Palette(index.0),
        StyleColor::Rgb(color) => ColorSource::Rgb(rgb(color)),
    }
}
fn resolve(color: StyleColor, colors: &native::Colors, fallback: [u8; 3]) -> [u8; 3] {
    match color {
        StyleColor::None => fallback,
        StyleColor::Palette(index) => rgb(colors.palette[index.0 as usize]),
        StyleColor::Rgb(color) => rgb(color),
    }
}
/// SDK-owned reusable extraction state; only Bkpr's immutable image cache is
/// retained across snapshots. Pixel buffers are captured once per generation.
pub(super) struct Resources {
    state: sdk::RenderState<'static>,
    rows: native::RowIterator<'static>,
    cells: native::CellIterator<'static>,
    images: native::ImageIterator<'static>,
    pub image_cache: HashMap<(u32, u64), Arc<Image>>,
}
impl Resources {
    pub fn new() -> Result<Self> {
        Ok(Self {
            state: sdk::RenderState::new()?,
            rows: native::RowIterator::new()?,
            cells: native::CellIterator::new()?,
            images: native::ImageIterator::new()?,
            image_cache: HashMap::new(),
        })
    }
    pub fn invalidate(&mut self) -> Result<()> {
        self.state = sdk::RenderState::new()?;
        Ok(())
    }
    pub fn capture(
        &mut self,
        terminal: &sdk::Terminal<'static, 'static>,
        size: Size,
    ) -> Result<Frame> {
        let snapshot = self.state.update(terminal)?;
        let colors = snapshot.colors()?;
        let cursor = snapshot.cursor()?;
        let mut row_iter = self.rows.update(&snapshot)?;
        let mut rows = Vec::with_capacity(size.rows as usize);
        let mut graphemes = Vec::new();
        while let Some(row) = row_iter.next() {
            let placeholders = row.raw_row()?.has_kitty_virtual_placeholder()?;
            let mut cell_iter = self.cells.update(row)?;
            let mut cells = Vec::with_capacity(size.cols as usize);
            while let Some(cell) = cell_iter.next() {
                let raw = cell.raw_cell()?;
                let width = match raw.wide()? {
                    CellWide::Wide => 2,
                    CellWide::SpacerHead | CellWide::SpacerTail => 0,
                    _ => 1,
                };
                let count = cell.graphemes_len()? as usize;
                if count > 65536 {
                    return Err(Error("grapheme exceeds bounded snapshot length".into()));
                }
                graphemes.resize(count, '\0');
                if count > 0 {
                    cell.graphemes_buf(&mut graphemes)?;
                }
                // Native placements, not Unicode placeholders, are authoritative.
                let text = if placeholders && graphemes.first() == Some(&'\u{10eeee}') {
                    String::new()
                } else {
                    graphemes.iter().collect()
                };
                let style = cell.style()?;
                let foreground = rgb(cell.fg_color()?.unwrap_or(colors.foreground));
                let background = rgb(cell.bg_color()?.unwrap_or(colors.background));
                let background_source = match raw.content_tag()? {
                    CellContentTag::BgColorPalette => {
                        ColorSource::Palette(raw.bg_color_palette()?.0)
                    }
                    CellContentTag::BgColorRgb => ColorSource::Rgb(background),
                    _ => source(style.bg_color),
                };
                let hyperlink = if raw.has_hyperlink()? {
                    read_hyperlink(terminal, cells.len() as u16, rows.len() as u32)?
                } else {
                    None
                };
                cells.push(Cell {
                    text,
                    width,
                    hyperlink,
                    style: Style {
                        foreground,
                        background,
                        underline_color: resolve(style.underline_color, &colors, foreground),
                        foreground_source: source(style.fg_color),
                        background_source,
                        underline_source: source(style.underline_color),
                        bold: style.bold,
                        italic: style.italic,
                        faint: style.faint,
                        blink: style.blink,
                        inverse: style.inverse,
                        invisible: style.invisible,
                        strikethrough: style.strikethrough,
                        overline: style.overline,
                        underline: style.underline as u8,
                    },
                });
            }
            rows.push(Row { cells });
        }
        let graphics = terminal.kitty_graphics()?;
        let mut iter = self.images.update(&snapshot)?;
        let mut seen = HashSet::new();
        let mut images = Vec::new();
        let mut placements = Vec::new();
        while let Some(placement) = iter.next() {
            if placements.len() >= 65536 {
                return Err(Error("image placements exceed snapshot limit".into()));
            }
            if placement.protocol()? != native::ImageProtocol::Kitty {
                return Err(Error("unsupported engine image protocol".into()));
            }
            let id = placement.image_id()?;
            let generation = placement.image_generation()?;
            let key = (id, generation);
            if seen.insert(key) {
                if let Some(image) = self.image_cache.get(&key) {
                    images.push(Arc::clone(image));
                } else if let Some(image) = read_image(&graphics, id, generation)? {
                    let image = Arc::new(image);
                    self.image_cache.insert(key, Arc::clone(&image));
                    images.push(image);
                }
            }
            if !self.image_cache.contains_key(&key) {
                continue;
            }
            placements.push(Placement {
                image_id: id,
                generation,
                placement_id: placement.kitty_placement_id()?.unwrap_or(0),
                col: placement.viewport_col()?,
                row: placement.viewport_row()?,
                offset_x: placement.cell_offset_x()?,
                offset_y: placement.cell_offset_y()?,
                width: placement.pixel_width()?,
                height: placement.pixel_height()?,
                source_x: placement.source_x()?,
                source_y: placement.source_y()?,
                source_width: placement.source_width()?,
                source_height: placement.source_height()?,
                layer: match placement.layer()? {
                    native::ImageLayer::BelowBackground => ImageLayer::BelowBackground,
                    native::ImageLayer::BelowText => ImageLayer::BelowText,
                    _ => ImageLayer::AboveText,
                },
                z: placement.z()?,
            });
        }
        self.image_cache.retain(|key, _| seen.contains(key));
        let scrollbar = terminal.scrollbar()?;
        let frame = Frame {
            size,
            rows,
            cursor: Cursor {
                x: cursor.viewport.map_or(0, |p| p.x),
                y: cursor.viewport.map_or(0, |p| p.y),
                visible: cursor.visible && cursor.viewport.is_some(),
                blinking: cursor.blinking,
                shape: match cursor.visual_style {
                    native::CursorVisualStyle::Bar => CursorShape::Bar,
                    native::CursorVisualStyle::Underline => CursorShape::Underline,
                    native::CursorVisualStyle::BlockHollow => CursorShape::Hollow,
                    _ => CursorShape::Block,
                },
                color: colors.cursor.map(rgb),
            },
            foreground: rgb(colors.foreground),
            background: rgb(colors.background),
            palette: colors.palette.into_iter().map(rgb).collect(),
            images,
            placements,
            alternate_screen: terminal.active_screen()? == sdk::screen::Screen::Alternate,
            scrollback_total: scrollbar.total,
            scrollback_offset: scrollbar
                .total
                .saturating_sub(scrollbar.len)
                .saturating_sub(scrollbar.offset),
        };
        snapshot.clean()?;
        Ok(frame)
    }
}
fn read_hyperlink(terminal: &sdk::Terminal<'_, '_>, x: u16, y: u32) -> Result<Option<String>> {
    let reference = terminal.grid_ref(sdk::terminal::Point::Viewport(
        sdk::terminal::PointCoordinate { x, y },
    ))?;
    let mut bytes = Vec::new();
    let len = match reference.hyperlink_uri(&mut bytes) {
        Ok(0) => return Ok(None),
        Ok(len) => len,
        Err(sdk::Error::OutOfSpace { required }) => required,
        Err(error) => return Err(error.into()),
    };
    if len > 65536 {
        return Err(Error("hyperlink exceeds bounded URI length".into()));
    }
    bytes.resize(len, 0);
    let len = reference.hyperlink_uri(&mut bytes)?;
    bytes.truncate(len);
    Ok(Some(
        String::from_utf8(bytes).map_err(|_| Error("invalid UTF-8 hyperlink".into()))?,
    ))
}
fn read_image(
    graphics: &sdk::kitty::graphics::Graphics<'_>,
    id: u32,
    generation: u64,
) -> Result<Option<Image>> {
    use sdk::kitty::graphics::ImageFormat;
    let native = graphics
        .image(id)
        .ok_or_else(|| Error("render placement references absent image".into()))?;
    let width = native.width()?;
    let height = native.height()?;
    let format = match native.format()? {
        ImageFormat::Rgba => PixelFormat::Rgba,
        ImageFormat::Rgb => PixelFormat::Rgb,
        ImageFormat::GrayAlpha => PixelFormat::GrayAlpha,
        ImageFormat::Gray => PixelFormat::Gray,
        _ => return Err(Error("unexpected decoded pixel format".into())),
    };
    let Some(pixels) = native.data()? else {
        return Ok(None);
    };
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| Error("image dimensions overflow".into()))?;
    if count == 0
        || pixels.len() > IMAGE_MAX_BYTES
        || count.checked_mul(format.bytes_per_pixel()) != Some(pixels.len())
    {
        return Err(Error("invalid bounded image buffer".into()));
    }
    Ok(Some(Image {
        id,
        generation,
        width,
        height,
        format,
        pixels: pixels.to_vec(),
    }))
}
