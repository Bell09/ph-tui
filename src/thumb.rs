//! Preview images: terminal graphics detection, download, cache and decode.
//!
//! Every preview is normalized to one fixed pixel size so the sidebar draws
//! artwork at a consistent shape no matter what the site serves. The actual
//! protocol (kitty, sixel, iTerm2, or unicode half-blocks) is chosen by
//! `ratatui-image` from what the terminal reports.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use image::DynamicImage;
use ratatui_image::picker::Picker;

/// Preview images are scaled to this box before rendering.
pub const PREVIEW_WIDTH: u32 = 480;
pub const PREVIEW_HEIGHT: u32 = 270;

/// Keep the cache bounded; the oldest files are dropped first.
const MAX_CACHE_FILES: usize = 200;

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";

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
    let bytes = match fs::read(cache_path) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        _ => {
            let bytes = download(url)?;
            if let Some(parent) = cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(cache_path, &bytes);
            if let Some(parent) = cache_path.parent() {
                prune(parent, MAX_CACHE_FILES);
            }
            bytes
        }
    };

    let image = image::load_from_memory(&bytes).context("could not decode preview image")?;
    Ok(normalize(image))
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

fn client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let built = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
        .context("could not build the image HTTP client")?;
    Ok(CLIENT.get_or_init(|| built))
}

fn download(url: &str) -> Result<Vec<u8>> {
    let response = client()?
        .get(url)
        .header("Accept", "image/*")
        .send()
        .with_context(|| format!("could not fetch preview {url}"))?
        .error_for_status()
        .with_context(|| format!("preview request failed for {url}"))?;
    let bytes = response.bytes().context("could not read preview body")?;
    Ok(bytes.to_vec())
}

/// Drop the oldest files so the cache stays under `max` entries.
fn prune(directory: &Path, max: usize) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let meta = entry.metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), path))
        })
        .collect();

    if files.len() <= max {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    for (_, path) in files.iter().take(files.len() - max) {
        let _ = fs::remove_file(path);
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
            (480u32, 270u32),
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
}
