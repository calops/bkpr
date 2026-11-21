use base64::Engine;
use bkpr_protocol::{Frame, Image, ImageSourceEncoding, PixelFormat, Size};
use bkpr_terminal_engine::Terminal;
use std::io::Write;

const SIDE: u32 = 64;

fn terminal() -> Terminal {
    Terminal::new(Size {
        cols: 40,
        rows: 40,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap()
}

fn transmit(term: &mut Terminal, keys: &str, payload: &[u8]) {
    let payload = base64::engine::general_purpose::STANDARD.encode(payload);
    // C=1 keeps the cursor still, so every placement stays on screen and in the frame.
    term.feed(format!("\x1b_G{keys},C=1,q=2;{payload}\x1b\\").as_bytes())
        .unwrap();
}

fn zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// A compressible RGB PNG, optionally declared as a two-frame animation.
fn png(rgb: &[u8], animated: bool) -> Vec<u8> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, SIDE, SIDE);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        if animated {
            encoder.set_animated(2, 0).unwrap();
        }
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgb).unwrap();
        if animated {
            writer.write_image_data(rgb).unwrap();
        }
    }
    png
}

fn image(frame: &Frame, id: u32) -> &Image {
    frame.images.iter().find(|image| image.id == id).unwrap()
}

fn source(frame: &Frame, id: u32) -> Option<(ImageSourceEncoding, Vec<u8>)> {
    let source = image(frame, id).source.as_deref()?;
    Some((source.encoding, source.bytes.to_vec()))
}

#[test]
fn still_png_and_zlib_transmissions_keep_their_exact_original_payload() {
    let rgb = vec![7u8; (SIDE * SIDE * 3) as usize];
    let rgba = vec![9u8; (SIDE * SIDE * 4) as usize];
    let png = png(&rgb, false);
    let (zlib_rgb, zlib_rgba) = (zlib(&rgb), zlib(&rgba));
    let size = format!("s={SIDE},v={SIDE}");

    let mut term = terminal();
    transmit(&mut term, "a=T,f=100,i=1", &png);
    transmit(&mut term, &format!("a=T,f=24,{size},o=z,i=2"), &zlib_rgb);
    transmit(&mut term, &format!("a=T,f=32,{size},o=z,i=3"), &zlib_rgba);
    transmit(&mut term, &format!("a=T,f=24,{size},i=4"), &rgb);
    let frame = term.snapshot().unwrap();

    let png_image = image(&frame, 1);
    assert_eq!(png_image.format, PixelFormat::Rgba);
    assert_eq!(png_image.pixels.len(), (SIDE * SIDE * 4) as usize);
    assert_eq!(source(&frame, 1), Some((ImageSourceEncoding::Png, png)));
    assert_eq!(image(&frame, 2).pixels, rgb);
    assert_eq!(
        source(&frame, 2),
        Some((ImageSourceEncoding::Zlib, zlib_rgb))
    );
    assert_eq!(image(&frame, 3).pixels, rgba);
    assert_eq!(
        source(&frame, 3),
        Some((ImageSourceEncoding::Zlib, zlib_rgba))
    );
    // Raw payloads equal the pixels; nothing extra is retained or forwarded.
    assert_eq!(source(&frame, 4), None);
}

#[test]
fn payloads_that_cannot_stand_for_the_shown_pixels_are_not_forwarded() {
    let rgb = vec![7u8; (SIDE * SIDE * 3) as usize];
    let size = format!("s={SIDE},v={SIDE}");
    // Incompressible pixels: their zlib stream is larger than the pixels themselves.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let noise: Vec<u8> = (0..SIDE * SIDE * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect();

    let mut term = terminal();
    // An APNG shows its default image here, but would animate if forwarded.
    transmit(&mut term, "a=T,f=100,i=1", &png(&rgb, true));
    transmit(
        &mut term,
        &format!("a=T,f=24,{size},o=z,i=2"),
        &zlib(&noise),
    );
    transmit(&mut term, &format!("a=T,f=24,{size},o=z,i=3"), &zlib(&rgb));
    // Adding an animation frame: the original no longer describes everything the image shows.
    let edit = vec![1u8; 4 * 4 * 3];
    transmit(&mut term, "a=f,f=24,s=4,v=4,x=0,y=0,i=3", &edit);
    let frame = term.snapshot().unwrap();

    assert_eq!(source(&frame, 1), None);
    assert_eq!(image(&frame, 2).pixels, noise);
    assert_eq!(source(&frame, 2), None);
    assert_eq!(source(&frame, 3), None);
}
