//! Compact JSON scenes. Tables are response-local; deltas replace complete rows.
//! A run stores style/link table indices, concatenated text and RLE cell layout.
//! Empty layout is the ASCII/width-one fast path; explicit byte lengths preserve
//! engine cell boundaries without re-segmenting Unicode or inferring widths.
use anyhow::{Result, bail, ensure};
use bkpr_terminal::{Cell, ColorSource, Frame, ImageKey, Row, Style};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

const VERSION: u8 = 1;
const MAX_CELLS: usize = 1_000_000;
// Match the existing response budget; compression must not reduce supported text.
const MAX_TEXT_BYTES: usize = 96 * 1024 * 1024;
const MAX_EXPANDED_TEXT_BYTES: usize = MAX_TEXT_BYTES;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

// (UTF-8 bytes per cell, cell width, repeat count).
type Layout = (u32, u8, u32);
#[derive(Serialize, Deserialize)]
struct Run(u32, Option<u32>, String, Vec<Layout>);
#[derive(Serialize, Deserialize)]
struct Scene {
    version: u8,
    revision: u64,
    base_revision: Option<u64>,
    frame: Frame,
    styles: Vec<Style>,
    hyperlinks: Vec<String>,
    rows: Vec<(usize, Vec<Run>)>,
}

#[derive(PartialEq, Eq, Hash)]
struct StyleKey {
    colors: [[u8; 3]; 3],
    sources: [(u8, [u8; 3]); 3],
    flags: u16,
    underline: u8,
}
impl From<&Style> for StyleKey {
    fn from(style: &Style) -> Self {
        fn source(source: ColorSource) -> (u8, [u8; 3]) {
            match source {
                ColorSource::Default => (0, [0; 3]),
                ColorSource::Palette(index) => (1, [index, 0, 0]),
                ColorSource::Rgb(rgb) => (2, rgb),
            }
        }
        let flags = [
            style.bold,
            style.italic,
            style.faint,
            style.blink,
            style.inverse,
            style.invisible,
            style.strikethrough,
            style.overline,
        ]
        .into_iter()
        .enumerate()
        .fold(0, |flags, (bit, set)| flags | ((set as u16) << bit));
        Self {
            colors: [style.foreground, style.background, style.underline_color],
            sources: [
                source(style.foreground_source),
                source(style.background_source),
                source(style.underline_source),
            ],
            flags,
            underline: style.underline,
        }
    }
}

pub(crate) fn encode(
    frame: &Frame,
    revision: u64,
    baseline: Option<&(u64, Arc<Frame>)>,
    frame_after: u64,
    resources: &[ImageKey],
) -> Result<Value> {
    let baseline = baseline.filter(|(base_revision, previous)| {
        *base_revision == frame_after
            && previous.size == frame.size
            && previous.rows.len() == frame.rows.len()
    });
    let mut styles = Vec::new();
    let mut style_ids = HashMap::new();
    let mut hyperlinks = Vec::new();
    let mut link_ids = HashMap::new();
    let mut rows = Vec::new();
    for (index, row) in frame.rows.iter().enumerate() {
        if baseline.is_some_and(|(_, previous)| row == &previous.rows[index]) {
            continue;
        }
        let mut runs: Vec<Run> = Vec::new();
        for cell in &row.cells {
            let style_id = *style_ids
                .entry(StyleKey::from(&cell.style))
                .or_insert_with(|| {
                    let id = styles.len() as u32;
                    styles.push(cell.style.clone());
                    id
                });
            let link_id = cell.hyperlink.as_deref().map(|link| {
                *link_ids.entry(link).or_insert_with(|| {
                    let id = hyperlinks.len() as u32;
                    hyperlinks.push(link.to_owned());
                    id
                })
            });
            if !runs
                .last()
                .is_some_and(|run| run.0 == style_id && run.1 == link_id)
            {
                runs.push(Run(style_id, link_id, String::new(), Vec::new()));
            }
            let run = runs.last_mut().unwrap();
            run.2.push_str(&cell.text);
            let bytes = u32::try_from(cell.text.len())?;
            if let Some(last) = run
                .3
                .last_mut()
                .filter(|last| last.0 == bytes && last.1 == cell.width)
            {
                last.2 += 1;
            } else {
                run.3.push((bytes, cell.width, 1));
            }
        }
        for run in &mut runs {
            if run.3.len() == 1 && run.3[0].0 == 1 && run.3[0].1 == 1 && run.2.is_ascii() {
                run.3.clear();
            }
        }
        rows.push((index, runs));
    }
    let known: HashSet<_> = resources.iter().copied().collect();
    let metadata = Frame {
        size: frame.size,
        rows: Vec::new(),
        cursor: frame.cursor.clone(),
        foreground: frame.foreground,
        background: frame.background,
        palette: frame.palette.clone(),
        images: frame
            .images
            .iter()
            .filter(|image| !known.contains(&image.key()))
            .cloned()
            .collect(),
        placements: frame.placements.clone(),
        alternate_screen: frame.alternate_screen,
        scrollback_total: frame.scrollback_total,
        scrollback_offset: frame.scrollback_offset,
    };
    Ok(serde_json::to_value(Scene {
        version: VERSION,
        revision,
        base_revision: baseline.map(|(revision, _)| *revision),
        frame: metadata,
        styles,
        hyperlinks,
        rows,
    })?)
}

/// Validated, expanded row replacements. Metadata contains only newly supplied
/// image resources; the client retains its generation-keyed image cache.
pub(crate) struct DecodedScene {
    pub(crate) revision: u64,
    pub(crate) base_revision: Option<u64>,
    frame: Frame,
    rows: Vec<(usize, Row)>,
}
impl DecodedScene {
    /// Reject stale/missing baselines before touching the consumer's frame.
    pub(crate) fn apply(self, revision: &mut u64, frame: &mut Option<Frame>) -> Result<()> {
        let mut metadata = self.frame;
        if let Some(base) = self.base_revision {
            ensure!(
                *revision == base,
                "compact scene baseline revision mismatch"
            );
            let previous = frame
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("compact scene missing baseline"))?;
            ensure!(
                previous.size == metadata.size
                    && previous.rows.len() == usize::from(metadata.size.rows),
                "compact scene baseline geometry mismatch"
            );
            metadata.rows = std::mem::take(&mut previous.rows);
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

/// Bounds and all run shapes are checked before allocating expanded cells.
pub(crate) fn decode(value: Value) -> Result<DecodedScene> {
    let metadata_rows = value
        .get("frame")
        .and_then(|frame| frame.get("rows"))
        .and_then(Value::as_array);
    ensure!(
        metadata_rows.is_some_and(Vec::is_empty),
        "compact scene metadata must not contain cells"
    );
    let size = bkpr_terminal::Size::deserialize(&value["frame"]["size"])?;
    crate::authority::valid_size(size)?;
    let capacity = usize::from(size.cols) * usize::from(size.rows);
    ensure!(capacity <= MAX_CELLS, "compact scene cell limit");
    for (field, limit) in [
        ("styles", capacity),
        ("hyperlinks", capacity),
        ("rows", usize::from(size.rows)),
    ] {
        ensure!(
            value[field]
                .as_array()
                .is_some_and(|items| items.len() <= limit),
            "compact scene table limit"
        );
    }
    for (field, limit) in [
        ("palette", 256),
        ("images", 1024),
        ("placements", MAX_CELLS),
    ] {
        ensure!(
            value["frame"][field]
                .as_array()
                .is_some_and(|items| items.len() <= limit),
            "compact scene resource count limit"
        );
    }
    let mut encoded_images = 0usize;
    for image in value["frame"]["images"].as_array().unwrap() {
        let pixels = image["pixels"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("compact scene image encoding"))?;
        encoded_images = encoded_images.saturating_add(pixels.len());
        ensure!(
            encoded_images <= MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4 * 1024,
            "compact scene encoded image byte limit"
        );
    }
    let scene: Scene = serde_json::from_value(value)?;
    ensure!(
        scene.version == VERSION,
        "unsupported compact scene version"
    );
    let cols = usize::from(scene.frame.size.cols);
    let height = usize::from(scene.frame.size.rows);
    ensure!(
        scene.base_revision.is_none_or(|base| base < scene.revision),
        "compact scene non-advancing delta revision"
    );
    let mut image_bytes = 0usize;
    for image in &scene.frame.images {
        let expected = (image.width as usize)
            .checked_mul(image.height as usize)
            .and_then(|pixels| pixels.checked_mul(image.format.bytes_per_pixel()));
        ensure!(
            expected == Some(image.pixels.len()),
            "compact scene image dimensions mismatch"
        );
        image_bytes = image_bytes.saturating_add(image.pixels.len());
        ensure!(
            image_bytes <= MAX_IMAGE_BYTES,
            "compact scene image byte limit"
        );
    }
    let mut bytes = scene
        .hyperlinks
        .iter()
        .try_fold(0usize, |bytes, link| bytes.checked_add(link.len()))
        .ok_or_else(|| anyhow::anyhow!("compact scene text overflow"))?;
    let mut previous = None;
    let mut expanded_bytes = 0usize;
    for (index, runs) in &scene.rows {
        ensure!(
            *index < height && previous.is_none_or(|previous| previous < *index),
            "compact scene row order/index"
        );
        previous = Some(*index);
        ensure!(runs.len() <= cols, "compact scene run limit");
        let mut cells = 0usize;
        for Run(style, link, text, layout) in runs {
            ensure!(
                (*style as usize) < scene.styles.len()
                    && link.is_none_or(|id| (id as usize) < scene.hyperlinks.len()),
                "compact scene table index"
            );
            bytes = bytes.saturating_add(text.len());
            ensure!(
                bytes <= MAX_TEXT_BYTES && layout.len() <= cols,
                "compact scene text/layout limit"
            );
            let before = cells;
            if layout.is_empty() {
                ensure!(
                    text.is_ascii(),
                    "compact scene implicit layout requires ASCII"
                );
                cells = cells.saturating_add(text.len());
            } else {
                let mut offset = 0usize;
                for &(length, width, count) in layout {
                    ensure!(
                        width <= 2 && count > 0 && length <= 8192,
                        "compact scene invalid cell layout"
                    );
                    cells = cells.saturating_add(count as usize);
                    ensure!(cells <= cols, "compact scene row cell limit");
                    for _ in 0..count {
                        offset = offset
                            .checked_add(length as usize)
                            .filter(|end| *end <= text.len())
                            .ok_or_else(|| anyhow::anyhow!("compact scene text layout overflow"))?;
                        ensure!(
                            text.is_char_boundary(offset),
                            "compact scene split UTF-8 cell"
                        );
                    }
                }
                ensure!(offset == text.len(), "compact scene unused cell text");
            }
            ensure!(cells <= cols, "compact scene row cell limit");
            expanded_bytes = expanded_bytes.saturating_add(text.len()).saturating_add(
                link.map_or(0, |id| scene.hyperlinks[id as usize].len())
                    .saturating_mul(cells - before),
            );
            ensure!(
                expanded_bytes <= MAX_EXPANDED_TEXT_BYTES,
                "compact scene expanded text limit"
            );
        }
        ensure!(cells == cols, "compact scene incomplete row");
    }
    if scene.base_revision.is_none() {
        ensure!(
            scene.rows.len() == height,
            "compact scene incomplete full frame"
        );
    }
    if bytes > MAX_TEXT_BYTES {
        bail!("compact scene hyperlink byte limit");
    }
    let rows = scene
        .rows
        .into_iter()
        .map(|(index, runs)| {
            let mut cells = Vec::with_capacity(cols);
            for Run(style, link, text, layout) in runs {
                let style = &scene.styles[style as usize];
                let hyperlink = link.map(|id| &scene.hyperlinks[id as usize]);
                let mut push = |text: &str, width| {
                    cells.push(Cell {
                        text: text.to_owned(),
                        width,
                        style: style.clone(),
                        hyperlink: hyperlink.cloned(),
                    })
                };
                if layout.is_empty() {
                    for offset in 0..text.len() {
                        push(&text[offset..offset + 1], 1);
                    }
                } else {
                    let mut offset = 0;
                    for (length, width, count) in layout {
                        for _ in 0..count {
                            let end = offset + length as usize;
                            push(&text[offset..end], width);
                            offset = end;
                        }
                    }
                }
            }
            (index, Row { cells })
        })
        .collect();
    Ok(DecodedScene {
        revision: scene.revision,
        base_revision: scene.base_revision,
        frame: scene.frame,
        rows,
    })
}
