//! The interactive application: navigation, background loading, and drawing.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Result};
use image::DynamicImage;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;
use unicode_width::UnicodeWidthChar;

type Term = ratatui::DefaultTerminal;

use crate::http;
use crate::model::{watch_url, Category, FeedKind, Video};
use crate::player;
use crate::select::{Item, Kind, List};
use crate::theme::Theme;
use crate::thumb;
use crate::tui::{self, Key};
use crate::update;

const MIN_WIDTH: u16 = 56;
const MIN_HEIGHT: u16 = 16;
const POLL: Duration = Duration::from_millis(100);
const THUMB_DEBOUNCE: Duration = Duration::from_millis(180);
const TOAST_TTL: Duration = Duration::from_secs(6);
const THEME_POLL: Duration = Duration::from_secs(1);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

enum Nav {
    Browse,
    Search,
    Update,
    Quit,
    Xhamster,
}

enum UiEvent {
    Select(usize),
    Back,
    Ctrl(char),
}

#[derive(Clone, Copy)]
enum Tone {
    Info,
    Success,
    Error,
}

struct Toast {
    text: String,
    tone: Tone,
    at: Instant,
}

enum UpdateState {
    Checking,
    Disabled,
    NoRelease,
    Current(String),
    Newer(String),
}

#[derive(Clone)]
enum Source {
    Category(Category),
    Search(String),
    Xhamster { query: String },
}

impl Source {
    fn crumb(&self) -> String {
        match self {
            Source::Category(category) => format!("Categories · {}", category.name),
            Source::Search(query) => format!("Search · {query}"),
            Source::Xhamster { query } => format!("XHamster · {query}"),
        }
    }

    fn panel_title(&self) -> &'static str {
        match self {
            Source::Category(_) => "Videos",
            Source::Search(_) => "Results",
            Source::Xhamster { .. } => "XHamster Results",
        }
    }

    fn fetch(&self, page: u32) -> Result<Vec<Video>> {
        match self {
            Source::Category(category) => http::category_videos(&category.id, page),
            Source::Search(query) => http::search_videos(query, page),
            Source::Xhamster { query } => http::xhamster_search(query, page),
        }
    }
}

/// Resolve the identifier a selected item should hand to playback, previews
/// and the browser. xHamster items carry a full URL; PornHub items use a key.
fn item_watch_url(item: &Item) -> String {
    item.url.clone().unwrap_or_else(|| watch_url(&item.key))
}

enum ThumbState {
    Idle,
    Loading,
    Ready(Box<StatefulProtocol>),
    Failed,
}

struct Thumb {
    /// Viewkey of the current selection.
    target: Option<String>,
    state: ThumbState,
    /// Selection changed recently; wait for it to settle before fetching.
    pending: Option<(String, Option<String>, Instant)>,
    rx: Option<(String, Receiver<Result<DynamicImage>>)>,
}

impl Thumb {
    fn new() -> Self {
        Self {
            target: None,
            state: ThumbState::Idle,
            pending: None,
            rx: None,
        }
    }

    fn reset(&mut self) {
        self.target = None;
        self.state = ThumbState::Idle;
        self.pending = None;
        self.rx = None;
    }

    /// Point the preview at the current selection. Jumping through a list
    /// quickly only fetches the preview for wherever the cursor settles.
    fn sync(&mut self, item: Option<&Item>) {
        let target = match item {
            Some(item) if item.kind == Kind::Video => Some(item_watch_url(item)),
            _ => None,
        };
        if self.target == target {
            return;
        }

        let preview = item.and_then(|item| item.image.clone());
        self.target = target;
        self.state = if self.target.is_some() {
            ThumbState::Loading
        } else {
            ThumbState::Idle
        };
        self.rx = None;
        self.pending = self
            .target
            .clone()
            .map(|key| (key, preview, Instant::now()));
    }

    fn poll(&mut self, picker: &Picker) {
        if self.rx.is_none() {
            if let Some((key, preview, at)) = &self.pending {
                if at.elapsed() >= THUMB_DEBOUNCE {
                    let key = key.clone();
                    let preview = preview.clone();
                    self.pending = None;

                    let (tx, rx) = mpsc::channel();
                    let worker = key.clone();
                    thread::spawn(move || {
                        let url = match preview {
                            Some(url) => Ok(url),
                            None => http::preview_url(&worker),
                        };
                        let result =
                            url.and_then(|url| thumb::load(&url, &thumb::cache_path(&worker)));
                        let _ = tx.send(result);
                    });
                    self.rx = Some((key, rx));
                }
            }
        }

        let received = self
            .rx
            .as_ref()
            .map(|(key, rx)| (key.clone(), rx.try_recv()));
        match received {
            Some((key, Ok(Ok(image)))) => {
                if self.target.as_deref() == Some(key.as_str()) {
                    self.state = ThumbState::Ready(Box::new(picker.new_resize_protocol(image)));
                }
                self.rx = None;
            }
            Some((key, Ok(Err(_)))) | Some((key, Err(TryRecvError::Disconnected))) => {
                if self.target.as_deref() == Some(key.as_str()) {
                    self.state = ThumbState::Failed;
                }
                self.rx = None;
            }
            Some((_, Err(TryRecvError::Empty))) | None => {}
        }
    }
}

pub struct App {
    picker: Picker,
    thumb: Thumb,
    theme: Theme,
    /// Palette file being followed, plus its mtime: lets `poll_theme` notice
    /// a wallpaper change with a single `stat()` in the steady state.
    theme_src: Option<PathBuf>,
    theme_mtime: Option<SystemTime>,
    theme_poll_at: Instant,
    update: UpdateState,
    update_rx: Option<Receiver<Option<String>>>,
    toast: Option<Toast>,
    last_query: String,
    visible_rows: usize,
}

impl App {
    pub fn new(no_update_check: bool, warning: Option<String>) -> Self {
        Self::with_picker(no_update_check, thumb::picker(), warning)
    }

    fn with_picker(no_update_check: bool, picker: Picker, warning: Option<String>) -> Self {
        let (update, update_rx) = if no_update_check {
            (UpdateState::Disabled, None)
        } else {
            (UpdateState::Checking, Some(update::spawn_check()))
        };
        let toast = warning.map(|text| Toast {
            text,
            tone: Tone::Error,
            at: Instant::now(),
        });
        let (theme, theme_src) = match Theme::load_with_source() {
            Some((theme, path)) => (theme, Some(path)),
            None => (Theme::default(), None),
        };
        let theme_mtime = theme_src
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok());
        Self {
            picker,
            thumb: Thumb::new(),
            theme,
            theme_src,
            theme_mtime,
            theme_poll_at: Instant::now(),
            update,
            update_rx,
            toast,
            last_query: String::new(),
            visible_rows: 10,
        }
    }

    #[cfg(test)]
    fn headless() -> Self {
        Self::with_picker(true, thumb::halfblocks_picker(), None)
    }

    pub fn run(&mut self, term: &mut Term) -> Result<()> {
        loop {
            match self.home(term)? {
                Nav::Browse => self.browse_categories(term)?,
                Nav::Search => self.search(term)?,
                Nav::Xhamster => self.xhamster(term)?,
                Nav::Update => self.update_screen(term)?,
                Nav::Quit => break,
            }
        }
        Ok(())
    }

    fn quit(&self) -> ! {
        ratatui::restore();
        std::process::exit(0)
    }

    fn set_toast(&mut self, text: impl Into<String>, tone: Tone) {
        self.toast = Some(Toast {
            text: text.into(),
            tone,
            at: Instant::now(),
        });
    }

    fn active_toast(&self) -> Option<&Toast> {
        self.toast
            .as_ref()
            .filter(|toast| toast.at.elapsed() < TOAST_TTL)
    }

    fn poll_update(&mut self) {
        let received = self.update_rx.as_ref().map(Receiver::try_recv);
        match received {
            Some(Ok(Some(version))) => {
                self.update = if update::is_newer(&version, update::VERSION) {
                    UpdateState::Newer(version)
                } else {
                    UpdateState::Current(version)
                };
                self.update_rx = None;
            }
            Some(Ok(None)) => {
                self.update = UpdateState::NoRelease;
                self.update_rx = None;
            }
            Some(Err(TryRecvError::Empty)) | None => {}
            Some(Err(TryRecvError::Disconnected)) => {
                self.update = UpdateState::NoRelease;
                self.update_rx = None;
            }
        }
    }

    /// Watch the followed palette file for edits, repainting when a wallpaper
    /// tool rewrites its colours. Returns true when the palette actually
    /// changed so callers can redraw exactly then.
    fn poll_theme(&mut self) -> bool {
        if self.theme_poll_at.elapsed() < THEME_POLL {
            return false;
        }
        self.theme_poll_at = Instant::now();

        let Some(latest) = Theme::latest_file() else {
            if self.theme_src.is_some() {
                self.theme_src = None;
                self.theme_mtime = None;
                let changed = self.theme != Theme::default();
                self.theme = Theme::default();
                return changed;
            }
            return false;
        };

        // Steady state: the file we follow is still the newest. If its mtime
        // hasn't moved there is nothing to inspect; if it is mid-write we keep
        // the last good theme instead of flashing to an older palette.
        if self.theme_src.as_ref() == Some(&latest) {
            let Ok(meta) = std::fs::metadata(&latest) else {
                return false;
            };
            let mtime = meta.modified().ok();
            if self.theme_mtime == mtime {
                return false;
            }
            if let Some(theme) = Theme::reload_from(&latest) {
                let changed = self.theme != theme;
                self.theme = theme;
                self.theme_mtime = mtime;
                return changed;
            }
            return false;
        }

        let Some(path) = Theme::source_file() else {
            if self.theme_src.as_ref().is_some_and(|src| !src.exists()) {
                self.theme_src = None;
                self.theme_mtime = None;
                let changed = self.theme != Theme::default();
                self.theme = Theme::default();
                return changed;
            }
            return false;
        };

        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let mtime = meta.modified().ok();
        if self.theme_src.as_ref() == Some(&path) && self.theme_mtime == mtime {
            return false;
        }

        let mut changed = false;
        if let Some(theme) = Theme::reload_from(&path) {
            changed = self.theme != theme;
            self.theme = theme;
            self.theme_src = Some(path);
            self.theme_mtime = mtime;
        }
        changed
    }

    // ---------------------------------------------------------------- screens

    /// Landing screen: a feed bar with the selected feed's videos beneath it.
    fn home(&mut self, term: &mut Term) -> Result<Nav> {
        let mut feed = FeedKind::Recommendations;
        let mut page = 1u32;
        self.thumb.reset();

        'feeds: loop {
            let active = feed;
            let crumb = format!("Home · {}", active.label());
            let subtitle = if active.paginated() {
                format!("page {page}")
            } else {
                "front page".to_string()
            };
            let videos = self
                .loading(term, &crumb, active.label(), &subtitle, move || {
                    http::feed_videos(active, page)
                })?
                .unwrap_or_default();

            let mut items: Vec<Item> = videos
                .iter()
                .map(|video| {
                    Item::new(Kind::Video, video.viewkey.clone(), video.title.clone())
                        .meta(format!("#{}", video.viewkey))
                        .detail(
                            "Enter streams the video, ctrl-d downloads it, ctrl-o opens it in a browser.",
                        )
                        .image(video.preview.clone())
                        .url(video.watch_url.clone())
                })
                .collect();

            if active.paginated() {
                if page > 1 {
                    items.push(
                        Item::new(Kind::Nav, "PREV", "<<< Previous page")
                            .detail("Go back one page of this feed."),
                    );
                }
                items.push(
                    Item::new(Kind::Nav, "NEXT", ">>> Next page")
                        .detail("Load the next page of this feed."),
                );
            }

            let mut list = List::new(items);
            let mut next: Option<(FeedKind, u32)> = None;

            loop {
                self.poll_update();
                self.poll_theme();
                self.thumb.sync(list.selected());
                self.thumb.poll(&self.picker);
                term.draw(|frame| self.render_feed_page(frame, active, page, &mut list))?;

                let Some(key) = tui::poll_key(POLL)? else {
                    continue;
                };
                match key {
                    Key::Char('r') => next = Some((FeedKind::Recommendations, 1)),
                    Key::Char('h') => next = Some((FeedKind::Hottest, 1)),
                    Key::Char('v') => next = Some((FeedKind::MostViewed, 1)),
                    Key::Char('n') => next = Some((FeedKind::Newest, 1)),
                    Key::Char('t') => next = Some((FeedKind::TopRated, 1)),
                    Key::Tab | Key::BackTab => {
                        let index = FeedKind::ALL
                            .iter()
                            .position(|candidate| *candidate == active)
                            .unwrap_or(0);
                        let step = if key == Key::Tab {
                            1
                        } else {
                            FeedKind::ALL.len() - 1
                        };
                        next = Some((FeedKind::ALL[(index + step) % FeedKind::ALL.len()], 1));
                    }
                    Key::Char('/') => return Ok(Nav::Search),
                    Key::Char('b') | Key::Char('1') => return Ok(Nav::Browse),
                    Key::Char('x') => return Ok(Nav::Xhamster),
                    Key::Char('2') | Key::Char('u') => return Ok(Nav::Update),
                    Key::Char('q') => return Ok(Nav::Quit),
                    Key::Ctrl('c') => self.quit(),
                    Key::Ctrl('d') => self.download_selected(term, &list)?,
                    Key::Ctrl('o') => self.browser_selected(&list),
                    Key::Up => list.up(),
                    Key::Down => list.down(),
                    Key::PageUp => list.page_up(self.visible_rows.max(1)),
                    Key::PageDown => list.page_down(self.visible_rows.max(1)),
                    Key::Home => list.home(),
                    Key::End => list.end(),
                    Key::Enter => {
                        let Some(index) = list.selected_index() else {
                            continue;
                        };
                        let item = list.items[index].clone();
                        if item.kind == Kind::Nav {
                            page = if item.key == "NEXT" {
                                page + 1
                            } else {
                                page.saturating_sub(1).max(1)
                            };
                            next = Some((active, page));
                            break;
                        }
                        let title = item.label.clone();
                        let url = item_watch_url(&item);
                        self.play(term, &title, &url)?;
                    }
                    _ => {}
                }
                if next.is_some() {
                    break;
                }
            }

            if let Some((selected, selected_page)) = next {
                feed = selected;
                page = selected_page;
                continue 'feeds;
            }
        }
    }

    fn browse_categories(&mut self, term: &mut Term) -> Result<()> {
        let categories = self.loading(
            term,
            "Categories",
            "Fetching categories",
            "reading the category index",
            http::categories,
        )?;
        let Some(categories) = categories else {
            return Ok(());
        };

        if categories.is_empty() {
            self.set_toast("The category index came back empty", Tone::Error);
            return Ok(());
        }

        let items: Vec<Item> = categories
            .iter()
            .map(|category| {
                Item::new(Kind::Category, category.id.clone(), category.name.clone())
                    .meta(format!("#{}", category.id))
                    .detail("Press enter to list this category's newest videos.")
            })
            .collect();
        let mut list = List::new(items);

        loop {
            match self.list_screen(term, "Categories", "Browse", &mut list, true, &[])? {
                UiEvent::Back => return Ok(()),
                UiEvent::Ctrl(_) => {}
                UiEvent::Select(index) => {
                    let item = list.items[index].clone();
                    let category = Category {
                        id: item.key,
                        name: item.label,
                    };
                    self.browse_results(term, Source::Category(category))?;
                }
            }
        }
    }

    fn search(&mut self, term: &mut Term) -> Result<()> {
        let initial = self.last_query.clone();
        let query = self.text_input(
            term,
            "Search query",
            "Type a query and press Enter. Results keep the site's own ranking.",
            &initial,
        )?;
        let Some(query) = query else {
            return Ok(());
        };
        if query.is_empty() {
            return Ok(());
        }
        self.last_query = query.clone();
        self.browse_results(term, Source::Search(query))
    }

    /// xHamster source: opens the search prompt and loads results from
    /// xhamster.desi. Pressing `x` from the home screen lands here.
    fn xhamster(&mut self, term: &mut Term) -> Result<()> {
        let query = self.text_input(
            term,
            "xHamster search",
            "Type a query and press Enter. Results keep the site's own ranking.",
            "",
        )?;
        let Some(query) = query else {
            return Ok(());
        };
        if query.is_empty() {
            return Ok(());
        }
        self.browse_results(term, Source::Xhamster { query })
    }

    fn browse_results(&mut self, term: &mut Term, source: Source) -> Result<()> {
        let mut page: u32 = 1;

        loop {
            let crumb = source.crumb();
            let fetch = source.clone();
            let videos = self.loading(
                term,
                &crumb,
                "Loading videos",
                &format!("page {page}"),
                move || fetch.fetch(page),
            )?;
            let Some(videos) = videos else {
                return Ok(());
            };

            if videos.is_empty() {
                self.set_toast("No videos found on this page", Tone::Info);
            }

            let mut items: Vec<Item> = videos
                .iter()
                .map(|video| {
                    Item::new(Kind::Video, video.viewkey.clone(), video.title.clone())
                        .meta(format!("#{}", video.viewkey))
                        .detail("Enter streams the video, ctrl-d downloads it, ctrl-o opens it in a browser.")
                        .image(video.preview.clone())
                        .url(video.watch_url.clone())
                })
                .collect();

            if page > 1 {
                items.push(
                    Item::new(Kind::Nav, "PREV", "<<< Previous page")
                        .detail("Go back one page of results."),
                );
            }
            items.push(
                Item::new(Kind::Nav, "NEXT", ">>> Next page")
                    .detail("Load the next page of results."),
            );

            let mut list = List::new(items);
            let mut refetch = false;

            while !refetch {
                match self.list_screen(
                    term,
                    &crumb,
                    source.panel_title(),
                    &mut list,
                    true,
                    &[("ctrl-d", "download"), ("ctrl-o", "browser")],
                )? {
                    UiEvent::Back => return Ok(()),
                    UiEvent::Ctrl('d') => self.download_selected(term, &list)?,
                    UiEvent::Ctrl('o') => self.browser_selected(&list),
                    UiEvent::Ctrl(_) => {}
                    UiEvent::Select(index) => {
                        let item = &list.items[index];
                        match item.kind {
                            Kind::Nav => {
                                if item.key == "NEXT" {
                                    page += 1;
                                } else {
                                    page = page.saturating_sub(1).max(1);
                                }
                                refetch = true;
                            }
                            _ => {
                                let title = item.label.clone();
                                let url = item_watch_url(item);
                                self.play(term, &title, &url)?;
                            }
                        }
                    }
                }
            }
        }
    }

    fn choose_quality(&mut self, term: &mut Term, title: &str) -> Result<Option<u8>> {
        let mut list = List::new(vec![
            Item::new(Kind::Quality, "1", "Best available")
                .meta("highest")
                .detail("Use the highest video and audio streams the site offers."),
            Item::new(Kind::Quality, "2", "720p")
                .meta("1280×720")
                .detail("Cap the video height at 720 pixels."),
            Item::new(Kind::Quality, "3", "480p")
                .meta("854×480")
                .detail("Cap the video height at 480 pixels."),
            Item::new(Kind::Quality, "4", "360p")
                .meta("640×360")
                .detail("Cap the video height at 360 pixels."),
        ]);

        loop {
            let crumb = format!("Download · {}", tui::truncate(title, 40));
            match self.list_screen(term, &crumb, "Quality", &mut list, false, &[])? {
                UiEvent::Back => return Ok(None),
                UiEvent::Select(index) => {
                    if let Ok(quality) = list.items[index].key.parse::<u8>() {
                        return Ok(Some(quality));
                    }
                }
                UiEvent::Ctrl(_) => {}
            }
        }
    }

    fn download_selected(&mut self, term: &mut Term, list: &List) -> Result<()> {
        let Some(item) = list.selected() else {
            return Ok(());
        };
        if item.kind != Kind::Video {
            return Ok(());
        }

        let title = item.label.clone();
        let url = item_watch_url(item);
        if let Some(quality) = self.choose_quality(term, &title)? {
            self.download(term, &title, &url, quality)?;
        }
        Ok(())
    }

    fn browser_selected(&mut self, list: &List) {
        let Some(item) = list.selected() else {
            return;
        };
        if item.kind != Kind::Video {
            return;
        }
        if player::open_in_browser(&item_watch_url(item)) {
            self.set_toast("Opened in your browser", Tone::Success);
        } else {
            self.set_toast("Could not find a browser opener", Tone::Error);
        }
    }

    fn play(&mut self, term: &mut Term, title: &str, identifier: &str) -> Result<()> {
        ratatui::restore();
        let ok = player::play(title, identifier);
        *term = ratatui::init();
        term.clear()?;
        self.thumb.reset();
        if ok {
            self.set_toast("Playback finished", Tone::Success);
        } else {
            self.set_toast("Video unavailable or restricted", Tone::Error);
        }
        Ok(())
    }

    fn download(
        &mut self,
        term: &mut Term,
        title: &str,
        identifier: &str,
        quality: u8,
    ) -> Result<()> {
        ratatui::restore();
        let ok = player::download(title, identifier, quality);
        *term = ratatui::init();
        term.clear()?;
        self.thumb.reset();
        if ok {
            self.set_toast("Download complete", Tone::Success);
        } else {
            self.set_toast("Download failed", Tone::Error);
        }
        Ok(())
    }

    fn update_screen(&mut self, term: &mut Term) -> Result<()> {
        let mut tick = 0usize;
        loop {
            self.poll_update();
            self.poll_theme();
            term.draw(|frame| self.render_update(frame, tick))?;
            tick = tick.wrapping_add(1);

            let Some(key) = tui::poll_key(POLL)? else {
                continue;
            };
            match key {
                Key::Esc | Key::Enter | Key::Char('q') => return Ok(()),
                Key::Ctrl('c') => self.quit(),
                Key::Char('r') => {
                    self.update = UpdateState::Checking;
                    self.update_rx = Some(update::spawn_check());
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------ input

    fn list_screen(
        &mut self,
        term: &mut Term,
        crumb: &str,
        panel_title: &str,
        list: &mut List,
        filterable: bool,
        extra: &[(&str, &str)],
    ) -> Result<UiEvent> {
        loop {
            self.poll_update();
            self.poll_theme();
            self.thumb.sync(list.selected());
            self.thumb.poll(&self.picker);
            term.draw(|frame| {
                self.render_list_page(frame, crumb, panel_title, list, filterable, extra);
            })?;

            let Some(key) = tui::poll_key(POLL)? else {
                continue;
            };
            match key {
                Key::Esc => return Ok(UiEvent::Back),
                Key::Ctrl('c') => self.quit(),
                Key::Ctrl('d') => return Ok(UiEvent::Ctrl('d')),
                Key::Ctrl('o') => return Ok(UiEvent::Ctrl('o')),
                Key::Ctrl('n') => list.down(),
                Key::Ctrl('p') => list.up(),
                Key::Ctrl('u') => list.clear_filter(),
                Key::Ctrl('w') => list.delete_word(),
                Key::Enter => {
                    if let Some(index) = list.selected_index() {
                        return Ok(UiEvent::Select(index));
                    }
                }
                Key::Up => list.up(),
                Key::Down => list.down(),
                Key::PageUp => list.page_up(self.visible_rows.max(1)),
                Key::PageDown => list.page_down(self.visible_rows.max(1)),
                Key::Home => list.home(),
                Key::End => list.end(),
                Key::Backspace => list.backspace(),
                Key::Char(ch) if filterable && !ch.is_control() => list.push_char(ch),
                _ => {}
            }
        }
    }

    fn text_input(
        &mut self,
        term: &mut Term,
        title: &str,
        hint: &str,
        initial: &str,
    ) -> Result<Option<String>> {
        let mut chars: Vec<char> = initial.chars().collect();
        let mut cursor = chars.len();
        self.thumb.reset();

        loop {
            self.poll_update();
            self.poll_theme();
            term.draw(|frame| self.render_input(frame, title, hint, &chars, cursor))?;

            let Some(key) = tui::poll_key(POLL)? else {
                continue;
            };
            match key {
                Key::Enter => {
                    let value: String = chars.iter().collect();
                    return Ok(Some(value.trim().to_string()));
                }
                Key::Esc => return Ok(None),
                Key::Ctrl('c') => self.quit(),
                Key::Ctrl('u') => {
                    chars.clear();
                    cursor = 0;
                }
                Key::Ctrl('w') => {
                    while cursor > 0 && chars[cursor - 1].is_whitespace() {
                        chars.remove(cursor - 1);
                        cursor -= 1;
                    }
                    while cursor > 0 && !chars[cursor - 1].is_whitespace() {
                        chars.remove(cursor - 1);
                        cursor -= 1;
                    }
                }
                Key::Char(ch) if !ch.is_control() => {
                    chars.insert(cursor, ch);
                    cursor += 1;
                }
                Key::Backspace if cursor > 0 => {
                    cursor -= 1;
                    chars.remove(cursor);
                }
                Key::Delete if cursor < chars.len() => {
                    chars.remove(cursor);
                }
                Key::Left => cursor = cursor.saturating_sub(1),
                Key::Right => cursor = (cursor + 1).min(chars.len()),
                Key::Home => cursor = 0,
                Key::End => cursor = chars.len(),
                _ => {}
            }
        }
    }

    fn loading<T, F>(
        &mut self,
        term: &mut Term,
        crumb: &str,
        title: &str,
        subtitle: &str,
        work: F,
    ) -> Result<Option<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(work());
        });

        self.thumb.reset();
        let started = Instant::now();
        let mut tick = 0usize;

        loop {
            self.poll_update();
            self.poll_theme();
            match receiver.try_recv() {
                Ok(result) => return result.map(Some),
                Err(TryRecvError::Disconnected) => bail!("background request failed"),
                Err(TryRecvError::Empty) => {}
            }

            let elapsed = started.elapsed();
            term.draw(|frame| self.render_loading(frame, crumb, title, subtitle, tick, elapsed))?;
            tick = tick.wrapping_add(1);

            if let Some(key) = tui::poll_key(POLL)? {
                match key {
                    Key::Esc => return Ok(None),
                    Key::Ctrl('c') => self.quit(),
                    _ => {}
                }
            }
        }
    }

    // --------------------------------------------------------------- drawing

    fn render_list_page(
        &mut self,
        frame: &mut Frame,
        crumb: &str,
        panel_title: &str,
        list: &mut List,
        filterable: bool,
        extra: &[(&str, &str)],
    ) {
        let theme = self.theme;
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, &theme, area);
            return;
        }

        let chunks = chrome_layout(area);
        self.render_header(frame, chunks[0], crumb);
        render_rule(frame, chunks[1], &theme);

        let (list_area, details_area) = body_layout(chunks[2]);
        let counter = format!("{} / {}", list.matches.len(), list.items.len());
        self.render_list_panel(frame, list_area, list, panel_title, &counter, filterable);

        let selected = list.selected();
        self.render_details(frame, details_area, selected);

        render_rule(frame, chunks[3], &theme);

        let status = format!("{} items · {} shown", list.items.len(), list.matches.len());
        let mut hints: Vec<(&str, &str)> = vec![("↑↓", "move"), ("enter", "select")];
        hints.extend_from_slice(extra);
        if filterable {
            hints.push(("type", "filter"));
        }
        hints.push(("esc", "back"));
        hints.push(("ctrl-c", "quit"));
        self.render_status(frame, chunks[4], &status, &hints);
    }

    /// Home keeps the two chrome rules, but spends its body's first row on the
    /// feed bar before the video list and preview.
    fn render_feed_page(&mut self, frame: &mut Frame, feed: FeedKind, page: u32, list: &mut List) {
        let theme = self.theme;
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, &theme, area);
            return;
        }

        let chunks = chrome_layout(area);
        self.render_header(frame, chunks[0], &format!("Home · {}", feed.label()));
        render_rule(frame, chunks[1], &theme);

        let body = chunks[2];
        let bar_area = Rect::new(body.x, body.y, body.width, 1);
        let content = Rect::new(
            body.x,
            body.y + 1,
            body.width,
            body.height.saturating_sub(1),
        );
        render_feed_bar(frame, bar_area, feed, &theme);

        let (list_area, details_area) = body_layout(content);
        let counter = format!("{} / {}", list.matches.len(), list.items.len());
        self.render_list_panel(frame, list_area, list, feed.label(), &counter, false);
        self.render_details(frame, details_area, list.selected());

        render_rule(frame, chunks[3], &theme);
        let status = format!(
            "{} videos · {} shown · page {page}",
            list.items.len(),
            list.matches.len()
        );
        self.render_status(frame, chunks[4], &status, &home_hints());
    }

    fn render_list_panel(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        list: &mut List,
        title: &str,
        counter: &str,
        filterable: bool,
    ) {
        let theme = self.theme;
        let block = panel(&theme, title).title_bottom(
            Line::from(Span::styled(
                format!(" {counter} "),
                Style::default().fg(theme.faint),
            ))
            .right_aligned(),
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            self.visible_rows = 0;
            return;
        }

        let mut rows_area = inner;
        if filterable && inner.height >= 2 {
            let mut spans = vec![Span::styled("⌕ ", Style::default().fg(theme.accent))];
            if list.filter.is_empty() {
                spans.push(Span::styled(
                    "type to filter",
                    Style::default().fg(theme.faint),
                ));
            } else {
                spans.push(Span::styled(
                    list.filter.clone(),
                    Style::default().fg(theme.text),
                ));
                spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
            rows_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
        }

        let visible = rows_area.height as usize;
        list.clamp_window(visible);
        self.visible_rows = visible;
        if visible == 0 {
            return;
        }

        if list.matches.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "(no matches)",
                    Style::default().fg(theme.faint),
                ))
                .alignment(Alignment::Center),
                rows_area,
            );
            return;
        }

        let overflow = list.matches.len() > visible;
        let content_width = rows_area.width.saturating_sub(if overflow { 1 } else { 0 }) as usize;

        let mut lines: Vec<Line> = Vec::with_capacity(visible);
        for slot in 0..visible {
            let Some(candidate) = list.matches.get(list.offset + slot) else {
                break;
            };
            let item = &list.items[candidate.index];
            let selected = list.offset + slot == list.cursor;
            lines.push(row_line(
                &theme,
                item,
                &candidate.positions,
                content_width,
                selected,
            ));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines)),
            Rect::new(
                rows_area.x,
                rows_area.y,
                content_width as u16,
                rows_area.height,
            ),
        );

        if overflow {
            let bar = Rect::new(
                rows_area.x + rows_area.width - 1,
                rows_area.y,
                1,
                rows_area.height,
            );
            let mut state = ScrollbarState::new(list.matches.len() - visible).position(list.offset);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("█")
                .track_symbol(Some("│"))
                .thumb_style(Style::default().fg(theme.accent))
                .track_style(Style::default().fg(theme.faint));
            frame.render_stateful_widget(scrollbar, bar, &mut state);
        }
    }

    fn render_details(&mut self, frame: &mut Frame, area: Rect, item: Option<&Item>) {
        let theme = self.theme;
        let is_video = item.map(|item| item.kind) == Some(Kind::Video);
        let block = panel(&theme, if is_video { "Preview" } else { "Details" });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width < 4 || inner.height < 2 {
            return;
        }

        let mut body = inner;
        if is_video {
            let mut preview_height = None;
            match &mut self.thumb.state {
                ThumbState::Ready(protocol) => {
                    let rows = preview_rows(inner);
                    if rows > 0 {
                        let preview = Rect::new(inner.x, inner.y, inner.width, rows);
                        frame.render_stateful_widget(
                            StatefulImage::new(),
                            preview,
                            protocol.as_mut(),
                        );
                        preview_height = Some(rows);
                    }
                }
                ThumbState::Loading => {
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled("◌ ", Style::default().fg(theme.accent)),
                            Span::styled("loading preview…", Style::default().fg(theme.faint)),
                        ])),
                        Rect::new(inner.x, inner.y, inner.width, 1),
                    );
                    body = Rect::new(
                        inner.x,
                        inner.y + 2,
                        inner.width,
                        inner.height.saturating_sub(2),
                    );
                }
                ThumbState::Idle | ThumbState::Failed => {}
            }
            if let Some(rows) = preview_height {
                // No gap between the image and its caption: compact.
                body = Rect::new(
                    inner.x,
                    inner.y + rows,
                    inner.width,
                    inner.height.saturating_sub(rows),
                );
            }
        }

        if body.height == 0 {
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        match item {
            Some(item) => {
                let tag = match item.kind {
                    Kind::Video => "VIDEO",
                    Kind::Category => "CATEGORY",
                    Kind::Action => "ACTION",
                    Kind::Nav => "PAGE",
                    Kind::Quality => "QUALITY",
                };
                lines.push(Line::from(Span::styled(
                    tag,
                    Style::default()
                        .fg(theme.accent_soft)
                        .add_modifier(Modifier::BOLD),
                )));

                let width = body.width.saturating_sub(1) as usize;
                for chunk in tui::wrap(&item.label, width) {
                    lines.push(Line::from(Span::styled(
                        chunk,
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    )));
                }

                if !item.meta.is_empty() {
                    lines.push(Line::from(Span::styled(
                        item.meta.clone(),
                        Style::default().fg(theme.faint),
                    )));
                }

                if !item.detail.is_empty() {
                    for chunk in tui::wrap(&item.detail, width) {
                        lines.push(Line::from(Span::styled(
                            chunk,
                            Style::default().fg(theme.dim),
                        )));
                    }
                }

                if item.kind == Kind::Video {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "enter",
                            Style::default()
                                .fg(theme.accent_soft)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" watch · ", Style::default().fg(theme.faint)),
                        Span::styled(
                            "ctrl-d",
                            Style::default()
                                .fg(theme.accent_soft)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" download", Style::default().fg(theme.faint)),
                    ]));
                }
            }
            None => lines.push(Line::from(Span::styled(
                "Nothing selected",
                Style::default().fg(theme.faint),
            ))),
        }

        frame.render_widget(
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }),
            body,
        );
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, crumb: &str) {
        let theme = self.theme;
        let (badge, badge_color) = match &self.update {
            UpdateState::Newer(version) => (format!("↑ v{version} available"), theme.warn),
            _ => (format!("v{}", update::VERSION), theme.faint),
        };
        let badge_width = tui::width_of(&badge) as u16 + 2;

        let chunks = Layout::horizontal([
            Constraint::Min(10),
            Constraint::Length(badge_width.min(area.width)),
        ])
        .split(area);

        let left = Line::from(vec![
            Span::styled(
                " ◈ ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "ph-tui",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ›  ", Style::default().fg(theme.faint)),
            Span::styled(crumb.to_string(), Style::default().fg(theme.dim)),
        ]);
        frame.render_widget(Paragraph::new(left), chunks[0]);
        frame.render_widget(
            Paragraph::new(
                Line::from(Span::styled(badge, Style::default().fg(badge_color))).right_aligned(),
            ),
            chunks[1],
        );
    }

    /// Status on the left, keybind guide on the right — one utui-style row.
    fn render_status(&self, frame: &mut Frame, area: Rect, fallback: &str, hints: &[(&str, &str)]) {
        let theme = self.theme;
        let status = if let Some(toast) = self.active_toast() {
            let color = match toast.tone {
                Tone::Info => theme.warn,
                Tone::Success => theme.success,
                Tone::Error => theme.error,
            };
            Line::from(vec![
                Span::styled(" ● ", Style::default().fg(color)),
                Span::styled(toast.text.clone(), Style::default().fg(color)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" · ", Style::default().fg(theme.faint)),
                Span::styled(fallback.to_string(), Style::default().fg(theme.faint)),
            ])
        };

        let guide = hints_line(&theme, hints);
        let guide_width = (guide.width() as u16 + 1).min(area.width);
        let chunks =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(guide_width)]).split(area);
        frame.render_widget(Paragraph::new(status), chunks[0]);
        frame.render_widget(Paragraph::new(guide).right_aligned(), chunks[1]);
    }

    fn render_input(
        &mut self,
        frame: &mut Frame,
        title: &str,
        hint: &str,
        chars: &[char],
        cursor: usize,
    ) {
        let theme = self.theme;
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, &theme, area);
            return;
        }

        let chunks = chrome_layout(area);
        // utui-style: the search prompt owns the top-most row, where the
        // header normally sits. `/` marks the prompt and the query follows it.
        self.render_search_prompt(frame, chunks[0], chars, cursor);
        render_rule(frame, chunks[1], &theme);
        let (main, details) = body_layout(chunks[2]);

        let block = panel(&theme, title);
        let inner = block.inner(main);
        frame.render_widget(block, main);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint.to_string(),
                Style::default().fg(theme.faint),
            )))
            .wrap(Wrap { trim: true })
            .block(Block::default().padding(Padding::new(2, 1, 0, 0))),
            inner,
        );

        let tips = Item::new(Kind::Action, "", "Search tips").detail(
            "Enter starts the search. Esc returns without searching. Ctrl-W deletes a word.",
        );
        self.render_details(frame, details, Some(&tips));

        render_rule(frame, chunks[3], &theme);
        self.render_status(
            frame,
            chunks[4],
            "type a query · enter to search",
            &[
                ("enter", "search"),
                ("←→", "edit"),
                ("esc", "cancel"),
                ("ctrl-c", "quit"),
            ],
        );
    }

    /// Draw the utui-style prompt on the top row: `/` then the query, with
    /// horizontal scrolling so the caret stays visible in a long query.
    fn render_search_prompt(&self, frame: &mut Frame, area: Rect, chars: &[char], cursor: usize) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = self.theme;
        let row = Rect::new(area.x, area.y, area.width, 1);
        let prefix = "/ ";
        let prefix_width = tui::width_of(prefix);
        let max = area.width.saturating_sub(prefix_width as u16) as usize;

        let mut spans = vec![Span::styled(
            prefix,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )];

        if chars.is_empty() {
            spans.push(Span::styled("search…", Style::default().fg(theme.faint)));
            frame.render_widget(Paragraph::new(Line::from(spans)), row);
            frame.set_cursor_position((area.x + prefix_width as u16, row.y));
            return;
        }

        // Drop leading characters until the caret fits inside the row.
        let mut start = 0usize;
        while tui::width_of(&chars[start..cursor].iter().collect::<String>()) >= max
            && start < cursor
        {
            start += 1;
        }

        let visible: String = chars[start..].iter().collect();
        spans.push(Span::styled(
            tui::truncate(&visible, max),
            Style::default().fg(theme.text),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), row);

        let caret_offset = tui::width_of(&chars[start..cursor].iter().collect::<String>());
        let caret_x =
            (area.x + prefix_width as u16 + caret_offset as u16).min(row.right().saturating_sub(1));
        frame.set_cursor_position((caret_x, row.y));
    }

    fn render_loading(
        &mut self,
        frame: &mut Frame,
        crumb: &str,
        title: &str,
        subtitle: &str,
        tick: usize,
        elapsed: Duration,
    ) {
        let theme = self.theme;
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, &theme, area);
            return;
        }

        let chunks = chrome_layout(area);
        self.render_header(frame, chunks[0], crumb);
        render_rule(frame, chunks[1], &theme);

        let body = chunks[2];
        let width = body.width.saturating_sub(8).clamp(30, 64);
        let height = 7u16;
        let x = body.x + body.width.saturating_sub(width) / 2;
        let y = body.y + body.height.saturating_sub(height) / 2;
        let card = Rect::new(x, y, width, height);

        frame.render_widget(Clear, card);
        let block = panel(&theme, "Working").padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(card);
        frame.render_widget(block, card);

        let spinner = SPINNER[tick % SPINNER.len()];
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("{spinner} "),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    title.to_string(),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                subtitle.to_string(),
                Style::default().fg(theme.dim),
            )),
            Line::from(Span::styled(
                format!("{}s elapsed", elapsed.as_secs()),
                Style::default().fg(theme.faint),
            )),
        ];
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);

        render_rule(frame, chunks[3], &theme);
        self.render_status(
            frame,
            chunks[4],
            "request in flight",
            &[("esc", "cancel"), ("ctrl-c", "quit")],
        );
    }

    fn render_update(&mut self, frame: &mut Frame, tick: usize) {
        let theme = self.theme;
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, &theme, area);
            return;
        }

        let chunks = chrome_layout(area);
        self.render_header(frame, chunks[0], "Updates");
        render_rule(frame, chunks[1], &theme);

        let block = panel(&theme, "Release");
        let inner = block.inner(chunks[2]);
        frame.render_widget(block, chunks[2]);

        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "ph-tui ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("v{}", update::VERSION),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(""),
        ];

        match &self.update {
            UpdateState::Checking => lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", SPINNER[tick % SPINNER.len()]),
                    Style::default().fg(theme.accent),
                ),
                Span::styled("asking crates.io…", Style::default().fg(theme.warn)),
            ])),
            UpdateState::Disabled => lines.push(Line::from(Span::styled(
                "Update checks are disabled for this run.",
                Style::default().fg(theme.dim),
            ))),
            UpdateState::NoRelease => lines.push(Line::from(Span::styled(
                "No published release was found on crates.io.",
                Style::default().fg(theme.dim),
            ))),
            UpdateState::Current(version) => lines.push(Line::from(Span::styled(
                format!("✓ v{version} is the latest release."),
                Style::default().fg(theme.success),
            ))),
            UpdateState::Newer(version) => lines.push(Line::from(Span::styled(
                format!("↑ v{version} is available."),
                Style::default().fg(theme.warn),
            ))),
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "cargo install ph-tui --force",
            Style::default().fg(theme.accent_soft),
        )));
        lines.push(Line::from(Span::styled(
            "cargo install --path . --force   (from a checkout)",
            Style::default().fg(theme.faint),
        )));

        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: true })
                .block(Block::default().padding(Padding::new(2, 1, 0, 0))),
            inner,
        );

        render_rule(frame, chunks[3], &theme);
        self.render_status(
            frame,
            chunks[4],
            "checks happen once per run",
            &[("r", "recheck"), ("esc", "back"), ("ctrl-c", "quit")],
        );
    }
}

fn chrome_layout(area: Rect) -> [Rect; 5] {
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // top rule
        Constraint::Min(2),    // body
        Constraint::Length(1), // bottom rule
        Constraint::Length(1), // status + keybind guide
    ])
    .split(area);
    [chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]]
}

/// The two hairline dividers in the minimal chrome: one under the header,
/// one above the status/keybind row.
fn render_rule(frame: &mut Frame, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(theme.faint),
        ))),
        area,
    );
}

/// One compact row of feeds. The active feed is highlighted; narrow terminals
/// fall back to just the active feed and the categories shortcut.
fn render_feed_bar(frame: &mut Frame, area: Rect, active: FeedKind, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let feeds: &[FeedKind] = if area.width < 96 {
        std::slice::from_ref(&active)
    } else {
        &FeedKind::ALL
    };
    let mut spans = Vec::new();
    for (index, feed) in feeds.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ·  ", Style::default().fg(theme.faint)));
        }
        let selected = *feed == active;
        let style = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.dim)
        };
        spans.push(Span::styled(feed.key(), style));
        spans.push(Span::styled(format!(" {}", feed.label()), style));
    }
    spans.push(Span::styled("  ·  ", Style::default().fg(theme.faint)));
    spans.push(Span::styled(
        "b",
        Style::default()
            .fg(theme.accent_soft)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        " Browse categories",
        Style::default().fg(theme.dim),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Home keybind guide shown on the bottom status row.
fn home_hints() -> [(&'static str, &'static str); 7] {
    [
        ("↑↓", "move"),
        ("enter", "watch"),
        ("tab", "feed"),
        ("/", "search"),
        ("x", "xhamster"),
        ("b", "browse"),
        ("q", "quit"),
    ]
}

fn hints_line(theme: &Theme, hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, (key, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ·  ", Style::default().fg(theme.faint)));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default()
                .fg(theme.accent_soft)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            (*action).to_string(),
            Style::default().fg(theme.dim),
        ));
    }
    Line::from(spans)
}

fn body_layout(area: Rect) -> (Rect, Rect) {
    if area.width >= 100 {
        // Give the preview pane a larger share so the image is noticeably
        // bigger while the list keeps most of the width.
        let chunks = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(area);
        (chunks[0], chunks[1])
    } else {
        let chunks =
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
        (chunks[0], chunks[1])
    }
}

fn panel(theme: &Theme, title: &str) -> Block<'static> {
    // No box: the title sits on its own row, with content starting below it.
    // The only lines in the UI are the two chrome rules.
    Block::default().title(Line::from(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )))
}

fn row_line(
    theme: &Theme,
    item: &Item,
    positions: &[usize],
    width: usize,
    selected: bool,
) -> Line<'static> {
    let base = if selected {
        Style::default()
            .bg(theme.selected_bg)
            .fg(theme.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(match item.kind {
            Kind::Nav => theme.accent_soft,
            _ => theme.text,
        })
    };
    let highlight = if selected {
        Style::default()
            .bg(theme.selected_bg)
            .fg(theme.warn)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    };
    let marker = if selected {
        Span::styled(
            "▌ ",
            Style::default().bg(theme.selected_bg).fg(theme.accent),
        )
    } else {
        Span::styled("  ", base)
    };

    let mut spans = vec![marker];
    let mut used = 2usize;

    let meta_width = if item.meta.is_empty() {
        0
    } else {
        tui::width_of(&item.meta) + 2
    };
    let label_width = width.saturating_sub(used + meta_width);

    let mut truncated = false;
    for (index, ch) in item.label.chars().enumerate() {
        let char_width = ch.width().unwrap_or(0);
        if used - 2 + char_width > label_width {
            truncated = true;
            break;
        }
        let style = if positions.contains(&index) {
            highlight
        } else {
            base
        };
        spans.push(Span::styled(ch.to_string(), style));
        used += char_width;
    }
    if truncated && label_width > used - 2 {
        spans.push(Span::styled("…", base));
        used += 1;
    }

    if !item.meta.is_empty() && used + 2 < width {
        let room = width.saturating_sub(used + 1);
        let meta = tui::truncate(&item.meta, room);
        let meta_width = tui::width_of(&meta);
        let gap = width.saturating_sub(used + meta_width + 1);
        if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), base));
            used += gap;
        }
        let meta_style = if selected {
            Style::default().bg(theme.selected_bg).fg(theme.dim)
        } else {
            Style::default().fg(theme.faint)
        };
        spans.push(Span::styled(meta, meta_style));
        used += meta_width;
    }

    if width > used {
        spans.push(Span::styled(" ".repeat(width - used), base));
    }

    Line::from(spans)
}

fn preview_rows(inner: Rect) -> u16 {
    // Reserve only a compact strip for the title, metadata and key hint.
    let max = inner.height.saturating_sub(4);
    if max < 4 {
        return 0;
    }
    // Terminal cells are roughly twice as tall as they are wide, so this is
    // the row count for a 16:9 image spanning the pane width.
    let by_width = (inner.width as u32 * 9 / 32) as u16;
    by_width.clamp(4, max).min(24)
}

fn render_too_small(frame: &mut Frame, theme: &Theme, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .split(area);
    let message = Text::from(vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("resize to at least {MIN_WIDTH}×{MIN_HEIGHT}"),
            Style::default().fg(theme.dim),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(message).alignment(Alignment::Center),
        chunks[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App, list: &mut List) -> String {
        let backend = TestBackend::new(120, 36);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| {
            app.render_list_page(
                frame,
                "Search · test",
                "Results",
                list,
                true,
                &[("ctrl-d", "download")],
            );
        })
        .unwrap();

        let buffer = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn render_feed(app: &mut App, feed: FeedKind, width: u16, height: u16) -> String {
        let mut list = List::new(vec![Item::new(
            Kind::Video,
            "abc123",
            "A recommended video",
        )
        .meta("#abc123")]);
        let backend = TestBackend::new(width, height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| {
            app.render_feed_page(frame, feed, 1, &mut list);
        })
        .unwrap();

        let buffer = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn home_shows_every_feed_in_the_bar() {
        let screen = render_feed(&mut App::headless(), FeedKind::Recommendations, 140, 40);
        assert!(screen.contains("Home · Recommendations"), "{screen}");
        for feed in FeedKind::ALL {
            assert!(
                screen.contains(feed.label()),
                "missing {}: {screen}",
                feed.label()
            );
        }
        assert!(screen.contains("Browse categories"), "{screen}");
        assert!(screen.contains("A recommended video"), "{screen}");
        assert!(screen.contains("x xhamster"), "missing x hint: {screen}");
    }

    #[test]
    fn home_feed_bar_compacts_on_narrow_terminals() {
        let screen = render_feed(&mut App::headless(), FeedKind::Newest, 70, 24);
        assert!(screen.contains("Newest"), "{screen}");
        assert!(screen.contains("Browse categories"), "{screen}");
        assert!(!screen.contains("Hottest"), "{screen}");
    }

    #[test]
    fn search_prompt_owns_the_top_row() {
        let mut app = App::headless();
        let chars: Vec<char> = "pov".chars().collect();
        let backend = TestBackend::new(120, 36);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| {
            app.render_input(frame, "Search query", "Type a query", &chars, chars.len());
        })
        .unwrap();

        let buffer = term.backend().buffer();
        let row = |y: u16| -> String {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        };
        assert!(row(0).starts_with("/ pov"), "prompt not on top: {}", row(0));
        assert!(
            !row(0).contains("ph-tui"),
            "header should yield the top row: {}",
            row(0)
        );
        assert!(
            row(1).chars().all(|ch| ch == '─' || ch == ' '),
            "rule should follow the prompt: {}",
            row(1)
        );
    }

    #[test]
    fn renders_a_results_page() {
        let mut app = App::headless();
        let mut list = List::new(vec![
            Item::new(Kind::Video, "abc123", "A first video title").meta("#abc123"),
            Item::new(Kind::Video, "def456", "Another video title").meta("#def456"),
            Item::new(Kind::Nav, "NEXT", ">>> Next page"),
        ]);

        let screen = render(&mut app, &mut list);
        assert!(screen.contains("ph-tui"), "header missing: {screen}");
        assert!(screen.contains("A first video title"), "row missing");
        assert!(screen.contains("VIDEO"), "details tag missing");
        assert!(screen.contains(">>> Next page"), "navigation row missing");
    }

    #[test]
    fn minimal_chrome_has_two_rules_and_no_boxes() {
        let mut app = App::headless();
        let mut list = List::new(vec![Item::new(Kind::Video, "abc", "Alpha")]);
        let screen = render(&mut app, &mut list);
        for glyph in ['┌', '┐', '└', '┘', '│'] {
            assert!(!screen.contains(glyph), "box glyph {glyph} still rendered");
        }
        let rules = screen
            .lines()
            .filter(|line| !line.is_empty() && line.chars().all(|ch| ch == '─'))
            .count();
        assert_eq!(rules, 2, "expected two chrome rules: {screen}");
    }

    #[test]
    fn filter_text_is_visible() {
        let mut app = App::headless();
        let mut list = List::new(vec![Item::new(Kind::Video, "abc", "Alpha")]);
        list.push_char('a');
        let screen = render(&mut app, &mut list);
        assert!(
            screen.contains("type to filter") || screen.contains("a"),
            "{screen}"
        );
    }

    #[test]
    fn preview_uses_more_of_the_details_pane() {
        // A 96-column pane should get a 24-row 16:9 preview, not the old
        // 16-row cap, and only four rows are reserved for the caption.
        assert_eq!(preview_rows(Rect::new(0, 0, 96, 48)), 24);
        assert_eq!(preview_rows(Rect::new(0, 0, 96, 28)), 24);
        // Too short for a preview plus caption: leave it to the text.
        assert_eq!(preview_rows(Rect::new(0, 0, 96, 7)), 0);
    }

    #[test]
    fn row_lines_never_exceed_their_width() {
        let item =
            Item::new(Kind::Video, "abc", "A deliberately long video title").meta("#abcdef123456");
        let theme = Theme::default();
        for width in [10usize, 24, 40, 80] {
            let line = row_line(&theme, &item, &[0], width, true);
            assert!(line.width() <= width, "width {width}: {}", line.width());
        }
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut app = App::headless();
        let mut list = List::new(vec![Item::new(Kind::Video, "abc", "Title")]);
        let backend = TestBackend::new(30, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| app.render_list_page(frame, "Home", "Start", &mut list, true, &[]))
            .unwrap();
    }

    #[test]
    fn renders_a_ready_preview_in_both_layouts() {
        for (width, height) in [(80u16, 24u16), (140, 42)] {
            let mut app = App::headless();
            let mut list = List::new(vec![Item::new(Kind::Video, "abc", "Preview video")]);
            let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
                480,
                270,
                image::Rgba([20u8, 40, 60, 255]),
            ));
            let protocol = app.picker.new_resize_protocol(image);
            app.thumb.state = ThumbState::Ready(Box::new(protocol));

            let backend = TestBackend::new(width, height);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|frame| {
                app.render_list_page(frame, "Search", "Results", &mut list, true, &[])
            })
            .unwrap();
        }
    }
}
