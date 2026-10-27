//! Monochrome SVG masks and tinted Kitty images. Rasterization and encoding happen
//! at metadata ingestion; painting only looks up images and emits placements.
use std::collections::BTreeMap;

use base64::Engine;
use resvg::{tiny_skia, usvg};

// Distribute the upstream notices with the embedded asset, including packaged WASM.
#[used]
static LICENSE: &str = concat!(
    include_str!("../assets/NOTICE"),
    include_str!("../assets/MaterialDesign-LICENSE"),
    include_str!("../assets/Apache-2.0.txt"),
);

pub const CLEAR_PLACEMENTS: &str = "\x1b_Ga=d,d=a,q=2\x1b\\";
const MAX_SIDE: u32 = 512;
// Retain earlier sizes/colors. At the ceiling, fall back rather than repeatedly
// rasterizing evicted entries or allowing an unbounded WASI allocation.
const MAX_MASKS: usize = 64;
const MAX_IMAGES: usize = 1024;
const MAX_BYTES: usize = 16 * 1024 * 1024;
type Size = (u32, u32);
type Key = (Size, [u8; 3]);

#[derive(Default)]
enum Artwork {
    #[default]
    Bee,
    Hive,
}

impl Artwork {
    fn svg(&self) -> &'static str {
        match self {
            Self::Bee => include_str!("../assets/bee.svg"),
            Self::Hive => include_str!("../assets/hive.svg"),
        }
    }

    fn image_id_base(&self) -> u32 {
        match self {
            Self::Bee => 0,
            Self::Hive => MAX_IMAGES as u32,
        }
    }
}

struct Image {
    upload: String,
    placement: String,
    uploaded: bool,
}

#[derive(Default)]
pub struct Graphics {
    artwork: Artwork,
    tree: Option<usvg::Tree>,
    masks: BTreeMap<Size, Vec<u8>>,
    images: BTreeMap<Key, Image>,
    active: BTreeMap<String, Key>,
    enabled: bool,
    current_size: Option<Size>,
    bytes: usize,
}

impl std::fmt::Debug for Graphics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graphics")
            .field("masks", &self.masks.len())
            .field("images", &self.images.len())
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Graphics {
    pub fn hive() -> Self {
        Self {
            artwork: Artwork::Hive,
            ..Self::default()
        }
    }

    pub fn prepare<'a>(
        &mut self,
        supported: bool,
        cell: Option<[u32; 2]>,
        styles: impl Iterator<Item = &'a str>,
        ansi: &[Option<[u8; 3]>; 16],
        foreground: Option<[u8; 3]>,
    ) {
        self.active.clear();
        self.enabled = supported;
        let Some([cell_width, height]) = cell.filter(|_| supported) else {
            return;
        };
        let Some(width) = cell_width.checked_mul(2) else {
            return;
        };
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return;
        }
        let size = (width, height);
        // Native grids can release images while resizing. Renew transport state
        // at a size transition while keeping rasterization and tint caches intact.
        let size_changed = self.current_size != Some(size);
        self.current_size = Some(size);
        if !self.masks.contains_key(&size) {
            let bytes = (width * height) as usize;
            if self.masks.len() >= MAX_MASKS || self.bytes + bytes > MAX_BYTES {
                return;
            }
            if self.tree.is_none() {
                // Keep the upstream notices reachable in the packaged WASM too.
                std::hint::black_box(LICENSE);
                self.tree =
                    usvg::Tree::from_str(self.artwork.svg(), &usvg::Options::default()).ok();
            }
            let Some(tree) = &self.tree else { return };
            let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
                return;
            };
            let svg_size = tree.size();
            let scale = (width as f32 / svg_size.width()).min(height as f32 / svg_size.height());
            let transform = tiny_skia::Transform::from_row(
                scale,
                0.0,
                0.0,
                scale,
                (width as f32 - svg_size.width() * scale) / 2.0,
                (height as f32 - svg_size.height() * scale) / 2.0,
            );
            resvg::render(tree, transform, &mut pixmap.as_mut());
            // Only alpha is retained. Tinting later produces straight RGBA, as
            // required by Kitty, rather than tiny-skia's premultiplied channels.
            self.masks
                .insert(size, pixmap.data().chunks_exact(4).map(|p| p[3]).collect());
            self.bytes += bytes;
        }
        for style in styles {
            let Some(rgb) = resolve_color(style, ansi, foreground) else {
                continue;
            };
            let key = (size, rgb);
            if size_changed && let Some(image) = self.images.get_mut(&key) {
                image.uploaded = false;
            }
            if !self.images.contains_key(&key) {
                let mask = &self.masks[&size];
                let encoded_size = (mask.len() * 4).div_ceil(3) * 4;
                if self.images.len() >= MAX_IMAGES || self.bytes + encoded_size + 1024 > MAX_BYTES {
                    continue;
                }
                let mut rgba = Vec::with_capacity(mask.len() * 4);
                for alpha in mask {
                    rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], *alpha]);
                }
                // Both artwork caches share one native plugin grid. Keep their
                // transport IDs disjoint even at the per-cache entry limit.
                let id = self.artwork.image_id_base() + self.images.len() as u32 + 1;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&rgba);
                let mut upload = String::new();
                let chunks: Vec<_> = encoded.as_bytes().chunks(4096).collect();
                for (index, chunk) in chunks.iter().enumerate() {
                    let more = u8::from(index + 1 < chunks.len());
                    if index == 0 {
                        upload.push_str(&format!(
                            "\x1b_Ga=t,f=32,s={width},v={height},i={id},q=2,m={more};"
                        ));
                    } else {
                        upload.push_str(&format!("\x1b_Gq=2,m={more};"));
                    }
                    upload.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
                    upload.push_str("\x1b\\");
                }
                self.bytes += upload.len();
                let placement = match self.artwork {
                    Artwork::Bee => format!("\x1b_Ga=p,i={id},C=1,q=2\x1b\\  "),
                    // The stem occupies the first marker cell. Position the
                    // hive half a cell left, using the existing left gutter,
                    // and restore the logical cursor before its two spaces.
                    Artwork::Hive => format!(
                        "\x1b[1D\x1b_Ga=p,i={id},X={},C=1,q=2\x1b\\\x1b[1C  ",
                        cell_width / 2,
                    ),
                };
                self.images.insert(
                    key,
                    Image {
                        upload,
                        placement,
                        uploaded: false,
                    },
                );
            }
            self.active.insert(style.to_owned(), key);
        }
    }

    pub fn begin_frame(&self) -> &'static str {
        if self.enabled || !self.images.is_empty() {
            CLEAR_PLACEMENTS
        } else {
            ""
        }
    }

    /// Called after text fitting. Always occupies the original two-cell slot;
    /// C=1 leaves cursor policy to the two literal spaces, never the image.
    /// Its raster already has the exact client pixel size; omit c/r to avoid
    /// rescaling against the native session's shared cell measurements.
    pub fn icon(&mut self, style: &str) -> String {
        let Some(key) = self.active.get(style) else {
            return "● ".into();
        };
        let image = self.images.get_mut(key).expect("prepared image");
        let mut result = String::new();
        if !image.uploaded {
            result.push_str(&image.upload);
            image.uploaded = true;
        }
        result.push_str(&image.placement);
        result
    }
}

fn resolve_color(
    style: &str,
    ansi: &[Option<[u8; 3]>; 16],
    foreground: Option<[u8; 3]>,
) -> Option<[u8; 3]> {
    let codes = style.strip_prefix("\x1b[")?.strip_suffix('m')?;
    let codes: Vec<u16> = codes
        .split(';')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match codes.as_slice() {
        [22, 38, 2, r, g, b] => Some([
            u8::try_from(*r).ok()?,
            u8::try_from(*g).ok()?,
            u8::try_from(*b).ok()?,
        ]),
        [22, code @ 30..=37] => ansi[usize::from(code - 30)],
        [22, code @ 90..=97] => ansi[usize::from(code - 90 + 8)],
        [22, 39] => foreground,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BLUE: &str = "\x1b[22;38;2;30;90;180m";
    const RED: &str = "\x1b[22;38;2;200;40;20m";

    fn prepare(graphics: &mut Graphics, cell: [u32; 2], styles: &[&str]) {
        graphics.prepare(true, Some(cell), styles.iter().copied(), &[None; 16], None);
    }

    #[test]
    fn rasterization_is_shared_between_colors_and_reused_at_earlier_sizes() {
        let mut graphics = Graphics::default();
        prepare(&mut graphics, [10, 20], &[BLUE, RED]);
        assert_eq!(graphics.masks.len(), 1);
        assert_eq!(graphics.images.len(), 2);
        let mask = graphics.masks[&(20, 20)].as_ptr();
        let payload = graphics.images[&((20, 20), [30, 90, 180])].upload.as_ptr();
        prepare(&mut graphics, [12, 24], &[BLUE]);
        prepare(&mut graphics, [10, 20], &[BLUE, RED]);
        assert_eq!(graphics.masks.len(), 2);
        assert_eq!(graphics.images.len(), 3);
        assert_eq!(graphics.masks[&(20, 20)].as_ptr(), mask);
        assert_eq!(
            graphics.images[&((20, 20), [30, 90, 180])].upload.as_ptr(),
            payload
        );
        assert!(graphics.masks[&(20, 20)].contains(&0));
        assert!(graphics.masks[&(20, 20)].iter().any(|a| *a > 0 && *a < 255));
    }

    #[test]
    fn wire_payload_is_straight_rgba_chunked_and_uploaded_once() {
        let mut graphics = Graphics::default();
        prepare(&mut graphics, [24, 48], &[BLUE]);
        let upload = graphics.icon(BLUE);
        let mut encoded = String::new();
        for part in upload.split("\x1b_G").skip(1) {
            let command = part.split("\x1b\\").next().unwrap();
            if let Some((_, payload)) = command.split_once(';') {
                assert!(payload.len() <= 4096);
                encoded.push_str(payload);
            }
        }
        let rgba = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(rgba.len(), 48 * 48 * 4);
        for pixel in rgba.chunks_exact(4) {
            assert_eq!(pixel[..3], [30, 90, 180]);
        }
        assert!(upload.contains("a=t,f=32,s=48,v=48"));
        assert_eq!(graphics.icon(BLUE), "\x1b_Ga=p,i=1,C=1,q=2\x1b\\  ");
        assert_eq!(graphics.begin_frame(), CLEAR_PLACEMENTS);
        prepare(&mut graphics, [24, 48], &[BLUE]);
        assert!(!graphics.icon(BLUE).contains("a=t"));
        prepare(&mut graphics, [10, 20], &[BLUE]);
        prepare(&mut graphics, [24, 48], &[BLUE]);
        assert!(graphics.icon(BLUE).contains("a=t"));
    }

    #[test]
    fn unknown_capability_size_and_color_keep_the_text_marker() {
        let mut graphics = Graphics::default();
        graphics.prepare(false, Some([10, 20]), [BLUE].into_iter(), &[None; 16], None);
        assert_eq!(graphics.icon(BLUE), "● ");
        graphics.prepare(true, None, [BLUE].into_iter(), &[None; 16], None);
        assert_eq!(graphics.icon(BLUE), "● ");
        for cell in [[0, 20], [u32::MAX, 20], [10, 20]] {
            prepare(&mut graphics, cell, &["\x1b[22;34m"]);
            assert_eq!(graphics.icon("\x1b[22;34m"), "● ");
        }
        assert!(graphics.images.is_empty());
    }

    #[test]
    fn ansi_uses_confirmed_rgb_and_cache_limits_preserve_earlier_entries() {
        let mut graphics = Graphics::default();
        let mut ansi = [None; 16];
        ansi[4] = Some([17, 42, 99]);
        let style = "\x1b[22;34m";
        graphics.prepare(true, Some([8, 16]), [style].into_iter(), &ansi, None);
        assert!(graphics.images.contains_key(&((16, 16), [17, 42, 99])));
        graphics.bytes = MAX_BYTES;
        prepare(&mut graphics, [9, 18], &[BLUE]);
        assert_eq!(graphics.icon(BLUE), "● ");
        graphics.prepare(true, Some([8, 16]), [style].into_iter(), &ansi, None);
        assert!(graphics.icon(style).contains("a=p"));
    }

    #[test]
    fn bee_and_hive_keep_distinct_images_and_reuse_each_artwork_cache() {
        let mut bee = Graphics::default();
        let mut hive = Graphics::hive();
        prepare(&mut bee, [10, 20], &[BLUE]);
        prepare(&mut hive, [10, 20], &[BLUE]);
        let key = ((20, 20), [30, 90, 180]);
        assert!(bee.images[&key].placement.contains("i=1,"));
        assert!(hive.images[&key].placement.contains("i=1025,"));
        assert!(hive.images[&key].placement.contains("\x1b[1D"));
        assert!(hive.images[&key].placement.contains("X=5,C=1"));
        assert!(hive.images[&key].placement.ends_with("\x1b[1C  "));
        assert_ne!(bee.masks[&(20, 20)], hive.masks[&(20, 20)]);
        assert!(hive.masks[&(20, 20)].contains(&0));
        let mask = hive.masks[&(20, 20)].as_ptr();
        let payload = hive.images[&key].upload.as_ptr();
        assert!(hive.icon(BLUE).contains("a=t"));
        prepare(&mut hive, [10, 20], &[BLUE]);
        assert_eq!(hive.masks[&(20, 20)].as_ptr(), mask);
        assert_eq!(hive.images[&key].upload.as_ptr(), payload);
        assert!(!hive.icon(BLUE).contains("a=t"));
    }
}
