//! Preview images: terminal graphics detection, download, cache and decode.
//!
//! Every preview is normalized to one fixed pixel size so the sidebar draws
//! artwork at a consistent shape no matter what the site serves. The size is
//! deliberately small: the TUI renders into a sidebar, so decoding a 1080p
//! still only wastes memory and bandwidth. The protocol (kitty, sixel, iTerm2,
//! or unicode half-blocks) is chosen by `ratatui-image` from the terminal.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use image::DynamicImage;
use ratatui_image::picker::Picker;

/// Preview images are scaled to this box before rendering. 320x180 is ample
/// for a sidebar and keeps the decoded RGBA buffer around 220 KiB per image.
pub const PREVIEW_WIDTH: u32 = 320;
pub const PREVIEW_HEIGHT: u32 = 180;

/// Keep the disk cache bounded by both entry count and total size. A corrupt
/// or stale cache file is replaced on the next load instead of failing forever.
const MAX_CACHE_FILES: usize = 160;
const MAX_CACHE_BYTES: u64 = 24 * 1024 * 1024;
/// A preview larger than this is almost certainly a full-size still the site
/// served by mistake; refusing it keeps a single selection from spiking RSS.
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024;
const DOWNLOAD_ATTEMPTS: usize = 2;

const PORNHUB_REFERRER: &str = "https://www.pornhub.com/";
const XHAMSTER_REFERRER: &str = "https://xhamster.desi/";

/// Ask the terminal which image protocol and font size it supports, falling
/// back to unicode half-blocks on anything that does not answer.
pub fn picker() -> Picker {
    use std::io::IsTerminal;

    let dumb = std::env::var("TERM")
        .map(|term| term == "dumb" || term == "linux")
        .unwrap_or(false);
    if dumb || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Picker::halfblocks();
    }
    Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
}

#[cfg(test)]
pub fn halfblocks_picker() -> Picker {
    Picker::halfblocks()
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ph-tui")
        .join("previews")
}

/// One cache file per video key. Keys come from the site and are alphanumeric,
/// but sanitize anyway so a malformed page can never steer the path.
pub fn cache_path(viewkey: &str) -> PathBuf {
    let safe: String = viewkey
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    cache_dir().join(format!("{safe}.img"))
}

/// Read a cached preview or download it, then decode and normalize it.
///
/// Blocking on purpose: callers run this on a worker thread while the UI
/// keeps rendering and polling for the result.
pub fn load(url: &str, cache_path: &Path) -> Result<DynamicImage> {
    if let Some(bytes) = cached_bytes(cache_path) {
        if let Some(image) = decode_preview(&bytes) {
            return Ok(normalize(image));
        }
        // A truncated download must not poison the cache permanently.
        let _ = fs::remove_file(cache_path);
    }

    let bytes = download(url)?;
    if bytes.is_empty() {
        bail!("preview response was empty for {url}");
    }
    write_cache(cache_path, &bytes);
    if let Some(parent) = cache_path.parent() {
        prune(parent, MAX_CACHE_FILES, MAX_CACHE_BYTES);
    }

    let image = decode_preview(&bytes).context("could not decode preview image")?;
    Ok(normalize(image))
}

fn cached_bytes(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

fn decode_preview(bytes: &[u8]) -> Option<DynamicImage> {
    image::load_from_memory(bytes).ok()
}

/// Center-crop to 16:9 and scale to the fixed preview size.
fn normalize(image: DynamicImage) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return image;
    }

    let target = PREVIEW_WIDTH as f64 / PREVIEW_HEIGHT as f64;
    let current = width as f64 / height as f64;
    let (crop_width, crop_height) = if current > target {
        let w = (height as f64 * target).round() as u32;
        (w.min(width), height)
    } else {
        let h = (width as f64 / target).round() as u32;
        (width, h.min(height))
    };

    let x = (width - crop_width) / 2;
    let y = (height - crop_height) / 2;
    image.crop_imm(x, y, crop_width, crop_height).resize_exact(
        PREVIEW_WIDTH,
        PREVIEW_HEIGHT,
        image::imageops::FilterType::Triangle,
    )
}

/// Write through a temporary file so an interrupted run can never leave a
/// half-written image that later looks like a valid cache hit.
fn write_cache(path: &Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("{}.{nonce}.tmp", std::process::id()));
    if fs::write(&tmp, bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
    let _ = fs::remove_file(&tmp);
}

/// CDNs often reject image requests without a page referer. Retrying with a
/// no-cache header also recovers signed URLs that were briefly unavailable.
fn download(url: &str) -> Result<Vec<u8>> {
    let mut last = anyhow!("preview request failed for {url}");
    for attempt in 0..DOWNLOAD_ATTEMPTS {
        match fetch_once(url, attempt > 0) {
            Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
            Ok(_) => last = anyhow!("preview response was empty for {url}"),
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn fetch_once(url: &str, retry: bool) -> Result<Vec<u8>> {
    let mut request = crate::http::client()?
        .get(url)
        .header(
            "Accept",
            "image/avif,image/webp,image/apng,image/*,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Referer", referer_for(url));
    if retry {
        request = request
            .header("Cache-Control", "no-cache")
            .header("Pragma", "no-cache");
    }

    let response = request
        .send()
        .with_context(|| format!("could not fetch preview {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("preview request failed for {url} with HTTP {status}");
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        bail!("preview is unexpectedly large for {url}");
    }
    let bytes = response.bytes().context("could not read preview body")?;
    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        bail!("preview body is unexpectedly large for {url}");
    }
    Ok(bytes.to_vec())
}

fn referer_for(url: &str) -> &'static str {
    if url.contains("xhamster") || url.contains("xhcdn") {
        XHAMSTER_REFERRER
    } else {
        PORNHUB_REFERRER
    }
}

/// Drop the oldest files until both the entry count and byte budget are met.
fn prune(directory: &Path, max_files: usize, max_bytes: u64) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension != "img") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(UNIX_EPOCH), path, meta.len()))
        })
        .collect();

    let mut count = files.len();
    let mut bytes: u64 = files.iter().map(|(_, _, size)| *size).sum();
    if count <= max_files && bytes <= max_bytes {
        return;
    }

    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, size) in files {
        if count <= max_files && bytes <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            count -= 1;
            bytes = bytes.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn image(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            width,
            height,
            Rgba([30u8, 40, 60, 255]),
        ))
    }

    #[test]
    fn every_source_lands_on_the_same_size() {
        for (width, height) in [
            (320u32, 180u32),
            (640, 360),
            (720, 404),
            (640, 480),
            (300, 600),
        ] {
            let out = normalize(image(width, height));
            assert_eq!(out.width(), PREVIEW_WIDTH, "width for {width}x{height}");
            assert_eq!(out.height(), PREVIEW_HEIGHT, "height for {width}x{height}");
        }
    }

    #[test]
    fn degenerate_images_are_left_alone() {
        let out = normalize(image(0, 0));
        assert_eq!(out.width(), 0);
        assert_eq!(out.height(), 0);
    }

    #[test]
    fn cache_paths_are_sanitized() {
        let path = cache_path("../../etc/passwd");
        assert_eq!(path.extension().unwrap(), "img");
        assert!(!path.to_string_lossy().contains(".."));
    }

    #[test]
    fn corrupt_cached_bytes_are_rejected() {
        assert!(decode_preview(b"not an image").is_none());
        assert!(decode_preview(&[]).is_none());
    }

    #[test]
    fn download_referer_follows_the_source_site() {
        assert_eq!(referer_for("https://cdn.example/a.jpg"), PORNHUB_REFERRER);
        assert_eq!(
            referer_for("https://ic-vt-nss.xhcdn.com/a/b.webp"),
            XHAMSTER_REFERRER
        );
        assert_eq!(
            referer_for("https://xhamster.desi/thumb.jpg"),
            XHAMSTER_REFERRER
        );
    }

    #[test]
    fn prune_bounds_total_bytes() {
        let directory = std::env::temp_dir().join(format!("ph-tui-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        for index in 0..3 {
            fs::write(directory.join(format!("{index}.img")), vec![0u8; 10]).unwrap();
        }
        fs::write(directory.join("keep.tmp"), vec![0u8; 10]).unwrap();

        prune(&directory, 10, 15);

        let files: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "img"))
            .collect();
        let total: u64 = files
            .iter()
            .filter_map(|entry| entry.metadata().ok().map(|meta| meta.len()))
            .sum();
        assert!(files.len() <= 2, "left {} cache files", files.len());
        assert!(total <= 15, "left {total} cached bytes");
        assert!(directory.join("keep.tmp").exists());
        fs::remove_dir_all(&directory).unwrap();
    }
}
