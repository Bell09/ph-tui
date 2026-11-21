use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{watch_url, Category, FeedKind, Video};

const BASE: &str = "https://www.pornhub.com";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
        .context("failed to build HTTP client")
}

fn get_html(url: &str) -> Result<String> {
    let response = client()?
        .get(url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("unexpected HTTP status {status} for {url}");
    }
    response.text().context("failed to read response body")
}

#[derive(Debug, Deserialize)]
struct CategoryResponse {
    categories: Vec<RawCategory>,
}

#[derive(Debug, Deserialize)]
struct RawCategory {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    category: String,
}

/// Fetch every category from the webmasters API, prettified and sorted.
pub fn categories() -> Result<Vec<Category>> {
    let body = get_html(&format!("{BASE}/webmasters/categories"))?;
    let parsed: CategoryResponse =
        serde_json::from_str(&body).context("unexpected categories API response")?;

    let mut categories: Vec<Category> = parsed
        .categories
        .into_iter()
        .filter_map(|raw| {
            let id = match raw.id {
                Value::String(s) if !s.is_empty() => s,
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            if raw.category.is_empty() {
                return None;
            }
            Some(Category {
                id,
                name: prettify_category(&raw.category),
            })
        })
        .collect();

    categories.sort_by_key(|c| c.name.to_lowercase());
    Ok(categories)
}

/// `"big-tits"` -> `"Big Tits"`.
pub fn prettify_category(raw: &str) -> String {
    raw.replace('-', " ")
        .split(' ')
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize_first(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Videos in a home feed.
///
/// `Recommendations` is the personalized front page (it does not paginate);
/// the rest are the site's own ordering pages, which do. All of them use the
/// same listing parser as search and categories, so previews come for free.
pub fn feed_videos(feed: FeedKind, page: u32) -> Result<Vec<Video>> {
    let url = match feed {
        FeedKind::Recommendations => format!("{BASE}/"),
        FeedKind::Hottest => format!("{BASE}/video?o=ht&page={page}"),
        FeedKind::MostViewed => format!("{BASE}/video?o=mv&page={page}"),
        FeedKind::Newest => format!("{BASE}/video?o=cm&page={page}"),
        FeedKind::TopRated => format!("{BASE}/video?o=tr&page={page}"),
    };
    let html = get_html(&url)?;
    Ok(videos_from_html(&html))
}

/// Videos listed for a category.
pub fn category_videos(category_id: &str, page: u32) -> Result<Vec<Video>> {
    let url = format!("{BASE}/video?c={category_id}&page={page}");
    let html = get_html(&url)?;
    Ok(videos_from_html(&html))
}

/// Search results, filtered so that at least one query keyword appears in the
/// title (the site's search page is loose about relevance).
pub fn search_videos(query: &str, page: u32) -> Result<Vec<Video>> {
    let url = format!("{BASE}/video/search");
    let response = client()?
        .get(&url)
        .query(&[("search", query), ("page", &page.to_string())])
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        bail!("unexpected HTTP status {} for {url}", response.status());
    }
    let html = response.text().context("failed to read response body")?;
    Ok(filter_by_keywords(videos_from_html(&html), query))
}

pub fn filter_by_keywords(videos: Vec<Video>, query: &str) -> Vec<Video> {
    let keywords: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    if keywords.is_empty() {
        return videos;
    }
    videos
        .into_iter()
        .filter(|video| {
            let title = video.title.to_lowercase();
            keywords.iter().any(|keyword| title.contains(keyword))
        })
        .collect()
}

/// Extract unique, titled video links from a results page.
pub fn videos_from_html(html: &str) -> Vec<Video> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").expect("static selector is valid");
    let images = Selector::parse("img").expect("static selector is valid");

    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut videos: Vec<Video> = Vec::new();

    for element in document.select(&selector) {
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Some(viewkey) = viewkey_from_href(href) else {
            continue;
        };
        let title = element.value().attr("title").unwrap_or("").trim();
        let preview = preview_from_link(&element, &images);

        if let Some(&index) = positions.get(&viewkey) {
            // The title anchor often follows the thumbnail anchor; keep the
            // first title but fill in an image discovered later.
            if videos[index].preview.is_none() {
                videos[index].preview = preview;
            }
            continue;
        }
        if title.is_empty() {
            continue;
        }

        positions.insert(viewkey.clone(), videos.len());
        videos.push(Video {
            viewkey,
            title: title.to_string(),
            preview,
        });
    }

    videos
}

/// Most listing anchors wrap their `<img>` and expose the still frame on one
/// of several data attributes. The signed `data-image` URLs are still images
/// even when the path contains `.mp4`.
fn preview_from_link(link: &ElementRef, images: &Selector) -> Option<String> {
    for image in link.select(images) {
        for attribute in ["data-thumb_url", "data-image", "data-src", "src"] {
            let Some(url) = image.value().attr(attribute) else {
                continue;
            };
            let url = url.trim();
            if url.starts_with("http") {
                return Some(url.to_string());
            }
        }
    }
    None
}

fn viewkey_from_href(href: &str) -> Option<String> {
    const MARKER: &str = "view_video.php?viewkey=";
    let start = href.find(MARKER)? + MARKER.len();
    let key: String = href[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    (!key.is_empty()).then_some(key)
}

/// Resolve the preview image URL for a video by reading its watch page.
///
/// Watch pages are big, so resolved URLs are memoized for the session. The
/// caller runs this off the UI thread.
pub fn preview_url(viewkey: &str) -> Result<String> {
    if let Some(url) = preview_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(viewkey).cloned())
    {
        return Ok(url);
    }

    let html = get_html(&watch_url(viewkey))?;
    let url = og_image(&html)
        .with_context(|| format!("watch page for {viewkey} has no preview image"))?;

    if let Ok(mut cache) = preview_cache().lock() {
        if cache.len() >= PREVIEW_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(viewkey.to_string(), url.clone());
    }
    Ok(url)
}

const PREVIEW_CACHE_LIMIT: usize = 64;

fn preview_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pull `og:image` out of a watch page.
pub fn og_image(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    for pattern in [
        "meta[property=\"og:image\"]",
        "meta[name=\"og:image\"]",
        "meta[property=\"og:image:url\"]",
    ] {
        let selector = Selector::parse(pattern).expect("static selector is valid");
        for element in document.select(&selector) {
            let Some(content) = element.value().attr("content") else {
                continue;
            };
            let content = content.trim();
            if !content.is_empty() {
                return Some(content.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prettifies_category_names() {
        assert_eq!(prettify_category("big-tits"), "Big Tits");
        assert_eq!(prettify_category("anal"), "Anal");
        assert_eq!(prettify_category("ebony-and-black"), "Ebony And Black");
        assert_eq!(prettify_category(""), "");
    }

    #[test]
    fn extracts_and_dedupes_videos() {
        let html = r#"
            <html><body>
              <a href="/view_video.php?viewkey=abc123" title="First video">
                <img data-image="https://cdn.example/abc.jpg" src="data:image/png;base64,AAAA">
              </a>
              <a href="/view_video.php?viewkey=abc123" title="First video">dupe</a>
              <a href="/view_video.php?viewkey=def456&pkey=1" title="  Second video  ">two</a>
              <a href="/view_video.php?viewkey=ghi789" title="">untitled</a>
              <a href="/model/whatever">not a video</a>
            </body></html>
        "#;
        let videos = videos_from_html(html);
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].viewkey, "abc123");
        assert_eq!(
            videos[0].preview.as_deref(),
            Some("https://cdn.example/abc.jpg")
        );
        assert_eq!(videos[1].viewkey, "def456");
        assert_eq!(videos[1].title, "Second video");
        assert_eq!(videos[1].preview, None);
    }

    #[test]
    fn thumbnails_are_read_from_listing_markup() {
        let html = r#"
            <a href="/view_video.php?viewkey=one" title="One">
              <img src="data:image/gif;base64,AAAA"
                   data-thumb_url="https://cdn.example/one.jpg"
                   data-image="https://cdn.example/one-frame.jpg">
            </a>
            <a href="/view_video.php?viewkey=two" title="Two">
              <img data-image="https://cdn.example/two.mp4/rs:fit:320:180">
            </a>
            <a href="/view_video.php?viewkey=three" title="Three"></a>
        "#;
        let videos = videos_from_html(html);
        assert_eq!(
            videos[0].preview.as_deref(),
            Some("https://cdn.example/one.jpg")
        );
        assert_eq!(
            videos[1].preview.as_deref(),
            Some("https://cdn.example/two.mp4/rs:fit:320:180")
        );
        assert_eq!(videos[2].preview, None);
    }

    #[test]
    fn viewkeys_stop_at_non_alphanumeric_characters() {
        assert_eq!(
            viewkey_from_href("/view_video.php?viewkey=abc-123&x=1"),
            Some("abc".to_string())
        );
        assert_eq!(viewkey_from_href("/view_video.php?viewkey="), None);
    }

    #[test]
    fn search_filter_keeps_titles_with_a_keyword() {
        let videos = vec![
            Video {
                viewkey: "a".into(),
                title: "Big Tits Compilation".into(),
                preview: None,
            },
            Video {
                viewkey: "b".into(),
                title: "Something unrelated".into(),
                preview: None,
            },
            Video {
                viewkey: "c".into(),
                title: "Amateur BIG scene".into(),
                preview: None,
            },
        ];
        let filtered = filter_by_keywords(videos, "big amateur");
        let keys: Vec<&str> = filtered.iter().map(|v| v.viewkey.as_str()).collect();
        assert_eq!(keys, ["a", "c"]);
    }

    #[test]
    fn finds_open_graph_images() {
        let html = r#"
            <html><head>
              <meta property="og:title" content="A video" />
              <meta property="og:image" content="https://cdn.example/one.jpg" />
              <meta property="og:image:width" content="640" />
            </head></html>
        "#;
        assert_eq!(
            og_image(html).as_deref(),
            Some("https://cdn.example/one.jpg")
        );

        let reversed = r#"<meta content="https://cdn.example/two.jpg" property="og:image">"#;
        assert_eq!(
            og_image(reversed).as_deref(),
            Some("https://cdn.example/two.jpg")
        );

        assert_eq!(og_image("<html><head></head></html>"), None);
        assert_eq!(og_image(r#"<meta property="og:image" content="  ">"#), None);
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_home_feeds() {
        for feed in FeedKind::ALL {
            let videos = feed_videos(feed, 1).expect("home feed request");
            assert!(!videos.is_empty(), "{} was empty", feed.label());
            assert!(
                videos.iter().any(|video| video.preview.is_some()),
                "{} carried no thumbnails",
                feed.label()
            );
        }
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_categories() {
        let categories = categories().expect("categories request");
        assert!(!categories.is_empty());
        assert!(categories.iter().any(|c| c.name == "Big Tits"));
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_category_videos() {
        let videos = category_videos("37", 1).expect("video request");
        assert!(!videos.is_empty());
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_search() {
        let videos = search_videos("big tits", 1).expect("search request");
        assert!(!videos.is_empty());
        assert!(
            videos.iter().any(|video| video.preview.is_some()),
            "listing page carried no thumbnails"
        );
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_preview_url() {
        let videos = search_videos("big tits", 1).expect("search request");
        let video = videos.first().expect("at least one result");
        let url = preview_url(&video.viewkey).expect("preview url");
        assert!(url.starts_with("http"), "unexpected preview url: {url}");
    }
}
