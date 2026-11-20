use super::{Budget, generated::native_wire as g, types};
use crate::{
    IMAGE_CHUNK_BYTES, IMAGE_MAX_BYTES, ImageDescriptor, ImageEncoding, ImageResource, PixelFormat,
};
use anyhow::{Result, ensure};
use bytes::Bytes;
use planus::{Builder, Offset, UnionOffset};

fn write_descriptor(
    builder: &mut Builder,
    descriptor: &ImageDescriptor,
    budget: &mut Budget,
) -> Result<Offset<g::ImageDescriptor>> {
    descriptor.validate()?;
    let key = types::write_image_key(builder, &descriptor.key, budget, 1)?;
    let format = match descriptor.format {
        PixelFormat::Rgb => g::ImagePixelFormat::Rgb,
        PixelFormat::Rgba => g::ImagePixelFormat::Rgba,
        PixelFormat::GrayAlpha => g::ImagePixelFormat::GrayAlpha,
        PixelFormat::Gray => g::ImagePixelFormat::Gray,
    };
    Ok(g::ImageDescriptor::create(
        builder,
        key,
        descriptor.width,
        descriptor.height,
        format,
        descriptor.decoded_len,
    ))
}
fn read_descriptor(
    descriptor: g::ImageDescriptorRef<'_>,
    budget: &mut Budget,
) -> Result<ImageDescriptor> {
    let descriptor = ImageDescriptor {
        key: types::read_image_key(descriptor.key()?, budget, 1)?,
        width: descriptor.width()?,
        height: descriptor.height()?,
        decoded_len: descriptor.decoded_len()?,
        format: match descriptor.format()? {
            g::ImagePixelFormat::Rgb => PixelFormat::Rgb,
            g::ImagePixelFormat::Rgba => PixelFormat::Rgba,
            g::ImagePixelFormat::GrayAlpha => PixelFormat::GrayAlpha,
            g::ImagePixelFormat::Gray => PixelFormat::Gray,
        },
    };
    descriptor.validate()?;
    Ok(descriptor)
}
fn encoded_length(
    descriptor: &ImageDescriptor,
    encoding: ImageEncoding,
    length: u64,
) -> Result<()> {
    ensure!(
        length > 0
            && length <= descriptor.decoded_len
            && (encoding != ImageEncoding::Raw || length == descriptor.decoded_len),
        "image encoded length limit"
    );
    Ok(())
}
pub(super) fn write_resource(
    builder: &mut Builder,
    resource: &ImageResource,
    budget: &mut Budget,
) -> Result<UnionOffset<g::ImageResource>> {
    Ok(match resource {
        ImageResource::Preparing { descriptor } => {
            let descriptor = write_descriptor(builder, descriptor, budget)?;
            let value = g::ImagePreparing::create(builder, descriptor);
            g::ImageResource::create_image_preparing(builder, value)
        }
        ImageResource::Start {
            descriptor,
            encoding,
            encoded_len,
        } => {
            encoded_length(descriptor, *encoding, *encoded_len)?;
            let descriptor = write_descriptor(builder, descriptor, budget)?;
            let encoding = match encoding {
                ImageEncoding::Raw => g::ImageEncoding::Raw,
                ImageEncoding::Zstd => g::ImageEncoding::Zstd,
                ImageEncoding::Png => g::ImageEncoding::Png,
                ImageEncoding::Zlib => g::ImageEncoding::Zlib,
            };
            let value = g::ImageStart::create(builder, descriptor, encoding, *encoded_len);
            g::ImageResource::create_image_start(builder, value)
        }
        ImageResource::Data { key, offset, data } => {
            ensure!(
                !data.is_empty()
                    && data.len() <= IMAGE_CHUNK_BYTES
                    && offset
                        .checked_add(data.len() as u64)
                        .is_some_and(|end| end <= IMAGE_MAX_BYTES as u64),
                "image chunk byte limit"
            );
            budget.bytes(data.len())?;
            let key = types::write_image_key(builder, key, budget, 1)?;
            let data = builder.create_vector(data.as_ref());
            let value = g::ImageData::create(builder, key, *offset, data);
            g::ImageResource::create_image_data(builder, value)
        }
        ImageResource::End { key, encoded_len } => {
            ensure!(
                *encoded_len > 0 && *encoded_len <= IMAGE_MAX_BYTES as u64,
                "image encoded length limit"
            );
            let key = types::write_image_key(builder, key, budget, 1)?;
            let value = g::ImageEnd::create(builder, key, *encoded_len);
            g::ImageResource::create_image_end(builder, value)
        }
    })
}
pub(super) fn read_resource(
    resource: g::ImageResourceRef<'_>,
    owner: Option<&Bytes>,
    budget: &mut Budget,
) -> Result<ImageResource> {
    Ok(match resource {
        g::ImageResourceRef::ImagePreparing(value) => ImageResource::Preparing {
            descriptor: read_descriptor(value.descriptor()?, budget)?,
        },
        g::ImageResourceRef::ImageStart(value) => {
            let descriptor = read_descriptor(value.descriptor()?, budget)?;
            let encoding = match value.encoding()? {
                g::ImageEncoding::Raw => ImageEncoding::Raw,
                g::ImageEncoding::Zstd => ImageEncoding::Zstd,
                g::ImageEncoding::Png => ImageEncoding::Png,
                g::ImageEncoding::Zlib => ImageEncoding::Zlib,
            };
            let encoded_len = value.encoded_len()?;
            encoded_length(&descriptor, encoding, encoded_len)?;
            ImageResource::Start {
                descriptor,
                encoding,
                encoded_len,
            }
        }
        g::ImageResourceRef::ImageData(value) => {
            let key = types::read_image_key(value.key()?, budget, 1)?;
            let offset = value.offset()?;
            let data = value.data()?;
            ensure!(
                !data.is_empty()
                    && data.len() <= IMAGE_CHUNK_BYTES
                    && offset
                        .checked_add(data.len() as u64)
                        .is_some_and(|end| end <= IMAGE_MAX_BYTES as u64),
                "image chunk byte limit"
            );
            budget.bytes(data.len())?;
            let data = match owner {
                Some(owner) => {
                    let start = (data.as_ptr() as usize)
                        .checked_sub(owner.as_ptr() as usize)
                        .ok_or_else(|| anyhow::anyhow!("image data precedes receive allocation"))?;
                    let end = start
                        .checked_add(data.len())
                        .ok_or_else(|| anyhow::anyhow!("image data range overflow"))?;
                    ensure!(end <= owner.len(), "image data outside receive allocation");
                    owner.slice(start..end)
                }
                None => Bytes::copy_from_slice(data),
            };
            ImageResource::Data { key, offset, data }
        }
        g::ImageResourceRef::ImageEnd(value) => {
            let key = types::read_image_key(value.key()?, budget, 1)?;
            let encoded_len = value.encoded_len()?;
            ensure!(
                encoded_len > 0 && encoded_len <= IMAGE_MAX_BYTES as u64,
                "image encoded length limit"
            );
            ImageResource::End { key, encoded_len }
        }
    })
}
