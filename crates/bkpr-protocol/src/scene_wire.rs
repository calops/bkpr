//! Planus scenes with response-local style/link tables and complete row deltas.
//! Explicit UTF-8 byte lengths preserve engine cell boundaries and widths; no
//! Unicode re-segmentation is performed. JSON is only a cold diagnostic bridge.
use crate::{Cell, Frame, Image, ImageKey, Row, Size};
use anyhow::{Context, Result, ensure};
use planus::{Builder, ReadAsRoot};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[rustfmt::skip]
#[allow(dead_code, unused_imports, clippy::all)]
#[path = "scene_wire/scene_generated.rs"]
mod generated;
#[path = "scene_wire/scalars.rs"]
mod scalars;
use generated::scene as g;
use scalars::*;

const VERSION: u8 = 1;
const MAX_CELLS: usize = 1_000_000;
const MAX_TEXT_BYTES: usize = 96 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGES: usize = 1024;

type Layout = (u32, u8, u32);
#[derive(Default)]
struct Run {
    style: u32,
    link: Option<u32>,
    text: String,
    layout: Vec<g::Layout>,
}

impl Run {
    fn finish(&mut self, builder: &mut Builder) -> planus::Offset<g::Run> {
        let layout = if self.layout.len() == 1
            && self.layout[0].length == 1
            && self.layout[0].width == 1
            && self.text.is_ascii()
        {
            &[][..]
        } else {
            self.layout.as_slice()
        };
        let text = builder.create_string(&self.text);
        let layout = builder.create_vector(layout);
        let offset = g::Run::create(
            builder,
            self.style,
            self.link.is_some(),
            self.link.unwrap_or(0),
            text,
            layout,
        );
        self.text.clear();
        self.layout.clear();
        offset
    }
}

/// Encode directly from the canonical frame, borrowing image pixels and styles.
/// Only changed rows are gathered; neither the frame nor its images are cloned.
pub fn encode(
    frame: &Frame,
    revision: u64,
    baseline: Option<&(u64, Arc<Frame>)>,
    frame_after: u64,
    resources: &[ImageKey],
) -> Result<Vec<u8>> {
    crate::valid_size(frame.size)?;
    let cols = usize::from(frame.size.cols);
    let height = usize::from(frame.size.rows);
    ensure!(cols * height <= MAX_CELLS, "scene cell limit");
    ensure!(frame.rows.len() == height, "scene incomplete frame");
    ensure!(
        frame.palette.len() <= 256 && frame.placements.len() <= MAX_CELLS,
        "scene resource count limit"
    );
    let baseline = baseline.filter(|(base, previous)| {
        *base == frame_after && previous.size == frame.size && previous.rows.len() == height
    });
    ensure!(
        baseline.is_none_or(|(base, _)| *base < revision),
        "scene non-advancing delta revision"
    );
    let changed =
        |index: usize, row: &Row| baseline.is_none_or(|(_, previous)| row != &previous.rows[index]);
    // Validate all input bounds before building. These limits also bound the
    // worst-case FlatBuffer well below its signed 32-bit offset ceiling.
    let mut expanded_text = 0usize;
    for (index, row) in frame.rows.iter().enumerate() {
        ensure!(row.cells.len() == cols, "scene incomplete row");
        if !changed(index, row) {
            continue;
        }
        for cell in &row.cells {
            ensure!(
                cell.width <= 2 && cell.text.len() <= 8192,
                "scene invalid cell layout"
            );
            expanded_text = expanded_text
                .saturating_add(cell.text.len())
                .saturating_add(cell.hyperlink.as_ref().map_or(0, String::len));
            ensure!(expanded_text <= MAX_TEXT_BYTES, "scene expanded text limit");
        }
    }
    let known: HashSet<_> = resources.iter().copied().collect();
    let mut image_count = 0usize;
    let mut image_bytes = 0usize;
    for image in frame
        .images
        .iter()
        .filter(|image| !known.contains(&image.key()))
    {
        image_count += 1;
        image_bytes = image_bytes.saturating_add(image.pixels.len());
        ensure!(
            image_count <= MAX_IMAGES && image_bytes <= MAX_IMAGE_BYTES,
            "scene image resource limit"
        );
        check_image(image.width, image.height, image.format, image.pixels.len())?;
    }

    let mut builder = Builder::new();
    let mut styles = Vec::new();
    let mut style_ids = HashMap::new();
    let mut links = Vec::new();
    let mut link_ids = HashMap::new();
    let mut rows = Vec::new();
    let mut run = Run::default();
    let mut offsets = Vec::new();
    for (index, row) in frame.rows.iter().enumerate() {
        if !changed(index, row) {
            continue;
        }
        for cell in &row.cells {
            let style = write_style(&cell.style);
            let style_id = *style_ids.entry(style).or_insert_with(|| {
                let id = styles.len() as u32;
                styles.push(style);
                id
            });
            let link = cell.hyperlink.as_deref().map(|link| {
                *link_ids.entry(link).or_insert_with(|| {
                    let id = links.len() as u32;
                    links.push(builder.create_string(link));
                    id
                })
            });
            if run.style != style_id || run.link != link {
                if !run.layout.is_empty() {
                    offsets.push(run.finish(&mut builder));
                }
                run.style = style_id;
                run.link = link;
            }
            run.text.push_str(&cell.text);
            let length = cell.text.len() as u32;
            if let Some(part) = run
                .layout
                .last_mut()
                .filter(|part| part.length == length && part.width == cell.width)
            {
                part.count += 1;
            } else {
                run.layout.push(g::Layout {
                    length,
                    width: cell.width,
                    count: 1,
                });
            }
        }
        offsets.push(run.finish(&mut builder));
        let runs = builder.create_vector(offsets.as_slice());
        offsets.clear();
        rows.push(g::Row::create(&mut builder, index as u64, runs));
    }
    let styles = builder.create_vector(styles.as_slice());
    let hyperlinks = builder.create_vector(links.as_slice());
    let rows = builder.create_vector(rows.as_slice());
    let palette: Vec<_> = frame.palette.iter().copied().map(pack).collect();
    let palette = builder.create_vector(palette.as_slice());
    let placements: Vec<_> = frame.placements.iter().map(write_placement).collect();
    let placements = builder.create_vector(placements.as_slice());
    let mut images = Vec::with_capacity(image_count);
    for image in frame
        .images
        .iter()
        .filter(|image| !known.contains(&image.key()))
    {
        let pixels = builder.create_vector(image.pixels.as_slice());
        images.push(g::Image::create(
            &mut builder,
            image.id,
            image.generation,
            image.width,
            image.height,
            write_format(image.format),
            pixels,
        ));
    }
    let images = builder.create_vector(images.as_slice());
    let cursor = &frame.cursor;
    let cursor = g::Cursor::create(
        &mut builder,
        cursor.x,
        cursor.y,
        cursor.visible,
        cursor.blinking,
        write_shape(cursor.shape),
        cursor.color.is_some(),
        cursor.color.map_or(0, pack),
    );
    let size = frame.size;
    let metadata = g::Frame::create(
        &mut builder,
        g::Size {
            cols: size.cols,
            rows: size.rows,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        },
        cursor,
        pack(frame.foreground),
        pack(frame.background),
        palette,
        images,
        placements,
        frame.alternate_screen,
        frame.mouse_reporting,
        frame.scrollback_total,
        frame.scrollback_offset,
    );
    let root = g::Scene::create(
        &mut builder,
        VERSION,
        revision,
        baseline.is_some(),
        baseline.map_or(0, |(base, _)| *base),
        metadata,
        styles,
        hyperlinks,
        rows,
    );
    // Planus 1.3 exposes only a borrowed finished slice, not owned extraction.
    // The API's owned output requires this final copy; source image pixels were
    // borrowed directly, without an intermediate cloned Frame or Image.
    Ok(builder.finish(root, None).to_vec())
}

/// Validated row replacements and newly supplied generation-keyed resources.
pub struct DecodedScene {
    pub revision: u64,
    pub base_revision: Option<u64>,
    frame: Frame,
    rows: Vec<(usize, Row)>,
}
impl DecodedScene {
    /// Resources supplied by this update, before any consumer state is changed.
    pub fn image_keys(&self) -> impl Iterator<Item = ImageKey> + '_ {
        self.frame.images.iter().map(|image| image.key())
    }
    /// Check every fallible prerequisite before moving the baseline's rows.
    pub fn apply(self, revision: &mut u64, frame: &mut Option<Frame>) -> Result<()> {
        if let Some(base) = self.base_revision {
            ensure!(*revision == base, "scene baseline revision mismatch");
            let previous = frame.as_ref().context("scene missing baseline")?;
            ensure!(
                previous.size == self.frame.size
                    && previous.rows.len() == usize::from(self.frame.size.rows),
                "scene baseline geometry mismatch"
            );
        }
        let mut metadata = self.frame;
        if self.base_revision.is_some() {
            // decode already validated sorted indices and complete row shapes;
            // no Result-producing operation follows the first mutation.
            metadata.rows = std::mem::take(&mut frame.as_mut().unwrap().rows);
            for (index, row) in self.rows {
                metadata.rows[index] = row;
            }
        } else {
            metadata.rows = self.rows.into_iter().map(|(_, row)| row).collect();
        }
        *frame = Some(metadata);
        *revision = self.revision;
        Ok(())
    }
}

fn check_image(width: u32, height: u32, format: crate::PixelFormat, length: usize) -> Result<()> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(format.bytes_per_pixel()));
    ensure!(expected == Some(length), "scene image dimensions mismatch");
    Ok(())
}

fn read_size(frame: g::FrameRef<'_>) -> Result<Size> {
    let size = frame.size()?;
    Ok(Size {
        cols: size.cols(),
        rows: size.rows(),
        cell_width: size.cell_width(),
        cell_height: size.cell_height(),
    })
}

fn base_revision(scene: g::SceneRef<'_>) -> Result<Option<u64>> {
    let present = scene.has_base_revision()?;
    let base = scene.base_revision()?;
    ensure!(
        present || base == 0,
        "scene absent base revision has a payload"
    );
    Ok(present.then_some(base))
}

fn run_link(run: g::RunRef<'_>) -> Result<Option<u32>> {
    let present = run.has_link()?;
    let link = run.link()?;
    ensure!(present || link == 0, "scene absent hyperlink has a payload");
    Ok(present.then_some(link))
}

/// Planus reads lazily: visit every field and every vector before returning a
/// root to the allocating decoder. This pass allocates no expanded cells,
/// strings, image bytes, or owned schema tables, including on a late error.
fn validate(bytes: &[u8]) -> Result<g::SceneRef<'_>> {
    ensure!(
        bytes.len() <= i32::MAX as usize,
        "scene buffer exceeds format limit"
    );
    let scene = g::SceneRef::read_as_root(bytes)?;
    ensure!(
        scene.version()? == VERSION,
        "unsupported Planus scene version"
    );
    let revision = scene.revision()?;
    let base = base_revision(scene)?;
    ensure!(
        base.is_none_or(|base| base < revision),
        "scene non-advancing delta revision"
    );
    let frame = scene.frame()?;
    let size = read_size(frame)?;
    crate::valid_size(size)?;
    let cols = usize::from(size.cols);
    let height = usize::from(size.rows);
    let capacity = cols * height;
    ensure!(capacity <= MAX_CELLS, "scene cell limit");
    read_cursor(frame.cursor()?)?;
    unpack(frame.foreground()?)?;
    unpack(frame.background()?)?;
    frame.alternate_screen()?;
    frame.mouse_reporting()?;
    frame.scrollback_total()?;
    frame.scrollback_offset()?;
    let palette = frame.palette()?;
    let images = frame.images()?;
    let placements = frame.placements()?;
    let styles = scene.styles()?;
    let links = scene.hyperlinks()?;
    let rows = scene.rows()?;
    ensure!(
        palette.len() <= 256 && images.len() <= MAX_IMAGES && placements.len() <= MAX_CELLS,
        "scene resource count limit"
    );
    ensure!(
        styles.len() <= capacity && links.len() <= capacity && rows.len() <= height,
        "scene table limit"
    );
    for color in palette {
        unpack(color)?;
    }
    for style in styles {
        read_style(style)?;
    }
    let mut text_bytes = 0usize;
    for link in links {
        text_bytes = text_bytes.saturating_add(link?.len());
        ensure!(text_bytes <= MAX_TEXT_BYTES, "scene hyperlink byte limit");
    }
    let mut image_bytes = 0usize;
    for image in images {
        let image = image?;
        image.id()?;
        image.generation()?;
        let pixels = image.pixels()?;
        check_image(
            image.width()?,
            image.height()?,
            read_format(image.format()?),
            pixels.len(),
        )?;
        image_bytes = image_bytes.saturating_add(pixels.len());
        ensure!(image_bytes <= MAX_IMAGE_BYTES, "scene image byte limit");
    }
    for placement in placements {
        read_placement(placement)?;
    }
    let mut previous = None;
    let mut expanded_bytes = 0usize;
    for row in rows {
        let row = row?;
        let index = usize::try_from(row.index()?).context("scene row index exceeds usize")?;
        ensure!(
            index < height && previous.is_none_or(|previous| previous < index),
            "scene row order/index"
        );
        previous = Some(index);
        let runs = row.runs()?;
        ensure!(runs.len() <= cols, "scene run limit");
        let mut cells = 0usize;
        for run in runs {
            let run = run?;
            let style = run.style()? as usize;
            let link = run_link(run)?;
            ensure!(
                style < styles.len() && link.is_none_or(|id| (id as usize) < links.len()),
                "scene table index"
            );
            let text = run.text()?;
            let layout = run.layout()?;
            text_bytes = text_bytes.saturating_add(text.len());
            ensure!(
                text_bytes <= MAX_TEXT_BYTES && layout.len() <= cols,
                "scene text/layout limit"
            );
            let before = cells;
            if layout.is_empty() {
                ensure!(text.is_ascii(), "scene implicit layout requires ASCII");
                cells = cells.saturating_add(text.len());
            } else {
                let mut offset = 0usize;
                for part in layout {
                    let length = part.length() as usize;
                    let count = part.count() as usize;
                    ensure!(
                        part.width() <= 2 && count > 0 && length <= 8192,
                        "scene invalid cell layout"
                    );
                    cells = cells.saturating_add(count);
                    ensure!(cells <= cols, "scene row cell limit");
                    for _ in 0..count {
                        offset = offset
                            .checked_add(length)
                            .filter(|end| *end <= text.len())
                            .context("scene text layout overflow")?;
                        ensure!(text.is_char_boundary(offset), "scene split UTF-8 cell");
                    }
                }
                ensure!(offset == text.len(), "scene unused cell text");
            }
            ensure!(cells <= cols, "scene row cell limit");
            let link_bytes = match link {
                Some(id) => links
                    .get(id as usize)
                    .context("scene hyperlink index")??
                    .len(),
                None => 0,
            };
            expanded_bytes = expanded_bytes
                .saturating_add(text.len())
                .saturating_add(link_bytes.saturating_mul(cells - before));
            ensure!(
                expanded_bytes <= MAX_TEXT_BYTES,
                "scene expanded text limit"
            );
        }
        ensure!(cells == cols, "scene incomplete row");
    }
    ensure!(
        base.is_some() || rows.len() == height,
        "scene incomplete full frame"
    );
    Ok(scene)
}

fn read_metadata(frame: g::FrameRef<'_>) -> Result<Frame> {
    let palette = frame.palette()?.iter().map(unpack).collect::<Result<_>>()?;
    let placements = frame
        .placements()?
        .iter()
        .map(read_placement)
        .collect::<Result<_>>()?;
    let images = frame
        .images()?
        .iter()
        .map(|image| {
            let image = image?;
            Ok(Arc::new(Image {
                id: image.id()?,
                generation: image.generation()?,
                width: image.width()?,
                height: image.height()?,
                format: read_format(image.format()?),
                pixels: image.pixels()?.to_vec(),
            }))
        })
        .collect::<Result<_>>()?;
    Ok(Frame {
        size: read_size(frame)?,
        rows: Vec::new(),
        cursor: read_cursor(frame.cursor()?)?,
        foreground: unpack(frame.foreground()?)?,
        background: unpack(frame.background()?)?,
        palette,
        images,
        placements,
        alternate_screen: frame.alternate_screen()?,
        mouse_reporting: frame.mouse_reporting()?,
        scrollback_total: frame.scrollback_total()?,
        scrollback_offset: frame.scrollback_offset()?,
    })
}

/// Fully validate borrowed input before allocating expanded canonical cells or
/// copying image pixels. No partially decoded scene can reach the consumer.
pub fn decode(bytes: &[u8]) -> Result<DecodedScene> {
    decode_validated(validate(bytes)?)
}

fn decode_validated(scene: g::SceneRef<'_>) -> Result<DecodedScene> {
    let frame = read_metadata(scene.frame()?)?;
    let cols = usize::from(frame.size.cols);
    let styles = scene.styles()?;
    let links = scene.hyperlinks()?;
    let mut rows = Vec::with_capacity(scene.rows()?.len());
    for row in scene.rows()? {
        let row = row?;
        let mut cells = Vec::with_capacity(cols);
        for run in row.runs()? {
            let run = run?;
            let style = read_style(
                styles
                    .get(run.style()? as usize)
                    .context("scene style index")?,
            )?;
            let hyperlink = match run_link(run)? {
                Some(id) => Some(links.get(id as usize).context("scene hyperlink index")??),
                None => None,
            };
            let mut push = |text: &str, width| {
                cells.push(Cell {
                    text: text.into(),
                    width,
                    style: style.clone(),
                    hyperlink: hyperlink.map(str::to_owned),
                })
            };
            let text = run.text()?;
            let layout = run.layout()?;
            if layout.is_empty() {
                for offset in 0..text.len() {
                    push(&text[offset..offset + 1], 1);
                }
            } else {
                let mut offset = 0;
                for part in layout {
                    for _ in 0..part.count() {
                        let end = offset + part.length() as usize;
                        push(&text[offset..end], part.width());
                        offset = end;
                    }
                }
            }
        }
        rows.push((row.index()? as usize, Row { cells }));
    }
    Ok(DecodedScene {
        revision: scene.revision()?,
        base_revision: base_revision(scene)?,
        frame,
        rows,
    })
}

/// Cold CLI/library diagnostic bridge. Full scenes become ordinary human Frame
/// JSON; deltas retain structured compact replacements and their base revision.
/// Neither representation is accepted as a native scene encoding.
pub fn to_json(bytes: &[u8]) -> Result<Value> {
    let scene = validate(bytes)?;
    if base_revision(scene)?.is_none() {
        let mut revision = 0;
        let mut frame = None;
        decode_validated(scene)?.apply(&mut revision, &mut frame)?;
        return Ok(serde_json::to_value(
            frame.context("scene missing full frame")?,
        )?);
    }
    let frame = read_metadata(scene.frame()?)?;
    let styles = scene
        .styles()?
        .iter()
        .map(read_style)
        .collect::<Result<Vec<_>>>()?;
    let links = scene
        .hyperlinks()?
        .iter()
        .collect::<planus::Result<Vec<_>>>()?;
    let mut rows = Vec::with_capacity(scene.rows()?.len());
    for row in scene.rows()? {
        let row = row?;
        let mut runs = Vec::with_capacity(row.runs()?.len());
        for run in row.runs()? {
            let run = run?;
            let layout: Vec<Layout> = run
                .layout()?
                .iter()
                .map(|part| (part.length(), part.width(), part.count()))
                .collect();
            runs.push((run.style()?, run_link(run)?, run.text()?, layout));
        }
        rows.push((row.index()?, runs));
    }
    Ok(serde_json::json!({
        "version": scene.version()?, "revision": scene.revision()?,
        "base_revision": base_revision(scene)?, "frame": frame,
        "styles": styles, "hyperlinks": links, "rows": rows,
    }))
}

#[cfg(test)]
#[path = "scene_wire/tests.rs"]
mod tests;
