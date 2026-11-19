//! Worker-only persistence of reusable raster inputs, never pane contents or layouts.
use std::{
    fmt::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use bkpr_protocol::PixelFormat;
use sha2::{Digest, Sha256};

use crate::{graphics_cache::DiskCache, prepared_png};

/// Decoded working pixels and their optional lossless transport encoding.
pub(crate) struct Raster {
    /// Exact packed pixels used by canonical composition.
    pub pixels: Vec<u8>,
    /// PNG bytes produced or verified on the worker.
    pub png: Option<Arc<[u8]>>,
    /// Whether the rendering callback ran instead of a persistent hit.
    #[cfg(test)]
    pub rendered: bool,
}

impl Raster {
    /// Retain working pixels without encoding or admitting a persistent resource.
    pub fn unencoded(pixels: Vec<u8>) -> Self {
        Self {
            pixels,
            png: None,
            #[cfg(test)]
            rendered: true,
        }
    }
}

/// A worker-local codec adapter around the shared bounded disk store.
#[derive(Default)]
pub(crate) struct RasterCache {
    disk: Option<DiskCache>,
    namespace: Option<Sha256>,
}

impl RasterCache {
    /// Select a caller-owned directory without filesystem access.
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            disk: root.map(DiskCache::new),
            namespace: None,
        }
    }

    fn key(&mut self, identity: &impl fmt::Debug, size: [u32; 2], format: PixelFormat) -> [u8; 32] {
        let namespace = self.namespace.get_or_insert_with(|| {
            let mut hash = Sha256::new();
            hash.update(include_bytes!("prepared_png.rs"));
            hash.update(b"bkpr-reusable-raster-v1\0");
            hash.update(std::env::consts::ARCH.as_bytes());
            hash.update(include_bytes!("graphics.rs"));
            hash.update(include_bytes!("decoration.rs"));
            hash.update(include_bytes!("joined.rs"));
            hash.update(include_bytes!("joined_contour.rs"));
            hash.update(include_bytes!("../Cargo.toml"));
            hash
        });
        struct HashText(Sha256);
        impl fmt::Write for HashText {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                self.0.update(text.as_bytes());
                Ok(())
            }
        }
        let mut hash = HashText(namespace.clone());
        write!(&mut hash, "{identity:?}|{size:?}|{format:?}").expect("hash formatting cannot fail");
        hash.0.finalize().into()
    }

    /// Read and decode one exact reusable raster on the worker.
    pub fn load(
        &mut self,
        identity: &impl fmt::Debug,
        size: [u32; 2],
        format: PixelFormat,
    ) -> Option<Raster> {
        self.disk.as_ref()?;
        let key = self.key(identity, size, format);
        let maximum = usize::try_from(size[0])
            .ok()?
            .checked_mul(usize::try_from(size[1]).ok()?)?
            .checked_mul(8)?
            .checked_add(8192)?;
        let disk = self.disk.as_mut()?;
        let bytes = disk.get(&key, maximum)?;
        match prepared_png::decode(&bytes, size[0], size[1], format) {
            Some(pixels) => Some(Raster {
                pixels,
                png: Some(bytes.into()),
                #[cfg(test)]
                rendered: false,
            }),
            None => {
                disk.remove(&key);
                None
            }
        }
    }

    /// Encode and best-effort persist pixels on the worker, retaining raw fallback.
    pub fn store(
        &mut self,
        identity: &impl fmt::Debug,
        size: [u32; 2],
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Raster {
        let png = prepared_png::encode(size[0], size[1], format, &pixels);
        if let Some(bytes) = &png
            && self.disk.is_some()
        {
            let key = self.key(identity, size, format);
            if let Some(disk) = self.disk.as_mut() {
                disk.put(&key, bytes);
            }
        }
        Raster {
            pixels,
            png,
            #[cfg(test)]
            rendered: true,
        }
    }

    /// Reuse an exact disk hit or run the supplied rasterizer once.
    pub fn prepare(
        &mut self,
        identity: &impl fmt::Debug,
        size: [u32; 2],
        format: PixelFormat,
        render: impl FnOnce() -> Option<Vec<u8>>,
    ) -> Option<Raster> {
        if let Some(cached) = self.load(identity, size, format) {
            return Some(cached);
        }
        let pixels = render()?;
        Some(self.store(identity, size, format, pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_reuses_exact_alpha_and_changed_visual_inputs_regenerate() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap().join("rasters");
        let pixels = vec![12, 34, 56, 78, 90, 123, 234, 0];
        let mut first = RasterCache::new(Some(directory.clone()));
        first
            .prepare(&("corner", [1, 2, 3]), [2, 1], PixelFormat::Rgba, || {
                Some(pixels.clone())
            })
            .unwrap();
        drop(first);
        let mut second = RasterCache::new(Some(directory));
        let hit = second
            .prepare(&("corner", [1, 2, 3]), [2, 1], PixelFormat::Rgba, || {
                panic!("persisted material rerasterized")
            })
            .unwrap();
        assert_eq!(hit.pixels, pixels);
        for changed in [("corner", [1, 2, 4]), ("shadow", [1, 2, 3])] {
            let replacement = vec![255; 8];
            let raster = second
                .prepare(&changed, [2, 1], PixelFormat::Rgba, || {
                    Some(replacement.clone())
                })
                .unwrap();
            assert_eq!(raster.pixels, replacement);
        }
        let resized = vec![42; 12];
        let raster = second
            .prepare(&("corner", [1, 2, 3]), [3, 1], PixelFormat::Rgba, || {
                Some(resized.clone())
            })
            .unwrap();
        assert_eq!(raster.pixels, resized);
    }

    #[test]
    fn corrupt_disk_bytes_regenerate_without_changing_the_result() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap().join("rasters");
        let mut cache = RasterCache::new(Some(directory));
        let identity = ("raised", [1, 2, 3]);
        let key = cache.key(&identity, [1, 1], PixelFormat::Rgba);
        assert!(cache.disk.as_mut().unwrap().put(&key, b"corrupt PNG"));
        let pixels = vec![12, 34, 56, 78];
        let restored = cache
            .prepare(&identity, [1, 1], PixelFormat::Rgba, || {
                Some(pixels.clone())
            })
            .unwrap();
        assert_eq!(restored.pixels, pixels);
        let reopened = cache
            .prepare(&identity, [1, 1], PixelFormat::Rgba, || {
                panic!("regenerated PNG was not reusable")
            })
            .unwrap();
        assert_eq!(reopened.pixels, pixels);
    }
}
