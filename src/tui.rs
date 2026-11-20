//! Keyboard decoding and small text helpers shared by the views.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Resize,
}

/// Block until a key (or resize) event arrives.
pub fn read_key() -> io::Result<Key> {
    loop {
        match event::read()? {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) => {
                if kind == KeyEventKind::Release {
                    continue;
                }
                return Ok(map_key(code, modifiers));
            }
            Event::Resize(_, _) => return Ok(Key::Resize),
            _ => continue,
        }
    }
}

/// Wait `timeout` for an event, returning `None` when the terminal is idle.
/// This is what keeps the render loop alive while a thumbnail downloads.
pub fn poll_key(timeout: Duration) -> io::Result<Option<Key>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    Ok(Some(read_key()?))
}

fn map_key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    if modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c) = code {
            return Key::Ctrl(c.to_ascii_lowercase());
        }
    }
    match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        _ => Key::Resize,
    }
}

pub fn width_of(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate(text: &str, max: usize) -> String {
    if width_of(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if used + width + 1 > max {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split_whitespace() {
            let word_width = width_of(word);
            if line_width > 0 && line_width + 1 + word_width > width {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            if line_width > 0 {
                line.push(' ');
                line_width += 1;
            }
            line.push_str(word);
            line_width += word_width;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_display_width() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert!(width_of(&truncate("hello world", 6)) <= 6);
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn wrapping_breaks_on_words() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
    }
}
