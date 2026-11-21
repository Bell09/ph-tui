/// A browsable category coming from the webmasters category API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    /// Human readable name, e.g. "Big Tits".
    pub name: String,
}

/// A home feed shown on the landing screen.
///
/// These mirror the site's own front-page and video-ordering pages rather
/// than being local lists, so switching feeds always fetches real videos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    /// The personalized front page (recommendations).
    Recommendations,
    Hottest,
    MostViewed,
    Newest,
    TopRated,
}

impl FeedKind {
    /// Order matters: Tab cycles through this list.
    pub const ALL: [FeedKind; 5] = [
        FeedKind::Recommendations,
        FeedKind::Hottest,
        FeedKind::MostViewed,
        FeedKind::Newest,
        FeedKind::TopRated,
    ];

    /// Single-key shortcut drawn in the feed bar.
    pub fn key(self) -> &'static str {
        match self {
            FeedKind::Recommendations => "r",
            FeedKind::Hottest => "h",
            FeedKind::MostViewed => "v",
            FeedKind::Newest => "n",
            FeedKind::TopRated => "t",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FeedKind::Recommendations => "Recommendations",
            FeedKind::Hottest => "Hottest",
            FeedKind::MostViewed => "Most Viewed",
            FeedKind::Newest => "Newest",
            FeedKind::TopRated => "Top Rated",
        }
    }

    /// The front page does not paginate; every other feed does.
    pub fn paginated(self) -> bool {
        self != FeedKind::Recommendations
    }
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
