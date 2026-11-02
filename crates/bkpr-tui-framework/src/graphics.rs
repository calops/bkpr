//! Bounded off-thread SVG icon preparation for ordinary Scene image resources.
//! No terminal output, uploads, protocol parser, or application artwork lives here.
use crate::ColorProbe;
use bkpr_protocol::{ColorSource, Frame, Image, ImageLayer, PixelFormat, Placement, Size};
use resvg::{tiny_skia, usvg};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

const MAX_SIDE: u32 = 512;
const MAX_COLORS: usize = 32;
const MAX_IMAGES: usize = 64;
const MAX_ASSETS: usize = 8;
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Trusted embedded path artwork. IDs must be unique within this preparer.
pub struct SvgAsset {
    pub id: u32,
    pub svg: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    width: u32,
    height: u32,
    colors: [[u8; 3]; MAX_COLORS],
    len: usize,
}
impl Config {
    fn new(size: Size, span: [u16; 2]) -> Option<Self> {
        let width = size.cell_width.checked_mul(u32::from(span[0]))?;
        let height = size.cell_height.checked_mul(u32::from(span[1]))?;
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return None;
        }
        Some(Self {
            width,
            height,
            colors: [[0; 3]; MAX_COLORS],
            len: 0,
        })
    }
    fn add(&mut self, rgb: Option<[u8; 3]>) {
        if let Some(rgb) = rgb
            && self.len < MAX_COLORS
            && !self.colors[..self.len].contains(&rgb)
        {
            self.colors[self.len] = rgb;
            self.len += 1;
        }
    }
}
#[derive(Clone)]
struct Entry {
    asset_id: u32,
    rgb: [u8; 3],
    image: Arc<Image>,
}
impl Entry {
    fn matches(&self, asset_id: u32, config: &Config, rgb: [u8; 3]) -> bool {
        self.asset_id == asset_id
            && self.rgb == rgb
            && self.image.width == config.width
            && self.image.height == config.height
    }
}
#[derive(Default)]
struct State {
    desired: Option<(u64, Config)>,
    published: Option<(u64, Vec<Entry>)>,
    stop: bool,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    generation: AtomicU64,
}

/// One coalescing worker, one pending configuration, one bounded publication.
/// The caller registers wake_fd with its existing poll loop, invokes drain on
/// readability, and repaints when drain returns true. No terminal I/O occurs.
pub struct SvgGraphics {
    span: [u16; 2],
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    wake: UnixStream,
    config: Option<Config>,
    ready: Vec<Entry>,
    foreground: Option<[u8; 3]>,
    palette: HashMap<usize, [u8; 3]>,
    color_revision: Option<u64>,
}
impl SvgGraphics {
    pub fn new(assets: &'static [SvgAsset], columns: u16, rows: u16) -> io::Result<Self> {
        if assets.is_empty()
            || assets.len() > MAX_ASSETS
            || columns == 0
            || rows == 0
            || assets.iter().any(|asset| asset.svg.len() > 64 * 1024)
            || assets
                .iter()
                .enumerate()
                .any(|(i, a)| assets[..i].iter().any(|b| a.id == b.id))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid SVG asset set or cell span",
            ));
        }
        let (wake, notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
        });
        let target = Arc::clone(&shared);
        // Distinguish resources from other preparers or a replacement instance
        // mounted under the same stable Scene surface id.
        static RESOURCE_GENERATION: AtomicU64 = AtomicU64::new(1);
        let resource_generation = RESOURCE_GENERATION.fetch_add(1, Ordering::Relaxed);
        let worker = thread::Builder::new()
            .name("bkpr-svg".into())
            .spawn(move || run(target, notify, assets, resource_generation))?;
        Ok(Self {
            span: [columns, rows],
            shared,
            worker: Some(worker),
            wake,
            config: None,
            ready: Vec::new(),
            foreground: None,
            palette: HashMap::new(),
            color_revision: None,
        })
    }
    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// Cheap bounded comparison; parsing, masks, tinting and cache eviction are
    /// worker-only. Pass the actual marker foreground sources, never a theme.
    pub fn prepare(&mut self, host: &ColorProbe, size: Size, sources: &[ColorSource]) {
        if self.color_revision != Some(host.revision) {
            self.foreground = host.foreground;
            self.palette.clone_from(&host.palette);
            self.color_revision = Some(host.revision);
        }
        let mut next = host
            .graphics
            .supported()
            .then(|| Config::new(size, self.span))
            .flatten();
        if let Some(config) = next.as_mut() {
            config.add(self.foreground);
            for source in sources {
                config.add(self.resolve(*source));
            }
            // Canonicalize order so row movement cannot invalidate raster work.
            config.colors[..config.len].sort_unstable();
            if config.len == 0 {
                next = None;
            }
        }
        if next == self.config {
            return;
        }
        self.config = next;
        self.ready.clear();
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.desired = next.map(|config| (generation, config));
        state.published = None;
        self.shared.changed.notify_one();
    }
    fn resolve(&self, source: ColorSource) -> Option<[u8; 3]> {
        match source {
            ColorSource::Default => self.foreground,
            ColorSource::Palette(index) => self.palette.get(&usize::from(index)).copied(),
            ColorSource::Rgb(rgb) => Some(rgb),
        }
    }
    pub fn drain(&mut self) -> io::Result<bool> {
        let mut bytes = [0; 128];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "chrome worker closed",
                    ));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        let published = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .published
            .take();
        if let Some((generation, entries)) = published
            && generation == self.shared.generation.load(Ordering::Acquire)
        {
            self.ready = entries;
            return Ok(true);
        }
        Ok(false)
    }

    /// Append a shared image and placement in local surface coordinates. The
    /// caller owns text/cursor policy. Use a stable Scene surface.id to namespace
    /// resources and let the existing renderer clip, delete and deduplicate them.
    pub fn place(
        &self,
        frame: &mut Frame,
        asset_id: u32,
        position: [i32; 2],
        offset: [u32; 2],
        source: ColorSource,
        placement_id: u32,
    ) -> bool {
        let [col, row] = position;
        let [offset_x, offset_y] = offset;
        let Some(config) = self.config.as_ref() else {
            return false;
        };
        if frame.size.cell_width.checked_mul(u32::from(self.span[0])) != Some(config.width)
            || frame.size.cell_height.checked_mul(u32::from(self.span[1])) != Some(config.height)
            || offset_x >= frame.size.cell_width
            || offset_y >= frame.size.cell_height
        {
            return false;
        }
        let Some(rgb) = self.resolve(source) else {
            return false;
        };
        let Some(entry) = self
            .ready
            .iter()
            .find(|entry| entry.matches(asset_id, config, rgb))
        else {
            return false;
        };
        if !frame
            .images
            .iter()
            .any(|image| image.key() == entry.image.key())
        {
            frame.images.push(Arc::clone(&entry.image));
        }
        frame.placements.push(Placement {
            image_id: entry.image.id,
            generation: entry.image.generation,
            placement_id,
            col,
            row,
            offset_x,
            offset_y,
            width: config.width,
            height: config.height,
            source_x: 0,
            source_y: 0,
            source_width: config.width,
            source_height: config.height,
            layer: ImageLayer::AboveText,
            z: 0,
        });
        true
    }
}
impl Drop for SvgGraphics {
    fn drop(&mut self) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stop = true;
            state.desired = None;
            state.published = None;
        }
        self.shared.changed.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn current(shared: &Shared, generation: u64) -> bool {
    shared.generation.load(Ordering::Acquire) == generation
}
fn mask(tree: &usvg::Tree, config: &Config) -> Option<Vec<u8>> {
    let mut pixmap = tiny_skia::Pixmap::new(config.width, config.height)?;
    let size = tree.size();
    let scale = (config.width as f32 / size.width()).min(config.height as f32 / size.height());
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (config.width as f32 - size.width() * scale) / 2.0,
        (config.height as f32 - size.height() * scale) / 2.0,
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    Some(
        pixmap
            .data()
            .chunks_exact(4)
            .map(|pixel| pixel[3])
            .collect(),
    )
}

struct Masks {
    size: [u32; 2],
    alpha: Vec<Option<Vec<u8>>>,
}

fn run(
    shared: Arc<Shared>,
    mut notify: UnixStream,
    assets: &'static [SvgAsset],
    resource_generation: u64,
) {
    let trees: Vec<_> = assets
        .iter()
        .map(|asset| usvg::Tree::from_str(asset.svg, &usvg::Options::default()).ok())
        .collect();
    let mut cache: VecDeque<Entry> = VecDeque::new();
    let mut cache_bytes = 0;
    let mut next_id = 1u32;
    let mut masks: Option<Masks> = None;
    loop {
        let (generation, config) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while !state.stop && state.desired.is_none() {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
            if state.stop {
                return;
            }
            state.desired.take().unwrap()
        };
        if !current(&shared, generation) {
            continue;
        }
        let mut entries = Vec::with_capacity((config.len * assets.len()).min(MAX_IMAGES));
        let mut bytes = 0;
        for rgb in config.colors[..config.len].iter().copied() {
            for (index, asset) in assets.iter().enumerate() {
                if !current(&shared, generation) {
                    break;
                }
                let image_bytes = config.width as usize * config.height as usize * 4;
                if entries.len() >= MAX_IMAGES || bytes + image_bytes > MAX_BYTES {
                    continue;
                }
                let entry = if let Some(index) =
                    cache.iter().position(|e| e.matches(asset.id, &config, rgb))
                {
                    let entry = cache.remove(index).unwrap();
                    cache.push_back(entry.clone());
                    entry
                } else {
                    // A geometry revisit satisfied by tinted pixels needs no
                    // mask allocation or rasterization. Prepare just this
                    // asset's mask lazily on the first actual pixel-cache miss.
                    if masks
                        .as_ref()
                        .is_none_or(|masks| masks.size != [config.width, config.height])
                    {
                        masks = Some(Masks {
                            size: [config.width, config.height],
                            alpha: (0..assets.len()).map(|_| None).collect(),
                        });
                    }
                    let prepared = &mut masks.as_mut().unwrap().alpha[index];
                    if prepared.is_none() {
                        *prepared = trees[index].as_ref().and_then(|tree| mask(tree, &config));
                    }
                    let Some(alpha) = prepared.as_ref() else {
                        continue;
                    };
                    let mut pixels = Vec::with_capacity(image_bytes);
                    for row in alpha.chunks(config.width as usize) {
                        if !current(&shared, generation) {
                            break;
                        }
                        for a in row {
                            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], *a]);
                        }
                    }
                    if pixels.len() != image_bytes {
                        break;
                    }
                    let Some(following) = next_id.checked_add(1) else {
                        return;
                    };
                    let entry = Entry {
                        asset_id: asset.id,
                        rgb,
                        image: Arc::new(Image {
                            id: next_id,
                            generation: resource_generation,
                            width: config.width,
                            height: config.height,
                            format: PixelFormat::Rgba,
                            pixels,
                        }),
                    };
                    next_id = following;
                    while cache.len() >= MAX_IMAGES || cache_bytes + image_bytes > MAX_BYTES {
                        let Some(old) = cache.pop_front() else { break };
                        cache_bytes -= old.image.pixels.len();
                    }
                    cache_bytes += image_bytes;
                    cache.push_back(entry.clone());
                    entry
                };
                bytes += image_bytes;
                entries.push(entry);
            }
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stop {
            return;
        }
        if !current(&shared, generation) {
            continue;
        }
        state.published = Some((generation, entries));
        drop(state);
        loop {
            match notify.write(&[1]) {
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static ASSETS: &[SvgAsset] = &[SvgAsset {
        id: 7,
        svg: r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><circle cx="8" cy="8" r="7"/></svg>"#,
    }];
    fn size(width: u32) -> Size {
        Size {
            cols: 8,
            rows: 2,
            cell_width: width,
            cell_height: 20,
        }
    }
    fn host() -> ColorProbe {
        let mut host = ColorProbe::default();
        let query = host.graphics.begin();
        let query = std::str::from_utf8(&query).unwrap();
        let id = query.split("i=").nth(1).unwrap().split(',').next().unwrap();
        host.feed(format!("graphics;Gi={id};OK").as_bytes());
        host.feed(b"10;rgb:ffff/0000/0000");
        host
    }
    fn prepared(graphics: &mut SvgGraphics) {
        let mut poll = libc::pollfd {
            fd: graphics.wake_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
        assert!(graphics.drain().unwrap());
    }
    fn placed(graphics: &SvgGraphics, width: u32) -> Frame {
        let mut frame = crate::blank(size(width));
        crate::text(&mut frame, 1, 0, "● label", false);
        assert!(graphics.place(
            &mut frame,
            7,
            [0, 0],
            [width / 2, 0],
            ColorSource::Default,
            1
        ));
        frame
    }
    #[test]
    fn resize_and_palette_invalidation_preserve_text_until_matching_pixels_are_ready() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let mut host = host();
        graphics.prepare(&host, size(10), &[]);
        prepared(&mut graphics);
        let first = placed(&graphics, 10);
        assert!(
            first.images[0]
                .pixels
                .chunks_exact(4)
                .any(|p| p[3] > 0 && p[3] < 255)
        );
        assert!(
            first.images[0]
                .pixels
                .chunks_exact(4)
                .all(|p| p[..3] == [255, 0, 0])
        );
        let original = crate::blank(size(11));
        let mut pending = original.clone();
        graphics.prepare(&host, size(11), &[]);
        assert!(!graphics.place(&mut pending, 7, [0, 0], [0, 0], ColorSource::Default, 1));
        assert_eq!(pending, original);
        prepared(&mut graphics);
        assert_eq!(placed(&graphics, 11).images[0].width, 22);
        graphics.prepare(&host, size(10), &[]);
        prepared(&mut graphics);
        let restored = placed(&graphics, 10);
        assert!(Arc::ptr_eq(&first.images[0], &restored.images[0]));
        host.feed(b"10;rgb:0000/ffff/0000");
        graphics.prepare(&host, size(10), &[]);
        let mut pending = crate::blank(size(10));
        assert!(!graphics.place(&mut pending, 7, [0, 0], [0, 0], ColorSource::Default, 1));
        prepared(&mut graphics);
        let recolored = placed(&graphics, 10);
        assert_ne!(first.images[0].key(), recolored.images[0].key());
        assert!(
            recolored.images[0]
                .pixels
                .chunks_exact(4)
                .all(|p| p[..3] == [0, 255, 0])
        );
    }
    #[test]
    fn placement_deduplicates_shared_pixels_without_touching_text_or_cursor() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        graphics.prepare(&host(), size(11), &[]);
        prepared(&mut graphics);
        let mut frame = placed(&graphics, 11);
        let rows = frame.rows.clone();
        let cursor = frame.cursor.clone();
        assert!(graphics.place(&mut frame, 7, [3, 1], [5, 0], ColorSource::Default, 2));
        assert_eq!(frame.images.len(), 1);
        assert_eq!(frame.placements.len(), 2);
        assert_eq!(frame.rows, rows);
        assert_eq!(frame.cursor, cursor);
        assert_eq!(frame.placements[0].offset_x, 5);
    }
    #[test]
    fn coalesced_resize_publishes_only_the_latest_geometry() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        for width in 1..=32 {
            graphics.prepare(&host, size(width), &[]);
        }
        // An earlier generation may have signalled before it was superseded.
        while graphics.ready.is_empty() {
            let mut poll = libc::pollfd {
                fd: graphics.wake_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
            graphics.drain().unwrap();
        }
        let frame = placed(&graphics, 32);
        assert_eq!(frame.images[0].width, 64);
        assert_eq!(frame.images[0].height, 20);
    }
}
