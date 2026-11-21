use std::cmp::min;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Action,
    Category,
    Video,
    Nav,
    Quality,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub key: String,
    pub label: String,
    /// Short text shown at the right edge of a row.
    pub meta: String,
    /// Long description shown in the details pane.
    pub detail: String,
    /// Preview image URL, when the source already knows one.
    pub image: Option<String>,
    /// Full watch URL (used by sources with URL-based identifiers, e.g. xHamster).
    pub url: Option<String>,
    /// Uploader/channel name, when the source exposes one.
    pub channel: Option<String>,
    /// Channel video listing URL, used by the `c` shortcut.
    pub channel_url: Option<String>,
}

impl Item {
    pub fn new(kind: Kind, key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind,
            key: key.into(),
            label: label.into(),
            meta: String::new(),
            detail: String::new(),
            image: None,
            url: None,
            channel: None,
            channel_url: None,
        }
    }

    pub fn meta(mut self, meta: impl Into<String>) -> Self {
        self.meta = meta.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn image(mut self, image: Option<String>) -> Self {
        self.image = image;
        self
    }

    pub fn url(mut self, url: Option<String>) -> Self {
        self.url = url;
        self
    }

    pub fn channel(mut self, name: Option<String>, url: Option<String>) -> Self {
        self.channel = name;
        self.channel_url = url;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub index: usize,
    pub score: i64,
    pub positions: Vec<usize>,
}

pub struct List {
    pub items: Vec<Item>,
    pub filter: String,
    pub cursor: usize,
    pub offset: usize,
    pub matches: Vec<Candidate>,
}

impl List {
    pub fn new(items: Vec<Item>) -> Self {
        let mut list = Self {
            items,
            filter: String::new(),
            cursor: 0,
            offset: 0,
            matches: Vec::new(),
        };
        list.refresh();
        list
    }

    fn refresh(&mut self) {
        self.matches = candidates(&self.items, &self.filter);
        if self.matches.is_empty() {
            self.cursor = 0;
            self.offset = 0;
        } else {
            self.cursor = min(self.cursor, self.matches.len() - 1);
        }
    }

    pub fn selected(&self) -> Option<&Item> {
        let candidate = self.matches.get(self.cursor)?;
        self.items.get(candidate.index)
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.matches
            .get(self.cursor)
            .map(|candidate| candidate.index)
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.cursor = 0;
        self.offset = 0;
        self.refresh();
    }

    pub fn push_char(&mut self, ch: char) {
        self.filter.push(ch);
        self.cursor = 0;
        self.offset = 0;
        self.refresh();
    }

    pub fn backspace(&mut self) {
        if self.filter.pop().is_some() {
            self.cursor = 0;
            self.offset = 0;
            self.refresh();
        }
    }

    pub fn delete_word(&mut self) {
        while self.filter.ends_with(char::is_whitespace) {
            self.filter.pop();
        }
        while self
            .filter
            .chars()
            .last()
            .is_some_and(|c| !c.is_whitespace())
        {
            self.filter.pop();
        }
        while self.filter.ends_with(char::is_whitespace) {
            self.filter.pop();
        }
        self.cursor = 0;
        self.offset = 0;
        self.refresh();
    }

    pub fn clear_filter(&mut self) {
        if !self.filter.is_empty() {
            self.set_filter(String::new());
        }
    }

    fn len(&self) -> usize {
        self.matches.len()
    }

    pub fn up(&mut self) {
        let len = self.len();
        if len > 0 {
            self.cursor = (self.cursor + len - 1) % len;
        }
    }

    pub fn down(&mut self) {
        let len = self.len();
        if len > 0 {
            self.cursor = (self.cursor + 1) % len;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.cursor = self.cursor.saturating_sub(page.max(1));
    }

    pub fn page_down(&mut self, page: usize) {
        let len = self.len();
        if len > 0 {
            self.cursor = min(self.cursor + page.max(1), len - 1);
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        let len = self.len();
        if len > 0 {
            self.cursor = len - 1;
        }
    }

    /// Keep the cursor inside the visible window.
    pub fn clamp_window(&mut self, height: usize) {
        let len = self.len();
        if len == 0 {
            self.cursor = 0;
            self.offset = 0;
            return;
        }
        self.cursor = min(self.cursor, len - 1);
        if height == 0 {
            self.offset = 0;
            return;
        }
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + height {
            self.offset = self.cursor + 1 - height;
        }
        let max_offset = len.saturating_sub(height);
        if self.offset > max_offset {
            self.offset = max_offset;
        }
    }
}

fn candidates(items: &[Item], filter: &str) -> Vec<Candidate> {
    let mut matches: Vec<Candidate> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            fuzzy_match(&item.label, filter).map(|(score, positions)| Candidate {
                index,
                score,
                positions,
            })
        })
        .collect();

    matches.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
    matches
}

/// Case-insensitive subsequence match. Returns a score and the matched char
/// positions so the view can highlight them.
fn fuzzy_match(haystack: &str, needle: &str) -> Option<(i64, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }

    let hay: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle.chars().map(lower).collect();

    let mut positions = Vec::with_capacity(needle.len());
    let mut cursor = 0usize;
    let mut score = 0i64;
    let mut last: Option<usize> = None;

    for expected in &needle {
        let mut found = None;
        while cursor < hay.len() {
            let current = cursor;
            cursor += 1;
            if lower(hay[current]) == *expected {
                found = Some(current);
                break;
            }
        }
        let index = found?;

        score += 2;
        if index == 0 || !hay[index - 1].is_alphanumeric() {
            score += 8;
        }
        if let Some(previous) = last {
            if index == previous + 1 {
                score += 6;
            }
        }
        last = Some(index);
        positions.push(index);
    }

    score -= (hay.len() as i64) / 20;
    Some((score, positions))
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, label: &str) -> Item {
        Item::new(Kind::Video, key, label)
    }

    #[test]
    fn fuzzy_matches_subsequences_case_insensitively() {
        assert!(fuzzy_match("Big Tits", "bt").is_some());
        assert!(fuzzy_match("Big Tits", "BIG").is_some());
        assert!(fuzzy_match("Big Tits", "xyz").is_none());
        assert!(fuzzy_match("anything", "").is_some());
    }

    #[test]
    fn exact_prefix_scores_higher_than_scattered_match() {
        let prefix = fuzzy_match("Teen video", "tee").unwrap().0;
        let scattered = fuzzy_match("The evening entertainment", "tee").unwrap().0;
        assert!(prefix > scattered, "{prefix} should beat {scattered}");
    }

    #[test]
    fn filtering_reorders_by_score_and_keeps_source_index() {
        let mut list = List::new(vec![
            item("a", "Something else"),
            item("b", "Big Tits Compilation"),
            item("c", "Tiny bit"),
        ]);
        assert_eq!(list.matches.len(), 3);

        list.set_filter("bigti".to_string());
        assert_eq!(list.matches.len(), 1);
        assert_eq!(list.selected().unwrap().key, "b");
        assert_eq!(list.selected_index(), Some(1));
    }

    #[test]
    fn empty_filter_lists_everything_in_order() {
        let list = List::new(vec![item("a", "One"), item("b", "Two")]);
        assert_eq!(list.matches.len(), 2);
        assert_eq!(list.selected_index(), Some(0));
    }

    #[test]
    fn navigation_wraps() {
        let mut list = List::new(vec![item("a", "One"), item("b", "Two")]);
        list.up();
        assert_eq!(list.selected_index(), Some(1));
        list.down();
        assert_eq!(list.selected_index(), Some(0));
    }

    #[test]
    fn filter_editing_resets_the_cursor() {
        let mut list = List::new(vec![item("a", "One"), item("b", "Two")]);
        list.down();
        list.push_char('t');
        assert_eq!(list.selected_index(), Some(1));
        list.backspace();
        assert_eq!(list.filter, "");
        assert_eq!(list.selected_index(), Some(0));
    }

    #[test]
    fn window_clamps_to_results() {
        let items = (0..30)
            .map(|i| item(&format!("k{i}"), &format!("Item {i}")))
            .collect();
        let mut list = List::new(items);
        for _ in 0..25 {
            list.down();
        }
        list.clamp_window(10);
        assert!(list.offset <= list.cursor);
        assert!(list.cursor < list.offset + 10);
    }
}
