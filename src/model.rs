/// A browsable category coming from the webmasters category API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    /// Human readable name, e.g. "Big Tits".
    pub name: String,
}

/// A single video result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Video {
    pub viewkey: String,
    pub title: String,
    /// Listing pages usually carry a thumbnail, which makes the preview pane
    /// instant instead of requiring a second request to the watch page.
    pub preview: Option<String>,
}

/// Build the watch page URL for a bare viewkey.
pub fn watch_url(viewkey: &str) -> String {
    format!("https://www.pornhub.com/view_video.php?viewkey={viewkey}")
}
