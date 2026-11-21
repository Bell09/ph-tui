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

/// Search results (PornHub). The site already ranks matches, so its order
/// is kept as-is. No local title-keyword filtering.
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
    Ok(videos_from_html(&html))
}

/// xHamster search results.
///
/// The site already ranks matches, so its order is kept as-is. Query parameters
/// are passed through reqwest so spaces and punctuation are encoded correctly.
pub fn xhamster_search(query: &str, page: u32) -> Result<Vec<Video>> {
    let url = format!("{BASE_XHAMSTER}/search");
    let response = client()?
        .get(&url)
        .query(&[("q", query), ("page", &page.to_string())])
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        bail!("unexpected HTTP status {} for {url}", response.status());
    }
    let html = response.text().context("failed to read response body")?;
    Ok(xhamster_videos_from_html(&html))
}

const BASE_XHAMSTER: &str = "https://xhamster.desi";

/// Videos from a channel/uploader page. PornHub pages paginate with `?page=N`;
/// xHamster creator, channel, and user pages use a `/N` path suffix.
pub fn channel_videos(url: &str, page: u32) -> Result<Vec<Video>> {
    let html = if url.starts_with(BASE_XHAMSTER) {
        let base = url.trim_end_matches('/');
        let page_url = if page > 1 {
            format!("{base}/{page}")
        } else {
            base.to_string()
        };
        get_html(&page_url)?
    } else {
        get_html(&format!("{url}?page={page}"))?
    };

    if url.starts_with(BASE_XHAMSTER) {
        Ok(xhamster_videos_from_html(&html))
    } else {
        Ok(videos_from_html(&html))
    }
}

/// Extract videos from xHamster listing HTML.
///
/// A video is rendered as two anchors that point at the same watch URL: the
/// thumbnail carries `aria-label` and the image, while the title carries
/// `title`. Dedupe by watch URL and keep whichever copy has the preview.
fn xhamster_videos_from_html(html: &str) -> Vec<Video> {
    let document = Html::parse_document(html);
    let blocks =
        Selector::parse("[data-role=\"thumb-list-block\"]").expect("static selector is valid");
    let links =
        Selector::parse("a[data-role=\"thumb-link\"][href]").expect("static selector is valid");
    let images = Selector::parse("img").expect("static selector is valid");
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut videos: Vec<Video> = Vec::new();
    let mut scoped = false;

    for block in document.select(&blocks) {
        scoped = true;
        collect_xhamster_videos(block, &links, &images, &mut positions, &mut videos);
    }
    if !scoped {
        collect_xhamster_videos(
            document.root_element(),
            &links,
            &images,
            &mut positions,
            &mut videos,
        );
    }

    videos
}

fn collect_xhamster_videos(
    root: ElementRef<'_>,
    links: &Selector,
    images: &Selector,
    positions: &mut HashMap<String, usize>,
    videos: &mut Vec<Video>,
) {
    for link in root.select(links) {
        let Some(href) = link.value().attr("href") else {
            continue;
        };
        let Some(watch_url) = xhamster_watch_url(href) else {
            continue;
        };
        let title = link
            .value()
            .attr("title")
            .or_else(|| link.value().attr("aria-label"))
            .map(|value| decode_entities(value.trim()))
            .filter(|value| !value.is_empty())
            .or_else(|| {
                let text = decode_entities(link.text().collect::<String>().trim());
                (!text.is_empty()).then_some(text)
            });
        let Some(title) = title else {
            continue;
        };
        let preview = preview_from_link(&link, images);
        let channel = channel_from_link(&link, Site::Xhamster);

        if let Some(&index) = positions.get(&watch_url) {
            if videos[index].preview.is_none() {
                videos[index].preview = preview;
            }
            if videos[index].channel_url.is_none() {
                videos[index].channel = channel.as_ref().map(|(name, _)| name.clone());
                videos[index].channel_url = channel.map(|(_, url)| url);
            }
            continue;
        }
        let Some(viewkey) = xhamster_viewkey(&watch_url) else {
            continue;
        };
        positions.insert(watch_url.clone(), videos.len());
        videos.push(Video {
            viewkey,
            title,
            preview,
            watch_url: Some(watch_url),
            channel: channel.as_ref().map(|(name, _)| name.clone()),
            channel_url: channel.map(|(_, url)| url),
        });
    }
}

/// Normalize an xHamster `/videos/<slug>-xh<hash>` link to an absolute watch
/// URL, dropping query strings and tracking fragments.
fn xhamster_watch_url(href: &str) -> Option<String> {
    let href = href.trim();
    let absolute = if let Some(rest) = href
        .strip_prefix("https://")
        .or_else(|| href.strip_prefix("http://"))
    {
        format!("https://{rest}")
    } else if href.starts_with('/') {
        format!("{BASE_XHAMSTER}{href}")
    } else {
        return None;
    };

    let path_start = absolute.find("/videos/")?;
    let path = &absolute[path_start..];
    let end = path.find(['?', '#']).unwrap_or(path.len());
    let path = &path[..end];
    let slug = path.rsplit('/').next()?;
    xhamster_viewkey_from_slug(slug)?;
    Some(format!("{BASE_XHAMSTER}{path}"))
}

/// Use the stable hash from the slug as the display key, e.g. `xhVXyuL`.
fn xhamster_viewkey(watch_url: &str) -> Option<String> {
    let slug = watch_url.rsplit('/').next()?;
    xhamster_viewkey_from_slug(slug)
}

fn xhamster_viewkey_from_slug(slug: &str) -> Option<String> {
    let (_, hash) = slug.rsplit_once("-xh")?;
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!("xh{hash}"))
}

/// Decode the HTML entities that listing pages leave in `title` attributes.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let decoded = after
            .find(';')
            .and_then(|end| entity_char(&after[1..end]).map(|ch| (ch, end)));
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Extract unique, titled video links from a results page.
///
/// Search, category and ordered-feed pages all wrap their real results in a
/// known `<ul>`. Scoping to it keeps the header dropdowns (hottest, recommended,
/// playlists) out of the list. The front page has no single container, so it
/// falls back to a document-wide scan.
pub fn videos_from_html(html: &str) -> Vec<Video> {
    let document = Html::parse_document(html);
    let images = Selector::parse("img").expect("static selector is valid");
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut videos: Vec<Video> = Vec::new();

    for container in ["#videoSearchResult", "#videoCategory"] {
        let selector = Selector::parse(container).expect("static selector is valid");
        if let Some(root) = document.select(&selector).next() {
            collect_videos(root, &images, &mut positions, &mut videos);
            if !videos.is_empty() {
                return videos;
            }
        }
    }

    collect_videos(
        document.root_element(),
        &images,
        &mut positions,
        &mut videos,
    );
    videos
}

fn collect_videos(
    root: ElementRef<'_>,
    images: &Selector,
    positions: &mut HashMap<String, usize>,
    videos: &mut Vec<Video>,
) {
    let selector = Selector::parse("a[href]").expect("static selector is valid");
    for element in root.select(&selector) {
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Some(viewkey) = viewkey_from_href(href) else {
            continue;
        };
        let title = decode_entities(element.value().attr("title").unwrap_or("").trim());
        let preview = preview_from_link(&element, images);
        let channel = channel_from_link(&element, Site::Pornhub);

        if let Some(&index) = positions.get(&viewkey) {
            // The title anchor often follows the thumbnail anchor; keep the
            // first title but fill in an image or channel discovered later.
            if videos[index].preview.is_none() {
                videos[index].preview = preview;
            }
            if videos[index].channel_url.is_none() {
                videos[index].channel = channel.as_ref().map(|(name, _)| name.clone());
                videos[index].channel_url = channel.map(|(_, url)| url);
            }
            continue;
        }
        if title.is_empty() {
            continue;
        }

        positions.insert(viewkey.clone(), videos.len());
        videos.push(Video {
            viewkey,
            title,
            preview,
            watch_url: None,
            channel: channel.as_ref().map(|(name, _)| name.clone()),
            channel_url: channel.map(|(_, url)| url),
        });
    }
}

#[derive(Clone, Copy)]
enum Site {
    Pornhub,
    Xhamster,
}

/// Find the uploader link inside the listing card that contains `link`.
/// PornHub wraps each result in `li.videoblock`; xHamster uses a div with the
/// `video-thumb` / `thumb-list__item` classes.
fn channel_from_link(link: &ElementRef<'_>, site: Site) -> Option<(String, String)> {
    let anchors = Selector::parse("a[href]").expect("static selector is valid");
    let container = link
        .ancestors()
        .filter_map(ElementRef::wrap)
        .take(10)
        .find(|element| match site {
            Site::Pornhub => {
                element.value().name() == "li"
                    && element
                        .value()
                        .classes()
                        .any(|class| class == "videoblock" || class == "videoBox")
            }
            Site::Xhamster => {
                element.value().name() == "div"
                    && element
                        .value()
                        .classes()
                        .any(|class| class == "video-thumb" || class == "thumb-list__item")
            }
        })?;

    for anchor in container.select(&anchors) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let candidate = match site {
            Site::Pornhub => pornhub_channel_url(href),
            Site::Xhamster => xhamster_channel_url(href),
        };
        let Some(url) = candidate else {
            continue;
        };
        let name = anchor_name(&anchor);
        if !name.is_empty() {
            return Some((name, url));
        }
    }
    None
}

fn anchor_name(anchor: &ElementRef<'_>) -> String {
    let title = anchor.value().attr("title").unwrap_or("").trim();
    let raw = if title.is_empty() {
        anchor.text().collect::<String>()
    } else {
        title.to_string()
    };
    decode_entities(raw.trim())
}

fn href_path(href: &str) -> Option<String> {
    let href = href.trim();
    if let Some(rest) = href
        .strip_prefix("http://")
        .or_else(|| href.strip_prefix("https://"))
    {
        let (_, path) = rest.split_once('/')?;
        Some(format!("/{path}"))
    } else if href.starts_with('/') {
        Some(href.to_string())
    } else {
        None
    }
}

fn clean_path(path: &str) -> &str {
    let end = path.find(['?', '#']).unwrap_or(path.len());
    &path[..end]
}

fn pornhub_channel_url(href: &str) -> Option<String> {
    let raw_path = href_path(href)?;
    let path = clean_path(&raw_path);
    let prefix = ["/model/", "/channels/", "/user/", "/pornstar/"]
        .into_iter()
        .find(|prefix| path.starts_with(prefix))?;
    let slug = path[prefix.len()..].split('/').next()?;
    if slug.is_empty() {
        return None;
    }
    Some(format!("https://www.pornhub.com{prefix}{slug}/videos"))
}

fn xhamster_channel_url(href: &str) -> Option<String> {
    let raw_path = href_path(href)?;
    let path = clean_path(&raw_path).trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match segments.as_slice() {
        [kind @ ("creators" | "channels"), slug] => Some(format!("{BASE_XHAMSTER}/{kind}/{slug}")),
        ["users", slug] | ["users", slug, "videos"] => {
            Some(format!("{BASE_XHAMSTER}/users/{slug}/videos"))
        }
        _ => None,
    }
}

fn entity_char(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let code = entity.strip_prefix('#')?;
            let value = if let Some(hex) = code.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                code.parse::<u32>().ok()?
            };
            char::from_u32(value)
        }
    }
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
/// `identifier` is either a bare PornHub viewkey or a full watch URL (used by
/// xHamster). Watch pages are big, so resolved URLs are memoized for the
/// session. The caller runs this off the UI thread.
pub fn preview_url(identifier: &str) -> Result<String> {
    if let Some(url) = preview_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(identifier).cloned())
    {
        return Ok(url);
    }

    let page_url = if identifier.starts_with("http") {
        identifier.to_string()
    } else {
        watch_url(identifier)
    };
    let html = get_html(&page_url)?;
    let url = og_image(&html)
        .with_context(|| format!("watch page for {identifier} has no preview image"))?;

    if let Ok(mut cache) = preview_cache().lock() {
        if cache.len() >= PREVIEW_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(identifier.to_string(), url.clone());
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
    fn extracts_channel_links_from_pornhub_cards() {
        let html = r#"
            <ul id="videoSearchResult">
              <li class="videoblock videoBox">
                <a href="/view_video.php?viewkey=abc" title="First video">
                  <img data-image="https://cdn.example/abc.jpg">
                </a>
                <a href="/model/jane-doe" title="Jane Doe">Jane Doe</a>
                <a href="/view_video.php?viewkey=abc" title="First video">First video</a>
              </li>
              <li class="videoblock videoBox">
                <a href="/view_video.php?viewkey=def" title="Second video">
                  <img data-image="https://cdn.example/def.jpg">
                </a>
                <a href="/channels/fake-hostel" class="bolded">Fake Hostel</a>
              </li>
            </ul>
        "#;
        let videos = videos_from_html(html);
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].channel.as_deref(), Some("Jane Doe"));
        assert_eq!(
            videos[0].channel_url.as_deref(),
            Some("https://www.pornhub.com/model/jane-doe/videos")
        );
        assert_eq!(videos[1].channel.as_deref(), Some("Fake Hostel"));
        assert_eq!(
            videos[1].channel_url.as_deref(),
            Some("https://www.pornhub.com/channels/fake-hostel/videos")
        );
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
    fn extracts_and_dedupes_xhamster_videos() {
        let html = r#"
            <header>
              <a data-role="thumb-link" href="https://xhamster.desi/videos/promo-xhPROMO"
                 aria-label="Header promo"></a>
            </header>
            <div data-role="thumb-list-block">
              <div class="video-thumb">
                <a data-role="thumb-link"
                   href="https://xhamster.desi/videos/first-video-xhABC123?from=search"
                   aria-label="First &amp; best">
                  <img src="data:image/gif;base64,AAAA"
                       srcset="https://cdn.example/first-small.jpg 526w"
                       data-src="https://cdn.example/first.jpg">
                </a>
                <a data-role="thumb-link"
                   href="https://xhamster.desi/videos/first-video-xhABC123"
                   title="First &amp; best">First &amp; best</a>
                <a class="video-uploader__name"
                   href="https://xhamster.desi/creators/first-creator">First Creator</a>
              </div>
              <div class="video-thumb">
                <a data-role="thumb-link" href="/videos/second-video-xhDEF456"
                   title="Second video">
                  <img src="https://cdn.example/second.jpg">
                </a>
                <a class="video-uploader__name"
                   href="https://xhamster.desi/creators/second-creator">Second Creator</a>
              </div>
              <a data-role="thumb-link" href="https://xhamster.desi/categories/big-tits"
                 title="Not a video"></a>
            </div>
        "#;
        let videos = xhamster_videos_from_html(html);
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].viewkey, "xhABC123");
        assert_eq!(videos[0].title, "First & best");
        assert_eq!(
            videos[0].preview.as_deref(),
            Some("https://cdn.example/first.jpg")
        );
        assert_eq!(
            videos[0].watch_url.as_deref(),
            Some("https://xhamster.desi/videos/first-video-xhABC123")
        );
        assert_eq!(videos[0].channel.as_deref(), Some("First Creator"));
        assert_eq!(
            videos[0].channel_url.as_deref(),
            Some("https://xhamster.desi/creators/first-creator")
        );
        assert_eq!(videos[1].viewkey, "xhDEF456");
        assert_eq!(videos[1].title, "Second video");
        assert_eq!(
            videos[1].preview.as_deref(),
            Some("https://cdn.example/second.jpg")
        );
        assert_eq!(
            videos[1].watch_url.as_deref(),
            Some("https://xhamster.desi/videos/second-video-xhDEF456")
        );
        assert_eq!(videos[1].channel.as_deref(), Some("Second Creator"));
        assert_eq!(
            videos[1].channel_url.as_deref(),
            Some("https://xhamster.desi/creators/second-creator")
        );
    }

    #[test]
    fn xhamster_channel_urls_cover_every_uploader_kind() {
        assert_eq!(
            xhamster_channel_url("https://xhamster.desi/creators/priyaqueen"),
            Some("https://xhamster.desi/creators/priyaqueen".to_string())
        );
        assert_eq!(
            xhamster_channel_url("/channels/jerky-girls"),
            Some("https://xhamster.desi/channels/jerky-girls".to_string())
        );
        assert_eq!(
            xhamster_channel_url("https://xhamster.desi/users/asian__sensation/videos"),
            Some("https://xhamster.desi/users/asian__sensation/videos".to_string())
        );
        assert_eq!(
            xhamster_channel_url("/users/asian__sensation"),
            Some("https://xhamster.desi/users/asian__sensation/videos".to_string())
        );
        assert_eq!(
            xhamster_channel_url("https://xhamster.desi/users/xhamster/blog"),
            None
        );
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
    fn keeps_the_search_results_and_ignores_header_dropdowns() {
        // Search pages embed "hottest" and "recommended" dropdown menus above
        // the real results; only `#videoSearchResult` belongs in the list.
        let html = r#"
            <html><body>
              <ul id="hottestMenuSection" class="videos">
                <li><a href="/view_video.php?viewkey=menu1" title="Menu video">x</a></li>
              </ul>
              <ul id="videoSearchResult" class="videos search-video-thumbs">
                <li><a href="/view_video.php?viewkey=real1" title="First result">x</a></li>
                <li><a href="/view_video.php?viewkey=real2" title="Second result">x</a></li>
              </ul>
              <ul id="bottomVideos" class="videos">
                <li><a href="/view_video.php?viewkey=bottom" title="Bottom promo">x</a></li>
              </ul>
            </body></html>
        "#;
        let videos = videos_from_html(html);
        let keys: Vec<&str> = videos.iter().map(|v| v.viewkey.as_str()).collect();
        assert_eq!(keys, ["real1", "real2"]);
    }

    #[test]
    fn decodes_entities_in_titles() {
        let html = r#"
            <ul id="videoCategory" class="videos">
              <a href="/view_video.php?viewkey=abc"
                 title="Tom &amp; Jerry&#039;s &quot;big&quot; scene"></a>
            </ul>
        "#;
        let videos = videos_from_html(html);
        assert_eq!(videos[0].title, "Tom & Jerry's \"big\" scene");
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
        assert!(
            videos.iter().any(|video| video.channel_url.is_some()),
            "listing page carried no channel links"
        );
        // Regression: title keyword filtering used to drop every `pov` hit.
        let pov = search_videos("pov", 1).expect("pov search request");
        assert!(!pov.is_empty(), "pov search returned nothing");
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_xhamster_search() {
        let videos = xhamster_search("big tits", 1).expect("xHamster search request");
        assert!(!videos.is_empty(), "xHamster search returned nothing");
        assert!(
            videos.iter().any(|video| video.preview.is_some()),
            "xHamster listing carried no thumbnails"
        );
        assert!(
            videos.iter().any(|video| video.channel_url.is_some()),
            "xHamster listing carried no creator links"
        );
        assert!(
            videos.iter().all(|video| video
                .watch_url
                .as_deref()
                .is_some_and(|url| url.starts_with("https://xhamster.desi/videos/"))),
            "xHamster results must carry full watch URLs"
        );
    }

    /// Hits the real site; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_channel_videos() {
        let pornhub = channel_videos("https://www.pornhub.com/model/sofi-becker/videos", 1)
            .expect("PornHub channel request");
        assert!(!pornhub.is_empty(), "PornHub channel returned nothing");

        let xhamster = channel_videos("https://xhamster.desi/creators/renatinharj", 1)
            .expect("xHamster creator request");
        assert!(!xhamster.is_empty(), "xHamster creator returned nothing");
        assert!(
            xhamster
                .iter()
                .any(|video| video.watch_url.as_deref().is_some()),
            "xHamster creator results must carry watch URLs"
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
