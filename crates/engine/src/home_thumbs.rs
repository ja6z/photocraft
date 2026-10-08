//! Home thumbnails: 360 px PNGs cached in `<data dir>/thumbnails/<fnv1a of the path>.png`.
//!
//! A document's thumbnail is rendered from its composite when it is saved or opened (the shell
//! does that off the UI thread with [`document_job`] and [`write_document_thumbnail`]). Files
//! never opened here get a preview when one is cheap enough to make ([`file_preview_png`]): flat
//! images up to [`PREVIEW_MAX_BYTES`] are decoded and reduced, PSD and PSB files give their merged
//! composite (or, when large or saved without one, their embedded thumbnail, read from the first
//! megabytes only), `.pcraft` files their `thumb.png`. Everything else keeps the placeholder.
//! Thumbnails are a cache: losing one only costs a re-render.
// The web has no files to preview: only the pieces document thumbnails use are reachable there.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use photocraft_doc::Document;

use crate::{EngineError, Result, Session};

/// Longest side of a Home thumbnail, in pixels.
pub const THUMB_PX: u32 = 360;
/// Flat images larger than this get no preview until they're opened (decoding them would take
/// too long and too much memory for a thumbnail).
pub const PREVIEW_MAX_BYTES: u64 = 48 << 20;
/// `.pcraft` files are read whole to find their `thumb.png`: only up to this size.
const PCRAFT_MAX_BYTES: u64 = 64 << 20;
/// How much of a PSD is read to find its embedded thumbnail (header and image resources).
const PSD_HEAD_BYTES: u64 = 16 << 20;
/// PSDs up to this size are read whole for their merged composite (sharper than the embedded
/// 160 px thumbnail); larger ones use the thumbnail.
const PSD_FULL_MAX_BYTES: u64 = 192 << 20;
/// Previews decode at most this many pixels.
const PREVIEW_MAX_PIXELS: u64 = 120_000_000;

/// FNV-1a: a hash that stays the same across builds and platforms, for thumbnail file names.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Where the Home thumbnail of the file at `path` lives (`None` without a data directory).
pub fn thumbnail_path(s: &Session, path: &str) -> Option<PathBuf> {
    let dir = s.home.data_dir.as_ref()?;
    Some(dir.join("thumbnails").join(format!("{:016x}.png", fnv1a(path))))
}

/// The cached thumbnail of `path` if there is one, and whether it is older than the file
/// (`modified`, Unix seconds).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn cached(s: &Session, path: &str, modified: Option<i64>) -> (Option<PathBuf>, bool) {
    let Some(t) = thumbnail_path(s, path) else { return (None, false) };
    let Ok(meta) = std::fs::metadata(&t) else { return (None, false) };
    let made = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok());
    let stale = matches!((made, modified), (Some(made), Some(modified)) if made < modified);
    (Some(t), stale)
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn cached(_s: &Session, _path: &str, _modified: Option<i64>) -> (Option<PathBuf>, bool) {
    (None, false)
}

/// Is there a thumbnail of `path` at least as new as the file?
pub fn is_fresh(s: &Session, path: &str) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok());
        let (thumb, stale) = cached(s, path, modified);
        thumb.is_some() && !stale
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, path);
        false
    }
}

/// Delete the cached thumbnail of `path` (best effort; it's only a cache).
pub(crate) fn remove(s: &Session, path: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(t) = thumbnail_path(s, path) {
        let _ = std::fs::remove_file(t);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (s, path);
}

fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("thumbnail: {e}"))
}

/// RGBA8 pixels as a PNG.
fn encode_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>> {
    let img = photocraft_codecs::Image::from_u8(width, height, photocraft_codecs::ChannelLayout::Rgba, rgba).map_err(other)?;
    let opts = photocraft_codecs::EncodeOptions { png_compression: photocraft_codecs::PngCompression::Fast, ..Default::default() };
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &opts).map_err(other)
}

/// A document's Home thumbnail: its composite, at most 360 px on the longer side, as PNG.
pub fn document_png(doc: &Document) -> Result<Vec<u8>> {
    let img = photocraft_compose::thumbnail(doc, THUMB_PX);
    encode_png(img.width, img.height, img.pixels)
}

/// The open document `index`'s snapshot and where its thumbnail goes; `None` when it has no file
/// yet or there is no data directory.
pub fn document_job(s: &Session, index: usize) -> Option<(Arc<Document>, PathBuf)> {
    let d = s.documents().get(index)?;
    let out = thumbnail_path(s, d.path.as_deref()?)?;
    Some((d.doc.clone(), out))
}

/// Render `doc`'s thumbnail into `out` (the shell runs this off the UI thread).
pub fn write_document_thumbnail(doc: &Document, out: &Path) -> Result<()> {
    write_png(out, &document_png(doc)?)
}

/// Write the thumbnail of the open document `index` now; the path written, or `None` when the
/// document has no file or there is no data directory.
pub fn write_thumbnail(s: &Session, index: usize) -> Result<Option<PathBuf>> {
    let Some((doc, out)) = document_job(s, index) else { return Ok(None) };
    write_document_thumbnail(&doc, &out)?;
    Ok(Some(out))
}

/// Write PNG bytes to a thumbnail path, creating the thumbnails folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_png(out: &Path, png: &[u8]) -> Result<()> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(other)?;
    }
    photocraft_format::atomic_write(out, png).map_err(other)
}
#[cfg(target_arch = "wasm32")]
pub fn write_png(out: &Path, _png: &[u8]) -> Result<()> {
    Err(other(format!("{}: no file system on the web", out.display())))
}

/// Area-average RGBA8 pixels down to at most `max` on the longer side.
fn reduce(width: u32, height: u32, rgba: &[u8], max: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    let longest = w.max(h);
    let max = (max.max(1) as usize).min(longest);
    let (ow, oh) = (((w * max) / longest).max(1), ((h * max) / longest).max(1));
    let mut out = vec![0u8; ow * oh * 4];
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, ((oy + 1) * h / oh).max(oy * h / oh + 1).min(h));
        for ox in 0..ow {
            let (x0, x1) = (ox * w / ow, ((ox + 1) * w / ow).max(ox * w / ow + 1).min(w));
            // Premultiplied sums, so transparent pixels don't darken the edges.
            let mut acc = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    let Some(px) = rgba.get(i..i + 4) else { continue };
                    let a = u64::from(px[3]);
                    acc[0] += u64::from(px[0]) * a;
                    acc[1] += u64::from(px[1]) * a;
                    acc[2] += u64::from(px[2]) * a;
                    acc[3] += a;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as u64;
            let o = (oy * ow + ox) * 4;
            if let Some(dst) = out.get_mut(o..o + 4) {
                for c in 0..3 {
                    if let Some(v) = acc[c].checked_div(acc[3]) {
                        dst[c] = v.min(255) as u8;
                    }
                }
                dst[3] = (acc[3] / n).min(255) as u8;
            }
        }
    }
    (ow as u32, oh as u32, out)
}

/// Decode an encoded image (JPEG, PNG, …) and reduce it to a thumbnail PNG.
fn image_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let limits = photocraft_codecs::Limits { max_pixels: PREVIEW_MAX_PIXELS, ..Default::default() };
    let img = photocraft_codecs::decode_with(bytes, &photocraft_codecs::DecodeOptions { limits, ..Default::default() }).map_err(other)?;
    let rgba = img.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
    let (w, h, px) = reduce(rgba.width(), rgba.height(), rgba.data(), THUMB_PX);
    encode_png(w, h, px)
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// The JPEG of a PSD/PSB's embedded thumbnail (image resource 1036), from the start of the file.
pub(crate) fn psd_thumbnail_jpeg(head: &[u8]) -> Option<&[u8]> {
    if head.get(..4)? != b"8BPS" {
        return None;
    }
    // Header (26 bytes), then the colour mode data and the image resources, each after its length.
    let mode_len = be_u32(head, 26)? as usize;
    let res_at = 30usize.checked_add(mode_len)?;
    let res_len = be_u32(head, res_at)? as usize;
    let mut at = res_at.checked_add(4)?;
    let end = at.checked_add(res_len)?.min(head.len());
    while at.checked_add(12)? <= end {
        if head.get(at..at + 4)? != b"8BIM" {
            return None;
        }
        let id = be_u16(head, at + 4)?;
        // Pascal name, padded so its length byte plus text is even.
        let name_len = usize::from(*head.get(at + 6)?);
        let name_total = (1 + name_len + 1) & !1;
        let size_at = at.checked_add(6 + name_total)?;
        let size = be_u32(head, size_at)? as usize;
        let data_at = size_at.checked_add(4)?;
        let data_end = data_at.checked_add(size)?;
        if id == 1036 {
            // 28-byte header: format 1 is JFIF (RGB).
            let data = head.get(data_at..data_end)?;
            return (be_u32(data, 0)? == 1).then(|| data.get(28..)).flatten();
        }
        at = data_end.checked_add(size & 1)?;
    }
    None
}

/// A PSD/PSB's merged composite as a thumbnail PNG; `None` when the file has no real one
/// (saved without Maximize Compatibility) or can't be read.
pub(crate) fn psd_composite_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let f = photocraft_psd::PsdFile::from_bytes(bytes).ok()?;
    if f.has_real_merged_data() == Some(false) || u64::from(f.header.width) * u64::from(f.header.height) > PREVIEW_MAX_PIXELS {
        return None;
    }
    let img = f.composite_rgba8().ok()?;
    let (w, h, px) = reduce(img.width, img.height, &img.data, THUMB_PX);
    encode_png(w, h, px).ok()
}

/// Read at most `limit` bytes from the start of `path`.
#[cfg(not(target_arch = "wasm32"))]
fn read_head(path: &str, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(limit).read_to_end(&mut out)).map_err(other)?;
    Ok(out)
}

/// A preview PNG for a file never opened here, or `None` when its type has no cheap preview (or
/// it is too large). Reads files, so call it off the UI thread.
#[cfg(not(target_arch = "wasm32"))]
pub fn file_preview_png(path: &str) -> Result<Option<Vec<u8>>> {
    let ext = Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let meta = std::fs::metadata(path).map_err(other)?;
    let size = meta.len();
    match ext.as_str() {
        "psd" | "psb" | "psdt" if meta.is_file() && size <= PSD_FULL_MAX_BYTES => {
            let bytes = read_head(path, PSD_FULL_MAX_BYTES)?;
            match psd_composite_png(&bytes) {
                Some(png) => Ok(Some(png)),
                None => psd_thumbnail_jpeg(&bytes).map(image_png).transpose(),
            }
        }
        "psd" | "psb" | "psdt" => {
            let head = read_head(path, PSD_HEAD_BYTES)?;
            psd_thumbnail_jpeg(&head).map(image_png).transpose()
        }
        "pcraft" if meta.is_file() && size <= PCRAFT_MAX_BYTES => {
            let bytes = read_head(path, PCRAFT_MAX_BYTES)?;
            match photocraft_format::read_thumbnail(&bytes) {
                Ok(Some(png)) => image_png(&png).map(Some),
                _ => Ok(None),
            }
        }
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "tga" | "qoi" | "ico" if meta.is_file() && size <= PREVIEW_MAX_BYTES => {
            image_png(&read_head(path, PREVIEW_MAX_BYTES)?).map(Some)
        }
        _ => Ok(None),
    }
}
#[cfg(target_arch = "wasm32")]
pub fn file_preview_png(path: &str) -> Result<Option<Vec<u8>>> {
    Err(other(format!("{path}: no file system on the web")))
}

/// Make the thumbnail of a file never opened here, if it has a cheap preview: `Ok(true)` when
/// one was written to `out`.
pub fn write_file_preview(path: &str, out: &Path) -> Result<bool> {
    match file_preview_png(path)? {
        Some(png) => write_png(out, &png).map(|()| true),
        None => Ok(false),
    }
}
