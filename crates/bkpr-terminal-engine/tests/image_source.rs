use base64::Engine;
use bkpr_protocol::{ImageSourceEncoding, PixelFormat, Size};
use bkpr_terminal_engine::Terminal;
use std::io::Write;

fn terminal() -> Terminal {
    Terminal::new(Size {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap()
}

fn transmit(term: &mut Terminal, keys: &str, payload: &[u8]) {
    let payload = base64::engine::general_purpose::STANDARD.encode(payload);
    term.feed(format!("\x1b_G{keys},q=2;{payload}\x1b\\").as_bytes())
        .unwrap();
}

#[test]
fn png_and_zlib_transmissions_keep_their_exact_original_payload() {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3, 4, 5, 6]).unwrap();
    }
    let raw = [9u8, 8, 7, 6, 5, 4];
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zlib.write_all(&raw).unwrap();
    let zlib = zlib.finish().unwrap();

    let mut term = terminal();
    transmit(&mut term, "a=T,f=100,i=1", &png);
    transmit(&mut term, "a=T,f=24,s=2,v=1,o=z,i=2", &zlib);
    transmit(&mut term, "a=T,f=24,s=2,v=1,i=3", &raw);
    let frame = term.snapshot().unwrap();
    let image = |id| frame.images.iter().find(|image| image.id == id).unwrap();

    let png_image = image(1);
    assert_eq!(png_image.format, PixelFormat::Rgba);
    assert_eq!(png_image.pixels, [1, 2, 3, 255, 4, 5, 6, 255]);
    let source = png_image.source.as_deref().unwrap();
    assert_eq!(
        (source.encoding, source.bytes.as_slice()),
        (ImageSourceEncoding::Png, png.as_slice())
    );

    let zlib_image = image(2);
    assert_eq!(zlib_image.pixels, raw);
    let source = zlib_image.source.as_deref().unwrap();
    assert_eq!(
        (source.encoding, source.bytes.as_slice()),
        (ImageSourceEncoding::Zlib, zlib.as_slice())
    );

    // Raw payloads equal the pixels; nothing extra is retained or forwarded.
    assert!(image(3).source.is_none());
}
