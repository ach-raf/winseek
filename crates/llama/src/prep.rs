//! Image preprocessing shared by the app's Tier 1 pass and the eval harness:
//! decode, downscale to <= 768 px long side, flatten alpha onto white, JPEG q90.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbImage};

pub const LONG_SIDE: u32 = 768;
pub const JPEG_QUALITY: u8 = 90;

/// Downscale (never upscale) so the long side is at most [`LONG_SIDE`], keeping aspect.
pub fn canonicalise(img: DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= LONG_SIDE {
        img
    } else {
        img.resize(LONG_SIDE, LONG_SIDE, FilterType::Lanczos3)
    }
}

/// Flatten to RGB8; transparency is composited over white.
fn to_rgb_on_white(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u32::from(p[3]);
        let mix = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    out
}

/// Decode `bytes`, canonicalise to <= 768 px long side and re-encode as JPEG q90.
pub fn prepare_jpeg(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode: {e}"))?;
    let img = canonicalise(img);
    let rgb = to_rgb_on_white(&img);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|e| format!("encode: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, seed: u8) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([seed, (x % 251) as u8, (y % 251) as u8]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn resize_keeps_aspect_and_caps_long_side() {
        let wide = canonicalise(DynamicImage::ImageRgb8(RgbImage::new(3000, 1500)));
        assert_eq!(wide.dimensions(), (768, 384));
        let tall = canonicalise(DynamicImage::ImageRgb8(RgbImage::new(1000, 4000)));
        assert_eq!(tall.dimensions(), (192, 768));
        let small = canonicalise(DynamicImage::ImageRgb8(RgbImage::new(300, 200)));
        assert_eq!(small.dimensions(), (300, 200), "never upscale");
        let exact = canonicalise(DynamicImage::ImageRgb8(RgbImage::new(768, 500)));
        assert_eq!(exact.dimensions(), (768, 500));
    }

    #[test]
    fn prepared_jpeg_is_jpeg_within_budget() {
        let out = prepare_jpeg(&png(2000, 1000, 5)).unwrap();
        assert_eq!(&out[..3], &[0xff, 0xd8, 0xff]);
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!(back.dimensions(), (768, 384));
        // alpha PNG is flattened onto white
        let rgba = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 0]));
        let mut b = Vec::new();
        DynamicImage::ImageRgba8(rgba).write_to(&mut Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
        let flat = image::load_from_memory(&prepare_jpeg(&b).unwrap()).unwrap().to_rgb8();
        assert!(flat.get_pixel(1, 1).0.iter().all(|&c| c > 240));
    }
}
