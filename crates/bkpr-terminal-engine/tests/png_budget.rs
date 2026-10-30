use base64::Engine;
use bkpr_protocol::{PixelFormat, Size};
use bkpr_terminal_engine::Terminal;

fn terminal() -> Terminal {
    Terminal::new(Size {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap()
}
fn encode(color: png::ColorType, pixels: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(pixels)
            .unwrap();
    }
    encoded
}
#[test]
fn pane_rejects_oversized_png_header_and_remains_usable_for_valid_images() {
    let mut term = terminal();
    let mut encoded = encode(png::ColorType::Rgba, &[1, 2, 3, 4]);
    // A tiny valid PNG header advertising 256 MiB of RGBA output. The SDK's
    // allocator-count regression proves this is denied before pixel allocation;
    // this test exercises Bkpr's configured quota through the real VT consumer.
    encoded[16..24].copy_from_slice(&[0, 0, 0x20, 0, 0, 0, 0x20, 0]);
    let crc = crc32(&encoded[12..29]);
    encoded[29..33].copy_from_slice(&crc.to_be_bytes());
    let payload = base64::engine::general_purpose::STANDARD.encode(encoded);
    term.feed(format!("\x1b_Ga=T,f=100,i=41,q=2;{payload}\x1b\\").as_bytes())
        .unwrap();
    assert!(term.snapshot().unwrap().images.is_empty());
    let valid = encode(png::ColorType::GrayscaleAlpha, &[17, 29]);
    let payload = base64::engine::general_purpose::STANDARD.encode(valid);
    term.feed(format!("\x1b_Ga=T,f=100,i=42,q=2;{payload}\x1b\\").as_bytes())
        .unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.images.len(), 1);
    assert_eq!(frame.images[0].id, 42);
    assert_eq!(frame.images[0].format, PixelFormat::Rgba);
    assert_eq!(frame.images[0].pixels, [17, 17, 17, 29]);
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                0xEDB8_8320 ^ (crc >> 1)
            };
        }
    }
    !crc
}
