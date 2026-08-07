# akapen

**akapen**（赤ペン, "red pen") — mark up your agent's homework.

A standalone TUI for reviewing documents in the terminal: read markdown beautifully rendered, select lines, attach comments, and send them to your coding agent — like a teacher grading homework with a red pen. Built for the agent review loop: the agent writes, you mark it up, the agent revises, and akapen shows you exactly what changed.

日本語版 README は [README.ja.md](README.ja.md) にあります。

Based on the line-comment experience of [herdr-reviewr](https://github.com/persiyanov/herdr-reviewr), reworked for markdown-first reading in a single-pane, two-mode design.

## Highlights

- **view mode** (default for `.md`): native markdown rendering (tui-markdown / pulldown-cmark) — tables, headings, code blocks, links, footnotes, math, task lists, front matter, in full color. No external renderer process.
- **source mode**: raw source with line numbers and syntect highlighting (100+ languages). Long lines wrap with gutter-aligned indentation; tabs expand to 8-column stops.
- **Comment anywhere**: line cursor and range selection work identically in both modes — `v` to select, `c` to comment, without leaving the rendered view. Comments appear as inline cards right under the lines they refer to.
- **Your files are never modified.** akapen is strictly read-only; comments are exported through a separate channel (clipboard, stdout, or a send command).
- **Built for the agent loop**: when the agent edits a file you have open, akapen detects it (`⚡`), and on reload highlights added/changed lines (green `+` gutter) and deletions (red `-` gutter) so re-review means reviewing the diff.
- **Session mode, always**: open one file or twenty with the same keybindings. `]` / `[` switch files; cursor, selection, and mode are remembered per file.
- **Terminal-native colors**: UI chrome uses plain ANSI colors and follows your terminal palette; syntax colors come from two-face themes (32 built-ins) or any `.tmTheme` file. Light/dark is auto-detected via OSC 11.
- **CJK-correct**: all width math uses `unicode-width`, so Japanese text never misaligns. On macOS, the input source is pinned to ASCII in command mode and restored on exit (`--ime jp` switches to Japanese while composing).

## Install

```sh
cargo install akapen
```

Or from source:

```sh
cargo build --release
# binary: target/release/akapen
```

No external binaries required — rendering is built in. On macOS the optional IME helper is compiled once with `swiftc` (auto-disabled if unavailable).

## Usage

```
akapen <file...> [--send-cmd <cmd> | --send-agent] [--theme <name>]
                 [--ime <off|ascii|jp>] [--light|--dark] [--callback <cmd>]
                 [--esc-quit <auto|always|never>]
```

| Flag | Meaning |
|---|---|
| `--send-cmd <cmd>` | `s` pipes the formatted export to this shell command's stdin |
| `--send-agent` | `s` sends to the sole herdr agent in the current tab (needs `herdr` on PATH) |
| `--theme <name>` | two-face theme name (default `Catppuccin Mocha`; `Solarized (light)` when light is detected) or a path to a `.tmTheme` file |
| `--ime <off\|ascii\|jp>` | macOS input-source control around the comment composer (default `ascii`) |
| `--light` / `--dark` | force the UI palette (default: auto-detect the terminal background via OSC 11) |
| `--callback <cmd>` | shell command spawned on exit (e.g. return to a file picker) |
| `--esc-quit <auto\|always\|never>` | whether `Esc` may quit (default `auto`: only with `--callback`; `always` = unconditionally, `never` = Esc stays a pure cancel) |

Typical loop with a picker (see the companion tool [ashiato](https://github.com/worldnine/ashiato), an mtime-sorted file picker):

```sh
ashiato . --open-cmd "akapen {} --send-agent"
```

## Keys

| Key | Action |
|---|---|
| `Tab` | toggle view ⇄ source (selection carries over; non-markdown files are source-only) |
| `j` / `k` | move cursor (extends the range while selecting) |
| `g` / `G`, `PgUp` / `PgDn`, `Ctrl+u` / `Ctrl+d` | jump / half-page moves |
| `v` | start selecting lines |
| `c` | comment on the selection (or cursor line). Re-selecting an existing comment's exact range edits it |
| `n` / `N` | jump to the next / previous comment block (selects the whole block) |
| `]` / `[` | next / previous file in the session |
| `l` | all-comments overlay |
| `Ctrl+p` | file-list overlay |
| `?` | key reference |
| `r` / `i` | on external change: reload / ignore |
| `e` | edit the file in `$EDITOR` (suspends the TUI, reloads with diff highlight on return) |
| `y` | copy all comments to the clipboard (comments are kept) |
| `s` | send via `--send-cmd` / `--send-agent` (comments are cleared only on success) |
| `d` | delete the comment under the cursor |
| `q` | quit (confirms if there are unsent comments) |
| `Esc` | cancel input / clear selection (never switches modes). With `--esc-quit` enabled it also quits like `q` — but only when nothing is pending (confirmation still guards unsent comments, and the prompt advertises `Esc/q to quit`) |

Mouse: wheel scrolls the view without moving the cursor; click moves the cursor; drag selects a line range; click the title-bar path to copy the full path; click `1/3 files` or the yellow `▌ N` badge to open the overlays.

## The agent loop

akapen assumes a loop of *send comments → the agent edits the file → re-review*:

1. Read the document (rendered), mark up lines, press `s`.
2. The agent edits the file. akapen notices (300 ms debounce) and shows `⚡ file changed — r reload · i ignore`. Nothing is replaced behind your back.
3. Press `r`: the title badge turns into `+N/-M`, and changed lines are highlighted (green `+` gutter for added/modified lines, red `-` for deletions, `▌` markers in view mode) until the next reload.
4. Comments on the reloaded file are cleared (their line anchors refer to the old content); comments on other session files are untouched.

While the comment composer is open, change detection is suspended — a comment being written is never disturbed.

## Output format

One comment = location + line-numbered snippet + body, reviewr-compatible. Blocks are sorted by file and start line, separated by a single blank line:

```
path/to/file.md:12-14
12: text of line 12
13:
14: text of line 14
comment body
```

Each snippet line is prefixed with its real line number, so blank lines inside a selection (`13: `) never collide with the blank lines that separate blocks.

## Sending

`y` copies only. `s` delivers, in one of two ways:

**`--send-agent`** (herdr auto-resolve, recommended): finds the sole agent in the current herdr tab (falling back to the sole workspace agent) and sends via `herdr agent prompt <pane> <text>` with direct argv passing — comments containing `"` or `$` are safe.

**`--send-cmd <cmd>`** (generic): pipes the formatted text to the command's stdin.

```sh
akapen doc.md --send-cmd 'cat >> review.txt'      # append to a file
akapen %S --send-cmd 'xargs -0 -I{} herdr agent prompt wY:p1K {}'
```

On failure (non-zero exit) the comments are **kept** and can be re-sent; on success they are cleared.

### hunk integration

[hunk](https://github.com/modem-dev/hunk) reviews an agent's change set as diffs. The adapter [`scripts/akapen2hunk`](scripts/akapen2hunk) converts akapen's export into hunk live-session comments, so your line comments appear inline in hunk's diff view:

```sh
akapen doc.md --send-cmd 'akapen2hunk [--repo <root>] [--focus]'
```

## Design

Design invariants and their rationale live in [docs/internals.md](docs/internals.md) (Japanese).

- Rust + [ratatui](https://ratatui.rs) 0.30. The event loop drains bursts (up to 64 events per frame) before drawing once — wheel-scroll storms stay smooth even on 70k-line files.
- view mode converts markdown to ratatui `Text` via a vendored, instrumented copy of tui-markdown that preserves source-line attribution, then wraps it with the same width logic as source mode. Cursor positions map exactly between modes.
- Comments live only in memory and the export channel; the file on disk is never written.

## License & credits

MIT License.

Portions are adapted from [herdr-reviewr](https://github.com/persiyanov/herdr-reviewr) (MIT, Copyright (c) 2026 Dmitry Persiyanov): the export block format, syntect line highlighting, the `path:start-end` location format, and the argument-parsing skeleton.

`third_party/tui-markdown` is a vendored copy of [tui-markdown](https://github.com/joshka/tui-markdown) 0.3.9 (MIT OR Apache-2.0, Josh McKinney), instrumented with source-line attribution for akapen. Upstream license texts are included in that directory.
