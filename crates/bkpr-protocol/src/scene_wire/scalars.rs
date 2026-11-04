use super::g;
use crate::{ColorSource, Cursor, CursorShape, ImageLayer, PixelFormat, Placement, Style};
use anyhow::{Context, Result, ensure};

pub(super) fn pack(rgb: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]])
}

pub(super) fn unpack(rgb: u32) -> Result<[u8; 3]> {
    ensure!(rgb <= 0x00ff_ffff, "scene RGB has nonzero reserved bits");
    let [_, r, g, b] = rgb.to_be_bytes();
    Ok([r, g, b])
}

fn source(value: ColorSource) -> (g::ColorKind, u32) {
    match value {
        ColorSource::Default => (g::ColorKind::Default, 0),
        ColorSource::Palette(index) => (g::ColorKind::Palette, u32::from(index)),
        ColorSource::Rgb(rgb) => (g::ColorKind::Rgb, pack(rgb)),
    }
}

fn read_source(kind: g::ColorKind, value: u32) -> Result<ColorSource> {
    Ok(match kind {
        g::ColorKind::Default => {
            ensure!(value == 0, "scene default color has a payload");
            ColorSource::Default
        }
        g::ColorKind::Palette => {
            ColorSource::Palette(u8::try_from(value).context("scene palette index exceeds u8")?)
        }
        g::ColorKind::Rgb => ColorSource::Rgb(unpack(value)?),
    })
}

pub(super) fn write_style(style: &Style) -> g::Style {
    let (foreground_kind, foreground_value) = source(style.foreground_source);
    let (background_kind, background_value) = source(style.background_source);
    let (underline_kind, underline_value) = source(style.underline_source);
    let flags = u16::from(style.bold)
        | (u16::from(style.italic) << 1)
        | (u16::from(style.faint) << 2)
        | (u16::from(style.blink) << 3)
        | (u16::from(style.inverse) << 4)
        | (u16::from(style.invisible) << 5)
        | (u16::from(style.strikethrough) << 6)
        | (u16::from(style.overline) << 7);
    g::Style {
        foreground: pack(style.foreground),
        background: pack(style.background),
        underline_color: pack(style.underline_color),
        foreground_kind,
        foreground_value,
        background_kind,
        background_value,
        underline_kind,
        underline_value,
        flags,
        underline: style.underline,
    }
}

pub(super) fn read_style(style: g::StyleRef<'_>) -> Result<Style> {
    let flags = style.flags();
    ensure!(flags & !0xff == 0, "scene unknown style flag bits");
    Ok(Style {
        foreground: unpack(style.foreground())?,
        background: unpack(style.background())?,
        underline_color: unpack(style.underline_color())?,
        foreground_source: read_source(style.foreground_kind()?, style.foreground_value())?,
        background_source: read_source(style.background_kind()?, style.background_value())?,
        underline_source: read_source(style.underline_kind()?, style.underline_value())?,
        bold: flags & 1 != 0,
        italic: flags & 2 != 0,
        faint: flags & 4 != 0,
        blink: flags & 8 != 0,
        inverse: flags & 16 != 0,
        invisible: flags & 32 != 0,
        strikethrough: flags & 64 != 0,
        overline: flags & 128 != 0,
        underline: style.underline(),
    })
}

pub(super) fn write_placement(p: &Placement) -> g::Placement {
    g::Placement {
        image_id: p.image_id,
        generation: p.generation,
        placement_id: p.placement_id,
        col: p.col,
        row: p.row,
        offset_x: p.offset_x,
        offset_y: p.offset_y,
        width: p.width,
        height: p.height,
        source_x: p.source_x,
        source_y: p.source_y,
        source_width: p.source_width,
        source_height: p.source_height,
        layer: match p.layer {
            ImageLayer::BelowBackground => g::ImageLayer::BelowBackground,
            ImageLayer::BelowText => g::ImageLayer::BelowText,
            ImageLayer::AboveText => g::ImageLayer::AboveText,
        },
        z: p.z,
    }
}

pub(super) fn read_placement(p: g::PlacementRef<'_>) -> Result<Placement> {
    Ok(Placement {
        image_id: p.image_id(),
        generation: p.generation(),
        placement_id: p.placement_id(),
        col: p.col(),
        row: p.row(),
        offset_x: p.offset_x(),
        offset_y: p.offset_y(),
        width: p.width(),
        height: p.height(),
        source_x: p.source_x(),
        source_y: p.source_y(),
        source_width: p.source_width(),
        source_height: p.source_height(),
        layer: match p.layer()? {
            g::ImageLayer::BelowBackground => ImageLayer::BelowBackground,
            g::ImageLayer::BelowText => ImageLayer::BelowText,
            g::ImageLayer::AboveText => ImageLayer::AboveText,
        },
        z: p.z(),
    })
}

pub(super) fn write_format(format: PixelFormat) -> g::PixelFormat {
    match format {
        PixelFormat::Rgb => g::PixelFormat::Rgb,
        PixelFormat::Rgba => g::PixelFormat::Rgba,
        PixelFormat::GrayAlpha => g::PixelFormat::GrayAlpha,
        PixelFormat::Gray => g::PixelFormat::Gray,
    }
}

pub(super) fn read_format(format: g::PixelFormat) -> PixelFormat {
    match format {
        g::PixelFormat::Rgb => PixelFormat::Rgb,
        g::PixelFormat::Rgba => PixelFormat::Rgba,
        g::PixelFormat::GrayAlpha => PixelFormat::GrayAlpha,
        g::PixelFormat::Gray => PixelFormat::Gray,
    }
}

pub(super) fn write_shape(shape: CursorShape) -> g::CursorShape {
    match shape {
        CursorShape::Bar => g::CursorShape::Bar,
        CursorShape::Block => g::CursorShape::Block,
        CursorShape::Underline => g::CursorShape::Underline,
        CursorShape::Hollow => g::CursorShape::Hollow,
    }
}

pub(super) fn read_cursor(cursor: g::CursorRef<'_>) -> Result<Cursor> {
    let color = cursor.color()?;
    let has_color = cursor.has_color()?;
    ensure!(
        has_color || color == 0,
        "scene absent cursor color has a payload"
    );
    Ok(Cursor {
        x: cursor.x()?,
        y: cursor.y()?,
        visible: cursor.visible()?,
        blinking: cursor.blinking()?,
        shape: match cursor.shape()? {
            g::CursorShape::Bar => CursorShape::Bar,
            g::CursorShape::Block => CursorShape::Block,
            g::CursorShape::Underline => CursorShape::Underline,
            g::CursorShape::Hollow => CursorShape::Hollow,
        },
        color: if has_color {
            Some(unpack(color)?)
        } else {
            None
        },
    })
}
