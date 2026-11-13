//! HEIC / HEIF through the Windows imaging codec (WIC, classic COM interfaces).
//!
//! The `image` crate cannot decode HEIC and bundling libheif is a patent / size problem, so the
//! file is decoded by the OS codec ("HEIF Image Extensions", installed on most Windows 10/11
//! machines; with the HEVC extension for iPhone photos) and re-encoded as JPEG, which the rest of
//! the pipeline (resize, embed, OCR) already understands. No codec -> `None` -> the file is
//! skipped as `no_heic_codec`.

use std::path::Path;

pub fn is_heic_path(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"))
}

/// The bytes the image pipeline should decode: HEIC/HEIF become JPEG, everything else is unchanged.
/// `None` = a HEIC file the machine's codecs cannot decode.
pub fn normalize(path: &str, bytes: Vec<u8>) -> Option<Vec<u8>> {
    if is_heic_path(path) {
        to_jpeg(&bytes)
    } else {
        Some(bytes)
    }
}

/// Decode any image the OS codecs know and re-encode it as JPEG. Classic synchronous WIC (the
/// WinRT async wrappers crashed the process after a failed decode). EXIF orientation is not applied.
pub fn to_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    // A fresh thread per call: its own COM apartment, whatever mode the caller's thread is in.
    std::thread::scope(|s| s.spawn(|| decode_encode(bytes)).join().ok().flatten())
}

/// Initialise COM for this (fresh) thread, run the conversion, release every interface *before*
/// uninitialising. The WIC stream borrows `bytes` without copying: they outlive it here.
fn decode_encode(bytes: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    // SAFETY: paired with the CoUninitialize below on the same thread (only when init succeeded).
    let inited = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let r = convert(bytes);
    if inited {
        unsafe { CoUninitialize() };
    }
    r
}

fn convert(bytes: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR, IWICImagingFactory, WICBitmapDitherTypeNone,
        WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, STATFLAG_NONAME, STREAM_SEEK_SET};
    use windows::Win32::UI::Shell::SHCreateMemStream;

    // SAFETY: COM calls on interfaces we own, on the thread that created them; every pointer
    // argument points at a live local.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let input = factory.CreateStream().ok()?;
        input.InitializeFromMemory(bytes).ok()?;
        let decoder = factory.CreateDecoderFromStream(&input, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let conv = factory.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat24bppBGR, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let out = SHCreateMemStream(None)?;
        let enc = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()).ok()?;
        enc.Initialize(&out, WICBitmapEncoderNoCache).ok()?;
        let mut fenc = None;
        enc.CreateNewFrame(&mut fenc, std::ptr::null_mut()).ok()?;
        let fenc = fenc?;
        fenc.Initialize(None).ok()?;
        fenc.SetSize(w, h).ok()?;
        let mut fmt = GUID_WICPixelFormat24bppBGR;
        fenc.SetPixelFormat(&mut fmt).ok()?;
        fenc.WriteSource(&conv, std::ptr::null()).ok()?;
        fenc.Commit().ok()?;
        enc.Commit().ok()?;
        let mut stat = std::mem::zeroed();
        out.Stat(&mut stat, STATFLAG_NONAME).ok()?;
        out.Seek(0, STREAM_SEEK_SET, None).ok()?;
        let mut buf = vec![0u8; stat.cbSize as usize];
        let mut read = 0u32;
        out.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut read)).ok().ok()?;
        buf.truncate(read as usize);
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heic_paths() {
        assert!(is_heic_path(r"C:\p\IMG_1.HEIC"));
        assert!(is_heic_path("a.heif"));
        assert!(!is_heic_path("a.heic.bak"));
        assert!(!is_heic_path("a.jpg"));
    }

    #[test]
    fn non_heic_is_untouched_and_garbage_heic_is_refused() {
        assert_eq!(normalize("a.jpg", vec![1, 2, 3]), Some(vec![1, 2, 3]));
        assert_eq!(normalize("a.heic", b"definitely not an image".to_vec()), None);
    }

    /// The codec path itself: a PNG made by the `image` crate goes through WIC and comes back as a JPEG.
    #[test]
    fn wic_round_trip_produces_a_jpeg() {
        let img = image::RgbImage::from_fn(32, 24, |x, y| image::Rgb([(x * 7) as u8, (y * 9) as u8, 90]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let jpeg = to_jpeg(&png).expect("WIC decodes PNG and encodes JPEG");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let back = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((back.width(), back.height()), (32, 24));
    }
}
