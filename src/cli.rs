use clap::Parser;

/// A terminal video browser with inline previews, written in Rust.
///
/// Browse categories, search videos, and stream them through mpv without
/// opening a browser.
#[derive(Debug, Parser)]
#[command(name = "ph-tui", version, about, long_about = None)]
pub struct Cli {
    /// Skip the background update check
    #[arg(long)]
    pub no_update_check: bool,
}
