//! Playback, downloads and browser hand-off, all through external tools.
//!
//! Nothing here talks to the TUI: the caller restores the terminal before
//! invoking these and re-enters the alternate screen afterwards.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::watch_url;

/// Stream a video through mpv, which resolves the stream with yt-dlp itself.
///
/// `mpv` is launched without `--really-quiet` so that stream-resolution errors
/// (for example an expired yt-dlp extractor) are visible instead of the player
/// exiting silently. `--force-window=yes` makes the window appear immediately,
/// giving feedback while yt-dlp resolves the URL.
pub fn play(title: &str, viewkey: &str) -> bool {
    let url = watch_url(viewkey);

    println!();
    println!("  ▶ {title}");
    println!("  opening mpv · press q there to come back");
    println!();

    let started = Instant::now();
    let status = Command::new("mpv")
        .args([
            "--force-window=yes",
            "--profile=fast",
            "--hwdec=auto",
            "--cache=yes",
            "--cache-secs=10",
            "--ytdl-format=bestvideo[height<=320]+bestaudio/best[height<=320]",
            "--ytdl-raw-options=cookies-from-browser=firefox",
        ])
        .arg(&url)
        .status();

    match status {
        Err(error) => {
            eprintln!("  could not launch mpv: {error}");
            wait(Duration::from_secs(2));
            false
        }
        Ok(_) => started.elapsed() >= Duration::from_secs(5),
    }
}

/// Download through yt-dlp into `$XDG_VIDEOS_DIR/ph-tui`.
pub fn download(title: &str, viewkey: &str, quality: u8) -> bool {
    let url = watch_url(viewkey);
    let directory = download_dir();

    if let Err(error) = fs::create_dir_all(&directory) {
        eprintln!("  could not create {}: {error}", directory.display());
        wait(Duration::from_secs(2));
        return false;
    }

    let (format, label) = match quality {
        2 => ("bestvideo[height<=720]+bestaudio/best[height<=720]", "720p"),
        3 => ("bestvideo[height<=480]+bestaudio/best[height<=480]", "480p"),
        4 => ("bestvideo[height<=360]+bestaudio/best[height<=360]", "360p"),
        _ => ("bestvideo+bestaudio/best", "best"),
    };

    println!();
    println!("  ↓ {title}");
    println!("  quality: {label}");
    println!("  into:    {}", directory.display());
    println!();

    let output_template = format!("{}/%(title)s.%(ext)s", directory.display());
    let status = Command::new("yt-dlp")
        .args([
            "--cookies-from-browser=firefox",
            "-f",
            format,
            "-o",
            &output_template,
            "--no-playlist",
            "--progress",
            &url,
        ])
        .status();

    match status {
        Ok(status) if status.success() => {
            println!();
            println!("  download complete · {}", directory.display());
            wait(Duration::from_millis(800));
            true
        }
        _ => {
            println!();
            println!("  download failed · the video may be unavailable or restricted");
            wait(Duration::from_secs(2));
            false
        }
    }
}

/// Open the watch page in the system browser.
pub fn open_in_browser(viewkey: &str) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };

    Command::new(opener)
        .arg(watch_url(viewkey))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn download_dir() -> PathBuf {
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ph-tui")
}

fn wait(duration: Duration) {
    std::thread::sleep(duration);
}
