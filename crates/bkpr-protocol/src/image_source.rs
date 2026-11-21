//! Original program image payloads carried inline by `planus_v1_sources` scenes.
use crate::{Image, ImageSourceEncoding, PixelFormat};
use anyhow::{Context, Result, ensure};
use std::io::Read;

/// Inline scene encoding whose images may carry their original PNG/zlib payload instead of pixels.
pub const SOURCES_SCENE_ENCODING: &str = "planus_v1_sources";

/// Decoded bytes produced per bounded inflate turn.
const INFLATE_CHUNK: usize = 64 * 1024;

/// The image's original payload, when it has one no larger than its pixels.
pub(crate) fn original_payload(image: &Image) -> Option<(ImageSourceEncoding, &[u8])> {
    let source = image.source.as_deref()?;
    (!source.bytes.is_empty() && source.bytes.len() <= image.pixels.len())
        .then_some((source.encoding, &source.bytes[..]))
}

/// Inflate one zlib stream to exactly `expected` bytes in bounded turns, rejecting trailing data.
pub(crate) fn inflate_exact(body: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut decoder = flate2::read::ZlibDecoder::new(body);
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(expected)?;
    let mut chunk = [0; INFLATE_CHUNK];
    while pixels.len() < expected {
        let remaining = expected - pixels.len();
        let count = decoder.read(&mut chunk[..remaining.min(INFLATE_CHUNK)])?;
        ensure!(count > 0, "image zlib body truncated");
        pixels.extend_from_slice(&chunk[..count]);
    }
    ensure!(
        decoder.read(&mut [0; 1])? == 0,
        "image zlib body exceeds its dimensions"
    );
    ensure!(
        decoder.total_in() as usize == body.len(),
        "image zlib trailing data"
    );
    Ok(pixels)
}

/// Decode a PNG to 8-bit RGBA pixels, as the terminal engine stores it. Dimensions and the
/// decoded size are checked against the declared shape before pixel allocation.
pub(crate) fn decode_png_rgba(
    body: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    decoded_len: usize,
) -> Result<Vec<u8>> {
    ensure!(
        format == PixelFormat::Rgba,
        "PNG image must be declared RGBA"
    );
    let mut decoder = png::Decoder::new(std::io::Cursor::new(body));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    decoder.set_limits(png::Limits {
        bytes: decoded_len + 1024 * 1024,
    });
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    ensure!(
        info.width == width && info.height == height && !info.is_animated(),
        "PNG dimensions differ from the declared image"
    );
    let (color, depth) = reader.output_color_type();
    ensure!(depth == png::BitDepth::Eight, "PNG output depth");
    let mut buffer = vec![0; reader.output_buffer_size().context("PNG output size")?];
    let frame = reader.next_frame(&mut buffer)?;
    // Forwarded bytes must be a complete file: read through IEND, checking every chunk CRC.
    reader.finish()?;
    buffer.truncate(frame.buffer_size());
    let pixels = match color {
        png::ColorType::Rgba => buffer,
        png::ColorType::GrayscaleAlpha => buffer
            .chunks_exact(2)
            .flat_map(|ga| [ga[0], ga[0], ga[0], ga[1]])
            .collect(),
        _ => anyhow::bail!("unexpected PNG output color type"),
    };
    ensure!(pixels.len() == decoded_len, "PNG decoded length mismatch");
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zlib_payload_inflates_exactly_and_rejects_trailing_or_short_data() {
        let pixels = vec![7u8; 4096];
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&pixels).unwrap();
        let payload = encoder.finish().unwrap();
        assert_eq!(inflate_exact(&payload, pixels.len()).unwrap(), pixels);
        let mut trailing = payload.clone();
        trailing.push(0);
        assert!(inflate_exact(&trailing, pixels.len()).is_err());
        assert!(inflate_exact(&payload, pixels.len() + 1).is_err());
        assert!(inflate_exact(&payload, pixels.len() - 1).is_err());
    }

    #[test]
    fn png_payload_decodes_to_the_engines_rgba_pixels() {
        let (width, height) = (16u32, 8u32);
        let rgb: Vec<u8> = (0..width * height * 3)
            .map(|index| (index * 7) as u8)
            .collect();
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgb)
                .unwrap();
        }
        let rgba: Vec<u8> = rgb
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        let decoded =
            decode_png_rgba(&png_bytes, width, height, PixelFormat::Rgba, rgba.len()).unwrap();
        assert_eq!(decoded, rgba);
        assert!(decode_png_rgba(&png_bytes, 8, height, PixelFormat::Rgba, 8 * 8 * 4).is_err());
        assert!(decode_png_rgba(&png_bytes, width, height, PixelFormat::Rgb, 16 * 8 * 3).is_err());
    }

    #[test]
    fn only_originals_no_larger_than_their_pixels_are_forwarded() {
        let mut image = Image {
            id: 1,
            generation: 1,
            width: 1,
            height: 1,
            format: PixelFormat::Rgb,
            pixels: vec![1, 2, 3],
            source: None,
        };
        assert!(original_payload(&image).is_none());
        image.source = Some(std::sync::Arc::new(crate::ImageSource {
            encoding: ImageSourceEncoding::Zlib,
            bytes: vec![0; 3].into(),
        }));
        assert_eq!(
            original_payload(&image),
            Some((ImageSourceEncoding::Zlib, &[0u8; 3][..]))
        );
        image.source = Some(std::sync::Arc::new(crate::ImageSource {
            encoding: ImageSourceEncoding::Zlib,
            bytes: vec![0; 4].into(),
        }));
        assert!(original_payload(&image).is_none());
    }
}
