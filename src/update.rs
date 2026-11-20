//! Non-blocking check against crates.io, driven from the UI thread.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::Value;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const CRATE_NAME: &str = "ph-tui";

/// Spawn the check. The receiver yields `Some(version)` when crates.io knows
/// the crate, and `None` when it does not (or the network is unavailable).
pub fn spawn_check() -> Receiver<Option<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(latest_version());
    });
    receiver
}

fn latest_version() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("ph-tui/{VERSION} (update check)"))
        .timeout(Duration::from_secs(4))
        .build()
        .ok()?;

    let response = client
        .get(format!("https://crates.io/api/v1/crates/{CRATE_NAME}"))
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }

    let json: Value = response.json().ok()?;
    json.get("crate")?
        .get("max_version")?
        .as_str()
        .map(str::to_string)
}

/// True when `candidate` is a newer semantic version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    version_parts(candidate) > version_parts(current)
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '+'])
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.1.0-beta.1", "0.0.9"));
    }
}
