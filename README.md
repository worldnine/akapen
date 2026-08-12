# akapen

**akapen**（赤ペン, "red pen") — mark up your agent's homework.

A standalone TUI for reviewing documents in the terminal: read markdown beautifully rendered, select lines, attach comments, and send them to your coding agent — like a teacher grading homework with a red pen. Built for the agent review loop: the agent writes, you mark it up, the agent revises, and akapen shows you exactly what changed.

For Markdown, it is also a **document time-machine**: Left/Right moves through one timeline of Git commits and bounded LOCAL snapshots without leaving the rendered document, keeps the nearest heading anchored, streams changed lines in left→right like an LLM, and backspaces removed lines away right→left (dim ghosts without a background) before the layout collapses. History effects use neutral brightness rather than add/delete colors, and the page frame changes while viewing the past. Arrow input scrubs a bottom timeline bar (◆ current / ▮ baseline / ● LOCAL / ◼ COMMIT) and the revision label immediately; Markdown renders once after 300ms idle, and `t` opens the full generation list. Comments made in the past carry the exact revision and historical snippet to the agent.

日本語版 README は [README.ja.md](README.ja.md) にあります。

Based on the line-comment experience of [herdr-reviewr](https://github.com/persiyanov/herdr-reviewr), reworked for markdown-first reading in a single-pane, two-mode design.

## Highlights

- **view mode** (default for `.md`): native markdown rendering (tui-markdown / pulldown-cmark) — tables, headings, code blocks, links, footnotes, math, task lists, front matter, in full color. No external renderer process. Tables are **width-adaptive**: when they exceed the pane width, columns shrink (down to their longest unbreakable token) and cells wrap — never truncated, no information loss.
- **source mode**: raw source with line numbers and syntect highlighting (100+ languages). Long lines wrap with gutter-aligned indentation; tabs expand to 8-column stops.
- **Comment anywhere**: line cursor and range selection work identically in both modes — `v` to select, `c` to comment, without leaving the rendered view. Comments appear as inline cards right under the lines they refer to.
- **Your files are never modified.** akapen is strictly read-only; comments are exported through a separate channel (clipboard, stdout, or a send command).
- **Built for the agent loop**: when the agent edits a file you have open, akapen detects it (`⚡`), stores the loaded generation, and marks present/changed review locations in green and deletion positions in red until you acknowledge them with `a`.
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
                 [--esc-quit <auto|always|never>] [--no-fx]
```

| Flag | Meaning |
|---|---|
| `--send-cmd <cmd>` | `s` pipes the formatted export to this shell command's stdin |
| `--send-agent` | `s` sends to the sole herdr agent in the current tab (needs `herdr` on PATH) |
| `--reply` | reply mode: export as `> quote` + comment (no file/line references), auto-reload on external change, no diff — see [Reply mode](#reply-mode--mark-up-the-agents-chat-output-akp) |
| `--theme <name>` | two-face theme name (default `Catppuccin Mocha`; `Solarized (light)` when light is detected) or a path to a `.tmTheme` file |
| `--ime <off\|ascii\|jp>` | macOS input-source control around the comment composer (default `ascii`) |
| `--light` / `--dark` | force the UI palette (default: auto-detect the terminal background via OSC 11) |
| `--no-fx` | disable the tachyonfx animations: the rotating purple→cyan gradient frame while browsing the past, and the toast fade-in/out (the static history border color and instant toasts stay) |
| `--callback <cmd>` | shell command spawned on exit (e.g. return to a file picker) |
| `--esc-quit <auto\|always\|never>` | whether `Esc` may quit (default `auto`: only with `--callback`; `always` = unconditionally, `never` = Esc stays a pure cancel) |

Typical loop with a picker (see the companion tool [ashiato](https://github.com/worldnine/ashiato), an mtime-sorted file picker):

```sh
ashiato . --open-cmd "akapen {} --send-agent"
```

## Keys

| Key | Action |
|---|---|
| `Left` / `Right` (view / source) | select an older / newer document from oldest `1/N` to present `NOW N/N`; input scrubs labels and the bottom timeline bar, then the final Markdown renders after 300ms idle |
| `t` | open the timeline generation list (browsing also shows a bottom timeline bar: ◆ current / ▮ baseline / ● LOCAL / ◼ COMMIT) |
| `Tab` | toggle view ⇄ source (selection carries over; non-markdown files are source-only) |
| `j` / `k` | move cursor (extends the range while selecting) |
| `g` / `G`, `PgUp` / `PgDn`, `Ctrl+u` / `Ctrl+d` | jump / half-page moves |
| `v` | start selecting lines |
| `c` | comment the selection (or the cursor line); re-selecting an existing comment's exact range edits it |
| `n` / `N` | center the next / previous difference from the review baseline |
| `]` / `[` | next / previous file in the session |
| `?` | key reference |
| `r` / `i` | on external change: reload / ignore |
| `a` | acknowledge NOW, or set the displayed historical generation as the review baseline; clears selection |
| `F7` / `Shift+F7` | alternate next / previous review-mark keys (`]c` / `[c`, `Alt+j` / `Alt+k` also work) |
| `Ctrl+n` / `Ctrl+p` | jump to the next / previous comment (moved from `n` / `N`) |
| `Ctrl+o` | file picker (moved from `Ctrl+p`) |
| `l` | all-comments overlay |
| `e` | edit the file in `$EDITOR` (suspends the TUI, reloads and acknowledges your own edit on return) |
| `y` | copy all comments to the clipboard (comments are kept) |
| `s` | send via `--send-cmd` / `--send-agent` (comments are cleared only on success) |
| `d` | delete the comment under the cursor |
| `q` | quit (confirms if there are unsent comments) |
| `Esc` | cancel input / clear selection (never switches modes). With `--esc-quit` enabled it also quits like `q` — but only when nothing is pending (confirmation still guards unsent comments, and the prompt advertises `Esc/q to quit`) |

Mouse: wheel scrolls the view without moving the cursor; click moves the cursor; drag selects a line range; click the title-bar path to copy the full path; click `1/3 files` or the `▌ N` counter to open the overlays.

## Document time machine and review

akapen treats every version as a complete document. Git commits and bounded LOCAL snapshots share one timeline; Git is optional.

1. Read and comment on the rendered document or its source.
2. When an agent edits the file, akapen shows `⚡`. Press `r` to load the new complete version.
3. The title shows `! N`; green `▌` marks present/changed locations and red `▀` marks deletion positions. Use `n` / `N` to visit them.
4. Repeated reloads accumulate review marks without moving the baseline. Press `a` at NOW to acknowledge them, or press `a` on a historical LOCAL/COMMIT generation to choose that generation as the baseline.

`Left` / `Right` move through the unified timeline while keeping Markdown rendered. The title bar labels each generation as `NOW`, `LOCAL`, or `COMMIT`; holding an arrow scrubs the labels and a bottom timeline bar immediately and renders once input settles — the axis marks ◆ the displayed generation, ▮ the review baseline, ● LOCAL snapshots and ◼ COMMITs, with NOW always at the right edge; it is dim left of the baseline (reviewed history) and normal from the baseline to NOW (the unreviewed stretch), and in view mode it replaces the frame's bottom border so no content rows are lost. `t` opens the full generation list. (The summary tail is clipped so the path never leaves the title.) Git commits whose content matches a LOCAL snapshot are shown once as COMMIT.

The title bar also identifies the baseline as `base N/M` (the footer keeps only the `← older · newer →` navigation), and labels the baseline generation itself as `BASELINE`. While you are in the past, the frame's border runs a rotating purple→cyan gradient (`--fx`, the default) — the time machine is unmissable; `--no-fx` restores the static history border color. Green/red marks compare that fixed baseline with whichever generation is displayed, in both directions through the timeline; browsing those comparisons never changes the unreviewed state at NOW.

LOCAL snapshots are content-addressed, gzip-compressed, and stored outside the repository under the user cache directory. The cache keeps at most 32 generations per file and 256 MiB globally while protecting NOW, the review baseline, unreviewed generations, and generations carrying comments. Review marks are line-granular everywhere — a one-cell table edit marks one row, a one-line paragraph edit marks one line, never the whole block. Non-Markdown UTF-8 files use the same timeline and review model in source mode, also with line-granularity marks.

Comments stay attached to the exact generation they describe when `r` loads a newer file. Comment export contains the revision (for historical generations), resolved absolute file path, line range, quoted source, and the comment—never a diff or hunk.

While the comment composer is open, change detection is suspended so the document is never replaced while you type.

## Output format

One comment = optional historical revision + location + line-numbered snippet + body, reviewr-compatible. Blocks are sorted by file and start line, separated by a single blank line:

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

This is an external compatibility adapter only. akapen itself does not retain, display, or export hunks.

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

The companion script [`scripts/akp`](scripts/akp) builds the document set: it resolves the sole agent in the current herdr tab, extracts the most recent text-bearing assistant messages from the session transcript (pi/claude/codex JSONL located by session id, hermes SQLite), and opens akapen on them. Codex timestamp-prefixed rollout files and its `response_item` / `output_text` records are supported. Re-invoking refreshes the documents in place.

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
