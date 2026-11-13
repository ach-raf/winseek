//! Image preprocessing shared by the app's Tier 1 pass and the eval harness:
//! decode, downscale to <= 768 px long side, flatten alpha onto white, JPEG q90.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbImage};

pub const LONG_SIDE: u32 = 768;
pub const JPEG_QUALITY: u8 = 90;

/// Downscale (never upscale) so the long side is at most [`LONG_SIDE`], keeping aspect.
pub fn canonicalise(img: DynamicImage) -> DynamicImage {
    canonicalise_to(img, LONG_SIDE)
}

/// [`canonicalise`] with another long side (the document parser takes 1024 px).
pub fn canonicalise_to(img: DynamicImage, long_side: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= long_side {
        img
    } else {
        img.resize(long_side, long_side, FilterType::Lanczos3)
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
    prepare_jpeg_long(bytes, LONG_SIDE)
}

/// What the first bytes of a file say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    /// A picture format the `image` crate reads here.
    Picture(Format),
    /// A picture format only the operating system's codecs read (TIFF, ICO, HEIF/AVIF, JPEG XR...).
    OsPicture,
    /// Zero bytes, or nothing but NULs (a file that was allocated and never written).
    Empty,
    /// HTML / XML / plain text saved under a picture's name.
    Text,
    /// Not a signature we know: maybe a camera format the OS has a codec for, maybe not a picture.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Webp,
}

/// Decide from the first bytes (never from the extension) what `bytes` is.
pub fn sniff(bytes: &[u8]) -> Sniff {
    let head = &bytes[..bytes.len().min(4096)];
    if head.iter().all(|&b| b == 0) {
        return Sniff::Empty;
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Sniff::Picture(Format::Jpeg)
    } else if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Sniff::Picture(Format::Png)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Sniff::Picture(Format::Gif)
    } else if head.starts_with(b"BM") && head.len() > 14 {
        Sniff::Picture(Format::Bmp)
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Sniff::Picture(Format::Webp)
    } else if head.starts_with(&[b'I', b'I', 42, 0]) || head.starts_with(&[b'M', b'M', 0, 42]) || head.starts_with(&[0, 0, 1, 0]) || head.starts_with(&[0x49, 0x49, 0xBC]) {
        Sniff::OsPicture
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && [&b"heic"[..], b"heix", b"hevc", b"mif1", b"msf1", b"avif", b"avis"].contains(&&head[8..12]) {
        Sniff::OsPicture
    } else if looks_like_text(head) {
        Sniff::Text
    } else {
        Sniff::Unknown
    }
}

fn plain(t: &str) -> bool {
    !t.chars().any(|c| c.is_control() && !c.is_whitespace())
}

/// Markup or prose: a UTF-8 (or UTF-16 BOM) head without control bytes.
fn looks_like_text(head: &[u8]) -> bool {
    let body = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    match std::str::from_utf8(body) {
        Ok(t) => plain(t),
        // a multi-byte character cut at the end of the head is still text
        Err(e) => e.error_len().is_none() && std::str::from_utf8(&body[..e.valid_up_to()]).is_ok_and(plain),
    }
}

/// Where a JPEG / PNG / GIF really starts when a short wrapper header (a cache container, a
/// download manager's prefix) sits in front of it: the offset within the first 64 bytes, if any.
pub fn embedded_picture_offset(bytes: &[u8]) -> Option<usize> {
    let head = &bytes[..bytes.len().min(80)];
    (1..64.min(head.len())).find(|&i| {
        let t = &head[i..];
        t.starts_with(&[0xFF, 0xD8, 0xFF, 0xE0]) || t.starts_with(&[0xFF, 0xD8, 0xFF, 0xE1]) || t.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) || t.starts_with(b"GIF89a") || t.starts_with(b"GIF87a")
    })
}

/// Why [`prepare_jpeg_checked`] gave no picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepError {
    /// The bytes are not a picture (skip reason code `empty_file` / `not_an_image`).
    NotPicture(&'static str),
    /// The `image` crate could not decode it. `sniff` says what the bytes claim to be; the caller may
    /// try the OS codecs. `too_big` = the decoder hit its memory limit (decode it scaled down instead).
    Undecodable { sniff: Sniff, detail: String, too_big: bool },
}

/// Only the first frame of a GIF, ignoring whatever follows (or breaks later in the stream).
fn gif_first_frame(bytes: &[u8]) -> Result<DynamicImage, String> {
    use image::AnimationDecoder;
    let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let frame = dec.into_frames().next().ok_or_else(|| "no frame".to_string())?.map_err(|e| e.to_string())?;
    Ok(DynamicImage::ImageRgba8(frame.into_buffer()))
}

/// [`prepare_jpeg_long`] that tells "not a picture" from "could not decode", sniffing the content
/// first. GIFs get a lenient first-frame decode.
pub fn prepare_jpeg_checked(bytes: &[u8], long_side: u32) -> Result<Vec<u8>, PrepError> {
    let mut kind = sniff(bytes);
    let mut bytes = bytes;
    if kind == Sniff::Unknown {
        if let Some(off) = embedded_picture_offset(bytes) {
            bytes = &bytes[off..];
            kind = sniff(bytes);
        }
    }
    match kind {
        Sniff::Empty => return Err(PrepError::NotPicture("empty_file")),
        Sniff::Text => return Err(PrepError::NotPicture("not_an_image")),
        _ => {}
    }
    match prepare_jpeg_long(bytes, long_side) {
        Ok(j) => Ok(j),
        Err(first) => {
            if kind == Sniff::Picture(Format::Gif) {
                if let Ok(img) = gif_first_frame(bytes) {
                    let img = canonicalise_to(img, long_side);
                    let mut out = Vec::new();
                    if image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), JPEG_QUALITY).encode_image(&to_rgb_on_white(&img)).is_ok() {
                        return Ok(out);
                    }
                }
            }
            let l = first.to_ascii_lowercase();
            Err(PrepError::Undecodable { sniff: kind, too_big: l.contains("memory limit") || l.contains("limits"), detail: first })
        }
    }
}

/// [`prepare_jpeg`] with another long side.
pub fn prepare_jpeg_long(bytes: &[u8], long_side: u32) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode: {e}"))?;
    let img = canonicalise_to(img, long_side);
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
    fn sniffing_reads_content_not_names() {
        assert_eq!(sniff(&png(4, 4, 1)), Sniff::Picture(Format::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]), Sniff::Picture(Format::Jpeg));
        assert_eq!(sniff(b"GIF89a\x01\x00"), Sniff::Picture(Format::Gif));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Sniff::Picture(Format::Webp));
        assert_eq!(sniff(b"II*\0\x08\0\0\0"), Sniff::OsPicture);
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0"), Sniff::OsPicture);
        assert_eq!(sniff(b""), Sniff::Empty);
        assert_eq!(sniff(&[0u8; 5000]), Sniff::Empty);
        assert_eq!(sniff(b"<!DOCTYPE html><html><body>404</body></html>"), Sniff::Text);
        assert_eq!(sniff(b"\xEF\xBB\xBF<?xml version=\"1.0\"?>"), Sniff::Text);
        assert_eq!(sniff(&[0x13, 0x37, 0xFF, 0x01, 0x02, 0x80, 0x90]), Sniff::Unknown);
    }

    #[test]
    fn non_pictures_are_not_failures() {
        assert_eq!(prepare_jpeg_checked(b"", 768), Err(PrepError::NotPicture("empty_file")));
        assert_eq!(prepare_jpeg_checked(b"<html>Not found</html>", 768), Err(PrepError::NotPicture("not_an_image")));
        assert!(prepare_jpeg_checked(&png(40, 30, 3), 768).is_ok());
        // a picture cut short is undecodable, not "not a picture"
        let cut = &png(300, 300, 3)[..60];
        assert!(matches!(prepare_jpeg_checked(cut, 768), Err(PrepError::Undecodable { sniff: Sniff::Picture(Format::Png), .. })));
        // unknown signature: handed back so the OS codecs get a try
        assert!(matches!(prepare_jpeg_checked(&[0x13, 0x37, 0xFF, 1, 2, 0x80, 0x90], 768), Err(PrepError::Undecodable { sniff: Sniff::Unknown, .. })));
    }

    #[test]
    fn a_picture_behind_a_short_wrapper_header_is_found() {
        let mut wrapped = b"Head\0\0\0\0\xde/\0\0\0\0\0\0".to_vec();
        wrapped.extend_from_slice(&png(30, 20, 7));
        assert_eq!(sniff(&wrapped), Sniff::Unknown);
        assert_eq!(embedded_picture_offset(&wrapped), Some(16));
        let j = prepare_jpeg_checked(&wrapped, 768).expect("decodes from the signature");
        assert_eq!(image::load_from_memory(&j).unwrap().dimensions(), (30, 20));
        assert_eq!(embedded_picture_offset(&png(4, 4, 1)), None, "offset 0 is not a wrapper");
    }

    #[test]
    fn a_gif_with_a_broken_tail_still_gives_its_first_frame() {
        use image::codecs::gif::GifEncoder;
        let frame = |c: u8| image::Frame::new(image::RgbaImage::from_pixel(8, 6, image::Rgba([c, 10, 200, 255])));
        let mut gif = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut gif);
            enc.encode_frame(frame(200)).unwrap();
            enc.encode_frame(frame(20)).unwrap();
        }
        // cut the stream inside the second frame and add junk where the next block should start
        let cut = gif.len() - 12;
        gif.truncate(cut);
        gif.extend_from_slice(&[0x99, 0x99, 0x99]);
        let j = prepare_jpeg_checked(&gif, 768).expect("first frame decodes");
        assert_eq!(image::load_from_memory(&j).unwrap().dimensions(), (8, 6));
        assert_eq!(gif_first_frame(&gif).unwrap().dimensions(), (8, 6));
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
    fn long_side_is_a_parameter() {
        let back = image::load_from_memory(&prepare_jpeg_long(&png(3000, 1500, 2), 1024).unwrap()).unwrap();
        assert_eq!(back.dimensions(), (1024, 512));
        let small = image::load_from_memory(&prepare_jpeg_long(&png(600, 300, 2), 1024).unwrap()).unwrap();
        assert_eq!(small.dimensions(), (600, 300), "never upscale");
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
