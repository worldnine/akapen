# akapen

**akapen**（赤ペン, "red pen") — mark up your agent's homework.

A standalone TUI for reviewing documents in the terminal: read markdown beautifully rendered, select lines, attach comments, and send them to your coding agent — like a teacher grading homework with a red pen. Built for the agent review loop: the agent writes, you mark it up, the agent revises, and akapen shows you exactly what changed.

日本語版 README は [README.ja.md](README.ja.md) にあります。

Based on the line-comment experience of [herdr-reviewr](https://github.com/persiyanov/herdr-reviewr), reworked for markdown-first reading in a single-pane, two-mode design.

## Highlights

- **view mode** (default for `.md`): native markdown rendering (tui-markdown / pulldown-cmark) — tables, headings, code blocks, links, footnotes, math, task lists, front matter, in full color. No external renderer process. Tables are **width-adaptive**: when they exceed the pane width, columns shrink (down to their longest unbreakable token) and cells wrap — never truncated, no information loss.
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
akapen <file...> [--send-cmd <cmd> | --send-agent] [--reply] [--theme <name>]
                 [--ime <off|ascii|jp>] [--light|--dark] [--callback <cmd>]
                 [--esc-quit <auto|always|never>]
```

| Flag | Meaning |
|---|---|
| `--send-cmd <cmd>` | `s` pipes the formatted export to this shell command's stdin |
| `--send-agent` | `s` sends to the sole herdr agent in the current tab (needs `herdr` on PATH) |
| `--reply` | reply mode: export as `> quote` + comment (no file/line references), auto-reload on external change, no diff — see [Reply mode](#reply-mode--mark-up-the-agents-chat-output-akp) |
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
| `n` / `N` | jump to the next / previous comment (selects that comment only — overlapping comments stay separate) |
| `]` / `[` | next / previous file in the session |
| `l` | all-comments overlay |
| `Ctrl+p` | file-list overlay |
| `?` | key reference |
| `r` / `i` | on external change: reload / ignore |
| `o` | toggle the hunk under the cursor to its old side vs HEAD (git repositories only; `o` again returns) |
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

## Git integration

When a file is opened **inside a git repository**, akapen reads `git diff HEAD -- <file>` once at startup (and again on each `r` reload) and fuses it into the review UI. Outside a repository — or with no `HEAD` yet — nothing changes.

- **Changed-line marks (3-1)**: added/modified lines get the same green `+` gutter and `▌` marker the reload diff uses (they merge, so session and git changes share one signal); lines immediately after a deleted block get the red `-` position mark — a thin marker only, the deleted content itself is shown with `o`.
- **Old-side toggle (3-2)**: `o` replaces the hunk under the cursor with its HEAD content — old line numbers in source mode, raw dim lines in the rendered view, both with a faint `~` mark. The display stays inside the hunk even when the line counts differ; `o` again (cursor still on the hunk) returns to the new side. The diff base is fixed at `HEAD`, but the loader takes the ref as an argument, so generation movement (`HEAD^`, tags, …) is a one-line change.
- **Untracked files** have no HEAD side: every line counts as added (the whole file is new) and `o` is disabled with an explanation.
- Snapshot semantics: the diff is taken at startup/reload only — mid-session divergence between the file and HEAD is ignored, and there is no line tracking or persistence (P3/P4).

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

## Reply mode — mark up the agent's chat output (akp)

`--reply` turns akapen into a quick red-pen for the agent's *conversation* output — the code examples and markdown the agent pasted into chat, which never touch a file:

```sh
akapen ... --send-agent --reply
```

- **Export format**: the file/line references are dropped and the selected snippet is quoted GitHub-style (`> ` lines); the comment sits on its own line after a blank line (the blank line keeps the comment out of the blockquote — CommonMark lazy continuation). Batches of comments are numbered (`1. > quote` + indented comment), so the receiving agent reads them as a list of distinct points to address in order. The number sits before the `> ` marker, so quoted content that itself contains list markers can never collide.
- **Auto-reload, no diff**: the document reloads automatically when it changes on disk, and the reload skips the diff — in reply mode the doc is a single agent message, so every refresh replaces the whole content.
- **Reply-mode UI**: the title shows `reply` (not the temp path), `]`/`[` moves between the recent messages, `e` (edit) and the file picker are disabled.

The companion script [`scripts/akp`](scripts/akp) builds the document set: it resolves the sole agent in the current herdr tab, extracts the most recent text-bearing assistant messages from the session transcript (pi/claude JSONL located by session id, hermes SQLite), and opens akapen on them. Re-invoking refreshes the documents in place.

### herdr plugin

The repository is also a [herdr plugin](herdr-plugin.toml): `herdr plugin link <this repo>` registers three actions — `akp.open` (bottom split), `akp.open-side` (side split), `akp.open-float` (session popup) — all selectable from the command palette. Each placement toggles (one pane per tab; the pane closes itself when akapen exits) and splits relative to the tab's *agent* pane, not the focused pane.

Convention: herdr integration code lives in the tool's own repo as a plugin (`herdr plugin link`) — new integrations go there, not in `~/.local/bin`.

## Design

Design invariants and their rationale live in [docs/internals.md](docs/internals.md) (Japanese).

- Rust + [ratatui](https://ratatui.rs) 0.30. The event loop drains bursts (up to 64 events per frame) before drawing once — wheel-scroll storms stay smooth even on 70k-line files.
- view mode converts markdown to ratatui `Text` via a vendored, instrumented copy of tui-markdown that preserves source-line attribution, then wraps it with the same width logic as source mode. Cursor positions map exactly between modes.
- Comments live only in memory and the export channel; the file on disk is never written.

## License & credits

MIT License.

Portions are adapted from [herdr-reviewr](https://github.com/persiyanov/herdr-reviewr) (MIT, Copyright (c) 2026 Dmitry Persiyanov): the export block format, syntect line highlighting, the `path:start-end` location format, and the argument-parsing skeleton.

`third_party/tui-markdown` is a vendored copy of [tui-markdown](https://github.com/joshka/tui-markdown) 0.3.9 (MIT OR Apache-2.0, Josh McKinney), instrumented with source-line attribution for akapen. Upstream license texts are included in that directory.
