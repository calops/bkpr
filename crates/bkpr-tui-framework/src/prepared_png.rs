//! Worker-only lossless transport preparation (PNG or zlib) and immutable image identity.

use bkpr_protocol::{Image, PixelFormat};
use std::{
    io::Cursor,
    sync::{Arc, Weak},
};

// Match canonical renderer byte bounds and the host's maximum image extent.
const MAX_DIMENSION: u32 = 10_000;
const MAX_PIXEL_BYTES: usize = 64 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 128 * 1024 * 1024;
const DECODER_WORKING_BYTES: usize = 8 * 1024 * 1024;

/// Lossless Kitty payload form of one image's exact pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportEncoding {
    /// A complete PNG file, transmitted with `f=100`.
    Png,
    /// The image's packed RGB or RGBA pixels as one zlib stream, transmitted with `o=z`.
    Zlib,
}

/// Worker-prepared Kitty payload bound to one immutable image allocation.
///
/// Cloning retains encoded bytes, but never keeps the source image alive. Native
/// IDs and generations are deliberately not sufficient transport identities.
#[derive(Clone, Debug)]
pub struct PreparedTransport {
    image: Weak<Image>,
    encoding: TransportEncoding,
    bytes: Arc<[u8]>,
}

impl PreparedTransport {
    /// Bind PNG bytes on a graphics worker, after encoding or verified decoding.
    ///
    /// The caller must establish that PNG dimensions, eight-bit channel format
    /// and decoded pixels match this exact immutable image, including alpha.
    /// Rendering relies on this invariant and performs no codec/content checks.
    pub fn from_verified(image: &Arc<Image>, png: Arc<[u8]>) -> Self {
        Self {
            image: Arc::downgrade(image),
            encoding: TransportEncoding::Png,
            bytes: png,
        }
    }

    /// Bind a zlib stream of this image's exact RGB/RGBA pixels, verified by the caller.
    ///
    /// The invariant of [`Self::from_verified`] applies: inflating the stream must
    /// yield exactly `image.pixels`. Gray formats have no Kitty raw form; `None`.
    pub fn from_verified_zlib(image: &Arc<Image>, zlib: Arc<[u8]>) -> Option<Self> {
        matches!(image.format, PixelFormat::Rgb | PixelFormat::Rgba).then(|| Self {
            image: Arc::downgrade(image),
            encoding: TransportEncoding::Zlib,
            bytes: zlib,
        })
    }

    /// Compress RGB/RGBA pixels on a worker thread, never while rendering.
    ///
    /// `None` for gray formats, or when compression would not shrink the payload
    /// and raw transmission is already the cheaper form.
    pub fn compress(image: &Arc<Image>) -> Option<Self> {
        if !matches!(image.format, PixelFormat::Rgb | PixelFormat::Rgba) {
            return None;
        }
        let mut encoder = flate2::write::ZlibEncoder::new(
            Vec::with_capacity(image.pixels.len() / 2),
            flate2::Compression::fast(),
        );
        std::io::Write::write_all(&mut encoder, &image.pixels).ok()?;
        let compressed = encoder.finish().ok()?;
        if compressed.len() >= image.pixels.len() {
            return None;
        }
        Self::from_verified_zlib(image, compressed.into())
    }

    /// Kitty payload form of [`Self::bytes`].
    pub(crate) fn encoding(&self) -> TransportEncoding {
        self.encoding
    }

    /// Allocation identity; retaining the weak pointer prevents address reuse.
    pub(crate) fn source_id(&self) -> usize {
        self.image.as_ptr() as usize
    }

    /// Whether the bound immutable source still has an owner.
    pub(crate) fn is_live(&self) -> bool {
        self.image.strong_count() != 0
    }

    /// Match allocation identity without hashing or examining source pixels.
    pub(crate) fn matches(&self, image: &Image) -> bool {
        self.is_live() && std::ptr::eq(self.image.as_ptr(), image)
    }

    /// Encoded residency charged by worker and terminal transport cache budgets.
    pub(crate) fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Borrow worker-verified transport bytes without copying them.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn pixel_bytes(width: u32, height: u32, format: PixelFormat) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return None;
    }
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(format.bytes_per_pixel())?;
    (bytes <= MAX_PIXEL_BYTES).then_some(bytes)
}

fn color_type(format: PixelFormat) -> png::ColorType {
    match format {
        PixelFormat::Rgb => png::ColorType::Rgb,
        PixelFormat::Rgba => png::ColorType::Rgba,
        PixelFormat::Gray => png::ColorType::Grayscale,
        PixelFormat::GrayAlpha => png::ColorType::GrayscaleAlpha,
    }
}

/// Encode exact packed eight-bit pixels on a graphics worker, never while rendering.
/// Invalid dimensions or byte counts miss before allocating codec/output buffers.
pub(crate) fn encode(
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: &[u8],
) -> Option<Arc<[u8]>> {
    if pixel_bytes(width, height, format)? != pixels.len() {
        return None;
    }
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(color_type(format));
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(pixels).ok()?;
        writer.finish().ok()?;
    }
    (encoded.len() <= MAX_PNG_BYTES).then(|| encoded.into())
}

// Reader stops inflating after the expected pixels and then drains IDAT without
// validating its zlib trailer. Feed the maintained streaming decoder through
// IEND with output headroom, so its inflater must verify completion and Adler-32.
// Call only after bounded Reader validation: StreamingDecoder has no public
// allocation-limit setter, but reuses the same already-validated metadata path.
fn verify_complete_stream(
    mut encoded: &[u8],
    options: png::DecodeOptions,
    output_limit: usize,
) -> Option<()> {
    let mut decoder = png::StreamingDecoder::new_with_options(options);
    let mut buffer = vec![0; output_limit.checked_add(1)?.min(64 * 1024)];
    let mut region = png::UnfilterRegion::default();
    let mut total = 0usize;
    let mut flushed = false;
    let mut stalled = false;
    loop {
        // Preserve the inflater's lookback region, not the full decoded image.
        // In particular, never call its finish path with a full output buffer:
        // png deliberately tolerates missing trailers in that situation.
        if region.available != 0 {
            buffer.copy_within(region.available..region.filled, 0);
            region.filled -= region.available;
            region.available = 0;
        }
        let previous_filled = region.filled;
        let (consumed, event) = decoder
            .update(encoded, Some(&mut region.as_buf(&mut buffer)))
            .ok()?;
        let produced = region.filled.checked_sub(previous_filled)?;
        total = total.checked_add(produced)?;
        if total > output_limit {
            return None;
        }
        encoded = encoded.get(consumed..)?;
        match event {
            png::Decoded::ImageDataFlushed => flushed = true,
            png::Decoded::ChunkComplete(png::chunk::IEND) => {
                return (flushed && total != 0).then_some(());
            }
            _ => {}
        }
        // An empty IDAT advances to CRC processing without consuming input or
        // producing pixels. Permit that state transition, but not a stuck loop.
        let no_progress = consumed == 0 && produced == 0;
        if encoded.is_empty() || (no_progress && stalled) {
            return None;
        }
        stalled = no_progress;
    }
}

/// Decode exact packed eight-bit pixels on a graphics worker, never while rendering.
/// Fixed-header preflight bounds dimensions/output before any codec allocation;
/// the maintained PNG decoder owns all syntax, checksum and decompression checks.
/// Unsupported formats, transparency conversions, animations and corrupt files miss.
pub(crate) fn decode(
    encoded: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Option<Vec<u8>> {
    let bytes = pixel_bytes(width, height, format)?;
    // The decoder allocates its chunk/inflater buffers at construction. Inspect
    // only the fixed IHDR layout first, not a second chunk parser or decoder.
    let header = encoded.get(..33)?;
    if encoded.len() > MAX_PNG_BYTES
        || header[..8] != *b"\x89PNG\r\n\x1a\n"
        || header[8..12] != 13u32.to_be_bytes()
        || header[12..16] != *b"IHDR"
        || header[16..20] != width.to_be_bytes()
        || header[20..24] != height.to_be_bytes()
        || header[24] != 8
        || header[25] != color_type(format) as u8
    {
        return None;
    }
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(encoded), options.clone());
    decoder.set_limits(png::Limits {
        bytes: DECODER_WORKING_BYTES,
    });
    let mut reader = decoder.read_info().ok()?;
    if reader.info().width != width
        || reader.info().height != height
        || reader.info().bit_depth != png::BitDepth::Eight
        || reader.info().color_type != color_type(format)
        || reader.info().trns.is_some()
        || reader.info().animation_control.is_some()
        || reader.info().frame_control.is_some()
        || reader.output_buffer_size()? != bytes
    {
        return None;
    }
    // Adam7 visits each source pixel once, with at most seven filter bytes per
    // source row. The ordinary path needs exactly one filter byte per row.
    let filter_rows = usize::try_from(height)
        .ok()?
        .checked_mul(if reader.info().interlaced { 7 } else { 1 })?;
    let filtered_limit = bytes.checked_add(filter_rows)?;
    let mut pixels = vec![0; bytes];
    let output = reader.next_frame(&mut pixels).ok()?;
    if output.width != width
        || output.height != height
        || output.bit_depth != png::BitDepth::Eight
        || output.color_type != color_type(format)
        || output.buffer_size() != bytes
    {
        return None;
    }
    // next_frame alone need not consume IEND or validate trailing chunk CRCs.
    reader.finish().ok()?;
    if reader.info().trns.is_some()
        || reader.info().animation_control.is_some()
        || reader.info().frame_control.is_some()
    {
        return None;
    }
    verify_complete_stream(encoded, options, filtered_limit)?;
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_eight_bit_roundtrip_preserves_every_channel() {
        for format in [
            PixelFormat::Rgb,
            PixelFormat::Rgba,
            PixelFormat::Gray,
            PixelFormat::GrayAlpha,
        ] {
            let pixels: Vec<_> = (0..6 * format.bytes_per_pixel())
                .map(|index| (index * 71) as u8)
                .collect();
            let encoded = encode(3, 2, format, &pixels).unwrap();
            assert_eq!(decode(&encoded, 3, 2, format).unwrap(), pixels);
            for other in [
                PixelFormat::Rgb,
                PixelFormat::Rgba,
                PixelFormat::Gray,
                PixelFormat::GrayAlpha,
            ] {
                if format != other {
                    assert!(decode(&encoded, 3, 2, other).is_none());
                }
            }
        }
    }

    #[test]
    fn dimensions_and_input_lengths_are_bounded() {
        assert!(encode(0, 1, PixelFormat::Gray, &[]).is_none());
        assert!(encode(1, 1, PixelFormat::Rgba, &[0; 3]).is_none());
        assert!(encode(u32::MAX, 1, PixelFormat::Gray, &[]).is_none());
        assert!(encode(MAX_DIMENSION, MAX_DIMENSION, PixelFormat::Rgba, &[]).is_none());
        let encoded = encode(1, 1, PixelFormat::Rgba, &[1, 2, 3, 4]).unwrap();
        assert!(decode(&encoded, 2, 1, PixelFormat::Rgba).is_none());
        let mut oversized = encoded.to_vec();
        oversized[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&oversized, u32::MAX, 1, PixelFormat::Rgba).is_none());
        assert!(decode(&oversized, 1, 1, PixelFormat::Rgba).is_none());
    }

    #[test]
    fn truncated_png_and_crc_failures_are_misses() {
        let encoded = encode(2, 1, PixelFormat::Rgba, &[0, 7, 255, 0, 9, 20, 30, 127]).unwrap();
        for end in 0..encoded.len() {
            assert!(decode(&encoded[..end], 2, 1, PixelFormat::Rgba).is_none());
        }
        let mut bad_header_crc = encoded.to_vec();
        bad_header_crc[29] ^= 1;
        assert!(decode(&bad_header_crc, 2, 1, PixelFormat::Rgba).is_none());
        let mut bad_end_crc = encoded.to_vec();
        *bad_end_crc.last_mut().unwrap() ^= 1;
        assert!(decode(&bad_end_crc, 2, 1, PixelFormat::Rgba).is_none());
    }

    fn png_with_split_trailer(trailer: &[u8]) -> Vec<u8> {
        // Complete stored DEFLATE block for filter byte 0 and RGBA [1,2,3,4].
        // The zlib Adler-32 (00 19 00 0b) belongs in the following IDAT chunk.
        const PIXELS_WITHOUT_TRAILER: &[u8] =
            &[0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, 0x00, 1, 2, 3, 4];
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_chunk(png::chunk::IDAT, &[]).unwrap();
            writer
                .write_chunk(png::chunk::IDAT, PIXELS_WITHOUT_TRAILER)
                .unwrap();
            writer.write_chunk(png::chunk::IDAT, &[]).unwrap();
            writer.write_chunk(png::chunk::IDAT, &[]).unwrap();
            writer.write_chunk(png::chunk::IDAT, trailer).unwrap();
            writer.write_chunk(png::chunk::IDAT, &[]).unwrap();
            writer.finish().unwrap();
        }
        encoded
    }

    #[test]
    fn split_zlib_trailer_and_empty_idat_chunks_are_verified() {
        let valid = png_with_split_trailer(&[0x00, 0x19, 0x00, 0x0b]);
        assert_eq!(
            decode(&valid, 1, 1, PixelFormat::Rgba).unwrap(),
            [1, 2, 3, 4]
        );
        // write_chunk supplies valid PNG CRCs for every malformed zlib fixture.
        for trailer in [
            &[][..],
            &[0x00][..],
            &[0x00, 0x19, 0x00][..],
            &[0x00, 0x19, 0x00, 0x0a][..],
        ] {
            let malformed = png_with_split_trailer(trailer);
            assert!(decode(&malformed, 1, 1, PixelFormat::Rgba).is_none());
        }
    }

    #[test]
    fn complete_stream_validation_bounds_cumulative_inflated_surplus() {
        // Valid stored zlib stream expanding to 65535 bytes: the correct first
        // pixel/filter followed by zero surplus. The checksum covers all bytes.
        let mut compressed = vec![0x78, 0x01, 0x01, 0xff, 0xff, 0x00, 0x00];
        compressed.extend([0, 1, 2, 3, 4]);
        compressed.resize(7 + 65535, 0);
        compressed.extend([0x00, 0x7c, 0x00, 0x0b]);
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_chunk(png::chunk::IDAT, &compressed).unwrap();
            writer.finish().unwrap();
        }
        assert!(decode(&encoded, 1, 1, PixelFormat::Rgba).is_none());
    }

    #[test]
    fn complete_stream_validation_preserves_inflater_lookback_across_turns() {
        let pixels: Vec<_> = (0..257 * 65 * 4).map(|index| (index * 71) as u8).collect();
        let encoded = encode(257, 65, PixelFormat::Rgba, &pixels).unwrap();
        assert_eq!(
            decode(&encoded, 257, 65, PixelFormat::Rgba).unwrap(),
            pixels
        );
    }

    #[test]
    fn identity_does_not_retain_or_confuse_sources() {
        let image = Arc::new(Image {
            id: 7,
            generation: 1,
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
            pixels: vec![1, 2, 3, 4],
            source: None,
        });
        let encoded = encode(1, 1, image.format, &image.pixels).unwrap();
        let prepared = PreparedTransport::from_verified(&image, encoded);
        assert!(prepared.matches(&image));
        assert!(!prepared.matches(&image.as_ref().clone()));
        drop(image);
        assert!(!prepared.is_live());
    }
}
