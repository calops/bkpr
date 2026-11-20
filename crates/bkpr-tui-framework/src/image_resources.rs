//! Validated image metadata independent of pixel residency.
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{IMAGE_MAX_BYTES, Image, ImageDescriptor, ImageKey};
use std::{collections::HashMap, sync::Arc};

/// Ordered, complete image metadata for a canonical scene, including pending pixels.
/// Construction validates the 1024-image and aggregate 64 MiB decoded quotas.
#[derive(Clone, Debug)]
pub struct ImageManifest {
    descriptors: Vec<ImageDescriptor>,
    indices: HashMap<ImageKey, usize>,
    /// Previous resident generations presented while a replacement is pending.
    stand_ins: HashMap<ImageKey, Arc<Image>>,
}

impl ImageManifest {
    /// Validate descriptors, unique keys, dimensions and aggregate decoded quota.
    /// No pixel storage is allocated or required.
    pub fn new(descriptors: Vec<ImageDescriptor>) -> Result<Self> {
        ensure!(descriptors.len() <= 1024, "image manifest count limit");
        let mut indices = HashMap::with_capacity(descriptors.len());
        let mut decoded_bytes = 0u64;
        for (index, descriptor) in descriptors.iter().enumerate() {
            descriptor.validate()?;
            ensure!(
                descriptor.width <= 8192 && descriptor.height <= 8192,
                "image manifest dimension limit"
            );
            decoded_bytes = decoded_bytes
                .checked_add(descriptor.decoded_len)
                .context("image manifest decoded byte total overflow")?;
            ensure!(
                decoded_bytes <= IMAGE_MAX_BYTES as u64,
                "aggregate image manifest decoded byte limit"
            );
            ensure!(
                indices.insert(descriptor.key, index).is_none(),
                "duplicate image manifest key"
            );
        }
        Ok(Self {
            descriptors,
            indices,
            stand_ins: HashMap::new(),
        })
    }

    /// Present `image`, an older resident generation of the same native image, while
    /// `key` is pending, as a terminal keeps the old pixels until a replacement completes.
    /// Requires identical dimensions and format so every placement crop stays valid.
    pub fn set_stand_in(&mut self, key: ImageKey, image: Arc<Image>) -> Result<()> {
        let descriptor = self
            .descriptor(key)
            .context("stand-in for undeclared image")?;
        ensure!(
            image.id == key.id
                && image.generation < key.generation
                && image.width == descriptor.width
                && image.height == descriptor.height
                && image.format == descriptor.format,
            "stand-in differs from its pending image"
        );
        self.stand_ins.insert(key, image);
        Ok(())
    }

    /// Stop presenting a stand-in, normally once the pending generation is resident.
    pub fn clear_stand_in(&mut self, key: ImageKey) {
        self.stand_ins.remove(&key);
    }

    /// Previous generation presented for a pending `key`, if any.
    pub fn stand_in(&self, key: ImageKey) -> Option<&Arc<Image>> {
        self.stand_ins.get(&key)
    }

    /// All validated descriptors in their original scene order.
    pub fn descriptors(&self) -> &[ImageDescriptor] {
        &self.descriptors
    }

    /// Look up an exact pane-local image generation without rebuilding an index.
    pub fn descriptor(&self, key: ImageKey) -> Option<&ImageDescriptor> {
        self.indices
            .get(&key)
            .map(|index| &self.descriptors[*index])
    }

    pub(crate) fn validate_image(&self, image: &Image) -> Result<()> {
        self.descriptor(image.key())
            .context("image resource absent from manifest")?
            .validate_image(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::PixelFormat;

    fn descriptor(id: u32, width: u32, height: u32) -> ImageDescriptor {
        ImageDescriptor {
            key: ImageKey { id, generation: 1 },
            width,
            height,
            format: PixelFormat::Rgba,
            decoded_len: u64::from(width) * u64::from(height) * 4,
        }
    }

    #[test]
    fn metadata_is_validated_before_any_pixels_exist() {
        let first = descriptor(1, 4096, 4096);
        let manifest = ImageManifest::new(vec![first]).unwrap();
        assert_eq!(manifest.descriptors(), &[first]);
        assert_eq!(manifest.descriptor(first.key), Some(&first));
        assert!(ImageManifest::new(vec![first, descriptor(2, 1, 1)]).is_err());
        let small = descriptor(1, 1, 1);
        assert!(ImageManifest::new(vec![small, small]).is_err());
        assert!(ImageManifest::new((0..1025).map(|id| descriptor(id, 1, 1)).collect()).is_err());
        for invalid in [
            ImageDescriptor {
                decoded_len: 3,
                ..small
            },
            descriptor(1, 0, 1),
            descriptor(1, 8193, 1),
            ImageDescriptor {
                width: u32::MAX,
                height: u32::MAX,
                decoded_len: u64::MAX,
                ..small
            },
        ] {
            assert!(ImageManifest::new(vec![invalid]).is_err());
        }
    }
}
