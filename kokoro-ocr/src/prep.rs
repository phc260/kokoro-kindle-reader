// Bytes on the wire -> the RGB buffer both models are fed from.
//
// COLOUR, not grayscale, and no inversion. What arrives here is what the reader rendered. A
// detector that has to find four words inside an illustration needs every bit of the contrast a
// flatten throws away, and these models were trained on ordinary photographs of the world rather
// than on scans. Do not add a flatten or an inversion; if a real dark-theme capture ever fails,
// the fix is a model-side one and belongs below this line, not in the extension.
//
// There is no upscaling step either, and its absence is the point: `recognize.rs` resizes every
// detected line to a fixed 48 px height from the SOURCE pixels, so small type is upsampled for
// free, per line. A page-wide 2x in front of that would resample twice and quadruple the
// detector's input for nothing.
//
// Both of those INVERT what the engine this replaced wanted, which makes them the two rules most
// likely to be "restored" by someone reasoning from a general-purpose OCR engine's needs. They
// were measured, not assumed. See ../README.md.

use std::io::Cursor;

use image::codecs::png::PngDecoder;
use image::{DynamicImage, ImageDecoder};

use crate::{Error, Limits, Rect};

/// Three bytes per pixel, row-major, no padding.
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Geometry only — a failing assertion that printed eight million pixels would be worse than
/// no message at all.
impl std::fmt::Debug for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgb({}x{}, {} bytes)", self.width, self.height, self.pixels.len())
    }
}

impl Rgb {
    /// The pixel at `(x, y)`, clamped into the image. Callers are already working in
    /// clamped rectangles; this exists so a rounding error cannot index out of bounds.
    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let i = ((y as usize) * (self.width as usize) + (x as usize)) * 3;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2]]
    }

    /// Copy out a rectangle. The rect must already be clamped to the image.
    pub fn crop(&self, r: Rect) -> Rgb {
        let (w, h) = (r.width().max(0) as u32, r.height().max(0) as u32);
        let mut pixels = Vec::with_capacity((w as usize) * (h as usize) * 3);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&self.at(r.x0 as u32 + x, r.y0 as u32 + y));
            }
        }
        Rgb { width: w, height: h, pixels }
    }

    /// Resample to an exact size.
    ///
    /// Bilinear (`Triangle`), matching the `cv2.resize` default that PaddleOCR's own
    /// preprocessing uses for both stages. Not a taste call: the models were trained and are
    /// evaluated upstream through that resampler, and a sharper filter here would be a silent
    /// difference from the reference pipeline in the one place the input meets the weights.
    pub fn resize(&self, w: u32, h: u32) -> Rgb {
        if w == self.width && h == self.height {
            return Rgb { width: w, height: h, pixels: self.pixels.clone() };
        }
        let src = image::RgbImage::from_raw(self.width, self.height, self.pixels.clone())
            .expect("rgb buffer is width*height*3 by construction");
        let out = image::imageops::resize(&src, w.max(1), h.max(1), image::imageops::FilterType::Triangle);
        Rgb { width: out.width(), height: out.height(), pixels: out.into_raw() }
    }
}

/// Decode a submitted image to RGB, refusing anything outside `limits` BEFORE the pixels are
/// allocated.
///
/// The order matters. `PngDecoder::new` parses the header alone, so the dimension and
/// pixel-count checks run against declared geometry while the only thing allocated is the
/// compressed body — which the transport already capped. Decoding first and measuring
/// afterwards is not a limit: a 40 KB PNG declaring 30000x30000 is a 900 megapixel
/// allocation that has already happened by the time anyone looks at it.
pub fn decode_rgb(bytes: &[u8], limits: &Limits) -> Result<Rgb, Error> {
    if bytes.len() > limits.max_body_bytes {
        return Err(Error::TooLarge(format!(
            "{} byte body over the {} byte limit",
            bytes.len(),
            limits.max_body_bytes
        )));
    }

    let decoder =
        PngDecoder::new(Cursor::new(bytes)).map_err(|e| Error::Decode(format!("not a PNG: {e}")))?;
    let (width, height) = decoder.dimensions();

    if width == 0 || height == 0 {
        return Err(Error::Decode("zero-sized image".into()));
    }
    if width > limits.max_dimension || height > limits.max_dimension {
        return Err(Error::TooLarge(format!(
            "{width}x{height} exceeds the {} px per-side limit",
            limits.max_dimension
        )));
    }
    // u64 throughout: the product of two u32 dimensions overflows u32 well inside the range
    // a PNG header can declare, and an overflowed product compares as SMALL — the check
    // would pass exactly the images it exists to reject.
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limits.max_pixels {
        return Err(Error::TooLarge(format!(
            "{pixels} px exceeds the {} px limit",
            limits.max_pixels
        )));
    }

    let image = DynamicImage::from_decoder(decoder)
        .map_err(|e| Error::Decode(format!("decode failed: {e}")))?;
    let rgb = image.into_rgb8();

    Ok(Rgb { width, height, pixels: rgb.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits::default()
    }

    /// A real PNG, encoded rather than hand-written: a literal byte array would carry
    /// hand-computed CRCs, and the decoder checks them.
    fn png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&vec![0u8; (w * h * 3) as usize], w, h, image::ExtendedColorType::Rgb8)
            .expect("encode");
        out
    }

    fn tiny_png() -> Vec<u8> {
        png(1, 1)
    }

    #[test]
    fn decodes_a_png_to_three_bytes_per_pixel() {
        let g = decode_rgb(&png(4, 3), &limits()).expect("decode");
        assert_eq!((g.width, g.height), (4, 3));
        assert_eq!(g.pixels.len(), 36);
    }

    #[test]
    fn a_grayscale_png_still_arrives_as_rgb() {
        // The extension is free to post a flattened page; the models take three channels
        // either way, so the decode is what normalizes it — not a second code path.
        use image::ImageEncoder;
        let mut gray = Vec::new();
        image::codecs::png::PngEncoder::new(&mut gray)
            .write_image(&[10u8, 200], 2, 1, image::ExtendedColorType::L8)
            .expect("encode");
        let g = decode_rgb(&gray, &limits()).expect("decode");
        assert_eq!(g.pixels, vec![10, 10, 10, 200, 200, 200]);
    }

    #[test]
    fn rejects_a_non_png() {
        let err = decode_rgb(b"\xff\xd8\xff\xe0 JFIF, not PNG", &limits()).unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "got {err:?}");
    }

    #[test]
    fn rejects_truncated_bytes() {
        let mut bytes = tiny_png();
        bytes.truncate(30);
        assert!(decode_rgb(&bytes, &limits()).is_err());
    }

    #[test]
    fn rejects_a_body_over_the_limit() {
        let limits = Limits { max_body_bytes: 8, ..Limits::default() };
        assert!(matches!(decode_rgb(&tiny_png(), &limits), Err(Error::TooLarge(_))));
    }

    #[test]
    fn rejects_declared_geometry_over_the_pixel_limit() {
        // The header is checked before the pixels exist, so a 1x1 image trips this the same
        // way a real bomb would - which is the property being tested.
        let limits = Limits { max_pixels: 0, ..Limits::default() };
        assert!(matches!(decode_rgb(&tiny_png(), &limits), Err(Error::TooLarge(_))));
    }

    #[test]
    fn cropping_takes_the_rectangle_asked_for() {
        let img = Rgb {
            width: 3,
            height: 2,
            pixels: vec![
                1, 1, 1, 2, 2, 2, 3, 3, 3, //
                4, 4, 4, 5, 5, 5, 6, 6, 6,
            ],
        };
        let c = img.crop(Rect { x0: 1, y0: 0, x1: 3, y1: 2 });
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.pixels, vec![2, 2, 2, 3, 3, 3, 5, 5, 5, 6, 6, 6]);
    }

    #[test]
    fn resizing_to_the_same_size_changes_nothing() {
        let img = Rgb { width: 2, height: 1, pixels: vec![9, 8, 7, 6, 5, 4] };
        let r = img.resize(2, 1);
        assert_eq!(r.pixels, img.pixels);
    }

    #[test]
    fn resizing_reaches_the_exact_requested_size() {
        let img = Rgb { width: 4, height: 4, pixels: vec![128; 48] };
        let r = img.resize(9, 3);
        assert_eq!((r.width, r.height), (9, 3));
        assert_eq!(r.pixels.len(), 9 * 3 * 3);
    }
}
