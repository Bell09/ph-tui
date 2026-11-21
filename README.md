# ph-tui

**A terminal video browser with inline previews, written in Rust.**

One native binary: scraping, filtering, previews and playback setup all live in
Rust. The interface is built with `ratatui`, previews use `ratatui-image`, and
`mpv` plus `yt-dlp` do the heavy lifting at playback time.

```
 ◆ ph-tui  ›  Search · big tits                    v0.1.0
────────────────────────────────────────────────────────────────────────────────
 Results                                   Preview
 ⌕ type to filter
 ▌ Big Tits Compilation            #a1b2c3  ▄▄▀▀▄▄▀▀▄▄▀▀▄▄▀▀▄▄▀▀▄▄▀▀▄▄
   Morning Routine                #d4e5f6  VIDEO
   Next page                               A very long video title
                                           #a1b2c3
                                           enter watch · ctrl-d download
                                                                 13 / 13
────────────────────────────────────────────────────────────────────────────────
 · 13 items · 13 shown      ↑↓ move · enter select · ctrl-d download · esc back
```

## Features

- **Native TUI** — header, split browser, details pane, status line and key hints
- **Minimal chrome** — no panel boxes; two single-line rules frame the content,
  with status and keybind guide sharing one utui-style row
- **Inline previews** — kitty, sixel or iTerm2 graphics, with unicode half-block
  fallback on terminals that support neither
- **Fast filtering** — fuzzy match with highlighted characters, paging and a
  scrollbar, all in-process
- **Home feeds** — a feed bar with Recommendations, Hottest, Most Viewed, Newest
  and Top Rated; each feed loads real videos with inline previews
- **Categories and search** — fetch from the site directly, page through results
- **xHamster search** — press `x` from home to search xHamster and play its results
- **Playback** — hand the watch page to `mpv`, which resolves the stream through
  `yt-dlp`; playback is always capped at 320p
- **Downloads** — pick best / 720p / 480p / 360p, written to `~/Videos/ph-tui`
- **Browser hand-off** — open the current video with `xdg-open`
- **Update check** — optional crates.io check shown in the header
- **Live theming** — follows `pywal`, `wallust` or `matugen` palettes and
  repaints when the wallpaper changes
- **Bounded caches** — preview images are stored under the user cache directory

## Requirements

| Tool | Purpose |
| --- | --- |
| `mpv` | playback |
| `yt-dlp` | stream resolution and downloads |

`yt-dlp` also needs `curl_cffi` to impersonate a browser when fetching video
pages. Without it, streams fail with HTTP 403 and mpv exits immediately (which
looks like "mpv never opened"). Install a compatible version with:

```sh
python3 -m pip install --user --break-system-packages 'curl_cffi>=0.10,<0.17'
```

The app checks for this at startup and shows a warning if it is missing.

Rust 1.86 or newer to build.

Playback and downloads pass `cookies-from-browser=firefox` to `mpv`/`yt-dlp`,
so a Firefox profile with access to the site should exist. Change the flag in
`src/player.rs` if you use a different browser.

## Theming

ph-tui uses the same 16-colour palettes as [utui](https://github.com/Bell09/utui)
when a wallpaper tool is installed. It checks, newest first:

- `~/.cache/wal/colors.json`, `~/.cache/wallust/colors.json` and
  `~/.cache/matugen/colors.json` (or the `$XDG_CACHE_HOME` equivalents)
- plain 16-line `colors` files in those same directories
- JSON outputs declared under `[templates]` in `~/.config/matugen/config.toml`
  or `~/.config/wallust/wallust.toml`
- `~/.local/share/matugen/colors.json`

Classic pywal/wallust `colors.color0`…`color15` palettes, base16
`base00`…`base0F` palettes and matugen Material-role JSON are all understood.
The newest valid palette wins; a palette being rewritten mid-write is ignored
until the write finishes, so the UI never flashes to stale colours.

The palette is re-read about once per second, so running `wal -i …`,
`wallust run …` or `matugen …` repaints the open TUI without a restart. Text,
accents, hairlines, warnings, success and error colours come from the palette;
`faint` and the selection highlight are derived from its background so they
stay readable on any wallpaper. No full-window background is painted, so
terminal transparency and blur keep working. With no palette file, ph-tui falls
back to its built-in pink palette.

## Install

From a checkout:

```sh
git clone <this-repo> ph-tui
cd ph-tui
cargo build --release
install -Dm755 target/release/ph-tui ~/.local/bin/ph-tui
```

Or use the helper script, which installs to `/usr/local/bin` by default:

```sh
./install.sh                 # install
./install.sh --prefix ~/.local
./install.sh --uninstall
```

## Usage

Run `ph-tui` in a terminal. The home screen opens straight into the
**Recommendations** feed (the personalized front page) and shows a feed bar:

```
 ◆ ph-tui  ›  Home · Recommendations                 v0.1.0
────────────────────────────────────────────────────────────────────────────────
 r Recommendations  ·  h Hottest  ·  v Most Viewed  ·  n Newest  ·  t Top Rated  ·  b Browse categories
 Recommendations
 ▌ A recommended video                      #abc123  ▄▄▀▀▄▄▀▀▄▄▀▀▄▄▀▀▄▄▀▀▄▄▀▀▄▄
   Another recommendation                   #def456  VIDEO
   ...                                               A recommended video
────────────────────────────────────────────────────────────────────────────────
 · N videos · N shown · page 1    ↑↓ move · enter watch · tab feed · / search · x xhamster · b browse · q quit
```

| Key | Action |
| --- | --- |
| `r`, `h`, `v`, `n`, `t` | Recommendations, Hottest, Most Viewed, Newest, Top Rated |
| `Tab` / `Shift-Tab` | Cycle feeds |
| `↑` / `↓`, `PgUp` / `PgDn` | Move through the feed / change page |
| `Enter` | Watch the selected video; `>>> Next page` works on ordered feeds |
| `Ctrl-D` / `Ctrl-O` | Download / open the selected video in a browser |
| `b` / `1` | Browse categories |
| `/` | Search videos |
| `x` | xHamster search |
| `q` | Quit |

The front page does not paginate; the four ordered feeds do. `2` / `u` still
check for updates from the home screen, but the update check is not in the bar.

### Lists

| Key | Action |
| --- | --- |
| `↑` / `↓`, `Ctrl-P` / `Ctrl-N` | Move |
| `PgUp` / `PgDn` | Page |
| `Home` / `End` | First / last item |
| type | Filter the list |
| `Backspace`, `Ctrl-W`, `Ctrl-U` | Edit the filter |
| `Enter` | Select |
| `Esc` | Back |
| `Ctrl-C` | Quit anywhere |

On a results page, `Ctrl-D` downloads the selected video (after a quality
prompt) and `Ctrl-O` opens it in the default browser. The preview pane loads
as you move the selection; it is fetched once and then read from the cache.

### Search input

Press `/` and the prompt appears on the top row, utui-style (`/ your query`),
with the caret in place while you type.

| Key | Action |
| --- | --- |
| `Enter` | Run the search |
| `←` / `→`, `Home` / `End` | Move the caret |
| `Ctrl-W`, `Ctrl-U` | Delete a word / the whole query |
| `Esc` | Cancel |

Results keep the site's own ranking; no titles are filtered out locally.

## Options

```
--no-update-check   Skip the background update check
-h, --help
-V, --version
```

## Files

| Path | Contents |
| --- | --- |
| `~/.cache/ph-tui/previews/` | normalized 480×270 preview images |
| `~/Videos/ph-tui/` | downloads (falls back to the home directory) |

The preview cache is pruned to its 200 most recent files.

## Development

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test                  # parser, list, preview, theme and render tests
cargo test -- --ignored     # live requests against the real site
```

The ignored tests perform real HTTP requests for all five home feeds,
categories, a category page, a search page and a preview image.

## Notes and limitations

- The site can rate-limit, redirect by region, or serve anti-bot challenges.
  Failed requests return to the previous screen with a status message instead
  of crashing the interface.
- Streaming depends on `mpv`, `yt-dlp` and `curl_cffi` supporting the site;
  keep `yt-dlp` up to date. mpv errors are shown in the terminal so stream
  failures are visible.
- Playback always requests 320p via
  `--ytdl-format=bestvideo[height<=320]+bestaudio/best[height<=320]`.
  The quality prompt on `Ctrl-D` only affects downloads.
- If `mpv` exits in under five seconds the stream is treated as unavailable,
  which usually means the video is deleted, private, region-locked, or the
  extractor could not resolve it.
- Graphics protocols are detected at startup. On terminals without kitty,
  sixel or iTerm2 support, previews render as unicode half-blocks.
- The layout stacks the list above the preview on terminals narrower than 100
  columns, and asks for at least 56×16.

## Disclaimer

This project does not host, store, or distribute any media. It only provides a
terminal interface to publicly accessible content. Use it responsibly and in
accordance with your local laws and the site's terms of service.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
