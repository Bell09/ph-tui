//! Playback, downloads and browser hand-off, all through external tools.
//!
//! Playback hands the terminal to `mpv`. Downloads stay in the background and
//! report progress over a channel, so the TUI never leaves the alternate screen.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::model::watch_url;

/// Progress updates from a background `yt-dlp` download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadEvent {
    Progress(u8),
    Done(bool),
}

/// Stream a video through mpv, which resolves the stream with yt-dlp itself.
///
/// The alternate screen stays active and mpv's output is silenced, so only the
/// mpv window is visible. `--force-window=yes` makes it appear immediately.
pub fn play(identifier: &str) -> bool {
    let url = source_url(identifier);

    Command::new("mpv")
        .args([
            "--force-window=yes",
            "--really-quiet",
            "--profile=fast",
            "--hwdec=auto",
            "--cache=yes",
            "--cache-secs=10",
            "--ytdl-format=bestvideo[height<=320]+bestaudio/best[height<=320]",
            "--ytdl-raw-options=cookies-from-browser=firefox",
        ])
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Start a download without touching the terminal. Progress and completion are
/// delivered back to the TUI through the returned channel.
pub fn spawn_download(identifier: &str, quality: u8) -> Result<Receiver<DownloadEvent>, String> {
    let url = source_url(identifier);
    let directory = download_dir();
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;

    let format = match quality {
        2 => "bestvideo[height<=720]+bestaudio/best[height<=720]",
        3 => "bestvideo[height<=480]+bestaudio/best[height<=480]",
        4 => "bestvideo[height<=360]+bestaudio/best[height<=360]",
        _ => "bestvideo+bestaudio/best",
    };
    let output_template = format!("{}/%(title)s.%(ext)s", directory.display());

    let mut child = Command::new("yt-dlp")
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
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not launch yt-dlp: {error}"))?;

    let (tx, rx) = mpsc::channel();
    let mut stderr = child.stderr.take().expect("stderr is piped");
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut line: Vec<u8> = Vec::new();
        let mut last = None;

        // yt-dlp rewrites progress with carriage returns, so split on both CR
        // and LF instead of waiting for a newline that may never arrive.
        loop {
            let read = match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            for byte in &chunk[..read] {
                if *byte == b'\r' || *byte == b'\n' {
                    emit_progress(&line, &mut last, &tx);
                    line.clear();
                } else {
                    line.push(*byte);
                }
            }
        }
        emit_progress(&line, &mut last, &tx);

        let ok = child.wait().is_ok_and(|status| status.success());
        let _ = tx.send(DownloadEvent::Done(ok));
    });

    Ok(rx)
}

fn emit_progress(line: &[u8], last: &mut Option<u8>, tx: &mpsc::Sender<DownloadEvent>) {
    let text = String::from_utf8_lossy(line);
    let Some(percent) = parse_percent(&text) else {
        return;
    };
    if *last != Some(percent) {
        let _ = tx.send(DownloadEvent::Progress(percent));
        *last = Some(percent);
    }
}

fn parse_percent(line: &str) -> Option<u8> {
    let sign = line.find('%')?;
    let token = line[..sign].split_whitespace().last()?;
    token
        .parse::<f32>()
        .ok()
        .map(|value| value.round().clamp(0.0, 100.0) as u8)
}

/// Open the watch page in the system browser.
pub fn open_in_browser(identifier: &str) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };

    Command::new(opener)
        .arg(source_url(identifier))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Accept either a bare viewkey or a full watch URL. xHamster results carry
/// their own URL; PornHub results only need the viewkey.
fn source_url(identifier: &str) -> String {
    if identifier.starts_with("http") {
        identifier.to_string()
    } else {
        watch_url(identifier)
    }
}

fn download_dir() -> PathBuf {
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ph-tui")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ytdlp_progress_lines() {
        assert_eq!(parse_percent("[download]  12.3% of 10.00MiB"), Some(12));
        assert_eq!(parse_percent("[download] 100% of 10.00MiB"), Some(100));
        assert_eq!(parse_percent("[download] Downloading item 1 of 2"), None);
    }
}
