use super::*;
use std::{collections::HashSet, slice, sync::Arc};

unsafe fn image_get<T: Default>(
    state: GhosttyRenderStateImageIterator,
    key: GhosttyRenderStateImageData,
) -> Result<T> {
    let mut value = T::default();
    unsafe {
        check(ghostty_render_state_image_get(state, key, out(&mut value)))?;
    }
    Ok(value)
}
unsafe fn pixels_get<T: Default>(
    image: GhosttyKittyGraphicsImage,
    key: GhosttyKittyGraphicsImageData,
) -> Result<T> {
    let mut value = T::default();
    unsafe {
        check(ghostty_kitty_graphics_image_get(
            image,
            key,
            out(&mut value),
        ))?;
    }
    Ok(value)
}
fn resolve(
    color: GhosttyStyleColor,
    colors: &GhosttyRenderStateColors,
    fallback: [u8; 3],
) -> [u8; 3] {
    match color.tag {
        GHOSTTY_STYLE_COLOR_RGB => rgb(unsafe { color.value.rgb }),
        GHOSTTY_STYLE_COLOR_PALETTE => rgb(colors.palette[unsafe { color.value.palette } as usize]),
        _ => fallback,
    }
}
fn color_source(color: GhosttyStyleColor) -> ColorSource {
    match color.tag {
        GHOSTTY_STYLE_COLOR_RGB => ColorSource::Rgb(rgb(unsafe { color.value.rgb })),
        GHOSTTY_STYLE_COLOR_PALETTE => ColorSource::Palette(unsafe { color.value.palette }),
        _ => ColorSource::Default,
    }
}
impl Terminal {
    pub fn snapshot(&mut self) -> Result<Frame> {
        unsafe {
            if self
                .callbacks
                .hold_since
                .is_some_and(|since| since.elapsed() >= std::time::Duration::from_secs(1))
            {
                let mode = GhosttyTerminalModeConfig {
                    mode: bkpr_terminal_sync_output_mode(),
                    value: false,
                };
                check(ghostty_terminal_set(
                    self.terminal,
                    GHOSTTY_TERMINAL_OPT_MODE,
                    (&mode as *const GhosttyTerminalModeConfig).cast(),
                ))?;
                held::release(&mut self.callbacks)?;
            }
            if self.callbacks.hold_since.is_none() {
                check(ghostty_render_state_update(
                    self.callbacks.render,
                    self.terminal,
                ))?;
            }
            let mut colors = sized::<GhosttyRenderStateColors>();
            check(ghostty_render_state_get(
                self.callbacks.render,
                GHOSTTY_RENDER_STATE_DATA_COLORS,
                out(&mut colors),
            ))?;
            let mut cursor = sized::<GhosttyRenderStateCursor>();
            check(ghostty_render_state_get(
                self.callbacks.render,
                GHOSTTY_RENDER_STATE_DATA_CURSOR,
                out(&mut cursor),
            ))?;
            check(ghostty_render_state_get(
                self.callbacks.render,
                GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                out(&mut self.rows),
            ))?;
            let mut rows = Vec::with_capacity(self.size.rows as usize);
            let mut codepoints = Vec::<u32>::new();
            while ghostty_render_state_row_iterator_next(self.rows) {
                let mut raw_row = GhosttyRow::default();
                check(ghostty_render_state_row_get(
                    self.rows,
                    GHOSTTY_RENDER_STATE_ROW_DATA_RAW,
                    out(&mut raw_row),
                ))?;
                let mut kitty_placeholder_row = false;
                check(ghostty_row_get(
                    raw_row,
                    GHOSTTY_ROW_DATA_KITTY_VIRTUAL_PLACEHOLDER,
                    out(&mut kitty_placeholder_row),
                ))?;
                check(ghostty_render_state_row_get(
                    self.rows,
                    GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                    out(&mut self.cells),
                ))?;
                let mut cells = Vec::with_capacity(self.size.cols as usize);
                while ghostty_render_state_row_cells_next(self.cells) {
                    let mut raw = GhosttyCell::default();
                    check(ghostty_render_state_row_cells_get(
                        self.cells,
                        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                        out(&mut raw),
                    ))?;
                    let mut wide: GhosttyCellWide = 0;
                    check(ghostty_cell_get(
                        raw,
                        GHOSTTY_CELL_DATA_WIDE,
                        out(&mut wide),
                    ))?;
                    let width = match wide {
                        GHOSTTY_CELL_WIDE_WIDE => 2,
                        GHOSTTY_CELL_WIDE_SPACER_TAIL | GHOSTTY_CELL_WIDE_SPACER_HEAD => 0,
                        _ => 1,
                    };
                    let mut count = 0u32;
                    check(ghostty_render_state_row_cells_get(
                        self.cells,
                        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                        out(&mut count),
                    ))?;
                    if count > 65536 {
                        return Err(Error("grapheme exceeds bounded snapshot length".into()));
                    }
                    codepoints.resize(count as usize, 0);
                    if count > 0 {
                        check(ghostty_render_state_row_cells_get(
                            self.cells,
                            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                            codepoints.as_mut_ptr().cast(),
                        ))?;
                    }
                    // These are graphics protocol coordinates, not renderable
                    // glyphs. Keep the engine's resolved placements authoritative;
                    // replaying placeholders would let another terminal reinterpret
                    // their source image IDs, palette indices and diacritics.
                    let text = if kitty_placeholder_row && codepoints.first() == Some(&0x10eeee) {
                        String::new()
                    } else {
                        codepoints
                            .iter()
                            .map(|value| {
                                char::from_u32(*value).unwrap_or(char::REPLACEMENT_CHARACTER)
                            })
                            .collect()
                    };
                    let mut style = sized::<GhosttyStyle>();
                    check(ghostty_render_state_row_cells_get(
                        self.cells,
                        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                        out(&mut style),
                    ))?;
                    let mut fg = colors.foreground;
                    let mut bg = colors.background;
                    let fg_result = ghostty_render_state_row_cells_get(
                        self.cells,
                        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
                        out(&mut fg),
                    );
                    if fg_result != GHOSTTY_INVALID_VALUE {
                        check(fg_result)?;
                    }
                    let bg_result = ghostty_render_state_row_cells_get(
                        self.cells,
                        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                        out(&mut bg),
                    );
                    if bg_result != GHOSTTY_INVALID_VALUE {
                        check(bg_result)?;
                    }
                    let mut background_source = color_source(style.bg_color);
                    let mut content_tag: GhosttyCellContentTag = 0;
                    check(ghostty_cell_get(
                        raw,
                        GHOSTTY_CELL_DATA_CONTENT_TAG,
                        out(&mut content_tag),
                    ))?;
                    match content_tag {
                        GHOSTTY_CELL_CONTENT_BG_COLOR_PALETTE => {
                            let mut index: GhosttyColorPaletteIndex = 0;
                            check(ghostty_cell_get(
                                raw,
                                GHOSTTY_CELL_DATA_COLOR_PALETTE,
                                out(&mut index),
                            ))?;
                            background_source = ColorSource::Palette(index);
                        }
                        GHOSTTY_CELL_CONTENT_BG_COLOR_RGB => {
                            background_source = ColorSource::Rgb(rgb(bg))
                        }
                        _ => {}
                    }
                    let mut has_link = false;
                    check(ghostty_cell_get(
                        raw,
                        GHOSTTY_CELL_DATA_HAS_HYPERLINK,
                        out(&mut has_link),
                    ))?;
                    let hyperlink = if self.callbacks.hold_since.is_some() {
                        self.callbacks
                            .held_links
                            .get(&(cells.len() as u16, rows.len() as u32))
                            .cloned()
                    } else if has_link {
                        read_hyperlink(self.terminal, cells.len() as u16, rows.len() as u32)?
                    } else {
                        None
                    };
                    cells.push(Cell {
                        text,
                        width,
                        hyperlink,
                        style: Style {
                            foreground: rgb(fg),
                            background: rgb(bg),
                            underline_color: resolve(style.underline_color, &colors, rgb(fg)),
                            foreground_source: color_source(style.fg_color),
                            background_source,
                            underline_source: color_source(style.underline_color),
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
            let (images, placements) = self.capture_images()?;
            let mut screen: GhosttyTerminalScreen = 0;
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
                out(&mut screen),
            ))?;
            let mut scrollbar = GhosttyTerminalScrollbar::default();
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_SCROLLBAR,
                out(&mut scrollbar),
            ))?;
            let frame = Frame {
                size: self.size,
                rows,
                cursor: Cursor {
                    x: if cursor.viewport_has_value {
                        cursor.viewport_x
                    } else {
                        0
                    },
                    y: if cursor.viewport_has_value {
                        cursor.viewport_y
                    } else {
                        0
                    },
                    visible: cursor.visible && cursor.viewport_has_value,
                    blinking: cursor.blinking,
                    shape: match cursor.visual_style {
                        GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR => CursorShape::Bar,
                        GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => {
                            CursorShape::Underline
                        }
                        GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW => {
                            CursorShape::Hollow
                        }
                        _ => CursorShape::Block,
                    },
                    color: if colors.cursor_has_value {
                        Some(rgb(colors.cursor))
                    } else {
                        None
                    },
                },
                foreground: rgb(colors.foreground),
                background: rgb(colors.background),
                palette: colors.palette.iter().copied().map(rgb).collect(),
                images,
                placements,
                alternate_screen: screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE,
                scrollback_total: scrollbar.total,
                scrollback_offset: scrollbar
                    .total
                    .saturating_sub(scrollbar.len)
                    .saturating_sub(scrollbar.offset),
            };
            check(ghostty_render_state_clean(self.callbacks.render))?;
            Ok(frame)
        }
    }
    unsafe fn capture_images(&mut self) -> Result<(Vec<Arc<Image>>, Vec<Placement>)> {
        unsafe {
            check(ghostty_render_state_get(
                self.callbacks.render,
                GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR,
                out(&mut self.images),
            ))?;
            let mut graphics: GhosttyKittyGraphics = ptr::null_mut();
            let graphics_result = ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                out(&mut graphics),
            );
            check(graphics_result)?;
            let mut images = Vec::new();
            let mut placements = Vec::new();
            let mut seen = HashSet::new();
            while ghostty_render_state_image_iterator_next(self.images) {
                if placements.len() >= 65536 {
                    return Err(Error("image placements exceed snapshot limit".into()));
                }
                let protocol: GhosttyImageProtocol =
                    image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL)?;
                if protocol != GHOSTTY_IMAGE_PROTOCOL_KITTY {
                    return Err(Error("unsupported engine image protocol".into()));
                }
                let id = image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID)?;
                let generation = image_get(
                    self.images,
                    GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION,
                )?;
                let key = (id, generation);
                if seen.insert(key) {
                    if let Some(image) = self.callbacks.image_cache.get(&key) {
                        images.push(Arc::clone(image));
                    } else {
                        if self.callbacks.hold_since.is_some() {
                            continue;
                        }
                        if let Some(image) = read_image(graphics, id, generation)? {
                            let image = Arc::new(image);
                            self.callbacks.image_cache.insert(key, Arc::clone(&image));
                            images.push(image);
                        }
                    }
                }
                if !self.callbacks.image_cache.contains_key(&key) {
                    continue;
                }
                let layer: GhosttyRenderStateImageLayer =
                    image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER)?;
                placements.push(Placement {
                    image_id: id,
                    generation,
                    placement_id: image_get(
                        self.images,
                        GHOSTTY_RENDER_STATE_IMAGE_DATA_KITTY_PLACEMENT_ID,
                    )?,
                    col: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL)?,
                    row: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW)?,
                    offset_x: image_get(
                        self.images,
                        GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
                    )?,
                    offset_y: image_get(
                        self.images,
                        GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y,
                    )?,
                    width: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH)?,
                    height: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT)?,
                    source_x: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X)?,
                    source_y: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y)?,
                    source_width: image_get(
                        self.images,
                        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
                    )?,
                    source_height: image_get(
                        self.images,
                        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
                    )?,
                    layer: match layer {
                        GHOSTTY_RENDER_STATE_IMAGE_LAYER_BELOW_BACKGROUND => {
                            ImageLayer::BelowBackground
                        }
                        GHOSTTY_RENDER_STATE_IMAGE_LAYER_BELOW_TEXT => ImageLayer::BelowText,
                        _ => ImageLayer::AboveText,
                    },
                    z: image_get(self.images, GHOSTTY_RENDER_STATE_IMAGE_DATA_Z)?,
                });
            }
            self.callbacks
                .image_cache
                .retain(|key, _| seen.contains(key));
            Ok((images, placements))
        }
    }
    /// Advances native Kitty animation timing. The returned absolute deadline
    /// (milliseconds in the caller's monotonic clock) requests the next frame.
    pub fn tick_images(&mut self, now_ms: u64) -> Result<Option<u64>> {
        let mut graphics: GhosttyKittyGraphics = ptr::null_mut();
        unsafe {
            check(ghostty_terminal_get(
                self.terminal,
                GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                out(&mut graphics),
            ))?;
            let mut deadline = 0;
            let result = ghostty_kitty_graphics_animation_tick(graphics, now_ms, &mut deadline);
            if result == GHOSTTY_NO_VALUE {
                Ok(None)
            } else {
                check(result)?;
                Ok(Some(deadline))
            }
        }
    }
}

pub(super) unsafe fn read_hyperlink(
    terminal: GhosttyTerminal,
    x: u16,
    y: u32,
) -> Result<Option<String>> {
    let mut reference = sized::<GhosttyGridRef>();
    let point = GhosttyPoint {
        tag: GHOSTTY_POINT_TAG_VIEWPORT,
        value: GhosttyPointValue {
            coordinate: GhosttyPointCoordinate { x, y },
        },
    };
    unsafe {
        check(ghostty_terminal_grid_ref(terminal, point, &mut reference))?;
    }
    let mut len = 0usize;
    let result =
        unsafe { ghostty_grid_ref_hyperlink_uri(&reference, ptr::null_mut(), 0, &mut len) };
    if result == GHOSTTY_SUCCESS && len == 0 {
        return Ok(None);
    }
    if result != GHOSTTY_OUT_OF_SPACE {
        check(result)?;
    }
    if len > 65536 {
        return Err(Error("hyperlink exceeds bounded URI length".into()));
    }
    let mut bytes = vec![0; len];
    unsafe {
        check(ghostty_grid_ref_hyperlink_uri(
            &reference,
            bytes.as_mut_ptr(),
            bytes.len(),
            &mut len,
        ))?;
    }
    bytes.truncate(len);
    Ok(Some(
        String::from_utf8(bytes).map_err(|_| Error("invalid UTF-8 hyperlink".into()))?,
    ))
}

pub(super) unsafe fn read_image(
    graphics: GhosttyKittyGraphics,
    id: u32,
    generation: u64,
) -> Result<Option<Image>> {
    unsafe {
        let native = ghostty_kitty_graphics_image(graphics, id);
        if native.is_null() {
            return Err(Error("render placement references absent image".into()));
        }
        let width: u32 = pixels_get(native, GHOSTTY_KITTY_IMAGE_DATA_WIDTH)?;
        let height: u32 = pixels_get(native, GHOSTTY_KITTY_IMAGE_DATA_HEIGHT)?;
        let format: GhosttyKittyImageFormat = pixels_get(native, GHOSTTY_KITTY_IMAGE_DATA_FORMAT)?;
        let len: usize = pixels_get(native, GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN)?;
        let mut pixels: *const u8 = ptr::null();
        let result = ghostty_kitty_graphics_image_get(
            native,
            GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR,
            out(&mut pixels),
        );
        if result == GHOSTTY_NO_VALUE {
            return Ok(None);
        }
        check(result)?;
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| Error("image dimensions overflow".into()))?;
        let format = match format {
            GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => PixelFormat::Rgba,
            GHOSTTY_KITTY_IMAGE_FORMAT_RGB => PixelFormat::Rgb,
            GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => PixelFormat::GrayAlpha,
            GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => PixelFormat::Gray,
            _ => return Err(Error("unexpected decoded pixel format".into())),
        };
        if count == 0
            || len > IMAGE_MAX_BYTES
            || count.checked_mul(format.bytes_per_pixel()) != Some(len)
            || pixels.is_null()
        {
            return Err(Error("invalid bounded image buffer".into()));
        }
        Ok(Some(Image {
            id,
            generation,
            width,
            height,
            format,
            pixels: slice::from_raw_parts(pixels, len).to_vec(),
        }))
    }
}
