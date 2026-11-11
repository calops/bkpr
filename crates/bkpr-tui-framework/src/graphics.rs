//! Off-thread SVG and material preparation for scene image resources.
//! Placement appends frame resources; terminal output remains renderer-owned.
use crate::ColorProbe;
use crate::decoration::{self, DecorationGeometry, DecorationStyle, PreparedDecoration};
use crate::joined::{JoinedCache, JoinedSurface, OpaqueSpan, PreparedJoined};
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
const MAX_DECORATIONS: usize = 11;
const MAX_DECORATION_CACHE: usize = 16;
const MAX_DECORATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_RETAINED_DECORATION_BYTES: usize = 1024 * 1024;

/// Trusted embedded SVG used as an alpha mask for caller-selected colors.
pub struct SvgAsset {
    /// Asset identifier, unique within one preparer.
    pub id: u32,
    /// SVG source, limited to 64 KiB.
    pub svg: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    width: u32,
    height: u32,
    colors: [[u8; 3]; MAX_COLORS],
    len: usize,
    cell: [u32; 2],
    decorations: [Option<DecorationStyle>; MAX_DECORATIONS],
    joined: Option<JoinedSurface>,
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
            cell: [size.cell_width, size.cell_height],
            decorations: [None; MAX_DECORATIONS],
            joined: None,
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
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PreparationWork {
    decoration_rasters: usize,
    decoration_hits: usize,
    decoration_entries: usize,
    decoration_bytes: usize,
}
struct Publication {
    generation: u64,
    entries: Vec<Entry>,
    decorations: Vec<PreparedDecoration>,
    joined: Option<Arc<PreparedJoined>>,
}

#[derive(Default)]
struct State {
    desired: Option<(u64, Config)>,
    published: Option<Publication>,
    stop: bool,
    #[cfg(test)]
    work: PreparationWork,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    generation: AtomicU64,
}

/// Coalescing image preparer with bounded caches and generation-scoped resources.
///
/// Poll [`Self::wake_fd`], call [`Self::drain`] on readability, and repaint when
/// it returns `true`. Disabling presentation retains worker caches.
pub struct SvgGraphics {
    enabled: bool,
    span: [u16; 2],
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    wake: UnixStream,
    config: Option<Config>,
    core_ready: bool,
    ready: Vec<Entry>,
    decorations: Vec<PreparedDecoration>,
    joined_request: Option<JoinedSurface>,
    joined: Option<Arc<PreparedJoined>>,
    joined_cache: JoinedCache,
    foreground: Option<[u8; 3]>,
    palette: HashMap<usize, [u8; 3]>,
    color_revision: Option<u64>,
}
impl SvgGraphics {
    /// Start a worker for one to eight uniquely identified assets and a nonzero
    /// cell span. Reject sources over 64 KiB; SVG parse failures leave that asset
    /// unavailable. Socket or worker creation failures return I/O errors.
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
            enabled: true,
            span: [columns, rows],
            shared,
            worker: Some(worker),
            wake,
            config: None,
            core_ready: false,
            ready: Vec::new(),
            decorations: Vec::new(),
            joined_request: None,
            joined: None,
            joined_cache: JoinedCache::default(),
            foreground: None,
            palette: HashMap::new(),
            color_revision: None,
        })
    }
    /// Borrow the nonblocking publication-notification descriptor for polling.
    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// Set presentation eligibility without changing terminal capability evidence.
    /// Disabling revokes prepared placements and pending work but retains caches.
    /// Returns whether eligibility changed; enabling requires another prepare call.
    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        if self.enabled == enabled {
            return false;
        }
        self.enabled = enabled;
        if !enabled {
            self.configure(None);
        }
        true
    }

    /// Return whether presentation is enabled, independently of host support.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Request icons for the measured cell size and actual foreground sources.
    /// Unresolved colors or unsupported graphics leave those icons unavailable.
    pub fn prepare(&mut self, host: &ColorProbe, size: Size, sources: &[ColorSource]) {
        self.prepare_with_decorations(host, size, sources, &[]);
    }

    /// Request icons and the first eleven distinct caller-supplied materials.
    ///
    /// Exact published resources remain usable while missing variants prepare.
    /// Unresolved colors, unsupported geometry, or exhausted preparation budgets
    /// leave the corresponding resources unavailable.
    pub fn prepare_with_decorations(
        &mut self,
        host: &ColorProbe,
        size: Size,
        sources: &[ColorSource],
        decorations: &[DecorationStyle],
    ) {
        if !self.enabled {
            return;
        }
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
            let mut count = 0;
            config.joined = self.joined_request;
            for style in decorations {
                if config.decorations[..count].contains(&Some(*style)) {
                    continue;
                }
                if count == MAX_DECORATIONS {
                    break;
                }
                config.decorations[count] = Some(*style);
                count += 1;
            }
            config.add(self.foreground);
            for source in sources {
                config.add(self.resolve(*source));
            }
            // Canonicalize order so row movement cannot invalidate raster work.
            config.colors[..config.len].sort_unstable();
            if config.len == 0
                && config.decorations.iter().all(Option::is_none)
                && config.joined.is_none()
            {
                next = None;
            }
        }
        self.configure(next);
    }

    fn configure(&mut self, next: Option<Config>) {
        if next == self.config {
            return;
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let published = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .published
            .take();
        if let Some(publication) = published
            && publication.generation == generation - 1
        {
            self.publish(publication);
        }
        let mut previous_materials = self.config;
        let mut next_materials = next;
        if let Some(config) = previous_materials.as_mut() {
            config.joined = None;
        }
        if let Some(config) = next_materials.as_mut() {
            config.joined = None;
        }
        if previous_materials != next_materials {
            // Completion applies to the whole request, but residency belongs to
            // each exact resource. A new color must not withdraw other icons or
            // unchanged contours while the worker prepares the missing variant.
            self.core_ready = false;
        }
        self.ready.retain(|entry| {
            next.is_some_and(|config| {
                config.colors[..config.len].contains(&entry.rgb)
                    && entry.matches(entry.asset_id, &config, entry.rgb)
            })
        });
        self.decorations.retain(|entry| {
            next.is_some_and(|config| {
                entry.cell == config.cell && config.decorations.contains(&Some(entry.style))
            })
        });
        self.joined = next.and_then(|config| {
            config
                .joined
                .and_then(|request| self.joined_cache.get(request, config.cell))
        });
        let complete = self.core_ready
            && next.is_some_and(|config| config.joined.is_none() || self.joined.is_some());
        self.config = next;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.desired = if complete {
            None
        } else {
            next.map(|config| (generation, config))
        };
        state.published = None;
        if state.desired.is_some() {
            self.shared.changed.notify_one();
        }
    }
    fn resolve(&self, source: ColorSource) -> Option<[u8; 3]> {
        match source {
            ColorSource::Default => self.foreground,
            ColorSource::Palette(index) => self.palette.get(&usize::from(index)).copied(),
            ColorSource::Rgb(rgb) => Some(rgb),
        }
    }
    /// Drain notifications and adopt the latest matching publication.
    /// Returns whether a publication was adopted, or an error if the wake socket failed.
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
        if let Some(publication) = published
            && publication.generation == self.shared.generation.load(Ordering::Acquire)
        {
            self.publish(publication);
            return Ok(true);
        }
        Ok(false)
    }

    fn publish(&mut self, publication: Publication) {
        self.ready = publication.entries;
        self.decorations = publication.decorations;
        self.core_ready = true;
        if let Some(prepared) = &publication.joined {
            self.joined_cache.remember(prepared);
        }
        self.joined = publication.joined;
    }

    /// Retain matching-cell material sources without adding placements.
    /// Inactive variants are admitted while total frame image bytes stay within
    /// 1 MiB; the renderer uploads them only when placed.
    pub fn retain_decoration_sources(&self, frame: &mut Frame, styles: &[DecorationStyle]) {
        let Some(config) = &self.config else {
            return;
        };
        if config.cell != [frame.size.cell_width, frame.size.cell_height] {
            return;
        }
        let mut bytes: usize = frame.images.iter().map(|image| image.pixels.len()).sum();
        for material in &self.decorations {
            if styles.contains(&material.style)
                && !frame
                    .images
                    .iter()
                    .any(|image| image.key() == material.image.key())
                && bytes.saturating_add(material.image.pixels.len())
                    <= MAX_RETAINED_DECORATION_BYTES
            {
                bytes += material.image.pixels.len();
                frame.images.push(Arc::clone(&material.image));
            }
        }
    }

    /// Append a prepared perimeter, reserving 64 placement IDs from `base`.
    /// Returns `false` without changing the frame when unavailable or unsupported.
    pub fn decorate_window(
        &self,
        frame: &mut Frame,
        rect: crate::Rect,
        style: DecorationStyle,
        base: u32,
    ) -> bool {
        self.decorations
            .iter()
            .find(|entry| entry.style == style)
            .is_some_and(|entry| entry.place(frame, rect, base))
    }

    /// Append a perimeter excluding caller-supplied cell rectangles.
    /// Reserve 64 placement IDs from `base`; failure leaves the frame unchanged.
    pub fn decorate_window_clipped(
        &self,
        frame: &mut Frame,
        rect: crate::Rect,
        style: DecorationStyle,
        base: u32,
        exclusions: &[crate::Rect],
    ) -> bool {
        self.decorations
            .iter()
            .find(|entry| entry.style == style)
            .is_some_and(|entry| entry.place_clipped(frame, rect, base, exclusions))
    }

    /// Append a perimeter using absolute pixel bounds and an ownership clip.
    /// Exclusions use frame cell coordinates. Reserve 64 placement IDs from
    /// `base`; failure leaves the frame and native cell geometry unchanged.
    pub fn decorate_window_with_geometry(
        &self,
        frame: &mut Frame,
        rect: crate::Rect,
        style: DecorationStyle,
        base: u32,
        exclusions: &[crate::Rect],
        geometry: DecorationGeometry,
    ) -> bool {
        self.decorations
            .iter()
            .find(|entry| entry.style == style)
            .is_some_and(|entry| entry.place_with_geometry(frame, rect, base, exclusions, geometry))
    }

    /// Set the requested rounded union; `None` withdraws its placements.
    /// Preparation starts once a supported icon/material configuration exists.
    /// Changing only this request preserves matching icon/material readiness.
    pub fn prepare_joined(&mut self, request: Option<JoinedSurface>) {
        self.joined_request = request;
        if !self.enabled {
            return;
        }
        let Some(mut config) = self.config else {
            return;
        };
        config.joined = request;
        self.configure(Some(config));
    }

    /// Return whether the requested rounded union has a prepared layout.
    pub fn joined_ready(&self) -> bool {
        self.joined.is_some()
    }

    /// Return full-cell coverage for the prepared union, or an empty slice.
    /// Spans use request coordinates and are not clipped to the destination frame.
    pub fn joined_opaque_spans(&self) -> &[OpaqueSpan] {
        self.joined
            .as_ref()
            .map_or(&[], |joined| joined.spans.as_slice())
    }

    /// Append union images and placements without changing glyphs or the cursor.
    /// Exclusions suppress only the above-text body, not its shadow or underlay.
    /// Reserve 8192 placement IDs from `base`; failure leaves the frame unchanged.
    pub fn decorate_joined(
        &self,
        frame: &mut Frame,
        base: u32,
        exclusions: &[crate::Rect],
    ) -> bool {
        self.joined
            .as_ref()
            .is_some_and(|joined| joined.place(frame, base, exclusions))
    }

    /// Append an icon above text without changing cells or the cursor.
    ///
    /// `position` is in local surface cells; `offset` is a physical-pixel offset
    /// within that cell. Use a stable scene surface ID and a unique placement ID.
    /// Unavailable resources or mismatched cell geometry leave the frame unchanged.
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
    let mut decoration_cache: VecDeque<PreparedDecoration> = VecDeque::new();
    let mut decoration_bytes = 0;
    let mut joined_cache = JoinedCache::default();
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
        let mut decorations = Vec::new();
        let mut published_bytes = 0;
        for style in config.decorations.into_iter().flatten() {
            if !current(&shared, generation) {
                break;
            }
            let Some([width, height]) = decoration::atlas_size(style, config.cell) else {
                continue;
            };
            let Some(image_bytes) = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
            else {
                continue;
            };
            if image_bytes > MAX_DECORATION_BYTES
                || published_bytes + image_bytes > MAX_DECORATION_BYTES
            {
                continue;
            }
            let entry = if let Some(index) = decoration_cache
                .iter()
                .position(|entry| entry.style == style && entry.cell == config.cell)
            {
                let entry = decoration_cache.remove(index).unwrap();
                decoration_cache.push_back(entry.clone());
                #[cfg(test)]
                {
                    shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .work
                        .decoration_hits += 1;
                }
                entry
            } else {
                let Some(pixels) = decoration::rasterize(style, config.cell) else {
                    continue;
                };
                #[cfg(test)]
                {
                    shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .work
                        .decoration_rasters += 1;
                }
                if !current(&shared, generation) {
                    break;
                }
                let Some(following) = next_id.checked_add(1) else {
                    return;
                };
                let entry = PreparedDecoration {
                    style,
                    cell: config.cell,
                    image: Arc::new(Image {
                        id: next_id,
                        generation: resource_generation,
                        width,
                        height,
                        format: PixelFormat::Rgba,
                        pixels,
                    }),
                };
                next_id = following;
                while decoration_cache.len() >= MAX_DECORATION_CACHE
                    || decoration_bytes + image_bytes > MAX_DECORATION_BYTES
                {
                    let Some(old) = decoration_cache.pop_front() else {
                        break;
                    };
                    decoration_bytes -= old.image.pixels.len();
                }
                decoration_bytes += image_bytes;
                decoration_cache.push_back(entry.clone());
                entry
            };
            published_bytes += image_bytes;
            decorations.push(entry);
        }
        let joined = config.joined.and_then(|request| {
            if !current(&shared, generation) {
                return None;
            }
            joined_cache.prepare(request, config.cell, resource_generation, &mut next_id)
        });
        #[cfg(test)]
        {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.work.decoration_entries = decoration_cache.len();
            state.work.decoration_bytes = decoration_bytes;
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stop {
            return;
        }
        if !current(&shared, generation) {
            continue;
        }
        state.published = Some(Publication {
            generation,
            entries,
            decorations,
            joined,
        });
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
    fn material() -> DecorationStyle {
        DecorationStyle {
            kind: decoration::DecorationKind::Recessed,
            background: [30; 3],
            surface: [20; 3],
            foreground: [200; 3],
            inset_px: [5, 5],
            outset_px: [0, 0],
            radius_px: 10,
            sigma_milli: 3000,
            offset_milli: [3500, 3500],
            shadow_opacity_milli: 720,
            lip_width_milli: 1200,
            lip_highlight_milli: 240,
            lip_shade_milli: 240,
        }
    }
    #[test]
    fn changed_requests_keep_exact_published_icons_and_contours_until_missing_variants_are_ready() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        let size = Size {
            cols: 12,
            rows: 8,
            ..size(10)
        };
        let rect = crate::Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        let fixed = ColorSource::Rgb([10, 20, 30]);
        let added = ColorSource::Rgb([40, 50, 60]);
        let style = material();
        let old = DecorationStyle {
            surface: [50; 3],
            ..style
        };
        let new = DecorationStyle {
            surface: [70; 3],
            ..style
        };
        let place = |graphics: &SvgGraphics, frame: &mut Frame, source| {
            graphics.place(frame, 7, [0, 0], [0, 0], source, 1)
        };
        graphics.prepare_with_decorations(&host, size, &[fixed], &[style, old]);
        prepared(&mut graphics);
        let mut first = crate::blank(size);
        assert!(place(&graphics, &mut first, ColorSource::Default));
        assert!(place(&graphics, &mut first, fixed));
        assert!(graphics.decorate_window(&mut first, rect, style, 64));
        assert!(graphics.decorate_window(&mut first, rect, old, 128));

        graphics.prepare_with_decorations(&host, size, &[fixed, added], &[style, new]);
        let mut pending = crate::blank(size);
        assert!(place(&graphics, &mut pending, ColorSource::Default));
        assert!(place(&graphics, &mut pending, fixed));
        assert!(graphics.decorate_window(&mut pending, rect, style, 64));
        for (original, retained) in first.images.iter().zip(&pending.images) {
            assert!(Arc::ptr_eq(original, retained));
        }
        assert!(!place(&graphics, &mut pending, added));
        assert!(!graphics.decorate_window(&mut pending, rect, new, 128));
        assert!(!graphics.decorate_window(&mut pending, rect, old, 128));

        prepared(&mut graphics);
        let mut complete = crate::blank(size);
        assert!(place(&graphics, &mut complete, ColorSource::Default));
        assert!(place(&graphics, &mut complete, fixed));
        assert!(graphics.decorate_window(&mut complete, rect, style, 64));
        for (retained, published) in pending.images.iter().zip(&complete.images) {
            assert!(Arc::ptr_eq(retained, published));
        }
        assert!(place(&graphics, &mut complete, added));
        assert!(
            complete.images[3]
                .pixels
                .chunks_exact(4)
                .all(|pixel| pixel[..3] == [40, 50, 60])
        );
        assert!(graphics.decorate_window(&mut complete, rect, new, 128));
        assert_ne!(first.images[3].key(), complete.images[4].key());
    }

    #[test]
    fn joined_request_adopts_undrained_matching_icons_and_contours_immediately() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        let size = Size {
            cols: 60,
            rows: 20,
            cell_width: 4,
            cell_height: 8,
        };
        let style = material();
        graphics.prepare_with_decorations(&host, size, &[], &[style]);
        let mut poll = libc::pollfd {
            fd: graphics.wake_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
        graphics.prepare_joined(Some(JoinedSurface {
            rects_px: [[60, 16, 180, 112], [20, 48, 66, 64]],
            style: DecorationStyle {
                kind: decoration::DecorationKind::RaisedSurface,
                inset_px: [0, 0],
                radius_px: 6,
                sigma_milli: 1200,
                offset_milli: [1400; 2],
                ..style
            },
        }));
        let mut frame = crate::blank(size);
        assert!(graphics.place(&mut frame, 7, [0, 0], [0, 0], ColorSource::Default, 1));
        assert!(graphics.decorate_window(
            &mut frame,
            crate::Rect {
                x: 0,
                y: 0,
                width: size.cols,
                height: size.rows,
            },
            style,
            64,
        ));
        assert!(!graphics.joined_ready());
    }

    #[test]
    fn joined_requests_preserve_existing_materials_and_reopen_shared_worker_pixels() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        let size = Size {
            cols: 60,
            rows: 20,
            cell_width: 4,
            cell_height: 8,
        };
        let style = material();
        graphics.prepare_with_decorations(&host, size, &[], &[style]);
        prepared(&mut graphics);
        let original_icon = Arc::clone(&graphics.ready[0].image);
        let original_material = Arc::clone(&graphics.decorations[0].image);
        let request = JoinedSurface {
            rects_px: [[60, 16, 180, 112], [20, 48, 66, 64]],
            style: DecorationStyle {
                kind: decoration::DecorationKind::RaisedSurface,
                inset_px: [0, 0],
                radius_px: 6,
                sigma_milli: 1200,
                offset_milli: [1400; 2],
                ..style
            },
        };
        graphics.prepare_joined(Some(request));
        assert!(!graphics.joined_ready());
        assert!(Arc::ptr_eq(&original_icon, &graphics.ready[0].image));
        assert!(Arc::ptr_eq(
            &original_material,
            &graphics.decorations[0].image
        ));
        prepared(&mut graphics);
        assert!(graphics.joined_ready());
        let original = Arc::clone(graphics.joined.as_ref().unwrap());
        graphics.prepare_joined(None);
        assert!(!graphics.joined_ready());
        graphics.prepare_joined(Some(request));
        assert!(graphics.joined_ready());
        assert!(Arc::ptr_eq(&original, graphics.joined.as_ref().unwrap()));
        assert!(Arc::ptr_eq(
            &original_material,
            &graphics.decorations[0].image
        ));
    }
    #[test]
    fn presentation_toggle_retains_cached_pixels_and_never_overrides_capability_loss() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let mut host = host();
        let size = Size {
            cols: 12,
            rows: 8,
            ..size(10)
        };
        let rect = crate::Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        let geometry = DecorationGeometry {
            bounds_px: [0, 0, 120, 160],
            clip_px: [0, 0, 120, 160],
        };
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        prepared(&mut graphics);
        let mut first = crate::blank(size);
        crate::text(&mut first, 1, 1, "native body", false);
        assert!(graphics.place(&mut first, 7, [0, 0], [0, 0], ColorSource::Default, 1));
        assert!(graphics.decorate_window(&mut first, rect, material(), 64));
        let icon = Arc::clone(&first.images[0]);
        let atlas = Arc::clone(&first.images[1]);
        let work = graphics.shared.state.lock().unwrap().work;

        assert!(graphics.set_enabled(false));
        let generation = graphics.shared.generation.load(Ordering::Acquire);
        // Disabled preparation must not enqueue even a changed raster request.
        for width in [11, 12, 10] {
            graphics.prepare_with_decorations(
                &host,
                Size {
                    cell_width: width,
                    ..size
                },
                &[ColorSource::Rgb([1, 2, 3])],
                &[DecorationStyle {
                    surface: [90; 3],
                    ..material()
                }],
            );
        }
        assert_eq!(
            graphics.shared.generation.load(Ordering::Acquire),
            generation
        );
        let state = graphics.shared.state.lock().unwrap();
        assert!(state.desired.is_none());
        assert!(state.published.is_none());
        assert_eq!(state.work, work);
        drop(state);
        assert!(!graphics.set_enabled(false));
        assert!(host.graphics.supported());
        // Existing application content/resources are never removed or changed.
        let mut native = first.clone();
        assert!(!graphics.place(&mut native, 7, [0, 0], [0, 0], ColorSource::Default, 2));
        assert!(!graphics.decorate_window(&mut native, rect, material(), 128));
        assert!(!graphics.decorate_window_clipped(&mut native, rect, material(), 128, &[]));
        assert!(!graphics.decorate_window_with_geometry(
            &mut native,
            rect,
            material(),
            128,
            &[],
            geometry,
        ));
        assert_eq!(native, first);

        assert!(graphics.set_enabled(true));
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        prepared(&mut graphics);
        let mut restored = crate::blank(size);
        assert!(graphics.place(&mut restored, 7, [0, 0], [0, 0], ColorSource::Default, 1));
        assert!(graphics.decorate_window(&mut restored, rect, material(), 64));
        assert!(Arc::ptr_eq(&icon, &restored.images[0]));
        assert!(Arc::ptr_eq(&atlas, &restored.images[1]));
        assert_eq!(
            graphics
                .shared
                .state
                .lock()
                .unwrap()
                .work
                .decoration_rasters,
            work.decoration_rasters
        );

        // Toggling back on is a preference, not fabricated capability evidence.
        host.graphics = crate::GraphicsProbe::default();
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        graphics.set_enabled(false);
        graphics.set_enabled(true);
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        let mut plain = crate::blank(size);
        let original = plain.clone();
        assert!(!graphics.place(&mut plain, 7, [0, 0], [0, 0], ColorSource::Default, 1));
        assert!(!graphics.decorate_window(&mut plain, rect, material(), 64));
        assert_eq!(plain, original);
        assert!(!host.graphics.supported());
        assert!(graphics.shared.state.lock().unwrap().desired.is_none());
    }

    #[test]
    fn disabling_revokes_an_undrained_worker_publication() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        graphics.prepare_with_decorations(&host, size(10), &[], &[material()]);
        let mut poll = libc::pollfd {
            fd: graphics.wake_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
        assert!(graphics.shared.state.lock().unwrap().published.is_some());
        graphics.set_enabled(false);
        assert!(!graphics.drain().unwrap());
        assert!(graphics.ready.is_empty());
        assert!(graphics.decorations.is_empty());
        graphics.set_enabled(true);
        graphics.prepare_with_decorations(&host, size(10), &[], &[material()]);
        prepared(&mut graphics);
        assert!(!placed(&graphics, 10).images.is_empty());
        assert_eq!(
            graphics
                .shared
                .state
                .lock()
                .unwrap()
                .work
                .decoration_rasters,
            1
        );
    }

    #[test]
    fn large_many_windows_share_sparse_atlas_and_screen_resize_is_ready_immediately() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        let large = Size {
            cols: 240,
            rows: 72,
            cell_width: 10,
            cell_height: 20,
        };
        graphics.prepare_with_decorations(&host, large, &[], &[material()]);
        prepared(&mut graphics);
        let mut frame = crate::blank(large);
        for index in 0..32 {
            assert!(graphics.decorate_window(
                &mut frame,
                crate::Rect {
                    x: index % 8 * 30,
                    y: index / 8 * 18,
                    width: 29,
                    height: 17,
                },
                material(),
                u32::from(index) * 8
            ));
        }
        assert_eq!(frame.images.len(), 1);
        let [width, height] = decoration::atlas_size(material(), [10, 20]).unwrap();
        assert_eq!(frame.images[0].pixels.len(), (width * height * 4) as usize);
        assert_eq!(frame.placements.len(), 32 * 8);
        let original = Arc::clone(&frame.images[0]);
        let smaller = Size {
            cols: 120,
            rows: 40,
            ..large
        };
        graphics.prepare_with_decorations(&host, smaller, &[], &[material()]);
        let mut resized = crate::blank(smaller);
        assert!(graphics.decorate_window(
            &mut resized,
            crate::Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 40,
            },
            material(),
            1
        ));
        assert!(Arc::ptr_eq(&original, &resized.images[0]));
    }
    #[test]
    fn finite_raised_materials_are_prewarmed_and_duplicate_variants_are_not_admitted() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        let size = Size {
            cols: 80,
            rows: 24,
            ..size(10)
        };
        let pane = material();
        let shadow = DecorationStyle {
            kind: decoration::DecorationKind::Raised,
            ..pane
        };
        let guide = |background| DecorationStyle {
            kind: decoration::DecorationKind::RaisedSurface,
            background,
            ..shadow
        };
        let materials = [
            pane,
            shadow,
            guide([30; 3]),
            guide([30, 40, 90]),
            guide([90, 80, 30]),
            guide([45; 3]),
            guide([30, 70, 80]),
            guide([70, 30, 30]),
            guide([30, 70, 30]),
            guide([70, 60, 30]),
            guide([30, 40, 100]),
        ];
        graphics.prepare_with_decorations(&host, size, &[], &materials);
        prepared(&mut graphics);
        let generation = graphics.shared.generation.load(Ordering::Acquire);
        let work = graphics
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .work;
        assert_eq!(work.decoration_rasters, materials.len());
        assert_eq!(work.decoration_entries, materials.len());
        // Mode changes select already prepared materials; none schedules work.
        for style in materials[2..].iter().copied().chain([materials[2]]) {
            let mut frame = crate::graphics_frame(size);
            assert!(graphics.decorate_window(
                &mut frame,
                crate::Rect {
                    x: 4,
                    y: 4,
                    width: 30,
                    height: 10
                },
                style,
                0,
            ));
        }
        for cols in [60, 120, 80] {
            graphics.prepare_with_decorations(&host, Size { cols, ..size }, &[], &materials);
        }
        assert_eq!(
            graphics.shared.generation.load(Ordering::Acquire),
            generation
        );
        assert_eq!(
            graphics
                .shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .work,
            work,
        );

        let mut deduplicated = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let repeated = [pane, shadow, materials[2], materials[2], materials[2]];
        deduplicated.prepare_with_decorations(&host, size, &[], &repeated);
        prepared(&mut deduplicated);
        let generation = deduplicated.shared.generation.load(Ordering::Acquire);
        let work = deduplicated
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .work;
        assert_eq!(work.decoration_rasters, 3);
        assert_eq!(work.decoration_entries, 3);
        deduplicated.prepare_with_decorations(&host, size, &[], &repeated[..3]);
        assert_eq!(
            deduplicated.shared.generation.load(Ordering::Acquire),
            generation
        );
    }
    #[test]
    fn material_work_is_bounded_and_screen_resize_does_not_schedule_it() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let host = host();
        let work = |graphics: &SvgGraphics| {
            graphics
                .shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .work
        };
        let size = Size {
            cols: 120,
            rows: 40,
            ..size(10)
        };
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        prepared(&mut graphics);
        let cold = work(&graphics);
        assert_eq!(cold.decoration_rasters, 1);
        assert_eq!(cold.decoration_hits, 0);
        assert_eq!(cold.decoration_entries, 1);
        let generation = graphics.shared.generation.load(Ordering::Acquire);
        for cols in [80, 100, 120, 180, 240, 120] {
            graphics.prepare_with_decorations(&host, Size { cols, ..size }, &[], &[material()]);
        }
        assert_eq!(
            graphics.shared.generation.load(Ordering::Acquire),
            generation
        );
        assert_eq!(work(&graphics), cold);

        // A material revisit reuses pixels; a new material computes once.
        let mut changed = material();
        changed.surface = [40; 3];
        graphics.prepare_with_decorations(&host, size, &[], &[changed]);
        prepared(&mut graphics);
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        prepared(&mut graphics);
        assert_eq!(work(&graphics).decoration_rasters, 2);
        assert_eq!(work(&graphics).decoration_hits, 1);

        // More unique materials than the entry budget must evict, not grow.
        for value in 50..70 {
            changed.surface = [value; 3];
            graphics.prepare_with_decorations(&host, size, &[], &[changed]);
            prepared(&mut graphics);
            let current = work(&graphics);
            assert!(current.decoration_entries <= MAX_DECORATION_CACHE);
            assert!(current.decoration_bytes <= MAX_DECORATION_BYTES);
        }
        let before = work(&graphics).decoration_rasters;
        graphics.prepare_with_decorations(&host, size, &[], &[material()]);
        prepared(&mut graphics);
        assert_eq!(work(&graphics).decoration_rasters, before + 1);
    }
    #[test]
    fn material_and_cell_changes_revoke_old_pixels_and_capability_loss_is_plain() {
        let mut graphics = SvgGraphics::new(ASSETS, 2, 1).unwrap();
        let mut host = host();
        let size = Size {
            cols: 12,
            rows: 8,
            cell_width: 10,
            cell_height: 20,
        };
        let rect = crate::Rect {
            x: 0,
            y: 0,
            width: 12,
            height: 8,
        };
        let mut style = material();
        graphics.prepare_with_decorations(&host, size, &[], &[style]);
        prepared(&mut graphics);
        let mut first = crate::blank(size);
        assert!(graphics.decorate_window(&mut first, rect, style, 1));
        style.surface = [210, 180, 160];
        graphics.prepare_with_decorations(&host, size, &[], &[style]);
        let mut pending = crate::blank(size);
        assert!(!graphics.decorate_window(&mut pending, rect, style, 1));
        prepared(&mut graphics);
        assert!(graphics.decorate_window(&mut pending, rect, style, 1));
        assert_ne!(first.images[0].key(), pending.images[0].key());
        let resized = Size {
            cell_width: 11,
            ..size
        };
        graphics.prepare_with_decorations(&host, resized, &[], &[style]);
        let mut frame = crate::blank(resized);
        assert!(!graphics.decorate_window(&mut frame, rect, style, 1));
        prepared(&mut graphics);
        assert!(graphics.decorate_window(&mut frame, rect, style, 1));
        host.graphics = crate::GraphicsProbe::default();
        graphics.prepare_with_decorations(&host, resized, &[], &[style]);
        let mut plain = crate::blank(resized);
        let original = plain.clone();
        assert!(!graphics.decorate_window(&mut plain, rect, style, 1));
        assert_eq!(plain, original);
    }
}
