mod app;
mod cli;
mod http;
mod model;
mod player;
mod select;
mod theme;
mod thumb;
mod tui;
mod update;

use std::process::{Command, ExitCode};

use anyhow::{bail, Result};
use clap::Parser;

use crate::app::App;
use crate::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            ratatui::restore();
            eprintln!("ph-tui: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    check_dependencies()?;
    let warning = impersonation_warning();

    let mut terminal = ratatui::init();
    let mut app = App::new(cli.no_update_check, warning);
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

fn check_dependencies() -> Result<()> {
    for (binary, purpose) in [
        ("mpv", "playback"),
        ("yt-dlp", "stream resolution and downloads"),
    ] {
        if !on_path(binary) {
            bail!("{binary} was not found on PATH (needed for {purpose})");
        }
    }
    Ok(())
}

/// Recent yt-dlp releases need `curl_cffi` to impersonate a browser when
/// fetching video pages. Without it every stream resolve fails with a 403,
/// which looks to the user like "mpv never opened". Warn early so the failure
/// is not silent.
fn impersonation_warning() -> Option<String> {
    let output = Command::new("yt-dlp")
        .arg("--list-impersonate-targets")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let available = text
        .lines()
        .any(|line| line.contains("curl_cffi") && !line.contains("unavailable"));
    if available {
        return None;
    }
    Some("yt-dlp lacks browser impersonation (curl_cffi); streams may 403 — see README".to_string())
}

fn on_path(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|directory| {
                std::fs::metadata(directory.join(binary))
                    .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}
