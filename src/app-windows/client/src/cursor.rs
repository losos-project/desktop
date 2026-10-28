//! The guest's own pointer pictures, made into what a Wayland pointer
//! surface shows (`protocol::CursorImage`).

use base64::Engine;

/// The largest pointer picture taken, in pixels a side: macOS's biggest
/// accessibility pointer at 2x, with room to spare.
const MAX_SIDE: u32 = 512;

/// A pointer picture in `wl_shm`'s ARGB8888: four bytes a pixel, blue
/// first, its colour multiplied by its alpha as Wayland expects.
#[derive(Debug, PartialEq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub argb: Vec<u8>,
}

/// Decodes the base64 PNG a guest sent.
pub fn pixels(png_base64: &str) -> Result<Pixels, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png_base64)
        .map_err(|e| format!("not base64: {e}"))?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Whatever the PNG holds comes out as 8-bit samples with an alpha.
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("a pointer picture of {width}x{height}"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or("a pointer picture too big to hold")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let samples = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::GrayscaleAlpha => 2,
        other => return Err(format!("a pointer picture in {other:?}")),
    };
    let mut argb = Vec::with_capacity((width * height * 4) as usize);
    for row in buf[..info.buffer_size()].chunks(info.line_size) {
        for px in row.chunks_exact(samples).take(width as usize) {
            let (r, g, b, a) = match *px {
                [r, g, b, a] => (r, g, b, a),
                [l, a] => (l, l, l, a),
                _ => unreachable!("chunks of {samples}"),
            };
            let pre = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
            argb.extend_from_slice(&[pre(b), pre(g), pre(r), a]);
        }
    }
    Ok(Pixels {
        width,
        height,
        argb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> String {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        base64::engine::general_purpose::STANDARD.encode(out)
    }

    #[test]
    fn rgba_becomes_premultiplied_bgra() {
        // Opaque red, then half-transparent white.
        let picture = png(
            2,
            1,
            png::ColorType::Rgba,
            &[255, 0, 0, 255, 255, 255, 255, 128],
        );
        assert_eq!(
            pixels(&picture).unwrap(),
            Pixels {
                width: 2,
                height: 1,
                argb: vec![0, 0, 255, 255, 128, 128, 128, 128],
            }
        );
    }

    #[test]
    fn grey_with_alpha_is_taken_too() {
        let picture = png(1, 1, png::ColorType::GrayscaleAlpha, &[200, 255]);
        assert_eq!(pixels(&picture).unwrap().argb, vec![200, 200, 200, 255]);
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(pixels("not base64!").is_err());
        assert!(pixels("aGVsbG8=").is_err());
        let huge = png(
            MAX_SIDE + 1,
            1,
            png::ColorType::GrayscaleAlpha,
            &vec![0; (MAX_SIDE as usize + 1) * 2],
        );
        assert!(pixels(&huge).is_err());
    }
}
