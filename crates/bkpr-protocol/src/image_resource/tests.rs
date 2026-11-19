use super::*;
use crate::{PROTOCOL_VERSION, Payload, Response, wire};

fn image() -> Image {
    Image {
        id: 7,
        generation: 19,
        width: 128,
        height: 64,
        format: PixelFormat::Rgba,
        pixels: (0..128 * 64)
            .flat_map(|index| [index as u8, (index / 11) as u8, 73, 0])
            .collect(),
    }
}
fn receive(image: &Image, encoding: ImageEncoding, bytes: &[u8]) -> Result<Image> {
    let descriptor = ImageDescriptor::from_image(image)?;
    let mut transfer = ImageTransfer::new(descriptor)?;
    assert!(
        transfer
            .accept(ImageResource::Preparing { descriptor }, || false)?
            .is_none()
    );
    assert!(
        transfer
            .accept(
                ImageResource::Start {
                    descriptor,
                    encoding,
                    encoded_len: bytes.len() as u64
                },
                || false
            )?
            .is_none()
    );
    for (index, data) in bytes.chunks(311).enumerate() {
        let response = Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(Payload::ImageResource(ImageResource::Data {
                key: image.key(),
                offset: (index * 311) as u64,
                data: Bytes::copy_from_slice(data),
            })),
            error: None,
            failure: None,
        };
        let encoded = wire::Codec::new().encode_response(&response)?.to_vec();
        let response = wire::decode_response_owned(encoded)?;
        let Some(Payload::ImageResource(resource)) = response.result else {
            panic!("missing binary resource")
        };
        assert!(transfer.accept(resource, || false)?.is_none());
    }
    transfer
        .accept(
            ImageResource::End {
                key: image.key(),
                encoded_len: bytes.len() as u64,
            },
            || false,
        )?
        .context("no final pixels")
}
#[test]
fn native_stream_preserves_every_pixel_including_hidden_rgb_and_raw_fallback() {
    let image = image();
    let (encoding, bytes) = encode_image(&image).unwrap();
    assert_eq!(encoding, ImageEncoding::Zstd);
    assert_eq!(receive(&image, encoding, &bytes).unwrap(), image);
    assert_eq!(
        receive(&image, ImageEncoding::Raw, &image.pixels).unwrap(),
        image
    );
    let tiny = Image {
        width: 1,
        height: 1,
        pixels: vec![1, 2, 3, 0],
        ..image
    };
    let (encoding, bytes) = encode_image(&tiny).unwrap();
    assert_eq!(encoding, ImageEncoding::Raw);
    assert!(matches!(bytes, std::borrow::Cow::Borrowed(_)));
}
#[test]
fn malformed_shapes_are_rejected_before_any_body_allocation() {
    let mut descriptor = ImageDescriptor::from_image(&image()).unwrap();
    for (width, height, length) in [
        (0, 1, 0),
        (u32::MAX, u32::MAX, u64::MAX),
        (8192, 8192, 268435456),
        (128, 64, 1),
    ] {
        descriptor.width = width;
        descriptor.height = height;
        descriptor.decoded_len = length;
        assert!(ImageTransfer::new(descriptor).is_err());
    }
}
#[test]
fn wrong_generation_noncontiguous_oversized_and_incomplete_streams_never_complete() {
    let image = image();
    let descriptor = ImageDescriptor::from_image(&image).unwrap();
    for malformed in [
        ImageResource::Data {
            key: image.key(),
            offset: 1,
            data: Bytes::from_static(b"x"),
        },
        ImageResource::Data {
            key: ImageKey {
                id: 7,
                generation: 20,
            },
            offset: 0,
            data: Bytes::from_static(b"x"),
        },
        ImageResource::Data {
            key: image.key(),
            offset: 0,
            data: Bytes::from(vec![0; IMAGE_CHUNK_BYTES + 1]),
        },
        ImageResource::End {
            key: image.key(),
            encoded_len: descriptor.decoded_len,
        },
    ] {
        let mut transfer = ImageTransfer::new(descriptor).unwrap();
        transfer
            .accept(
                ImageResource::Start {
                    descriptor,
                    encoding: ImageEncoding::Raw,
                    encoded_len: descriptor.decoded_len,
                },
                || false,
            )
            .unwrap();
        assert!(transfer.accept(malformed, || false).is_err());
        assert!(
            transfer
                .accept(
                    ImageResource::End {
                        key: image.key(),
                        encoded_len: descriptor.decoded_len
                    },
                    || false
                )
                .is_err()
        );
        assert!(transfer.body.is_empty());
    }
}
#[test]
fn corrupt_concatenated_and_wrong_content_size_zstd_bodies_are_rejected() {
    let image = image();
    let (_, encoded) = encode_image(&image).unwrap();
    let mut corrupt = encoded.to_vec();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xff;
    assert!(receive(&image, ImageEncoding::Zstd, &corrupt).is_err());
    let mut concatenated = encoded.to_vec();
    concatenated.extend_from_slice(&encoded);
    assert!(receive(&image, ImageEncoding::Zstd, &concatenated).is_err());
    let oversized = zstd::bulk::compress(&vec![0; image.pixels.len() * 2], 1).unwrap();
    assert!(receive(&image, ImageEncoding::Zstd, &oversized).is_err());
}
#[test]
fn native_resource_preallocation_limit_and_diagnostics_exclude_body() {
    let image = image();
    let resource = ImageResource::Data {
        key: image.key(),
        offset: 0,
        data: Bytes::from(vec![37; IMAGE_CHUNK_BYTES]),
    };
    let response = Response {
        version: PROTOCOL_VERSION,
        ok: true,
        result: Some(Payload::ImageResource(resource)),
        error: None,
        failure: None,
    };
    let encoded = wire::Codec::new()
        .encode_response(&response)
        .unwrap()
        .to_vec();
    assert!(encoded.len() <= IMAGE_RESOURCE_MAX_FRAME);
    assert!(serde_json::to_string(&response).unwrap().len() < 256);
    let framed = wire::encode_frame(wire::Kind::Response, &encoded).unwrap();
    assert!(
        wire::read_frame(
            &mut framed.as_slice(),
            wire::Kind::Response,
            IMAGE_RESOURCE_MAX_FRAME
        )
        .is_ok()
    );
    let oversized =
        wire::encode_frame(wire::Kind::Response, &vec![0; IMAGE_RESOURCE_MAX_FRAME + 1]).unwrap();
    assert!(
        wire::read_frame(
            &mut oversized.as_slice(),
            wire::Kind::Response,
            IMAGE_RESOURCE_MAX_FRAME
        )
        .is_err()
    );
}

#[test]
fn decompression_window_and_mid_decode_cancellation_remain_bounded() {
    let image = Image {
        width: 2048,
        height: 2048,
        pixels: vec![0; 16 * 1024 * 1024],
        ..image()
    };
    let descriptor = ImageDescriptor::from_image(&image).unwrap();
    let (encoding, encoded) = encode_image(&image).unwrap();
    let mut transfer = ImageTransfer::new(descriptor).unwrap();
    transfer
        .accept(
            ImageResource::Start {
                descriptor,
                encoding,
                encoded_len: encoded.len() as u64,
            },
            || false,
        )
        .unwrap();
    for (index, data) in encoded.chunks(IMAGE_CHUNK_BYTES).enumerate() {
        transfer
            .accept(
                ImageResource::Data {
                    key: image.key(),
                    offset: (index * IMAGE_CHUNK_BYTES) as u64,
                    data: Bytes::copy_from_slice(data),
                },
                || false,
            )
            .unwrap();
    }
    let turns = std::cell::Cell::new(0);
    let error = transfer
        .accept(
            ImageResource::End {
                key: image.key(),
                encoded_len: encoded.len() as u64,
            },
            || {
                let count = turns.get() + 1;
                turns.set(count);
                count >= 4
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert!(turns.get() <= 4 && transfer.body.is_empty());
    assert!(format!("{transfer:?}").len() < 512);

    let mut excessive_window = encoded.into_owned();
    assert_eq!(excessive_window[4] & 0x20, 0);
    excessive_window[5] = (24 - 10) << 3;
    assert!(receive(&image, ImageEncoding::Zstd, &excessive_window).is_err());
}

#[test]
fn compact_compressed_storage_and_resource_debug_do_not_retain_or_dump_raw_capacity() {
    let image = Image {
        width: 1024,
        height: 1024,
        pixels: vec![0; 4 * 1024 * 1024],
        ..image()
    };
    let (encoding, encoded) = encode_image(&image).unwrap();
    assert_eq!(encoding, ImageEncoding::Zstd);
    let std::borrow::Cow::Owned(encoded) = encoded else {
        panic!("expected compressed ownership")
    };
    assert_eq!(encoded.capacity(), encoded.len());
    assert!(encoded.len() < image.pixels.len() / 100);
    let resource = ImageResource::Data {
        key: image.key(),
        offset: 0,
        data: Bytes::from(vec![7; IMAGE_CHUNK_BYTES]),
    };
    assert!(format!("{resource:?}").len() < 256);
}

#[test]
fn resource_request_round_trips_structural_view_and_exact_generation() {
    let request = crate::Request::new(crate::Operation::Pane {
        pane_id: "pane".into(),
        request: crate::PaneRequest::ImageResource {
            view_id: "view".into(),
            key: ImageKey {
                id: 9,
                generation: u64::MAX,
            },
        },
    });
    let encoded = wire::Codec::new()
        .encode_request(&request)
        .unwrap()
        .to_vec();
    let decoded = wire::decode_request(&encoded).unwrap();
    assert!(
        matches!(decoded.operation, crate::Operation::Pane { pane_id, request: crate::PaneRequest::ImageResource { view_id, key } }
        if pane_id == "pane" && view_id == "view" && key.id == 9 && key.generation == u64::MAX)
    );
}
