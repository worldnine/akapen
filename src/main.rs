//! akapen — a markdown line-comment TUI.
//!
//! One pane, two modes: `view` (natively rendered markdown) and `source`
//! (raw source with line numbers and syntect highlighting, where comments
//! are anchored). This module is the entry point, the mode state machine,
//! and the event loop.
//!
//! Supports multiple files: `]`/`[` switch between files; per-file cursor,
//! selection, and mode are preserved. `Ctrl+p` shows the file picker and
//! `l` the all-comments list. Non-Markdown files are source-only.

mod comment;
mod config;
mod export;
mod highlight;
mod ime;
mod render;
mod source;
mod theme;
mod view;



use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::comment::{Comment, Selection};
use similar::{ChangeTag, TextDiff};
use crate::config::{Action, Config, EscQuit};
use crate::highlight::{Highlighter, Span as HiSpan, syntax_for, wrap_spans};
use crate::source::Source;
use crate::view::{
    border_color, changed_bg, scroll_offset_at, scroll_offset_drag, scroll_thumb,
    scrollbar_thumb, selected_bg, GutterCell, ViewState,
};

/// The kind of overlay currently open (Ctrl+p = files, `l` = comments,
/// `?` = help).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Overlay {
    /// File picker: switch to another file in the session.
    Files,
    /// All-comments list across every file.
    Comments,
    /// The full key reference (`?`).
    Help,
}

/// Whether `path` can be opened in view mode. The native renderer is
/// tui-markdown (pulldown-cmark), so only Markdown-family files render;
/// everything else (`.rs`, `.toml`, …) is source-only.
fn supports_view(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("md" | "markdown" | "mdx"))
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The 100ms event-poll/tick cadence.
const TICK_MS: u64 = 100;
/// Transient footer messages live this long.
const STATUS_SECS: Duration = Duration::from_secs(4);
/// Debounce for re-rendering the view after a resize.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(300);
/// Debounce for reloading the file after an external (agent) edit — the
/// writer may not be atomic, so wait for the dust to settle.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);
/// Two overlay clicks within this window on the same entry = double-click
/// (activates the selection, like Enter).
const DOUBLE_CLICK_MS: Duration = Duration::from_millis(400);
/// Files above this many lines get a view-mode memory warning (v1: warn only).
const HUGE_FILE: usize = 100_000;

/// Per-file state: everything that is unique to each file in the session.
/// Swapped in/out of the active App fields on file switch.
#[derive(Default)]
struct FileState {
    source: Source,
    spans: Vec<Vec<HiSpan>>,
    view: ViewState,
    mode: Mode,
    offset: usize,
    cursor: usize,
    selection: Option<Selection>,
    line_rows: Vec<usize>,
    base_rows: Vec<usize>,
    content_width: u16,
    gutter_cols: u16,
    file_stamp: Option<(SystemTime, u64)>,
    last_loaded_stamp: Option<(SystemTime, u64)>,
    file_changed: bool,
    reload_pending: Option<Instant>,
    last_change: Option<(usize, usize)>,
    last_added: HashSet<usize>,
    last_deleted_before: HashSet<usize>,
}

fn main() -> Result<()> {
    match Config::from_env()? {
        Action::Help => {
            println!(
                "akapen — read markdown rendered, comment on source lines\n\
                 \n\
                 usage: akapen <file...> [--send-cmd <cmd> | --send-agent] [--theme <name>]\n\
                 \x20                         [--ime <off|ascii|jp>] [--light|--dark]\n\
                 \x20                         [--callback <cmd>]\n\
                 \n\
                 \x20 --send-cmd <cmd>  pipe `s` export to a shell command via stdin\n\
                 \x20 --send-agent      send `s` export to the sole herdr agent in this tab\n\
                 \x20                   (else the sole workspace agent; needs herdr on PATH)\n\
                 \x20 --theme <name>    syntect theme name or path to a .tmTheme file\n\
                 \x20 --ime <off|ascii|jp> input-source control around the composer\n\
                 \x20                   (default ascii; needs swiftc on macOS)\n\
                 \x20 --light           force light mode (default: auto-detect\n\
                 \x20                   the terminal background via OSC 11)\n\
                 \x20 --dark            force dark mode\n\
                 \x20 --callback <cmd>  shell command to spawn on exit\n\
                 \x20                   (e.g. return to a file-picker after quit)\n\
                 \x20 --esc-quit <auto|always|never> whether Esc may quit\n\
                 \x20                   (default auto: only with --callback;\n\
                 \x20                   always = unconditionally, never = Esc\n\
                 \x20                   stays a pure cancel)\n\
                 \n\
                 keys:\n\
                 \x20 view mode:    j/k/arrows scroll, g/G top/bottom, PgUp/PgDn, Ctrl+u/Ctrl+d,\n\
                 \x20                v select, c comment, s send, y copy, d delete, e edit,\n\
                 \x20                r reload, ]/[ next/prev file, l comments, Ctrl+p files,\n\
                 \x20                Tab source mode\n\
                 \x20 source mode:  j/k move, v select, c comment, s send,\n\
                 \x20                y copy, d delete, e edit, r reload, n/N next comment,\n\
                 \x20                ]/[ next/prev file, l comments, Ctrl+p files,\n\
                 \x20                Tab view mode (Markdown files only)\n\
                 \x20 overlays:     j/k move, Enter jump/switch, d delete (comments),\n\
                 \x20                ? help, Esc/q close, click outside close\n\
                 \x20 input:        Enter confirm, Ctrl+j newline, Esc cancel"
            );
            Ok(())
        }
        Action::Version => {
            println!("akapen {VERSION}");
            Ok(())
        }
        Action::Run(config) => run(config),
    }
}

/// The TUI application state.
struct App {
    config: Config,
    /// All files in the session, in argument order.
    files: Vec<PathBuf>,
    /// Current file index into `files` and `file_states`.
    current_file_index: usize,
    /// Per-file state; index mirrors `files`.
    file_states: Vec<FileState>,
    /// The overlay currently open (Ctrl+p files / `l` comments / `?`
    /// help), if any.
    overlay: Option<Overlay>,
    /// Cursor row in the overlay (0-based).
    overlay_cursor: usize,
    /// Scroll offset of the overlay's list (rows): moves only when the
    /// content overflows the panel, so a list that fits never scrolls.
    overlay_offset: usize,
    /// Time + entry of the previous overlay click, for double-click
    /// detection (the second click on the same entry within
    /// [`DOUBLE_CLICK_MS`] activates it — Enter-equivalent).
    last_overlay_click: Option<(Instant, usize)>,
    source: Source,
    /// Pre-tokenized spans, one vec per source line.
    spans: Vec<Vec<HiSpan>>,
    /// The theme (re-rendering and view styles resolve colors from it).
    highlight: Highlighter,
    /// Rendered view-mode rows.
    view: ViewState,
    /// Current mode.
    mode: Mode,
    /// Source-mode scroll offset in display rows.
    offset: usize,
    /// Cursor line, 0-based source index.
    cursor: usize,
    /// Active line selection (`v`), extended with j/k.
    selection: Option<Selection>,
    /// Comment text being typed (mode == Input).
    input: String,
    /// Byte offset of the text cursor inside `input` (always on a char
    /// boundary). Arrows/Home/End move it; insertion and deletion happen
    /// here instead of only at the end.
    input_cursor: usize,
    /// When the composer is RE-editing an existing comment (the selection
    /// matched its range exactly): its index into `comments`. `None` =
    /// adding a new comment.
    editing_comment: Option<usize>,
    /// The 0-based range the input box is anchored to (mode == Input).
    input_start: usize,
    input_end: usize,
    /// The mode to return to when the composer closes: source-mode `c`
    /// returns to Comment, view-mode `c` stays in View (the composer is
    /// drawn inline in the rendered view).
    composer_return: Mode,
    /// Comments added this session, in add order.
    comments: Vec<Comment>,
    /// Transient footer message with its expiry; `true` = an operation
    /// that could not be done (rendered red, and a BEL beep was emitted).
    status: Option<(String, Instant, bool)>,
    /// Pending quit confirmation when unsent comments exist.
    confirm_quit: bool,
    /// Pending external-edit confirmation when unsent comments exist (`e`).
    confirm_edit: bool,
    /// Pending reload confirmation when unsent comments exist (`r`).
    confirm_reload: bool,
    /// Export text to print to stdout at the next loop turn (`s`): the TUI
    /// restores the terminal, prints, and re-enters raw mode.
    /// Cached wrapped row count per source line, for the current width.
    line_rows: Vec<usize>,
    /// Per-source-line wrap counts only (width-dependent cache); `line_rows`
    /// is this plus inline card/composer rows, recomputed each frame.
    base_rows: Vec<usize>,
    /// Content width the `base_rows` cache was built for.
    content_width: u16,
    /// View needs re-rendering (resize) and when that was first noticed.
    view_dirty: bool,
    view_dirty_since: Option<Instant>,
    /// Active while the comment composer is open: forces the Japanese input
    /// source for typing, back to ASCII on drop (see [`crate::ime`]).
    ime_guard: Option<ime::ImeGuard>,
    /// Source line where a left-drag selection started (mouse row select).
    drag_anchor: Option<usize>,
    /// Source-mode gutter width in columns (status + number + space).
    gutter_cols: u16,
    /// Session input-source control: ASCII in command mode, restore on
    /// exit (see [`crate::ime::SessionIme`]).
    ime_session: ime::SessionIme,
    /// Whether `ime_session.force_ascii()` has succeeded yet (the helper is
    /// compiled in the background on first run, so it is retried per tick).
    ime_forced: bool,
    /// The file's mtime+size as of the last poll (agent edits are detected
    /// by comparing against `last_loaded_stamp`).
    file_stamp: Option<(SystemTime, u64)>,
    /// The stamp of the content currently loaded in memory.
    last_loaded_stamp: Option<(SystemTime, u64)>,
    /// When a file change was noticed (and toasted); the badge shows ⚡
    /// until the user reloads with `r`.
    file_changed: bool,

    /// When a file change was noticed; the toast fires after
    /// [`RELOAD_DEBOUNCE`] (and never while the composer is open).
    reload_pending: Option<Instant>,
    /// Lines added/removed by the last reload, shown in the title badge
    /// until the next reload (`+N/-M`).
    last_change: Option<(usize, usize)>,
    /// Lines added or changed by the last reload (0-based indices in the
    /// new file). Highlighted in source mode until the next reload.
    last_added: HashSet<usize>,
    /// Lines in the new file that immediately follow a deletion block
    /// (0-based indices). Marked with a red `-` gutter until next reload.
    last_deleted_before: HashSet<usize>,
    running: bool,
    /// Resolved UI colors for the current `--light` / dark mode.
    ui_selected_bg: Color,
    ui_changed_bg: Color,
    ui_border: Color,
    ui_scrollbar: Color,
    /// Active scrollbar drag: `(start track row, start scroll offset)` —
    /// set on a thumb press, cleared on release (viewport-only scroll, so
    /// the cursor keeps its absolute position).
    scrollbar_drag: Option<(usize, usize)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Mode {
    /// Natively rendered markdown, read-only.
    #[default]
    View,
    /// Raw source with line numbers; comment anchoring.
    Source,
    /// Comment text entry (IME-safe: all keys go to the buffer).
    Input,
}

impl App {
    fn new(
        config: Config,
        source: Source,
        highlight: Highlighter,
        view: ViewState,
        light: bool,
    ) -> Self {
        let spans = highlight.highlight_with(&source.content, syntax_for(&config.files[0]));
        let ime_session = ime::SessionIme::new(config.ime);
        let files = config.files.clone();
        // Non-markdown files open in source mode (view is unavailable).
        let initial_mode = if supports_view(&files[0]) {
            Mode::View
        } else {
            Mode::Source
        };
        Self {
            config,
            files,
            current_file_index: 0,
            file_states: Vec::new(),
            overlay: None,
            overlay_cursor: 0,
            overlay_offset: 0,
            last_overlay_click: None,
            source,
            highlight,
            spans,
            view,
            mode: initial_mode,
            offset: 0,
            cursor: 0,
            selection: None,
            input: String::new(),
            input_cursor: 0,
            editing_comment: None,
            input_start: 0,
            input_end: 0,
            composer_return: Mode::Source,
            comments: Vec::new(),
            status: None,
            confirm_quit: false,
            confirm_edit: false,
            confirm_reload: false,
            line_rows: Vec::new(),
            base_rows: Vec::new(),
            content_width: 0,
            view_dirty: false,
            view_dirty_since: None,
            ime_guard: None,
            drag_anchor: None,
            gutter_cols: 0,
            ime_session,
            ime_forced: false,
            file_stamp: None,
            last_loaded_stamp: None,
            file_changed: false,
            reload_pending: None,
            last_change: None,
            last_added: HashSet::new(),
            last_deleted_before: HashSet::new(),
            running: true,
            ui_selected_bg: selected_bg(light),
            ui_changed_bg: changed_bg(light),
            ui_border: border_color(light),
            ui_scrollbar: scrollbar_thumb(light),
            scrollbar_drag: None,
        }
    }

    /// Set a transient footer message.
    /// Set a transient footer message (info: yellow).
    fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), false));
    }

    /// An operation that could not be done: red toast + a BEL beep (the
    /// terminal's standard "invalid operation" signal — vim beeps on
    /// errors too, and visual-bell settings are the user's choice).
    fn flash_err(&mut self, msg: impl Into<String>) {
        use std::io::Write;
        // BEL must be FLUSHED: Rust's stdout is line-buffered, so without
        // an explicit flush the beep would sit in the buffer forever
        // (no newline ever arrives while the TUI owns the terminal).
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
        self.status = Some((msg.into(), Instant::now(), true));
    }

    /// Rebuild the per-source-line wrap cache when the content width changes.
    /// Inline card/composer rows are folded in by [`App::refresh_line_rows`],
    /// which runs every frame (composer height grows as you type).
    fn ensure_row_cache(&mut self, width: u16) {
        if width == self.content_width && !self.base_rows.is_empty() {
            return;
        }
        self.base_rows = self
            .spans
            .iter()
            .map(|spans| wrap_spans(spans, width as usize).len())
            .collect();
        self.content_width = width;
    }

    /// Fold inline card rows (saved comments) and the composer box (while
    /// typing) into `line_rows`, so scrolling and cursor-following agree
    /// with what the renderer paints.
    fn refresh_line_rows(&mut self) {
        // Comment bars span the whole pane (gutter included). Only the
        // current file's cards count, and never the one being re-edited
        // (its card is hidden under the edit composer).
        let full_width = (self.content_width + self.gutter_cols) as usize;
        let mut extra = vec![0usize; self.base_rows.len()];
        let current = self.current_file_path().to_path_buf();
        for (raw, c) in self.comments.iter().enumerate() {
            if c.file_path != current
                || (self.mode == Mode::Input && self.editing_comment == Some(raw))
            {
                continue;
            }
            let i = (c.end as usize).saturating_sub(1);
            if i < extra.len() {
                extra[i] += card_line_count(c, full_width);
            }
        }
        if self.mode == Mode::Input && self.input_end < extra.len() {
            extra[self.input_end] +=
                composer_line_count(&self.input, self.input_cursor, full_width);
        }
        self.line_rows = self
            .base_rows
            .iter()
            .zip(extra)
            .map(|(b, e)| b + e)
            .collect();
    }

    /// The number of display rows in `width` columns for line `idx`.
    fn rows_of(&self, idx: usize) -> usize {
        self.line_rows.get(idx).copied().unwrap_or(1)
    }

    /// The row where line `idx` starts, given the current cache.
    fn row_of(&self, idx: usize) -> usize {
        self.line_rows[..idx].iter().sum()
    }

    /// The scrollable maximum offset for a viewport `height` rows tall.
    fn max_offset(&self, height: u16) -> usize {
        let total: usize = self.line_rows.iter().sum();
        total.saturating_sub(height as usize)
    }

    /// Keep the cursor line visible; return nothing, mutate `offset`.
    fn keep_cursor_visible(&mut self, height: u16) {
        if self.source.is_empty() || self.line_rows.is_empty() {
            self.offset = 0;
            return;
        }
        let start = self.row_of(self.cursor);
        let end = start + self.rows_of(self.cursor);
        let height = height.max(1) as usize;
        if start < self.offset {
            self.offset = start;
        } else if end > self.offset + height {
            self.offset = end.saturating_sub(height);
        }
    }

    /// While the composer is open, keep its bar on screen. The bar's
    /// height is already folded into `line_rows` (it grows as you type),
    /// so the scroll anchor is the bar's BOTTOM row, not the cursor line:
    /// on the last line the bar cannot fit below the content, and a plain
    /// [`App::keep_cursor_visible`] run on the pre-composer layout leaves
    /// it clipped off-screen. Called every frame while Input is active
    /// (Input mode has no scrolling keys, so a per-frame nudge cannot
    /// fight the user).
    fn keep_composer_visible(&mut self, height: usize) {
        if self.source.is_empty()
            || self.line_rows.is_empty()
            || self.input_end >= self.line_rows.len()
        {
            return;
        }
        let end = self.row_of(self.input_end) + self.rows_of(self.input_end);
        let height = height.max(1);
        if end > self.offset + height {
            self.offset = end.saturating_sub(height);
        }
    }

    /// View-mode pendant of [`App::keep_composer_visible`]: the composer
    /// bar is spliced below the selection's end block, so the view's
    /// scrollable extent is the document PLUS the bar — the bottom stop
    /// extends by the bar's height, or a comment on the LAST line could
    /// never reveal its bar (the old clamp at the document's last row hid
    /// it entirely). Called every frame while the view-origin composer is
    /// open, from [`draw_view`] — before the visible window is built, so
    /// the splice below lands on the adjusted offset.
    fn keep_composer_visible_view(&mut self, height: usize) {
        let height = height.max(1);
        let full_width = view_content_width(self);
        let h = composer_line_count(&self.input, self.input_cursor, full_width);
        let anchor = view_composer_anchor(self);
        let min_offset = (anchor + 1 + h).saturating_sub(height);
        let max_off = self.view.rows.len().saturating_sub(height) + h;
        self.view.offset = self.view.offset.max(min_offset).min(max_off);
    }
}

/// When stdin is not a terminal (xargs gives children /dev/null; scripts
/// redirect it), rebind fd 0 to a real tty so crossterm's event reader
/// can initialize. On macOS this must be the actual pty slave: /dev/tty
/// is a synthetic node that kqueue (mio) rejects with EINVAL, and
/// crossterm's own /dev/tty fallback therefore fails with "Failed to
/// initialize input reader".
fn ensure_terminal_stdin() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return;
    }
    #[cfg(target_os = "macos")]
    if rebind_to_controlling_pty() {
        return;
    }
    // Other platforms (and macOS without a matching pty): the controlling
    // terminal node itself — epoll etc. accept it, so crossterm works.
    if let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        use std::os::fd::AsRawFd;
        // SAFETY: both fds are valid; dup2 replaces fd 0 with the tty.
        unsafe {
            libc::dup2(tty.as_raw_fd(), libc::STDIN_FILENO);
        }
    }
}

/// macOS: find the real pty slave whose foreground process group is ours
/// (the controlling terminal's `tcgetpgrp` == our `getpgrp`) and rebind
/// stdin to it. Returns whether a match was found and dup2'ed.
#[cfg(target_os = "macos")]
fn rebind_to_controlling_pty() -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let me = unsafe { libc::getpgrp() };
    let Ok(rd) = std::fs::read_dir("/dev") else { return false };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("ttys") {
            continue;
        }
        let path = format!("/dev/{name}");
        let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&path)
        else {
            continue;
        };
        if unsafe { libc::tcgetpgrp(f.as_raw_fd()) } == me {
            // SAFETY: valid fds; rebind stdin to the real tty.
            unsafe {
                libc::dup2(f.as_raw_fd(), libc::STDIN_FILENO);
            }
            return true;
        }
    }
    false
}

fn run(config: Config) -> Result<()> {
    // A piped/redirected stdin must not kill the TUI (see above).
    ensure_terminal_stdin();
    // Compile the macOS IME helper in the background so the first composer
    // close never blocks on swiftc (see [`crate::ime::start_background_build`]).
    ime::start_background_build();
    let files = config.files.clone();
    let mut terminal = ratatui::init();
    // Light/dark resolution: --light/--dark win, else the terminal's
    // background is queried (OSC 11). Needs raw mode (init enables it)
    // and must run before the event loop consumes input; unanswerable
    // terminals fall back to dark.
    let light = config
        .light
        .unwrap_or_else(|| theme::detect_light().unwrap_or(false));
    // The syntax theme: --theme wins; a missing or unresolvable name
    // falls back to a default matching light/dark (highlight.rs).
    let highlight = Highlighter::new(config.theme.as_deref(), light);
    let size = terminal.size()?;

    // Load all files into FileState entries eagerly so spans/views are
    // ready before the first frame.
    let mut file_states: Vec<FileState> = Vec::with_capacity(files.len());
    for f in &files {
        let source = Source::load(f.clone())?;
        let view = render_view_with_cards(&source, view_render_width(size.width), &highlight, &[]);
        let spans = highlight.highlight_with(&source.content, syntax_for(f));
        let mut fs = FileState {
            source,
            spans,
            view,
            mode: if supports_view(f) { Mode::View } else { Mode::Source },
            ..Default::default()
        };
        // Record the on-disk stamp so the first poll doesn't treat the file
        // as freshly edited.
        if let Ok(meta) = std::fs::metadata(f) {
            let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
            fs.file_stamp = Some(stamp);
            fs.last_loaded_stamp = Some(stamp);
        }
        file_states.push(fs);
    }

    // Move the first file's loaded state into the live App fields. The
    // emptied slot is never read: switch_to_file always saves the live
    // state back into it before leaving the file.
    let source = std::mem::take(&mut file_states[0].source);
    let spans = std::mem::take(&mut file_states[0].spans);
    let view = std::mem::take(&mut file_states[0].view);

    let mut app = App::new(config, source, highlight, view, light);
    app.spans = spans;
    app.file_states = file_states;
    app.file_stamp = app.file_states[0].file_stamp;
    app.last_loaded_stamp = app.file_states[0].last_loaded_stamp;
    // Command mode always runs in ASCII so j/k etc. are never swallowed by
    // the IME. The first attempt may no-op while the helper compiles on
    // first run; the event loop retries until it sticks.
    app.ime_forced = app.ime_session.force_ascii();
    if app.source.len() > HUGE_FILE {
        app.flash(format!(
            "warning: {} lines — view mode keeps the whole render in memory",
            app.source.len()
        ));
    }
    // The hardware cursor stays hidden for the whole session (modern-TUI
    // pattern, like Hermes/pi.dev): the composer draws its own `▏` glyph,
    // and the IME still anchors its inline composition window to the
    // logical cursor position we keep publishing via set_cursor_position.
    let _ = execute!(std::io::stdout(), Hide);
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = event_loop(&mut terminal, &mut app);
    // Make sure the cursor is visible again after we leave raw mode.
    let _ = execute!(std::io::stdout(), Show);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    // Spawn the `--callback` command (if any) after fully shutting down
    // the TUI, so the callback inherits a clean terminal.
    if let Some(cmd) = &app.config.callback {
        let _ = Command::new("sh").arg("-c").arg(cmd).spawn();
    }
    res
}

/// Events processed per frame at most, so a pathological input flood can't
/// starve the draw (the rest is handled on the next frame).
const MAX_EVENTS_PER_FRAME: usize = 64;

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    // Paint the initial frame before waiting for input.
    terminal.draw(|f| draw(f, app))?;
    loop {
        // Wait up to one tick for input, then drain everything that
        // arrived. A fast wheel flick queues dozens of mouse events, and
        // each one used to trigger its own full redraw — on a large
        // document (or through herdr, which can deliver a burst at once)
        // that made scrolling crawl. Batching the burst into a single
        // frame keeps the wheel responsive.
        if event::poll(Duration::from_millis(TICK_MS))? {
            for _ in 0..MAX_EVENTS_PER_FRAME {
                if !event::poll(Duration::ZERO)? {
                    break;
                }
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        on_key(app, key.code, key.modifiers, Some(terminal))
                    }
                    Event::Mouse(mouse) => on_mouse(app, mouse),
                    Event::Resize(..) => mark_view_dirty(app),
                    _ => {}
                }
            }
        }
        terminal.draw(|f| draw(f, app))?;
        // Resize debounce: re-render the view once the pane settles.
        if app.view_dirty {
            let since = app.view_dirty_since.unwrap_or_else(Instant::now);
            if since.elapsed() >= RESIZE_DEBOUNCE {
                rerender_view(app);
                app.view_dirty = false;
                app.view_dirty_since = None;
            }
        }
        // Keep retrying the session's ASCII force until the helper exists
        // (first run compiles it in the background).
        if !app.ime_forced {
            app.ime_forced = app.ime_session.force_ascii();
        }
        // File-change polling: an external edit (the agent rewriting the
        // doc) is debounced into a toast + ⚡ badge; the reload itself is
        // manual (`r`), so content is never swapped under the user mid-work.
        poll_file_change(app);
        if let Some(since) = app.reload_pending
            && since.elapsed() >= RELOAD_DEBOUNCE
            && app.mode != Mode::Input
        {
            app.reload_pending = None;
            notify_file_changed(app);
        }
        // Expire transient footer messages.
        if app
            .status
            .as_ref()
            .is_some_and(|(_, at, _)| at.elapsed() > STATUS_SECS)
        {
            app.status = None;
        }
        if !app.running {
            return Ok(());
        }
    }
}

fn on_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut ratatui::DefaultTerminal>) {
    // Overlay intercepts its own keys first.
    if app.overlay.is_some() {
        return on_overlay_key(app, key, modifiers);
    }
    match app.mode {
        Mode::Input => on_input_key(app, key, modifiers),
        Mode::View => on_view_key(app, key, modifiers, terminal),
        Mode::Source => on_source_key(app, key, modifiers, terminal),
    }
}

/// Open an overlay, resetting the double-click tracker: a click in a
/// fresh session must never be mistaken for the tail of an old
/// double-click (and a delete may have shifted the entry indices).
fn open_overlay(app: &mut App, kind: Overlay, cursor: usize) {
    app.overlay = Some(kind);
    app.overlay_cursor = cursor;
    app.overlay_offset = 0;
    app.last_overlay_click = None;
}

/// Raw indices of `app.comments` in the overlay's order (file, then
/// start line). The overlay cursor indexes THIS list — Enter, delete,
/// and the drawer all map through it, so they can never disagree with
/// what is highlighted.
fn sorted_comment_indices(app: &App) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..app.comments.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ca, cb) = (&app.comments[a], &app.comments[b]);
        ca.file_path.cmp(&cb.file_path).then(ca.start.cmp(&cb.start))
    });
    idx
}

/// The display rows of the open overlay, top to bottom: `None` = a group
/// header (not selectable), `Some(i)` = the entry index in overlay order
/// (files: the file index; comments: the [`sorted_comment_indices`]
/// position). Shared by the drawer and the scroll math.
fn overlay_rows(app: &App) -> Vec<Option<usize>> {
    match app.overlay {
        Some(Overlay::Files) => (0..app.files.len()).map(Some).collect(),
        Some(Overlay::Comments) => {
            let idx = sorted_comment_indices(app);
            let mut rows = Vec::new();
            let mut last_file: Option<&PathBuf> = None;
            for (pos, &raw) in idx.iter().enumerate() {
                let file = &app.comments[raw].file_path;
                if last_file != Some(file) {
                    last_file = Some(file);
                    rows.push(None); // group header
                }
                rows.push(Some(pos));
            }
            rows
        }
        Some(Overlay::Help) | None => Vec::new(),
    }
}

/// How many list rows the open overlay's panel can show: its inner
/// height minus the title, the blank, and the footer hint rows.
fn overlay_visible_rows() -> usize {
    let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let panel = overlay_panel(Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    });
    panel.height.saturating_sub(5).max(1) as usize
}

/// The comments that render as cards in the CURRENT file right now:
/// only this file's comments, minus the one being re-edited (its card
/// would stack redundantly under the edit composer — the composer
/// already carries the text, so `comment` + `edit` never pile up).
fn visible_cards(app: &App) -> Vec<&Comment> {
    let current = app.current_file_path();
    app.comments
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            c.file_path == *current
                && !(app.mode == Mode::Input && app.editing_comment == Some(*i))
        })
        .map(|(_, c)| c)
        .collect()
}

/// Keep the overlay cursor's row inside the visible window: the offset
/// scrolls only when the content overflows (a list that fits never
/// scrolls).
fn keep_overlay_cursor_visible(app: &mut App) {
    let rows = overlay_rows(app);
    let visible = overlay_visible_rows();
    let max_offset = rows.len().saturating_sub(visible);
    let Some(cursor_row) = rows.iter().position(|r| *r == Some(app.overlay_cursor)) else {
        app.overlay_offset = app.overlay_offset.min(max_offset);
        return;
    };
    if cursor_row < app.overlay_offset {
        app.overlay_offset = cursor_row;
    } else if cursor_row >= app.overlay_offset + visible {
        app.overlay_offset = cursor_row + 1 - visible;
    }
    app.overlay_offset = app.overlay_offset.min(max_offset);
}

/// Handle keys while an overlay (files, comments, or help) is open.
fn on_overlay_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    match app.overlay {
        Some(Overlay::Files) => on_files_overlay_key(app, key, modifiers),
        Some(Overlay::Comments) => on_comments_overlay_key(app, key, modifiers),
        Some(Overlay::Help) => on_help_overlay_key(app, key, modifiers),
        None => {}
    }
}

/// The help overlay (`?`): j/k or the wheel scroll the reference — but
/// only when it overflows the panel (content that fits never scrolls).
/// Esc / q / `?` close it.
fn on_help_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let max = help_rows(app.esc_quit_enabled())
        .len()
        .saturating_sub(overlay_visible_rows());
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            app.overlay_cursor = (app.overlay_cursor + 1).min(max);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1).min(max);
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => app.overlay = None,
        _ => {}
    }
}

/// The file picker (Ctrl+p): j/k move, Enter switches to the file, and
/// Esc / q / Ctrl+p close it. The current file is marked in the list.
/// Enter-equivalent for the overlay selection: files → switch to the
/// file, comments → jump to the comment's file+line. Shared by the Enter
/// key and a double-click (see on_mouse).
fn activate_overlay_selection(app: &mut App) {
    match app.overlay {
        Some(Overlay::Files) => {
            if app.overlay_cursor < app.files.len() {
                app.overlay = None;
                app.switch_to_file(app.overlay_cursor);
            }
        }
        Some(Overlay::Comments) => {
            // Jump to the selected comment's file; its whole range becomes
            // the selection (all lines light up), cursor on the extent.
            // The cursor indexes the SORTED list; map through the indices.
            let idx = sorted_comment_indices(app);
            if let Some(&raw) = idx.get(app.overlay_cursor) {
                let c = &app.comments[raw];
                let target_path = c.file_path.clone();
                let target_start = c.start;
                let target_end = c.end;
                if let Some(pos) = app.files.iter().position(|f| f == &target_path) {
                    app.overlay = None;
                    app.switch_to_file(pos);
                    let last = app.source.len().saturating_sub(1);
                    let start = (target_start.saturating_sub(1) as usize).min(last);
                    let end = (target_end.saturating_sub(1) as usize).min(last);
                    app.selection = Some(Selection {
                        anchor: start,
                        cursor: end,
                    });
                    if app.mode == Mode::View {
                        app.view.goto_source_line(end);
                        app.view.keep_cursor_visible(app.view_viewport_rows());
                    } else {
                        app.cursor = end;
                        app.keep_cursor_visible(app.source_viewport_rows() as u16);
                    }
                }
            }
        }
        Some(Overlay::Help) | None => {}
    }
}

fn on_files_overlay_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    let total = app.files.len();
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 {
                app.overlay_cursor = (app.overlay_cursor + 1).min(total - 1);
                keep_overlay_cursor_visible(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
            keep_overlay_cursor_visible(app);
        }
        KeyCode::Enter => activate_overlay_selection(app),
        // Esc or q closes the picker (q never quits the app while a
        // picker is open); Ctrl+p toggles it closed again.
        KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => app.overlay = None,
        _ => {}
    }
}

/// The all-comments list (`l`): j/k move, Enter jumps to the comment's
/// file+line, `d` deletes it, and Esc / q / `l` close it.
fn on_comments_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let total = app.comments.len();
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 {
                app.overlay_cursor = (app.overlay_cursor + 1).min(total - 1);
                keep_overlay_cursor_visible(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
            keep_overlay_cursor_visible(app);
        }
        KeyCode::Enter => activate_overlay_selection(app),
        KeyCode::Char('d') => {
            // Delete the selected comment. The cursor indexes the SORTED
            // list; map through the indices to the raw vec for removal.
            let idx = sorted_comment_indices(app);
            if let Some(&raw) = idx.get(app.overlay_cursor) {
                app.comments.remove(raw);
                if app.overlay_cursor > 0 && app.overlay_cursor >= app.comments.len() {
                    app.overlay_cursor = app.comments.len().saturating_sub(1);
                }
                keep_overlay_cursor_visible(app);
                replace_view_preserving_cursor(app);
            }
        }
        // Esc or q closes the list; `l` toggles it closed again.
        KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char('l') => app.overlay = None,
        _ => {}
    }
}

/// Mouse handling: wheel scrolls like j/k, left-click moves the cursor,
/// left-drag selects a line range (source mode only). Clicking without
/// dragging never starts a selection, so the mouse stays optional.
fn on_mouse(app: &mut App, mouse: MouseEvent) {
    // With an overlay open the mouse drives the overlay only: a click
    // outside the panel closes it (modal dismiss), a click on an entry
    // selects it, and the wheel moves the selection (j/k semantics).
    // Nothing touches the content underneath.
    if app.overlay.is_some() {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
                let panel = overlay_panel(Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                });
                if mouse.row < panel.y
                    || mouse.row >= panel.y + panel.height
                    || mouse.column < panel.x
                    || mouse.column >= panel.x + panel.width
                {
                    // A click outside dismisses; it can't be part of a
                    // double-click either.
                    app.last_overlay_click = None;
                    app.overlay = None;
                } else if let Some(idx) = overlay_entry_at(app, mouse.row) {
                    // A second click within the window on the SAME entry
                    // is a double-click: it activates the selection (the
                    // Enter-equivalent, e.g. switching the file or
                    // jumping to the comment).
                    let is_double = app.last_overlay_click.is_some_and(
                        |(t, prev)| t.elapsed() < DOUBLE_CLICK_MS && prev == idx,
                    );
                    app.last_overlay_click = Some((Instant::now(), idx));
                    app.overlay_cursor = idx;
                    keep_overlay_cursor_visible(app);
                    if is_double {
                        activate_overlay_selection(app);
                    }
                } else {
                    // Title/header rows: not an entry, not a double-click.
                    app.last_overlay_click = None;
                }
            }
            MouseEventKind::ScrollDown => match app.overlay {
                Some(Overlay::Help) => {
                    let max = help_rows(app.esc_quit_enabled())
                        .len()
                        .saturating_sub(overlay_visible_rows());
                    app.overlay_cursor = (app.overlay_cursor + 1).min(max);
                }
                Some(Overlay::Files) => {
                    if !app.files.is_empty() {
                        app.overlay_cursor = (app.overlay_cursor + 1).min(app.files.len() - 1);
                        keep_overlay_cursor_visible(app);
                    }
                }
                _ => {
                    if !app.comments.is_empty() {
                        app.overlay_cursor =
                            (app.overlay_cursor + 1).min(app.comments.len() - 1);
                        keep_overlay_cursor_visible(app);
                    }
                }
            },
            MouseEventKind::ScrollUp => match app.overlay {
                Some(Overlay::Help) => {
                    let max = help_rows(app.esc_quit_enabled())
                        .len()
                        .saturating_sub(overlay_visible_rows());
                    app.overlay_cursor = app.overlay_cursor.saturating_sub(1).min(max);
                }
                _ => {
                    app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
                    keep_overlay_cursor_visible(app);
                }
            },
            _ => {}
        }
        return;
    }
    // The content area starts right under the title bar (row 0); the view
    // pane's frame adds its top border (source mode draws no frame).
    let content_row = mouse
        .row
        .saturating_sub(if app.view_active() { 2 } else { 1 }) as usize;
    // The scrollbar track is the pane's rightmost column (in view mode it
    // rides the frame's right border, in source mode the pane's own
    // edge). A press on the track jumps the viewport to the clicked row
    // and grabs the thumb; a drag scrubs it — the pointer may leave the
    // column while grabbed (the row clamps). Wheel events fall through to
    // the normal handling below. The track exists only while the content
    // overflows; when everything fits, clicks on the column fall through
    // to the content. Viewport-only scroll, like the wheel: the cursor
    // keeps its absolute position.
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let viewport = if app.view_active() {
        app.view_viewport_rows()
    } else {
        app.source_viewport_rows()
    };
    let content_len = if app.view_active() {
        app.view.rows.len()
    } else {
        app.line_rows.iter().sum()
    };
    let current_offset = if app.view_active() { app.view.offset } else { app.offset };
    // The track column: view mode rides the frame's right border (one
    // column inside it, where the right pad is); source mode has no frame,
    // so the track is the pane's own rightmost column.
    let track_col = if app.view_active() { w - 2 } else { w - 1 };
    let on_track = mouse.column == track_col
        && mouse.row as usize > if app.view_active() { 1 } else { 0 }
        && content_row < viewport
        && scroll_thumb(content_len, viewport, current_offset).is_some();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if on_track => {
            let row = content_row.min(viewport - 1);
            if let Some(target) = scroll_offset_at(content_len, viewport, row) {
                if app.view_active() {
                    app.view.offset = target;
                } else {
                    app.offset = target;
                }
                app.scrollbar_drag = Some((row, target));
            }
            return;
        }
        MouseEventKind::Drag(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            let (start_row, start_offset) = app.scrollbar_drag.unwrap();
            let row = content_row.min(viewport - 1);
            if let Some(target) = scroll_offset_drag(content_len, viewport, start_row, start_offset, row) {
                if app.view_active() {
                    app.view.offset = target;
                } else {
                    app.offset = target;
                }
            }
            return;
        }
        MouseEventKind::Up(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            app.scrollbar_drag = None;
            return;
        }
        _ => {}
    }
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            app.drag_anchor = None;
            // The selection survives scrolling: a selected range stays
            // virtually selected even when the cursor line scrolls off
            // screen (source_scroll moves the viewport only).
            match app.mode {
                Mode::View => app.view.wheel_scroll(1, app.view_viewport_rows()),
                Mode::Source | Mode::Input => source_scroll(app, 1),
            }
        }
        MouseEventKind::ScrollUp => {
            app.drag_anchor = None;
            match app.mode {
                Mode::View => app.view.wheel_scroll(-1, app.view_viewport_rows()),
                Mode::Source | Mode::Input => source_scroll(app, -1),
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // While the composer is open, the mouse must not touch the
            // cursor or the selection: the commented range is pinned
            // (input_start/input_end) and a click/drag rewriting the
            // selection mid-typing leaves a confusing state after Enter.
            // Wheel scrolling above stays available.
            if app.mode == Mode::Input {
                return;
            }
            // The title bar (row 0) is clickable: `▌ N` opens the comment
            // list, `1/3 files` the file picker, and the path copies the
            // full path to the clipboard. The hit regions come from the
            // same layout math draw_title uses.
            if mouse.row == 0 {
                let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
                match title_hit_at(app, w, mouse.column) {
                    Some(TitleHit::CommentCount) => open_overlay(app, Overlay::Comments, 0),
                    Some(TitleHit::FileCount) => {
                        open_overlay(app, Overlay::Files, app.current_file_index);
                    }
                    Some(TitleHit::Path) => {
                        let path = app.current_file_path().display().to_string();
                        match export::copy_to_clipboard(&path) {
                            Ok(()) => app.flash("path copied"),
                            Err(e) => app.flash_err(format!("path copy failed: {e:#}")),
                        }
                    }
                    None => {}
                }
                return;
            }
            if let Some(idx) = source_row_at(app, content_row, mouse.column as usize) {
                app.drag_anchor = Some(idx);
                // A click moves the cursor and drops any pending selection
                // (a drag right after re-creates it) — both modes alike.
                app.selection = None;
                if app.mode == Mode::View {
                    app.view.cursor = idx;
                    let viewport = app.view_viewport_rows();
                    app.view.keep_cursor_visible(viewport);
                } else {
                    app.cursor = idx;
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.mode == Mode::Input {
                return;
            }
            if let (Some(anchor), Some(end)) = (
                app.drag_anchor,
                source_row_at(app, content_row, mouse.column as usize),
            ) {
                app.selection = Some(Selection {
                    anchor,
                    cursor: end,
                });
                if app.mode == Mode::View {
                    app.view.cursor = end;
                    let viewport = app.view_viewport_rows();
                    app.view.keep_cursor_visible(viewport);
                } else {
                    app.cursor = end;
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.drag_anchor = None;
        }
        _ => {}
    }
}

/// Source-mode wheel scroll: the viewport alone moves. The cursor keeps
/// its absolute file position — it may scroll off screen, and scrolling
/// back finds it exactly where it was (herdr-review style). Keyboard j/k,
/// c, and G still pull the viewport to the cursor
/// (`keep_cursor_visible`), and clicking moves the cursor to the clicked
/// line. A selection survives scrolling unchanged.
fn source_scroll(app: &mut App, dir: i32) {
    let viewport = app.source_viewport_rows();
    let max = app.max_offset(viewport as u16);
    let o = (app.offset as i32 + dir).clamp(0, max as i32) as usize;
    app.offset = o;
}

/// Map a content row (0-based, below title bar and frame border) back to
/// the source line under it, in the current mode.
fn source_row_at(app: &App, content_row: usize, col: usize) -> Option<usize> {
    match app.mode {
        Mode::View => {
            let display = app.view.offset + content_row;
            // The text column: the page's left margin, the frame's left
            // border, and the text column's left pad shift the text right
            // of the mouse column (the marker column rides the border
            // itself).
            let text_col = col.saturating_sub(1 + 1 + 1);
            app.view.line_at_position(display, text_col)
        }
        Mode::Source | Mode::Input => {
            let width = source_content_width(app) as usize;
            source_line_at(app, width, content_row)
        }
    }
}

/// Source-mode content width (mirrors `draw_source`'s computation;
/// source mode draws no frame, so the whole terminal width is content
/// except the scrollbar's track on the rightmost column).
fn source_content_width(app: &App) -> u16 {
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let gutter_cols = 1 + app.source.gutter_width as u16 + 1;
    w.saturating_sub(gutter_cols + 1)
}

/// View-mode paragraph width: the terminal minus the page's left margin,
/// the frame's borders, and the text column's 1-column pads on each side
/// (the marker column rides the frame's left border, reserving no width
/// of its own). Mirrors `draw_view`'s `content.width`.
fn view_content_width(_app: &App) -> usize {
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    (w.saturating_sub(5)) as usize
}

/// Source mode: which source line contains `display_row` (0-based content
/// row), walking the same wrap/card/composer layout as `build_rows`.
fn source_line_at(app: &App, width: usize, display_row: usize) -> Option<usize> {
    // Comment bars span the whole pane (gutter included), matching
    // `build_rows`'s full_width.
    let full_width = width + app.gutter_cols as usize;
    // Same card set as build_rows: current file, minus the edited one.
    let cards = visible_cards(app);
    let rows: Vec<usize> = (0..app.source.len())
        .map(|idx| {
            // Wrap rows only — `rows_of` folds the attached bars in, which
            // would double-count the cards below.
            let line_rows = app.base_rows.get(idx).copied().unwrap_or(1);
            let card_rows: usize = cards
                .iter()
                .filter(|c| c.end as usize - 1 == idx)
                .map(|c| card_line_count(c, full_width))
                .sum();
            let composer_rows = if app.mode == Mode::Input && app.input_end == idx {
                composer_line_count(&app.input, app.input_cursor, full_width)
            } else {
                0
            };
            line_rows + card_rows + composer_rows
        })
        .collect();
    index_at_abs(&rows, app.offset, app.offset + display_row)
}

/// Which index's row span (in absolute rows) contains `abs_row`, walking
/// the same wrap/card/composer layout as `build_rows`. The source line AND
/// its attached bars (comment cards, the composer) all map to this index,
/// so a click on a bar selects the line it belongs to instead of drifting
/// by the bar's height. Comparing in absolute rows keeps the mapping stable
/// regardless of the scroll offset (a viewport-relative `display_row` must
/// be converted by the caller: `abs_row = offset + display_row`).
fn index_at_abs(rows: &[usize], offset: usize, abs_row: usize) -> Option<usize> {
    let mut row = 0usize;
    for (idx, &total) in rows.iter().enumerate() {
        if row + total <= offset {
            row += total;
            continue;
        }
        if row > abs_row {
            break;
        }
        if abs_row < row + total {
            return Some(idx);
        }
        row += total;
    }
    None
}

/// Poll the file's mtime+size; a difference from the last loaded stamp
/// arms the (debounced) reload. The poll runs every frame (~100 ms), which
/// is finer than the 500 ms in the design; the 300 ms debounce absorbs
/// burst writes either way.
fn poll_file_change(app: &mut App) {
    let Ok(meta) = std::fs::metadata(app.current_file_path()) else {
        return;
    };
    let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
    app.file_stamp = Some(stamp);
    if app.last_loaded_stamp != Some(stamp) && !app.file_changed && app.reload_pending.is_none() {
        app.reload_pending = Some(Instant::now());
    }
}

/// The debounced notification for an external edit: the persistent ⚡ badge
/// in the title and the footer prompt (`r reload · i ignore`) are the
/// notification — no transient toast (the prompt supersedes it anyway).
fn notify_file_changed(app: &mut App) {
    if app.file_changed {
        return;
    }
    app.file_changed = true;
}

/// Manual reload (`r`), Vim's `:e` model: the user decides when the
/// external edits replace the in-memory content. Unsent comments on THIS
/// file? Require a second `r` (same pattern as edit/quit); the persistent
/// prompt banner (prompt_message) carries the message.
fn reload_now(app: &mut App) {
    // The reload clears this file's comments (stale anchors), so a
    // confirmation protects them exactly like `e` and `q` protect theirs.
    let current = app.current_file_path().to_path_buf();
    let has_comments = app.comments.iter().any(|c| c.file_path == current);
    if has_comments && !app.confirm_reload {
        app.confirm_reload = true;
        return;
    }
    app.confirm_reload = false;

    match reload_source(app) {
        Ok(()) => {
            // Refresh the on-disk stamp so the next poll_file_change won't
            // re-detect the same edit as a pending change.
            if let Ok(meta) = std::fs::metadata(app.current_file_path()) {
                let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
                app.file_stamp = Some(stamp);
                app.last_loaded_stamp = Some(stamp);
            }
            app.file_changed = false;
        }
        // The read failed (non-UTF-8 content, e.g. a binary write or a
        // mid-write agent edit): toast the reason and keep the in-memory
        // content — the ⚡ prompt stays up and the next `r` retries.
        Err(e) => app.flash_err(format!("reload failed: {e:#}")),
    }
}

/// Open the file in `$EDITOR` (fallback `nano`), suspend the TUI while the
/// editor runs, then reload automatically on return. The diff from
/// [`reload_source`] lights up the changes. Triggers on `e` in both modes.
fn open_editor(app: &mut App, terminal: &mut ratatui::DefaultTerminal) {
    // Unsent comments on THIS file? Require a second `e` (same pattern as
    // quit); the persistent prompt banner (prompt_message) carries the
    // message. Other files' comments are unaffected by editing this one.
    let current = app.current_file_path().to_path_buf();
    let has_comments = app.comments.iter().any(|c| c.file_path == current);
    if has_comments && !app.confirm_edit {
        app.confirm_edit = true;
        return;
    }
    app.confirm_edit = false;

    // Clear this file's comments now — the user confirmed the edit.
    if has_comments {
        app.comments.retain(|c| c.file_path != current);
        replace_view_preserving_cursor(app);
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
    // `$EDITOR` may include arguments (e.g. `zed --wait`). Split into the
    // binary and its args, then append the file path last.
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("nano");
    let args: Vec<&str> = parts.collect();

    // Suspend the TUI entirely: `ratatui::restore()` leaves the alternate
    // screen, disables raw mode, and shows the cursor.
    ratatui::restore();

    let status = Command::new(bin)
        .args(&args)
        .arg(app.current_file_path())
        .status();

    // Replace the terminal with a fresh one: `ratatui::init()` re-enters
    // raw mode + alternate screen and returns a properly initialised
    // `DefaultTerminal`. The old terminal's Drop is harmless (it only
    // frees buffers, never touches the terminal).
    *terminal = ratatui::init();
    // `ratatui::init()` does EnterAlternateScreen + EnableMouseCapture,
    // but NOT Hide — the cursor stays visible until we hide it again.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), Hide);

    match status {
        Ok(s) if s.success() => {
            // Reload with diff highlighting — the user just edited the file.
            reload_now(app);
        }
        Ok(_) => app.flash_err(format!("{editor} exited with error")),
        Err(e) => app.flash_err(format!("{editor}: {e}")),
    }

    // Draw immediately — the alternate screen was just re-entered and is
    // blank. The fresh terminal is guaranteed to be in the correct state.
    let _ = terminal.draw(|f| draw(f, app));
}

/// Explicitly skip an external edit (`i`): the on-disk state counts as
/// seen, so the prompt does not re-appear until the next change.
fn ignore_change(app: &mut App) {
    if !app.file_changed {
        return;
    }
    app.file_changed = false;
    app.last_loaded_stamp = app.file_stamp;
    app.flash("file change ignored");
}

/// Approximate line-change counts: strip the common prefix and suffix, then
/// count the remaining old lines as removed and new lines as added. Good
/// enough for the reload toast (+N/-M); not a real diff (that is the
/// v1.5 row-diff feature).
fn line_change_counts(old: &[String], new: &[String]) -> (usize, usize) {
    let mut p = 0usize;
    while p < old.len() && p < new.len() && old[p] == new[p] {
        p += 1;
    }
    let mut s = 0usize;
    while s < old.len().saturating_sub(p) && s < new.len().saturating_sub(p)
        && old[old.len() - 1 - s] == new[new.len() - 1 - s]
    {
        s += 1;
    }
    (new.len() - p - s, old.len() - p - s)
}

/// Re-read the file after an external (agent) edit. Returns `Ok(())` when
/// the file was handled (read successfully — even if the content is
/// unchanged); `Err(e)` when the read failed mid-write (e.g. non-UTF-8
/// bytes), so the caller can surface the reason and the next attempt
/// retries.
///
/// On content change: the old vs new diff is computed for line-level
/// highlighting, all comments are cleared (anchors are stale), and the
/// view re-renders at the same width preserving the cursor fraction.
/// Triggered by `r` only — never while Input is open.
fn reload_source(app: &mut App) -> anyhow::Result<()> {
    let new_source = Source::load(app.current_file_path().to_path_buf())?;
    if new_source.content == app.source.content {
        return Ok(()); // touched but unchanged
    }
    let old_content = app.source.content.clone();
    let old_lines = std::mem::take(&mut app.source.lines);
    let (added, removed) = line_change_counts(&old_lines, &new_source.lines);

    // Diff for line-level highlighting: added/changed lines get a green
    // `+` gutter; lines immediately after a deletion get a red `-`.
    let diff = TextDiff::from_lines(&old_content, &new_source.content);
    let mut changed: HashSet<usize> = HashSet::new();
    let mut deleted_before: HashSet<usize> = HashSet::new();
    let mut new_idx = 0usize;
    let new_len = new_source.lines.len();
    for change in diff.iter_all_changes() {
        let n = change.value().lines().count();
        match change.tag() {
            ChangeTag::Equal => new_idx += n,
            ChangeTag::Insert => {
                for i in 0..n {
                    changed.insert(new_idx + i);
                }
                new_idx += n;
            }
            ChangeTag::Delete => {
                // The line at `new_idx` (or the last line for an
                // end-of-file deletion) follows this deletion block.
                let mark = new_idx.min(new_len.saturating_sub(1));
                if new_len > 0 {
                    deleted_before.insert(mark);
                }
            }
        }
    }
    app.last_added = changed;
    app.last_deleted_before = deleted_before;

    // Comments are anchored to the old content; clear them — but only
    // THIS file's (other files' anchors are untouched by this reload).
    let current = app.current_file_path().to_path_buf();
    let before = app.comments.len();
    app.comments.retain(|c| c.file_path != current);
    let comment_count = before - app.comments.len();

    let width = source_content_width(app);
    app.source = new_source;
    app.spans = app
        .highlight
        .highlight_with(&app.source.content, syntax_for(app.current_file_path()));
    // View: re-render at the same width, keeping the cursor fraction.
    let fraction = app.view.cursor_fraction();
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let mut view = render_view_with_cards(
        &app.source,
        view_render_width(w),
        &app.highlight,
        &app.comments,
    );
    view.goto_fraction(fraction);
    view.keep_cursor_visible(app.view_viewport_rows());
    app.view = view;
    // Comment layout: force the wrap cache to rebuild for the new spans.
    app.content_width = 0;
    app.ensure_row_cache(width);
    app.refresh_line_rows();
    // Cursor clamps to the new length; a selection is stale.
    let last = app.source.len().saturating_sub(1);
    app.cursor = app.cursor.min(last);
    app.selection = None;
    app.offset = app.offset.min(app.max_offset(app.source_viewport_rows() as u16));
    app.file_changed = false;
    app.last_change = Some((added, removed));
    let cleared = if comment_count > 0 {
        format!(" — {comment_count} comment(s) cleared")
    } else {
        String::new()
    };
    app.flash(format!("reloaded (+{added}/-{removed}){cleared}"));
    Ok(())
}

/// The last display row of the rendered block source line `end` belongs
/// to: every row before the first row a LATER source line starts on. A
/// merged paragraph's member lines share one start, so the block runs to
/// the group's last wrapped row; a 1:1 line (an HTML block line, a code
/// line) keeps exactly its own rows. The mapping is exact, so the card
/// and the composer sit directly under the end line — the same position
/// source mode shows — never inside a shared row. (The old heuristic
/// walked forward to the next BLANK row, so a comment ending inside an
/// HTML block dropped its card below the whole block: the two modes
/// disagreed about where the comment sits.)
fn block_last_row(starts: &[usize], n_rows: usize, end: usize) -> usize {
    let s = starts.get(end).copied().unwrap_or(0);
    (end + 1..starts.len())
        .map(|i| starts[i])
        .find(|&n| n > s)
        .unwrap_or(n_rows)
        .saturating_sub(1)
}

/// The display row the view-mode composer (and the card after Enter) sits
/// below: the rendered block bottom of the selection's END line (`input_end`
/// = the range max, so an upward selection still lands at the bottom).
fn view_composer_anchor(app: &App) -> usize {
    block_last_row(&app.view.source_starts, app.view.rows.len(), app.input_end)
}

/// Render the view with the inline comment cards folded into the layout:
/// each comment's card is inserted right after its end line's rendered
/// block, and the source-line mapping for everything below shifts by the
/// card's height (so scroll, cursor follow, and the view↔comment handoff
/// all stay consistent with what is painted).
fn render_view_with_cards(
    source: &Source,
    columns: u16,
    highlighter: &Highlighter,
    comments: &[Comment],
) -> ViewState {
    let mut view = ViewState::render(source, columns, highlighter);
    let mut card_rows = vec![false; view.rows.len()];
    let mut order: Vec<&Comment> = comments.iter().collect();
    order.sort_by_key(|c| c.end);
    for c in order {
        let end = (c.end as usize)
            .saturating_sub(1)
            .min(view.source_starts.len().saturating_sub(1));
        // The card sits below the end line's whole rendered block (a merged
        // paragraph, a heading, a blank line) — never inside it, never
        // above it.
        let last_row = block_last_row(&view.source_starts, view.rows.len(), end);
        let insert_at = last_row + 1;
        let lines = comment_bar_lines(c, columns as usize);
        for s in view.source_starts.iter_mut().skip(end + 1) {
            *s += lines.len();
        }
        for (i, line) in lines.into_iter().enumerate() {
            let spans: Vec<HiSpan> = line
                .spans
                .into_iter()
                .map(|s| HiSpan {
                    text: s.content.to_string(),
                    style: s.style,
                })
                .collect();
            view.rows.insert(insert_at + i, spans);
            card_rows.insert(insert_at + i, true);
            view.row_segments.insert(insert_at + i, Vec::new());
        }
    }
    view.card_rows = card_rows;
    view
}

/// Re-render the view after a comment was added/deleted (or a reload), and
/// keep the cursor line at the same screen row it was on — inserting a card
/// shifts the rows below, so without this the cursor jumps.
fn replace_view_preserving_cursor(app: &mut App) {
    let line = app.view.cursor;
    let screen = app.view.cursor_row() as isize - app.view.offset as isize;
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let width = view_render_width(w);
    // Current file's cards, minus the one being re-edited (hidden under
    // the edit composer).
    let file_comments: Vec<Comment> =
        visible_cards(app).into_iter().cloned().collect();
    let mut view = render_view_with_cards(&app.source, width, &app.highlight, &file_comments);
    view.goto_source_line(line);
    let target = view.cursor_row() as isize - screen;
    let viewport = app.view_viewport_rows();
    let max_off = view.rows.len().saturating_sub(viewport) as isize;
    view.offset = target.clamp(0, max_off.max(0)) as usize;
    app.view = view;
}

/// View → comment handoff. The view cursor is already a source line; a
/// merged paragraph row can cover several source lines at once, so without
/// a selection we land on the head of the row (what a user means by "this
/// line"). With a view-mode selection active, its exact line range is
/// pinned first and carried over unchanged; the comment cursor sits on its
/// extent, so j/k keep extending from where the user left off.
fn enter_source_mode(app: &mut App) {
    // Both directions hand the exact cursor line over (the line-based
    // navigation makes the view cursor meaningful line-by-line), so a
    // Tab round trip never shifts the line.
    if let Some(sel) = app.selection {
        app.cursor = sel.cursor;
    } else {
        app.cursor = app.view.cursor;
    }
    app.mode = Mode::Source;
    // The row cache is built during source-mode drawing; build it now so
    // keep_cursor_visible can walk it (line_rows starts empty, and row_of
    // would panic slicing past its end).
    let width = source_content_width(app);
    app.ensure_row_cache(width);
    app.refresh_line_rows();
    // Keep the cursor line's PHYSICAL screen row where it was in view mode:
    // the two modes wrap at different widths, so without this the
    // highlighted line jumps around on every toggle and the eye loses it.
    // View content sits one row lower (its frame's top border); the +1
    // keeps the terminal row identical across the toggle. Clamps at the
    // document edges (an allowed exception).
    let screen = app.view.cursor_row() as isize - app.view.offset as isize + 1;
    let target = app.row_of(app.cursor) as isize - screen;
    let max_off = app.max_offset(app.source_viewport_rows() as u16) as isize;
    app.offset = target.clamp(0, max_off.max(0)) as usize;
    // Every mode switch forces ASCII so j/k navigation is never swallowed
    // by the IME. No-op when --ime off or the helper hasn't been built yet.
    let _ = app.ime_session.force_ascii();
}

/// Comment → view handoff: land on the exact same source line, mirroring
/// [`enter_source_mode`]'s screen-row preservation so toggling never moves
/// the highlighted line. View is the home mode, so this is where Esc in
/// source mode lands too.
fn enter_view_mode(app: &mut App) {
    // The -1 mirrors enter_source_mode's +1: view content sits one row
    // lower (its frame's top border), so the cursor line keeps its
    // PHYSICAL terminal row across the toggle.
    let screen = if app.line_rows.is_empty() {
        0
    } else {
        app.row_of(app.cursor) as isize - app.offset as isize - 1
    };
    app.view.goto_source_line(app.cursor);
    let target = app.view.cursor_row() as isize - screen;
    let max_off = app.view.rows.len().saturating_sub(app.view_viewport_rows()) as isize;
    app.view.offset = target.clamp(0, max_off.max(0)) as usize;
    // The preserved screen row may leave the cursor's block (wrapped lines
    // or a comment card) clipped at the bottom; reveal it.
    app.view.keep_cursor_visible(app.view_viewport_rows());
    app.mode = Mode::View;
    // Every mode switch forces ASCII so j/k navigation is never swallowed
    // by the IME. No-op when --ime off or the helper hasn't been built yet.
    let _ = app.ime_session.force_ascii();
}

/// Which source lines a comment covers → per-line marker flags for the
/// view's gutter. Only comments for `current_file` are considered.
fn view_marker_flags(comments: &[Comment], n_lines: usize, current_file: &Path) -> Vec<bool> {
    let mut marked = vec![false; n_lines];
    if marked.is_empty() {
        return marked;
    }
    let last = n_lines.saturating_sub(1);
    for c in comments {
        if c.file_path != current_file {
            continue;
        }
        let start = (c.start.saturating_sub(1) as usize).min(last);
        let end = (c.end.saturating_sub(1) as usize).min(last);
        marked[start..=end].fill(true);
    }
    marked
}

/// Per-line flags: which source lines were added or changed by the last
/// reload. Mirrors [`view_marker_flags`] so the view gutter can show a
/// green `▌` for changed lines.
fn view_changed_flags(last_added: &HashSet<usize>, n_lines: usize) -> Vec<bool> {
    let mut changed = vec![false; n_lines];
    for &i in last_added {
        if i < n_lines {
            changed[i] = true;
        }
    }
    changed
}

/// Per-line flags: which source lines immediately follow a deletion block
/// from the last reload. The view gutter shows a red `▌` for them.
fn view_deleted_flags(last_deleted_before: &HashSet<usize>, n_lines: usize) -> Vec<bool> {
    let mut deleted = vec![false; n_lines];
    for &i in last_deleted_before {
        if i < n_lines {
            deleted[i] = true;
        }
    }
    deleted
}

/// `n`/`N` jump targets: each comment on `current_file` as its own
/// (0-based start, end) pair, sorted by start then end. Overlapping
/// comments stay separate targets, so a jump never selects the merged
/// union of several comments — only the lines one comment covers.
/// Identical ranges (stacked comments on the same lines) count once:
/// the selection would be the same either way.
fn comment_regions(comments: &[Comment], n_lines: usize, current_file: &Path) -> Vec<(usize, usize)> {
    if n_lines == 0 {
        return Vec::new();
    }
    let last = n_lines.saturating_sub(1);
    let mut regions: Vec<(usize, usize)> = comments
        .iter()
        .filter(|c| c.file_path == current_file)
        .map(|c| {
            let start = (c.start.saturating_sub(1) as usize).min(last);
            let end = (c.end.saturating_sub(1) as usize).min(last);
            (start, end)
        })
        .collect();
    regions.sort_unstable();
    regions.dedup();
    regions
}

/// Move the view cursor by one CONTENT line: blank source lines are
/// skipped (they render as gap rows — stopping on them is a wasted
/// keypress; the mouse, a selection's j/k extension, and source mode
/// still reach them). Stays put when only blanks remain in that direction.
fn view_move_cursor(app: &mut App, dir: isize) {
    let n = app.source.len() as isize;
    let mut i = app.view.cursor as isize;
    loop {
        i += dir;
        if i < 0 || i >= n {
            return;
        }
        if !app.source.lines[i as usize].trim().is_empty() {
            app.view.goto_source_line(i as usize);
            return;
        }
    }
}

/// View mode: the cursor walks the rendered rows; `v` starts a selection
/// (anchored at the paragraph head), j/k extend it while active, `c`
/// comments the selection, `n`/`N` jump to comment blocks, Tab toggles to
/// source mode carrying the selection over.
fn on_view_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut ratatui::DefaultTerminal>) {
    let viewport = app.view_viewport_rows();
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if app.selection.is_some() {
                extend_view_selection(app, 1);
            } else {
                view_move_cursor(app, 1);
                app.view.keep_cursor_visible(viewport);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selection.is_some() {
                extend_view_selection(app, -1);
            } else {
                view_move_cursor(app, -1);
                app.view.keep_cursor_visible(viewport);
            }
        }
        KeyCode::Char('g') => app.view.jump_top(),
        KeyCode::Char('G') => {
            app.view.jump_bottom();
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::PageDown => {
            app.view.move_cursor_display(viewport as isize);
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::PageUp => {
            app.view.move_cursor_display(-(viewport as isize));
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.view.move_cursor_display((viewport / 2) as isize);
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.view.move_cursor_display(-((viewport / 2) as isize));
            app.view.keep_cursor_visible(viewport);
        }
        // Selection start, parallel to source mode: the cursor line.
        KeyCode::Char('v') => {
            app.selection = Some(Selection::new(app.view.cursor));
        }
        KeyCode::Char('c') => {
            // Comment the view selection (or the cursor line — the exact
            // line, matching the line-based navigation) without leaving
            // view mode: the composer is drawn inline in the rendered view
            // and Enter/Esc return here. The draw pass keeps the bar on
            // screen (keep_composer_visible_view) — including on the last
            // line, where the bar extends past the document's last row.
            app.selection
                .get_or_insert_with(|| Selection::new(app.view.cursor));
            open_composer(app, Mode::View);
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            jump_comment(app, if key == KeyCode::Char('n') { 1 } else { -1 });
        }
        KeyCode::Tab => {
            // View is only reachable for Markdown-family files; a source
            // file (e.g. .rs) never leaves source mode. A mode flip
            // cancels a pending reload confirmation: the prompt's context
            // is the pane the user was looking at.
            app.confirm_reload = false;
            if supports_view(app.current_file_path()) {
                enter_source_mode(app);
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
        }
        // Comment management works from the view too (the parallel model:
        // d/y/s need no mode switch).
        KeyCode::Char('d') => delete_comment_at_cursor(app),
        KeyCode::Char('y') => export_all(app, false),
        KeyCode::Char('s') => export_all(app, true),
        KeyCode::Char('q') => request_quit(app),
        KeyCode::Char('r') => reload_now(app),
        KeyCode::Char('i') => ignore_change(app),
        KeyCode::Char('e') => {
            if let Some(t) = terminal {
                open_editor(app, t);
            }
        }
        KeyCode::Char(']') => {
            let next = (app.current_file_index + 1).min(app.files.len().saturating_sub(1));
            app.switch_to_file(next);
        }
        KeyCode::Char('[') => {
            let prev = app.current_file_index.saturating_sub(1);
            app.switch_to_file(prev);
        }
        // `l` opens the all-comments list; Ctrl+p opens the file picker;
        // `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
        }
        KeyCode::Char('?') => {
            open_overlay(app, Overlay::Help, 0);
        }
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            open_overlay(app, Overlay::Files, app.current_file_index);
        }
        KeyCode::Esc => {
            // View is the home mode: Esc cancels the quit confirmation
            // first, then a pending selection (parallel to source mode).
            // With esc-quit enabled the confirmation is the topmost
            // layer — Esc closes it for real, like a second q — and
            // with nothing pending Esc falls through to the quit path.
            if app.confirm_quit {
                if app.esc_quit_enabled() {
                    app.running = false;
                } else {
                    app.confirm_quit = false;
                    app.flash("quit cancelled");
                }
            } else if app.confirm_edit {
                app.confirm_edit = false;
                app.flash("edit cancelled");
            } else if app.confirm_reload {
                app.confirm_reload = false;
                app.flash("reload cancelled");
            } else if app.selection.take().is_some() {
                app.flash("selection cancelled");
            } else if app.esc_quit_enabled() {
                request_quit(app);
            }
        }
        _ => {}
    }
}

/// Extend the view-mode selection by one SOURCE LINE — byte-for-byte the
/// same semantics as source mode's [`extend_selection`] (the anchor
/// stays, the cursor moves, and the range normalizes — `k` past the
/// anchor grows the range upward). The phrase-precise gray shows the
/// growth even inside merged rows, so the cursor never looks stuck. Tab
/// carries the range unchanged and source mode shows it exactly.
fn extend_view_selection(app: &mut App, dir: isize) {
    let Some(sel) = &mut app.selection else {
        return;
    };
    if app.source.is_empty() {
        return;
    }
    let max = app.source.len() - 1;
    let next = (sel.cursor as isize + dir).clamp(0, max as isize) as usize;
    sel.cursor = next;
    app.view.goto_source_line(next);
    app.view.keep_cursor_visible(app.view_viewport_rows());
}
/// Move the cursor to the next (`n`) or previous (`N`) comment — the same
/// comment landing in both modes. Each comment is its own target:
/// overlapping comments are jumped to one by one, so the selection never
/// spans the merged union of several comments (stacked comments on the
/// same range count once). The comment becomes the selection (its lines
/// light up), so j/k can extend it and d/c act on the range. The cursor
/// lands on the comment's extent (bottom) — the selection model's
/// invariant is cursor == selection extent.
fn jump_comment(app: &mut App, dir: isize) {
    let regions = comment_regions(&app.comments, app.source.len(), app.current_file_path());
    if regions.is_empty() {
        return;
    }
    let cur = if app.mode == Mode::View {
        app.view.cursor as isize
    } else {
        app.cursor as isize
    };
    // Navigate relative to the current selection when there is one (the
    // cursor sits on the extent, which is INSIDE the block — searching
    // from the cursor would re-find the same block); otherwise fall back
    // to the cursor.
    let (low, high) = match app.selection {
        Some(sel) => {
            let (a, b) = sel.range();
            (a as isize, b as isize)
        }
        None => (cur, cur),
    };
    // Targets are individual comments sorted by (start, end), so a jump
    // compares the full pair lexicographically: the next comment is the
    // first one strictly after the current position — overlapping
    // comments come out in line order instead of merging into one block.
    let target = if dir > 0 {
        regions
            .iter()
            .find(|&&(s, e)| (s as isize, e as isize) > (low, high))
            .copied()
    } else {
        regions
            .iter()
            .rev()
            .find(|&&(s, e)| (s as isize, e as isize) < (low, high))
            .copied()
    };
    match target {
        Some((start, end)) => {
            app.selection = Some(Selection {
                anchor: start,
                cursor: end,
            });
            if app.mode == Mode::View {
                app.view.goto_source_line(end);
                app.view.keep_cursor_visible(app.view_viewport_rows());
            } else {
                app.cursor = end;
                app.keep_cursor_visible(app.source_viewport_rows() as u16);
            }
        }
        None => app.flash_err(if dir > 0 {
            "no comments below"
        } else {
            "no comments above"
        }),
    }
}

/// Flag the view for re-rendering after the resize debounce.
fn mark_view_dirty(app: &mut App) {
    app.view_dirty = true;
    if app.view_dirty_since.is_none() {
        app.view_dirty_since = Some(Instant::now());
    }
}

/// The render width: the content pane width, i.e. the terminal width
/// minus the page's left margin, the 2 columns the frame borders take,
/// and the text column's 1-column pads on each side. The marker column
/// rides the frame's left border, so it reserves no width of its own. The
/// render must wrap at exactly the width the pane displays, or the
/// source-line mapping drifts by a couple of columns. Both call sites
/// (startup and resize) go through this so they can never disagree.
fn view_render_width(terminal_width: u16) -> u16 {
    terminal_width.saturating_sub(2).saturating_sub(1).saturating_sub(2)
}

/// Re-render at the current terminal width, preserving the cursor fraction.
fn rerender_view(app: &mut App) {
    if let Ok((width, _)) = ratatui::crossterm::terminal::size() {
        let fraction = app.view.cursor_fraction();
        let width = view_render_width(width);
        let file_comments: Vec<Comment> =
            visible_cards(app).into_iter().cloned().collect();
        let mut view =
            render_view_with_cards(&app.source, width, &app.highlight, &file_comments);
        view.goto_fraction(fraction);
        // Reveal the cursor in the fresh view (draw_view no longer
        // auto-scrolls to it every frame).
        view.keep_cursor_visible(app.view_viewport_rows());
        app.view = view;
    }
}

/// Page-move the comment cursor by `delta` display rows (view-mode spec:
/// the cursor rides the page jump and the viewport follows, instead of a
/// viewport-only scroll that leaves the cursor behind off screen), landing
/// on the source line that owns the target row.
fn source_move_cursor_display(app: &mut App, delta: isize, viewport: u16) {
    if app.source.is_empty() || app.line_rows.is_empty() {
        return;
    }
    let target = (app.row_of(app.cursor) as isize + delta).max(0) as usize;
    let mut row = 0usize;
    let mut line = app.source.len() - 1;
    for (i, &rows) in app.line_rows.iter().enumerate() {
        if target < row + rows {
            line = i;
            break;
        }
        row += rows;
    }
    app.cursor = line;
    app.keep_cursor_visible(viewport);
}

/// Source mode: navigate, select, comment, manage, quit.
fn on_source_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut ratatui::DefaultTerminal>) {
    // `viewport` is the height keep_cursor_visible must scroll against: the
    // comment pane's real content height (draw_source's inner.height), not
    // the raw terminal height — the title bar and footer take those rows,
    // so scrolling against the terminal height fires the down branch
    // `height - viewport` rows late and hides the cursor in the pane's
    // bottom rows.
    let viewport = app.source_viewport_rows() as u16;
    match key {
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            source_move_cursor_display(app, (viewport / 2) as isize, viewport);
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            source_move_cursor_display(app, -((viewport / 2) as isize), viewport);
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.selection.is_some() {
                extend_selection(app, 1, viewport);
            } else {
                move_cursor(app, 1, viewport);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selection.is_some() {
                extend_selection(app, -1, viewport);
            } else {
                move_cursor(app, -1, viewport);
            }
        }
        KeyCode::Char('g') => {
            app.cursor = 0;
            app.offset = 0;
        }
        KeyCode::Char('G') => {
            app.cursor = app.source.len().saturating_sub(1);
            app.keep_cursor_visible(viewport);
        }
        KeyCode::Char('v') => {
            app.selection = Some(Selection::new(app.cursor));
        }
        KeyCode::Char('c') => open_composer(app, Mode::Source),
        KeyCode::Char('d') => delete_comment_at_cursor(app),
        KeyCode::Char('s') => export_all(app, true),
        KeyCode::Char('y') => export_all(app, false),
        KeyCode::Tab => {
            // Non-Markdown files are source-only: Tab is a no-op with a
            // toast instead of rendering raw source as fake markdown.
            // A mode flip cancels a pending reload confirmation: the
            // prompt's context is the pane the user was looking at.
            app.confirm_reload = false;
            if supports_view(app.current_file_path()) {
                enter_view_mode(app);
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
        }
        KeyCode::Esc => {
            // The quit confirmation is the most urgent state: Esc resolves
            // it before anything else (a pending selection otherwise
            // swallows the first Esc and the prompt feels stuck). With
            // esc-quit enabled the confirmation is the topmost layer —
            // Esc closes it for real, like a second q — and with nothing
            // pending Esc falls through to the quit path. Esc never
            // switches modes — Tab is the one toggle (a mode flip from a
            // reflexive Esc lost the reading position).
            if app.confirm_quit {
                if app.esc_quit_enabled() {
                    app.running = false;
                } else {
                    app.confirm_quit = false;
                    app.flash("quit cancelled");
                }
            } else if app.confirm_edit {
                app.confirm_edit = false;
                app.flash("edit cancelled");
            } else if app.confirm_reload {
                app.confirm_reload = false;
                app.flash("reload cancelled");
            } else if app.selection.take().is_some() {
                app.flash("selection cancelled");
            } else if app.esc_quit_enabled() {
                request_quit(app);
            }
        }
        KeyCode::PageDown => {
            source_move_cursor_display(app, viewport as isize, viewport);
        }
        KeyCode::PageUp => {
            source_move_cursor_display(app, -(viewport as isize), viewport);
        }
        KeyCode::Char('q') => request_quit(app),
        KeyCode::Char('r') => reload_now(app),
        KeyCode::Char('i') => ignore_change(app),
        KeyCode::Char('e') => {
            if let Some(t) = terminal {
                open_editor(app, t);
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            jump_comment(app, if key == KeyCode::Char('n') { 1 } else { -1 });
        }
        KeyCode::Char(']') => {
            let next = (app.current_file_index + 1).min(app.files.len().saturating_sub(1));
            app.switch_to_file(next);
        }
        KeyCode::Char('[') => {
            let prev = app.current_file_index.saturating_sub(1);
            app.switch_to_file(prev);
        }
        // `l` opens the all-comments list; Ctrl+p opens the file picker;
        // `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
        }
        KeyCode::Char('?') => {
            open_overlay(app, Overlay::Help, 0);
        }
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            open_overlay(app, Overlay::Files, app.current_file_index);
        }
        _ => {}
    }
}

/// Byte offset of the composer cursor's logical-line start (just after
/// the previous `\n`, or 0).
fn input_line_start(s: &str, cursor: usize) -> usize {
    s[..cursor].rfind('\n').map_or(0, |i| i + 1)
}

/// Byte offset of the composer cursor's logical-line end (just before
/// the next `\n`, or the text end).
fn input_line_end(s: &str, cursor: usize) -> usize {
    s[cursor..].find('\n').map_or(s.len(), |i| cursor + i)
}

/// `from` advanced by up to `col` characters, capped at `to`.
fn advance_chars(s: &str, from: usize, to: usize, col: usize) -> usize {
    s[from..to]
        .char_indices()
        .nth(col)
        .map(|(i, _)| from + i)
        .unwrap_or(to)
}

/// Move the composer cursor one logical line up (`dir < 0`) or down,
/// keeping the character column (clamped to the target line's length).
/// At the first/last line the cursor jumps to the text start/end — the
/// standard textarea fallback.
fn input_move_line(s: &str, cursor: usize, dir: isize) -> usize {
    let start = input_line_start(s, cursor);
    let col = s[start..cursor].chars().count();
    if dir < 0 {
        if start == 0 {
            return 0;
        }
        // `start - 1` is the previous line's `\n`; its line starts after
        // the newline before THAT.
        let prev_start = input_line_start(s, start - 1);
        advance_chars(s, prev_start, start - 1, col)
    } else {
        let end = input_line_end(s, cursor);
        if end == s.len() {
            return s.len();
        }
        let next_start = end + 1;
        let next_end = input_line_end(s, next_start);
        advance_chars(s, next_start, next_end, col)
    }
}

/// Input mode: printable keys insert at the text cursor (IME-safe);
/// arrows / Home / End (and Ctrl+a / Ctrl+e) move it, Backspace / Delete
/// edit around it, Ctrl+j inserts a newline, Enter confirms, Esc cancels.
fn on_input_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    // Guard: `input` can be replaced wholesale (tests set it directly;
    // future features might too). Keep the cursor inside it, on a char
    // boundary, before any positional edit.
    let mut cur = app.input_cursor.min(app.input.len());
    while !app.input.is_char_boundary(cur) {
        cur -= 1;
    }
    app.input_cursor = cur;
    match key {
        KeyCode::Enter => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                app.flash_err("empty comment — not added (Esc to cancel)");
                return;
            }
            if let Some(idx) = app.editing_comment {
                // Re-edit: replace the existing comment (and refresh its
                // snippet from the current source).
                if let Some(c) = app.comments.get_mut(idx) {
                    c.text = text;
                    c.lines = app.source.snippet(c.start, c.end);
                }
                app.flash(format!("comment updated ({} total)", app.comments.len()));
            } else {
                app.comments.push(Comment {
                    file_path: app.current_file_path().to_path_buf(),
                    start: app.input_start as u32 + 1,
                    end: app.input_end as u32 + 1,
                    lines: app
                        .source
                        .snippet(app.input_start as u32 + 1, app.input_end as u32 + 1),
                    text,
                });
                app.flash(format!("comment added ({} total)", app.comments.len()));
            }
            app.editing_comment = None;
            // The view folds cards into its layout; add the new one so a
            // later Tab to view shows it in place.
            replace_view_preserving_cursor(app);
            // Return to where the composer was opened from: source mode
            // (card visible) or view mode (markers update in place).
            app.mode = app.composer_return;
            // The selection was consumed by the comment: clear it so a
            // following j/k moves the cursor instead of extending a range.
            app.selection = None;
            // Back to ASCII so j/k is never captured by the IME.
            app.ime_guard = None;
        }
        KeyCode::Esc => {
            app.input.clear();
            app.input_cursor = 0;
            app.mode = app.composer_return;
            app.ime_guard = None;
            // A re-edit rendered the view without the edited card
            // (open_composer / the Tab flip); cancelling puts it back.
            if app.editing_comment.take().is_some() {
                replace_view_preserving_cursor(app);
            }
            app.flash("comment cancelled");
        }
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'j' => {
            app.input.insert(app.input_cursor, '\n');
            app.input_cursor += 1;
        }
        // Readline-style line jumps (Ctrl+a/e mirror Home/End).
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'a' => {
            app.input_cursor = input_line_start(&app.input, app.input_cursor);
        }
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'e' => {
            app.input_cursor = input_line_end(&app.input, app.input_cursor);
        }
        KeyCode::Home => {
            app.input_cursor = input_line_start(&app.input, app.input_cursor);
        }
        KeyCode::End => {
            app.input_cursor = input_line_end(&app.input, app.input_cursor);
        }
        KeyCode::Left => {
            if let Some((i, _)) = app.input[..app.input_cursor].char_indices().next_back() {
                app.input_cursor = i;
            }
        }
        KeyCode::Right => {
            if let Some(ch) = app.input[app.input_cursor..].chars().next() {
                app.input_cursor += ch.len_utf8();
            }
        }
        KeyCode::Up => {
            app.input_cursor = input_move_line(&app.input, app.input_cursor, -1);
        }
        KeyCode::Down => {
            app.input_cursor = input_move_line(&app.input, app.input_cursor, 1);
        }
        KeyCode::Tab => {
            // The composer is mode-independent: Tab flips the pane under it
            // (rendered view ⇄ raw source) while typing continues — the
            // target range stays pinned, only the representation changes.
            // Non-Markdown files are source-only: the flip to view is a
            // no-op there.
            if app.composer_return == Mode::View {
                enter_source_mode(app);
                app.composer_return = Mode::Source;
            } else if supports_view(app.current_file_path()) {
                enter_view_mode(app);
                app.composer_return = Mode::View;
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
            // enter_*_mode set the mode for the plain toggle; composing
            // continues, so stay in Input.
            app.mode = Mode::Input;
            // Flipping to view while re-editing: the view may still carry
            // the edited card (it was rendered before the edit began, or
            // while the composer sat over source mode) — hide it now.
            if app.composer_return == Mode::View && app.editing_comment.is_some() {
                replace_view_preserving_cursor(app);
            }
        }
        KeyCode::Char(c) => {
            app.input.insert(app.input_cursor, c);
            app.input_cursor += c.len_utf8();
        }
        KeyCode::Backspace => {
            if let Some((i, _)) = app.input[..app.input_cursor].char_indices().next_back() {
                app.input.remove(i);
                app.input_cursor = i;
            }
        }
        KeyCode::Delete => {
            if app.input_cursor < app.input.len() {
                app.input.remove(app.input_cursor);
            }
        }
        _ => {}
    }
}

/// Open the inline composer anchored to the current selection (or cursor
/// line). Shared by source-mode `c` and view-mode `c` (which selects the
/// line first).
fn open_composer(app: &mut App, return_to: Mode) {
    if app.source.is_empty() {
        app.flash_err("empty file — nothing to comment");
        return;
    }
    let (start, end) = app
        .selection
        .map(|s| s.range())
        .unwrap_or((app.cursor, app.cursor));
    // An EXACT range match flips the composer into re-edit mode: the
    // comment's text is prefilled and Enter replaces it instead of adding
    // a stacked duplicate. Any other range adds a new comment.
    let current = app.current_file_path().to_path_buf();
    let edit_idx = app.comments.iter().position(|c| {
        c.file_path == current
            && (c.start.saturating_sub(1) as usize) == start
            && (c.end.saturating_sub(1) as usize) == end
    });
    app.editing_comment = edit_idx;
    app.input = edit_idx
        .and_then(|i| app.comments.get(i))
        .map(|c| c.text.clone())
        .unwrap_or_default();
    // The text cursor starts at the end (prefilled text stays intact;
    // arrows/Home/End move into it).
    app.input_cursor = app.input.len();
    app.input_start = start;
    app.input_end = end;
    app.composer_return = return_to;
    app.mode = Mode::Input;
    // Editing from view mode: re-render right away so the edited comment's
    // card disappears (the composer replaces it — no stacked bars). This
    // must run AFTER the mode flip: visible_cards excludes the edited
    // card only while Input is active.
    if edit_idx.is_some() && return_to == Mode::View {
        replace_view_preserving_cursor(app);
    }
    // Optionally force Japanese for typing (--ime=jp); always back to
    // ASCII on close (dropped below) so j/k is never captured.
    app.ime_guard = Some(ime::ImeGuard::enter(app.config.ime));
    // Keep the inline input box on screen (first approximation on the
    // pre-composer layout; the draw pass's per-frame nudge —
    // keep_composer_visible / keep_composer_visible_view — keeps the
    // whole bar visible as the text grows, including on the last line).
    app.cursor = end;
    app.keep_cursor_visible(app.source_viewport_rows() as u16);
}

fn extend_selection(app: &mut App, delta: isize, height: u16) {
    let Some(sel) = &mut app.selection else {
        return;
    };
    if app.source.is_empty() {
        return;
    }
    let max = app.source.len() - 1;
    let next = (sel.cursor as isize + delta).clamp(0, max as isize) as usize;
    sel.cursor = next;
    app.cursor = next;
    app.keep_cursor_visible(height);
}

/// Delete every comment whose anchored range contains the cursor line.
fn delete_comment_at_cursor(app: &mut App) {
    // Both modes delete at their own cursor (the two cursors are the same
    // source line right after a Tab, but drift apart as each mode moves).
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    let before = app.comments.len();
    let current_file = app.current_file_path().to_path_buf();
    app.comments.retain(|c| !(c.file_path == current_file && c.covers(line)));
    let removed = before - app.comments.len();
    if removed > 0 {
        // The view folds the cards into its layout; drop the deleted ones.
        replace_view_preserving_cursor(app);
        app.flash(format!("deleted {removed} comment(s)"));
    } else {
        app.flash_err("no comment on this line");
    }
}

/// Quit; with unsent comments, require a second `q` (Esc cancels).
fn request_quit(app: &mut App) {
    if app.comments.is_empty() {
        app.running = false;
        return;
    }
    if app.confirm_quit {
        app.running = false;
    } else {
        app.confirm_quit = true;
        // The persistent prompt banner (prompt_message) carries the
        // message; a duplicate toast is not needed.
    }
}

/// Export all comments. `y` copies to the clipboard; `s` additionally
/// delivers via `--send-cmd` (e.g. a herdr pane) and clears the slate —
/// but ONLY on success: a
/// failed send keeps the comments for a retry, and nothing is queued for
/// stdout (terminal output after quit was confusing; pipe via the send
/// command instead, e.g. `--send-cmd "cat >> review.txt"`).
fn export_all(app: &mut App, send: bool) {
    if app.comments.is_empty() {
        app.flash_err("no comments yet");
        return;
    }
    let text = export::format_all(&app.comments);
    let mut parts: Vec<String> = Vec::new();
    let mut had_error = false;
    match export::copy_to_clipboard(&text) {
        Ok(()) => parts.push(format!("copied {} comment(s)", app.comments.len())),
        Err(e) => {
            parts.push(format!("clipboard failed: {e:#}"));
            had_error = true;
        }
    }
    if send {
        match app.config.send_cmd.clone() {
            Some(cmd) => match export::send_command(&cmd, &text) {
                Ok(()) => {
                    let count = app.comments.len();
                    app.comments.clear();
                    replace_view_preserving_cursor(app);
                    parts.push(format!("sent via {cmd} · {count} comment(s) cleared"));
                }
                Err(e) => {
                    parts.push(format!("send failed: {e:#} · comments kept"));
                    had_error = true;
                }
            },
            // `--send-agent`: resolve the sole herdr agent in the current
            // tab (else workspace) and submit directly — argv, no shell,
            // so quotes in the export can never break the delivery.
            None if app.config.send_agent => {
                match export::resolve_agent_pane() {
                    Ok(target) => match export::send_to_agent(&target, &text) {
                        Ok(()) => {
                            let count = app.comments.len();
                            app.comments.clear();
                            replace_view_preserving_cursor(app);
                            parts.push(format!(
                                "sent to {target} · {count} comment(s) cleared"
                            ));
                        }
                        Err(e) => {
                            parts.push(format!("send failed: {e:#} · comments kept"));
                            had_error = true;
                        }
                    },
                    Err(e) => {
                        parts.push(format!("no agent: {e:#} · comments kept"));
                        had_error = true;
                    }
                }
            }
            None => {
                parts.push("no --send-cmd/--send-agent — nothing sent (y copies)".into());
                had_error = true;
            }
        }
    }
    let joined = parts.join(" · ");
    if had_error {
        app.flash_err(joined);
    } else {
        app.flash(joined);
    }
}

fn move_cursor(app: &mut App, delta: isize, height: u16) {
    if app.source.is_empty() {
        return;
    }
    let max = app.source.len() - 1;
    let next = (app.cursor as isize + delta).clamp(0, max as isize) as usize;
    app.cursor = next;
    app.keep_cursor_visible(height);
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, app: &mut App) {
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ]);
    let [title, body, footer] = layout.areas(f.area());
    draw_title(f, title, app);
    match app.mode {
        Mode::Input => {
            if app.composer_return == Mode::View {
                draw_view(f, body, app)
            } else {
                draw_source(f, body, app)
            }
        }
        Mode::View => draw_view(f, body, app),
        Mode::Source => draw_source(f, body, app),
    }
    draw_footer(f, footer, app);
    if app.overlay.is_some() {
        draw_overlay(f, app);
    }
    // Toasts and prompts float above everything (overlays included): a
    // notification never displaces content. Prompts sit at the top,
    // transient toasts at the bottom — different positions, so they can
    // coexist without clashing.
    draw_prompt(f, app);
    draw_toast(f, app);
}

/// Smart-truncate a path for the title bar: keep the basename whole, add
/// directory components from the right while they fit, and collapse the
/// dropped remainder to `…/`. Never exceeds `max_cols` display columns
/// (unicode-aware, so CJK file names never overflow).
fn truncate_path(path: &Path, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if max_cols == 0 {
        return String::new();
    }
    let disp = path.display().to_string();
    if UnicodeWidthStr::width(disp.as_str()) <= max_cols {
        return disp;
    }
    // Prefer shorter spellings: cwd-relative, then ~-shortened.
    let mut candidates: Vec<String> = vec![disp.clone()];
    let cwd_rel = std::env::current_dir().ok().and_then(|cwd| {
        path.strip_prefix(&cwd)
            .ok()
            .map(|r| r.to_string_lossy().trim_start_matches('/').to_string())
            .filter(|r| !r.is_empty())
    });
    if let Some(rel) = cwd_rel {
        candidates.push(rel);
    }
    let home_rel = std::env::var("HOME")
        .ok()
        .and_then(|home| disp.strip_prefix(&home).map(|rest| format!("~{rest}")));
    if let Some(h) = home_rel {
        candidates.push(h);
    }
    let width = |s: &str| UnicodeWidthStr::width(s);
    let base = match candidates
        .iter()
        .filter(|c| width(c) <= max_cols)
        .max_by_key(|c| width(c))
    {
        Some(b) => b.clone(),
        None => candidates
            .into_iter()
            .min_by_key(|c| width(c))
            .unwrap_or(disp),
    };
    if width(&base) <= max_cols {
        return base;
    }
    // Drop directory components from the left; the basename stays whole.
    let components: Vec<&str> = base.split('/').collect();
    let file = components.last().copied().unwrap_or("");
    let dirs = &components[..components.len().saturating_sub(1)];
    let file_w = width(file);
    if file_w > max_cols {
        // Even the basename alone is too wide: clip it with a trailing `…`.
        return clip_ellipsis(file, max_cols);
    }
    // Re-add dirs from the right while the `…/` prefix still fits.
    let mut out = file.to_string();
    for d in dirs.iter().rev() {
        let candidate = format!("{d}/{out}");
        if width(&candidate) + 2 <= max_cols {
            out = candidate;
        } else {
            break;
        }
    }
    if out != file {
        format!("…/{out}")
    } else if !dirs.is_empty() && file_w + 2 <= max_cols {
        // The file name is deep in a tree and there is room for the marker:
        // `…/name.md` reads better than a bare `name.md`.
        format!("…/{file}")
    } else {
        out
    }
}

/// Clip `s` from the left to `max_cols` columns and end with `…`.
fn clip_ellipsis(s: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max_cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        if w + cw > max_cols.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if !s.is_empty() && w < max_cols {
        out.push('…');
    }
    out
}

/// What clicking a title-bar element does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TitleHit {
    /// The file path: copy the full path to the clipboard.
    Path,
    /// The `1/3 files` counter: open the file picker.
    FileCount,
    /// The `▌ N` counter: open the comment list.
    CommentCount,
}

/// Title-bar layout: the x extents of every clickable element, computed
/// once and shared by [`draw_title`] and the mouse hit-testing so a click
/// always lands exactly on what is drawn.
#[derive(Debug)]
struct TitleMetrics {
    /// The change badge (⚡ / +N/-M) and its width.
    change: String,
    change_w: u16,
    /// The truncated path text (click → copy full path).
    path: String,
    path_w: u16,
    /// The `1/3 files` counter (click → file picker) and its x/width.
    file_count: String,
    file_count_x: u16,
    file_count_w: u16,
    /// The y/s hint (not clickable) and its x/width.
    ys: String,
    ys_x: u16,
    ys_w: u16,
    /// Whether the y/s hint is shown (it yields to a long path).
    show_ys: bool,
    /// The `▌ N` counter (click → comment list) and its x/width.
    indicator: String,
    indicator_x: u16,
    indicator_w: u16,
    /// The `esc close` badge (esc-quit enabled only, not clickable) and
    /// its x/width. Flush right, to the right of the counter.
    esc_close: String,
    esc_close_x: u16,
    esc_close_w: u16,
}

/// The layout math for the title bar, shared by drawing and hit-testing
/// so they can never disagree.
fn title_metrics(app: &App, width: u16) -> TitleMetrics {
    let indicator = if app.comments.is_empty() {
        String::new()
    } else {
        format!(" ▌ {} ", app.comments.len())
    };
    let indicator_w = UnicodeWidthStr::width(indicator.as_str()) as u16;
    // `esc close`: the esc-quit affordance, shown only while it is real
    // (esc-quit enabled, and not composing — Esc cancels the composer
    // there). It is the top-right element, right of the comment counter;
    // the path yields to it before the y/s hint does.
    let esc_close = if app.esc_quit_enabled() && app.mode != Mode::Input {
        " esc close ".to_string()
    } else {
        String::new()
    };
    let esc_close_w = UnicodeWidthStr::width(esc_close.as_str()) as u16;
    // The y/s explainer reads the room: with no comments there is nothing
    // to copy or send (both keys flash "no comments yet"), and without a
    // send target (--send-cmd or --send-agent) there is nothing to send —
    // show only what is actionable. Still low priority: it right-aligns
    // before the indicator only when the path keeps ≥16 columns, and it
    // is hidden while composing (y/s are typing keys there).
    let ys = if app.mode == Mode::Input || app.comments.is_empty() {
        String::new()
    } else {
        let mut parts = vec!["y copy"];
        if app.config.send_cmd.is_some() || app.config.send_agent {
            parts.push("s send");
        }
        format!(" {}", parts.join(" · "))
    };
    let ys_w = UnicodeWidthStr::width(ys.as_str()) as u16;
    let cluster_w = indicator_w + ys_w + esc_close_w + 1;
    let ys_x = width.saturating_sub(cluster_w);
    let show_ys = ys_w > 0 && ys_x >= 16;
    let path_area_end = if show_ys {
        ys_x
    } else {
        width.saturating_sub(indicator_w + esc_close_w)
    };
    // While an external edit is pending the title shows ⚡; after a reload
    // it shows the +N/-M from that reload until the next change.
    let change = if app.file_changed {
        " ⚡ ".to_string()
    } else if let Some((a, r)) = app.last_change {
        format!(" +{a}/-{r} ")
    } else {
        String::new()
    };
    let change_w = UnicodeWidthStr::width(change.as_str()) as u16;
    // `1/3 files`: the current position in the session, matching the ]/[
    // navigation model (vim's `1/3` in the arg list). Hidden for a single
    // file. Click → file picker.
    let file_count = if app.files.len() > 1 {
        format!(
            " {}/{} files",
            app.current_file_index + 1,
            app.files.len()
        )
    } else {
        String::new()
    };
    let file_count_w = UnicodeWidthStr::width(file_count.as_str()) as u16;
    let path_max = path_area_end.saturating_sub(change_w + file_count_w + 1);
    let path = truncate_path(app.current_file_path(), path_max as usize);
    let path_w = UnicodeWidthStr::width(path.as_str()) as u16;
    TitleMetrics {
        change,
        change_w,
        path,
        path_w,
        file_count_x: change_w + 1 + path_w,
        file_count,
        file_count_w,
        ys_x,
        ys,
        ys_w,
        show_ys,
        indicator_x: width.saturating_sub(indicator_w + esc_close_w),
        indicator,
        indicator_w,
        esc_close_x: width.saturating_sub(esc_close_w),
        esc_close,
        esc_close_w,
    }
}

/// Which title-bar element the column `x` hits, using the same layout
/// math as [`title_metrics`].
fn title_hit_at(app: &App, width: u16, x: u16) -> Option<TitleHit> {
    let m = title_metrics(app, width);
    if m.indicator_w > 0 && x >= m.indicator_x && x < m.indicator_x + m.indicator_w {
        Some(TitleHit::CommentCount)
    } else if m.file_count_w > 0 && x >= m.file_count_x && x < m.file_count_x + m.file_count_w {
        Some(TitleHit::FileCount)
    } else if m.path_w > 0 && x > m.change_w && x < m.change_w + 1 + m.path_w {
        Some(TitleHit::Path)
    } else {
        None
    }
}

/// The title is file-centric: path + change state + session position. The
/// mode badge lives in the footer (statusline convention). The top-right
/// corner carries the comment-presence indicator with the `esc close`
/// badge to its right (esc-quit only); the y/s explainer sits between
/// (low priority, vanishes when the path needs the room). The path, the
/// file counter, and the comment counter are clickable buttons (see
/// on_mouse).
fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let m = title_metrics(app, area.width);
    if m.change_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.change,
                Style::default().fg(Color::Yellow),
            ))),
            Rect {
                x: area.x,
                y: area.y,
                width: m.change_w,
                height: 1,
            },
        );
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::raw(m.path))),
        Rect {
            x: area.x + m.change_w + 1,
            y: area.y,
            width: m.path_w,
            height: 1,
        },
    );
    if m.file_count_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.file_count,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: area.x + m.file_count_x,
                y: area.y,
                width: m.file_count_w,
                height: 1,
            },
        );
    }
    if m.show_ys {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.ys,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: area.x + m.ys_x,
                y: area.y,
                width: m.ys_w,
                height: 1,
            },
        );
    }
    if m.indicator_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.indicator,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: area.x + m.indicator_x,
                y: area.y,
                width: m.indicator_w,
                height: 1,
            },
        );
    }
    // The esc-quit affordance: a filled badge (dark gray background) at
    // the very top-right. It is not clickable; Esc does the work itself.
    if m.esc_close_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.esc_close,
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ))),
            Rect {
                x: area.x + m.esc_close_x,
                y: area.y,
                width: m.esc_close_w,
                height: 1,
            },
        );
    }
}

/// View mode: the native render with a row cursor. No per-frame
/// keep_cursor_visible here — like source mode, the wheel scrolls the
/// viewport only and the cursor is an absolute position that may sit off
/// screen; keyboard navigation, clicks, and the mode handoffs reveal it.
fn draw_view(f: &mut Frame, area: Rect, app: &mut App) {
    // View mode always draws the frame: the bordered "page" is the reading
    // mode's visual signature (source mode is the frameless raw editor).
    // The page floats one column off the screen's left edge — a margin so
    // the markers riding the border never touch the terminal edge (the
    // title and footer strips stay full-width).
    let margin = 1;
    let frame = Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin),
        height: area.height,
    };
    let m = 1;
    let inner = Rect {
        x: frame.x + m,
        y: frame.y + m,
        width: frame.width.saturating_sub(m * 2),
        height: frame.height.saturating_sub(m * 2),
    };
    // The text column floats one column off each border: the marker on
    // the left border needs a breath before the text (`> print(...)`), and
    // the right edge gets the same gap, so the paragraph reads as a set
    // column instead of a full-bleed block. The pads live INSIDE the
    // rendered rows (see [`ViewState::visible_text`]); `content` is the
    // width the rows (and the bars) are built at.
    let content = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    // Which source lines carry comments → a per-line flag for the view's
    // marker column (drawn over the frame's left border, so no render
    // width is reserved and the layout never shifts when the first comment
    // is added). Every line of a multi-line comment is flagged; the column
    // renders one marker on each line's first display row only, so the
    // range reads as a clean column instead of a noisy band.
    let marked = view_marker_flags(&app.comments, app.source.len(), app.current_file_path());
    // The selection is the LINE range itself (comment-identical);
    // `visible_text` resolves it to the exact spans via the phrase
    // segments.
    let sel = app.selection.map(|s| s.range());
    let changed = view_changed_flags(&app.last_added, app.source.len());
    let deleted = view_deleted_flags(&app.last_deleted_before, app.source.len());
    // The composer opened from view mode (`c` in view) is drawn inline
    // right under the cursor line, so the comment can be typed without
    // leaving the rendered view. While it is open it is part of the
    // layout: keep it on screen BEFORE the visible window is built, so
    // the splice below lands on the adjusted offset. The bar extends the
    // view's scrollable extent — a comment on the LAST line would
    // otherwise clamp the scroll at the document's last row and hide the
    // bar — and it grows as you type. Input mode has no scroll keys, so a
    // per-frame nudge cannot fight the user.
    let composing = app.mode == Mode::Input && app.composer_return == Mode::View;
    if composing {
        app.keep_composer_visible_view(inner.height as usize);
    }
    let border_style = Style::default().fg(app.ui_border);
    let (mut text, mut gutter) = app.view.visible_text(
        inner.height as usize,
        &marked,
        &changed,
        &deleted,
        sel,
        app.ui_selected_bg,
        border_style,
    );
    if composing {
        let full_width = content.width as usize;
        // Below the selection's rendered block (its END line's block — an
        // upward selection anchors at the range bottom, and a merged last
        // line anchors below the whole paragraph).
        let anchor = view_composer_anchor(app);
        let start_row = anchor.saturating_sub(app.view.offset) + 1;
        let lines = composer_lines(
            &app.input,
            app.input_cursor,
            app.input_start,
            app.input_end,
            full_width,
            app.editing_comment.is_some(),
        );
        if start_row <= text.lines.len() {
            let n = lines.len();
            // The bar floats in the text column like every other row: one
            // pad on each side.
            let padded: Vec<Line> = lines
                .into_iter()
                .map(|line| {
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(line.spans);
                    spans.push(Span::raw(" "));
                    Line::from(spans)
                })
                .collect();
            text.lines.splice(start_row..start_row, padded);
            // The composer rows carry no marker: keep the marker column
            // aligned with the text rows (the border shows through).
            gutter.splice(
                start_row..start_row,
                std::iter::repeat_n(GutterCell::border(border_style), n),
            );
        }
        // Terminal-cursor position inside the bar: on the `▏` glyph (the
        // macOS IME anchors its inline composition window here), sharing
        // the drawer's row math via composer_cursor_pos.
        let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
        // Top rule at `start_row`, text rows from `start_row + 1`.
        let row = start_row + 1 + crow;
        if row < inner.height as usize {
            f.set_cursor_position(Position {
                x: content.x + ccol as u16,
                y: inner.y + row as u16,
            });
        }
    }
    // A calm gray frame: the border marks the reading pane without
    // competing with the rendered content. A mid gray — ANSI bright-black
    // sank into the background.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.ui_border));
    // The text already starts at `offset` (see [`ViewState::visible_text`]),
    // so no Paragraph scroll is needed — the renderer only ever builds the
    // visible window.
    let p = Paragraph::new(text).block(block);
    f.render_widget(p, frame);
    // The marker column rides the frame's left border: `>` on the cursor
    // row, `▌` on marked rows, the border's `│` everywhere else. Written
    // over the border cells after the frame, so a marker replaces the
    // border glyph in place; the selection background extends over it,
    // running the cursor/selection band to the page edge.
    let buf = f.buffer_mut();
    for (i, cell) in gutter.iter().take(inner.height as usize).enumerate() {
        if let Some(c) = buf.cell_mut((frame.x, inner.y + i as u16)) {
            c.set_symbol(cell.glyph);
            c.set_style(cell.style);
        }
    }
    // The scrollbar sits one column inside the frame's right border: a
    // `▐` thumb, mirroring the marker column on the left. The thumb
    // tracks the VIEWPORT offset (wheel scroll moves the viewport only),
    // so it always reflects what is on screen; when the content fits, no
    // scrollbar is drawn and the border stays clean.
    if let Some((start, len)) =
        scroll_thumb(app.view.rows.len(), inner.height as usize, app.view.offset)
    {
        let thumb_fg = app.ui_scrollbar;
        let right = frame.x + frame.width - 2;
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((right, inner.y + i as u16)) {
                // Preserve the cell's current bg (the right pad's bg on
                // selected/cursor rows) so the band runs unbroken.
                let bg = c.style().bg;
                c.set_symbol("▐");
                let mut style = Style::default().fg(thumb_fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
    }
}

/// The footer's mode hint: the cursor's position as `L{line}/{total}`
/// (1-based source line — the cursor IS the review anchor, so the line
/// number is more actionable than a %), a few labeled actions for the
/// current context, then `? help` for the full key reference. Keys keep
/// their relative order across modes so a mode switch never rearranges
/// the hints.
fn footer_hints(app: &App) -> String {
    let pos = |line: usize, total: usize| {
        if total == 0 {
            "L0/0".to_string()
        } else {
            format!("L{}/{}", line + 1, total)
        }
    };
    match app.mode {
        Mode::Input => "Enter confirm · ^j newline · ←→↑↓ move · Esc cancel".to_string(),
        Mode::View => {
            let p = pos(app.view.cursor, app.source.len());
            // With a selection active, j/k EXTENDS it (the parallel model —
            // same as source mode); the footer must say so, or "j/k
            // scroll" silently grows the range after a Tab handoff. Esc
            // cancels the selection — spelled out, since it is the way
            // out of the SELECT state.
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    format!("{p} · {}–{} · j/k extend · c comment · Esc cancel · ? help", a + 1, b + 1)
                }
                None => format!("{p} · j/k scroll · v select · c comment · ? help"),
            }
        }
        Mode::Source => {
            let p = pos(app.cursor, app.source.len());
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    format!("{p} · {}–{} · j/k extend · c comment · Esc cancel · ? help", a + 1, b + 1)
                }
                None => format!("{p} · j/k move · v select · c comment · ? help"),
            }
        }
    }
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        footer_hints(app),
        Style::default().fg(Color::DarkGray),
    )];
    // The mode badge leads the footer (statusline convention): the title
    // above is file-centric, this is where the mode is read at a glance.
    // Color semantics: gray = view (calm reading), blue = source (the raw
    // editor), cyan = comment input (same as the composer bubble), magenta
    // = selection active (the transient `v` state — the badge flips to
    // SELECT so the mode is unmissable, and Esc cancels it). Yellow is
    // comments only, everywhere (the count lives in the top-right `▌ N`
    // indicator). All badges are width 8, so the hints never shift when
    // the mode changes.
    let (badge, badge_style) = match app.mode {
        Mode::Input => (
            format!("{:^8}", "COMMENT"),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Mode::View | Mode::Source if app.selection.is_some() => (
            format!("{:^8}", "SELECT"),
            Style::default().fg(Color::Black).bg(Color::LightMagenta),
        ),
        Mode::View => (
            format!("{:^8}", "VIEW"),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Mode::Source => (
            format!("{:^8}", "SOURCE"),
            Style::default().fg(Color::Black).bg(Color::LightBlue),
        ),
    };
    spans.insert(0, Span::styled(badge, badge_style));
    // Prompts (quit/edit confirmations, file-change) and transient toasts
    // are NOT in the footer: they float over the content as top-center
    // banners (see draw_prompt / draw_toast), so the hints never get
    // displaced.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A top-center banner over the content (row 1): the toast and the
/// persistent prompts share this rendering. Clear erases only the
/// banner's own rect — the layout never shifts, nothing scrolls.
/// A banner over the content at the given row: transient toasts float at
/// the BOTTOM (one row above the footer — the classic message-line
/// position), persistent prompts stay at the TOP. Clear erases only the
/// banner's own rect — the layout never shifts, nothing scrolls. Errors
/// render red (info stays yellow).
fn draw_banner(f: &mut Frame, area: Rect, row: u16, msg: &str, is_error: bool) {
    use ratatui::widgets::Clear;
    let w = UnicodeWidthStr::width(msg) as u16 + 2;
    let w = w.min(area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let rect = Rect {
        x,
        y: area.y + row,
        width: w,
        height: 1,
    };
    f.render_widget(Clear, rect);
    let text = clip_if_needed(msg, w.saturating_sub(2) as usize);
    let fg = if is_error { Color::Red } else { Color::Yellow };
    let style = Style::default()
        .fg(fg)
        .add_modifier(Modifier::BOLD)
        .bg(Color::Black);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text} "), style))),
        rect,
    );
}

/// A transient message floating at the bottom, one row above the footer
/// (vim's message-line position). Red = an operation that could not be
/// done (flash_err also beeped).
fn draw_toast(f: &mut Frame, app: &App) {
    if let Some((msg, _, is_error)) = &app.status {
        let h = f.area().height;
        draw_banner(f, f.area(), h.saturating_sub(2), msg, *is_error);
    }
}

/// The persistent prompt text, if any: quit confirmation, edit
/// confirmation, reload confirmation, or a pending file change.
/// Priority: quit > edit > reload > change (the old footer order). The
/// quit line advertises the Esc binding that is actually active.
fn prompt_message(app: &App) -> Option<Cow<'static, str>> {
    if app.confirm_quit {
        if app.esc_quit_enabled() {
            Some("unsent comments — Esc/q to quit".into())
        } else {
            Some("unsent comments — q to quit, Esc to cancel".into())
        }
    } else if app.confirm_edit {
        Some("unsent comments — e again to edit & clear, Esc to cancel".into())
    } else if app.confirm_reload {
        Some("unsent comments — r again to reload & clear, Esc to cancel".into())
    } else if app.file_changed {
        Some("file changed — r reload · i ignore".into())
    } else {
        None
    }
}

/// The persistent prompt banner: TOP-center, yellow. Prompts demand an
/// action and must not be missed; transient toasts live at the bottom, so
/// the two never clash.
fn draw_prompt(f: &mut Frame, app: &App) {
    if let Some(msg) = prompt_message(app) {
        draw_banner(f, f.area(), 1, &msg, false);
    }
}

/// Draw the all-comments overlay (Ctrl+p). Centered panel with sorted comment list.
fn draw_overlay(f: &mut Frame, app: &App) {
    match app.overlay {
        Some(Overlay::Files) => draw_files_overlay(f, app),
        Some(Overlay::Comments) => draw_comments_overlay(f, app),
        Some(Overlay::Help) => draw_help_overlay(f, app),
        None => {}
    }
}

/// The help reference rows (label, keys). Shared by the drawer and the
/// scroll clamp, so the list never scrolls past its own end; scrollable
/// with j/k or the wheel (small screens), closed by Esc / q / `?` or a
/// click outside the panel. The quit row reflects the active Esc binding.
fn help_rows(esc_quit: bool) -> Vec<(&'static str, &'static str)> {
    vec![
        ("move", "j/k · g/G · PgUp/PgDn · ^u/^d"),
        ("file", "]/[ · ^p files"),
        ("comment", "v select · Esc cancel · c add · d delete · n/N jump"),
        ("mode", "Tab view⇄source"),
        ("output", "y copy · s send"),
        ("list", "l comments · ? help"),
        ("reload", "r reload · i ignore · e edit"),
        ("quit", if esc_quit { "Esc/q quit" } else { "q quit · Esc cancel" }),
    ]
}

/// The full key reference (`?`): label + keys per category, scrollable
/// with j/k or the wheel (small screens), closed by Esc / q / `?` or a
/// click outside the panel.
fn draw_help_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label_style = Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::BOLD);

    let rows = help_rows(app.esc_quit_enabled());
    let visible = overlay_visible_rows();
    // Scroll only when the reference overflows the panel; a reference
    // that fits stays put (j/k are no-ops there).
    let offset = app.overlay_cursor.min(rows.len().saturating_sub(visible));
    let title_text = " help ".to_string();
    let title_fill =
        "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ])];
    for (label, keys) in rows.iter().skip(offset).take(visible) {
        lines.push(Line::from(vec![
            Span::styled(format!(" {label:<11}"), label_style),
            Span::styled(keys.to_string(), Style::default().fg(Color::White)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:scroll  Esc/q/?:close  click outside:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// The shortest suffix (component-wise) of `path` that no other file
/// shares — the file picker's row for it. A unique basename stays a bare
/// basename (no redundant directory prefix on every row); colliding
/// basenames grow parent components until the row is unambiguous.
fn unique_suffix(path: &Path, all: &[PathBuf]) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for len in 1..=parts.len() {
        let tail = &parts[parts.len() - len..];
        let collides = all.iter().any(|other| {
            if other == path {
                return false;
            }
            let op: Vec<String> = other
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            op.len() >= len && &op[op.len() - len..] == tail
        });
        if !collides {
            return tail.join("/");
        }
    }
    path.display().to_string()
}

/// The directory shared by every file in the session, shown in the picker
/// title so the rows stay short (e.g. `files (3) · testdata/`).
fn common_parent(files: &[PathBuf]) -> Option<String> {
    let lists: Vec<Vec<String>> = files
        .iter()
        .map(|f| {
            let comps: Vec<String> = f
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            comps[..comps.len().saturating_sub(1)].to_vec()
        })
        .collect();
    let first = lists.first()?;
    let mut n = 0usize;
    while n < first.len() && lists.iter().all(|l| l.get(n) == Some(&first[n])) {
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let dir = first[..n].join("/");
    Some(clip_if_needed(&dir, 24) + "/")
}

/// Clip `s` to `max_cols` with a trailing `…` only when it actually
/// overflows ([`clip_ellipsis`] always marks the clip, so short strings
/// would wrongly gain a `…`).
fn clip_if_needed(s: &str, max_cols: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_cols {
        s.to_string()
    } else {
        clip_ellipsis(s, max_cols)
    }
}

/// A centered panel rect (70% width/height) for overlay contents.
fn overlay_panel(area: Rect) -> Rect {
    let w = (area.width as f64 * 0.7) as u16;
    let h = (area.height as f64 * 0.7) as u16;
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    Rect {
        x: area.x + x,
        y: area.y + y,
        width: w,
        height: h,
    }
}

/// Map a screen row to the overlay entry under it (the cursor index), or
/// None for the title, group headers, gaps, or outside the panel.
/// Mirrors draw_overlay's layout (border at panel.y, title at +1, entries
/// from +2) so clicks land exactly on what is drawn.
fn overlay_entry_at(app: &App, row: u16) -> Option<usize> {
    let overlay = app.overlay?;
    let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let panel = overlay_panel(Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    });
    let first_entry = panel.y + 2; // below border + title
    if row < first_entry {
        return None;
    }
    // The visible list starts at the scroll offset; the clicked row maps
    // to that list position.
    let rel = (row - first_entry) as usize + app.overlay_offset;
    match overlay {
        Overlay::Files => (rel < app.files.len()).then_some(rel),
        // Help has no selectable entries (the wheel/j-k scroll it).
        Overlay::Help => None,
        Overlay::Comments => {
            // Same grouped layout as draw_comments_overlay: per file a
            // header row (not selectable) then one row per comment.
            overlay_rows(app).get(rel).copied().flatten()
        }
    }
}

/// Whether `files[i]`'s on-disk state differs from what the session last
/// loaded (or acknowledged with `i`) — the file picker's ⚡ badge. Reads
/// the disk stamp fresh: the poll loop only watches the CURRENT file, so
/// background files edited by an agent are checked here on demand.
fn file_externally_changed(app: &App, i: usize) -> bool {
    let seen = if i == app.current_file_index {
        app.last_loaded_stamp
    } else {
        app.file_states.get(i).and_then(|s| s.last_loaded_stamp)
    };
    let Some(seen) = seen else { return false };
    let Ok(meta) = std::fs::metadata(&app.files[i]) else {
        return false;
    };
    (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()) != seen
}

/// The file picker (Ctrl+p): every file in session order with its comment
/// count; the current file is highlighted. Files whose on-disk state
/// changed since their last load carry the title bar's ⚡ badge.
fn draw_files_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);

    let count = app.files.len();
    let title_text = match common_parent(&app.files) {
        Some(dir) => format!(" files ({count}) · {dir} "),
        None => format!(" files ({count}) "),
    };
    let title_fill = "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ])];

    let inner = panel.width.saturating_sub(2) as usize;
    let visible = overlay_visible_rows();
    let offset = app.overlay_offset.min(app.files.len().saturating_sub(visible));
    for (i, file) in app.files.iter().enumerate().skip(offset).take(visible) {
        let n = app.comments.iter().filter(|c| c.file_path == *file).count();
        let current = i == app.current_file_index;
        let selected = i == app.overlay_cursor;
        let cursor_mark = if selected { "▸ " } else { "  " };
        let name_style = if selected {
            cyan.add_modifier(Modifier::BOLD)
        } else if current {
            yellow
        } else {
            Style::default().fg(Color::White)
        };
        let count_str = if n > 0 {
            format!("  ({n})")
        } else {
            String::new()
        };
        let mode_tag = if supports_view(file) { " [view]" } else { " [src]" };
        // External edit pending: the same ⚡ the title bar shows, so a
        // glance at the picker tells which files the agent touched.
        let changed_mark = if file_externally_changed(app, i) {
            " ⚡"
        } else {
            ""
        };
        // The row: basename when unique, else the shortest unique path
        // suffix; clip only when the panel is too narrow for the right
        // side (⚡ + count + mode tag).
        let suffix = unique_suffix(file, &app.files);
        let reserved = 2
            + UnicodeWidthStr::width(changed_mark)
            + UnicodeWidthStr::width(count_str.as_str())
            + mode_tag.len();
        let name = clip_if_needed(&suffix, inner.saturating_sub(reserved));
        lines.push(Line::from(vec![
            Span::styled(format!("{cursor_mark}{name}"), name_style),
            Span::styled(changed_mark, Style::default().fg(Color::Yellow)),
            Span::styled(count_str, dark_gray),
            Span::styled(mode_tag, dark_gray),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:move  Enter:switch  Esc/q:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// The all-comments list (`l`): every file's comments sorted by path then
/// start line, with jump (Enter) and delete (d).
fn draw_comments_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);

    let mut lines: Vec<Line> = Vec::new();
    let idx = sorted_comment_indices(app);
    let count = idx.len();
    // Distinct files that carry comments: the title shows their common
    // parent and each group header the shortest unique suffix, so no
    // path component repeats on every row.
    let mut comment_files: Vec<PathBuf> = Vec::new();
    for &raw in &idx {
        let file = &app.comments[raw].file_path;
        if !comment_files.contains(file) {
            comment_files.push(file.clone());
        }
    }
    let title_text = if count > 0 {
        match common_parent(&comment_files) {
            Some(dir) => format!(" comments ({count}) · {dir} "),
            None => format!(" comments ({count}) "),
        }
    } else {
        " no comments ".to_string()
    };
    let title_fill = "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    lines.push(Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ]));

    if count == 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Esc/q:close",
            dark_gray,
        )));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dark_gray);
        f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
        return;
    }

    let inner = panel.width.saturating_sub(2) as usize;
    // Scroll only when the list overflows; a list that fits never moves.
    let rows = overlay_rows(app);
    let visible = overlay_visible_rows();
    let offset = app.overlay_offset.min(rows.len().saturating_sub(visible));
    for (p, row) in rows.iter().enumerate().skip(offset).take(visible) {
        match row {
            // A group header: the (shortest unique) path and its comment
            // count, once per file.
            None => {
                let Some(first_entry) = rows[p..].iter().find_map(|r| *r) else {
                    continue;
                };
                let file = &app.comments[idx[first_entry]].file_path;
                let suffix = unique_suffix(file, &comment_files);
                let group_count = idx
                    .iter()
                    .filter(|&&raw| app.comments[raw].file_path == *file)
                    .count();
                lines.push(Line::from(vec![Span::styled(
                    format!(" {suffix} ({group_count})"),
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            Some(entry) => {
                let c = &app.comments[idx[*entry]];
                let range = c.range_label();
                let selected = *entry == app.overlay_cursor;
                let loc_style = if selected {
                    cyan.add_modifier(Modifier::BOLD)
                } else {
                    yellow
                };
                let cursor_mark = if selected { "▸ " } else { "  " };
                // First line of comment text (indented). Range and body
                // share one row — the colors already separate them
                // (yellow range, gray body), so each comment stays on a
                // single line.
                let body_first = c.text.lines().next().unwrap_or("");
                let body_style = if selected {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let used = 2 + UnicodeWidthStr::width(range.as_str()) + 2;
                let budget = inner.saturating_sub(used);
                let body = clip_if_needed(body_first, budget);
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor_mark}{range}"), loc_style),
                    Span::styled(format!("  {body}"), body_style),
                ]));
            }
        }
    }

    // Footer hints.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:move  Enter:jump  d:delete  Esc/q:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// Source mode: status/gutter + highlighted content, wrapped by our own
/// width-aware algorithm (ratatui Wrap would double-wrap or misalign CJK).
fn draw_source(f: &mut Frame, area: Rect, app: &mut App) {
    // Source mode never draws a frame: every column goes to the source
    // (the framed view is the reading mode's visual signature). The
    // rightmost column is the scrollbar's track.
    let m = 0;
    let inner = Rect {
        x: area.x + m,
        y: area.y + m,
        width: area.width.saturating_sub(m * 2),
        height: area.height.saturating_sub(m * 2),
    };
    let gutter_cols = 1 + app.source.gutter_width as u16 + 1;
    app.gutter_cols = gutter_cols;
    let content_width = inner.width.saturating_sub(gutter_cols + 1);
    if app.source.is_empty() {
        f.render_widget(Paragraph::new("(empty file)"), area);
        return;
    }
    app.ensure_row_cache(content_width);
    app.refresh_line_rows();
    // While the composer is open, keep its bar on screen: the bar's
    // height is folded into line_rows (it grows as you type) and Input
    // mode has no scrolling keys, so a per-frame nudge cannot fight the
    // user — without it a comment on the last line left the bar below
    // the pane. Wheel/keys keep their own keep_cursor_visible discipline.
    if app.mode == Mode::Input {
        app.keep_composer_visible(inner.height as usize);
    }
    // The cursor is an absolute file position: wheel scroll moves the
    // viewport only, so don't yank the offset back every frame. Keyboard
    // j/k, c, and G call keep_cursor_visible themselves (herdr-review
    // style: scroll away, the cursor stays where it was).

    let (text, composer_cursor) = build_rows(app, inner.height, content_width);
    f.render_widget(Paragraph::new(text), area);
    // The scrollbar rides the pane's own right edge (source mode draws no
    // frame, so unlike view mode there is no border to sit inside — the
    // thumb goes all the way to the last column). The thumb appears only
    // when the content overflows the viewport; it tracks the viewport
    // offset (wheel scroll moves the viewport only), so it always reflects
    // what is on screen.
    if let Some((start, len)) = scroll_thumb(
        app.line_rows.iter().sum(),
        inner.height as usize,
        app.offset,
    ) {
        let thumb_fg = app.ui_scrollbar;
        let right = area.x + area.width - 1;
        let buf = f.buffer_mut();
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((right, inner.y + i as u16)) {
                // Preserve the cell's current bg (the gutter's bg on
                // selected/cursor rows) so the band runs unbroken.
                let bg = c.style().bg;
                c.set_symbol("▐");
                let mut style = Style::default().fg(thumb_fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
    }
    // The logical cursor position is still published every frame even
    // though the hardware cursor is hidden: the macOS IME anchors its
    // inline composition window to this spot, so conversion stays inside
    // the bar and follows the `▏` glyph.
    if let Some((col, row)) = composer_cursor {
        f.set_cursor_position(Position {
            x: inner.x + col,
            y: inner.y + row,
        });
    }
}

/// Build the visible source-mode rows: `[status][number] ` + content, each
/// source line wrapped to `content_width`, continuation rows bare content.
/// Selected lines get the selection background; the cursor line a `>` marker.
/// While the composer is open, also returns where the terminal cursor should
/// sit inside it (body-relative column/row), so the real bar cursor (and the
/// macOS IME's inline composition window) renders inside the bar.
fn build_rows(app: &App, height: u16, content_width: u16) -> (Text<'static>, Option<(u16, u16)>) {
    let mut out: Vec<Line> = Vec::new();
    let mut composer_cursor: Option<(u16, u16)> = None;
    let width = content_width as usize;
    // Comment bars span the whole pane (gutter included), pi.dev-style.
    let full_width = (content_width + app.gutter_cols) as usize;
    // The current file's cards (the edited one is hidden while composing).
    let cards = visible_cards(app);
    let mut row = 0usize;
    for idx in 0..app.source.len() {
        let line_rows = app.rows_of(idx);
        if row + line_rows <= app.offset {
            row += line_rows;
            continue;
        }
        if row >= app.offset + height as usize {
            break;
        }
        let selected = app.selection.is_some_and(|s| s.contains(idx));
        let commented = app.comments.iter().any(|c| c.covers(idx) && c.file_path == app.current_file_path());
        let changed = app.last_added.contains(&idx);
        let deleted_before = app.last_deleted_before.contains(&idx);
        let is_cursor = idx == app.cursor;
        let cursor_mark = if is_cursor {
            ">"
        } else if changed {
            "+"
        } else if deleted_before {
            "-"
        } else {
            " "
        };
        // The cursor line gets the same calm DarkGray background as view
        // mode (text colors untouched — a full-row reversal was fatiguing
        // and clashed with the syntax highlighting). A selected row shares
        // the background; the `>` marker keeps the cursor visible at the
        // selection edge. Changed lines get a subtle green background.
        let cursor_bg = is_cursor || selected;
        let changed_bg = changed && !cursor_bg;
        let deleted_fg = deleted_before && !cursor_bg && !changed_bg;
        let gutter_style = if cursor_bg {
            Style::default().bg(app.ui_selected_bg)
        } else if changed_bg {
            Style::default().bg(app.ui_changed_bg)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Selected lines: the number turns the composer's cyan — the
        // pending range reads at a glance and matches the color it will be
        // typed under. Commented lines: yellow (no `#` marker), so the
        // gutter shows at a glance which lines carry comments. Changed
        // lines: green `+` marker and green number.
        let num_style = if selected {
            Style::default().fg(Color::Cyan).bg(app.ui_selected_bg)
        } else if commented && !is_cursor {
            Style::default().fg(Color::Yellow)
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else if deleted_fg {
            Style::default().fg(Color::Red)
        } else {
            gutter_style
        };
        let num = Span::styled(
            format!("{:>width$} ", idx + 1, width = app.source.gutter_width),
            num_style,
        );
        let wrapped = wrap_spans(&app.spans[idx], width);
        // The cursor glyph is bold LightCyan — it must be findable at a
        // glance (yellow is the comment marker's color), same as view mode.
        // Changed lines: green `+`. Deleted-before lines: red `-`.
        let mark_style = if is_cursor {
            let s = Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD);
            if cursor_bg { s.bg(app.ui_selected_bg) } else { s }
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else if deleted_fg {
            Style::default().fg(Color::Red)
        } else {
            gutter_style
        };
        for (k, frags) in wrapped.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if k == 0 {
                spans.push(Span::styled(cursor_mark, mark_style));
                spans.push(num.clone());
            } else {
                // Continuation rows of a wrapped line: indent by the gutter
                // width so the text stays aligned under the first row's
                // text instead of starting at column 0 under the line
                // number. The reversed/selected background spans the whole
                // continuation row so the cursor/selection reads as one
                // block.
                let indent_style = if cursor_bg {
                    Style::default().bg(app.ui_selected_bg)
                } else if changed_bg {
                    Style::default().bg(app.ui_changed_bg)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(
                    " ".repeat(app.gutter_cols as usize),
                    indent_style,
                ));
            }
            for f in frags {
                let style = if cursor_bg {
                    f.style.bg(app.ui_selected_bg)
                } else if changed_bg {
                    f.style.bg(app.ui_changed_bg)
                } else {
                    f.style
                };
                spans.push(Span::styled(f.text.clone(), style));
            }
            // A cursor/selection/changed row's background runs to the
            // RIGHT EDGE of the pane (same calm color, pi.dev-style band)
            // — the text extent alone made the selection ragged and easy
            // to misread. This also covers blank lines (no fragments)
            // with no extra glyph.
            if cursor_bg || changed_bg {
                let used: usize = spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let bg = if cursor_bg {
                    app.ui_selected_bg
                } else {
                    app.ui_changed_bg
                };
                if used < full_width {
                    spans.push(Span::styled(
                        " ".repeat(full_width - used),
                        Style::default().bg(bg),
                    ));
                }
            }
            out.push(Line::from(spans));
        }
        // Inline comment bars: one per comment ending on this line, stacked
        // (current file only; the one being re-edited is hidden under the
        // edit composer).
        for c in cards
            .iter()
            .filter(|c| c.end as usize - 1 == idx)
        {
            out.extend(comment_bar_lines(c, full_width));
        }
        // The composer bar while typing, under the target range's last line.
        if app.mode == Mode::Input && app.input_end == idx {
            let start_row = out.len();
            out.extend(composer_lines(
                &app.input,
                app.input_cursor,
                app.input_start,
                app.input_end,
                full_width,
                app.editing_comment.is_some(),
            ));
            // Terminal-cursor position inside the bar: on the `▏` glyph.
            // The macOS IME draws its inline composition window at this
            // spot too. composer_cursor_pos shares the drawer's row math,
            // so the anchor can never drift from the glyph.
            let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
            // Top rule at `start_row`, text rows from `start_row + 1`.
            composer_cursor = Some((ccol as u16, (start_row + 1 + crow) as u16));
        }
        row += line_rows;
    }
    (Text::from(out), composer_cursor)
}

/// Display rows a saved-comment bar occupies under its anchor line
/// (top rule + wrapped body + bottom rule). Must match
/// [`comment_bar_lines`].
fn card_line_count(c: &Comment, full_width: usize) -> usize {
    let body: usize = c
        .text
        .split('\n')
        .map(|l| {
            wrap_spans(
                &[HiSpan {
                    text: l.to_string(),
                    style: Style::default(),
                }],
                full_width,
            )
            .len()
        })
        .sum();
    body.max(1) + 2
}

/// The composer's wrapped body rows as plain strings: the input with the
/// `▏` cursor glyph inserted at `cursor` (byte offset), split into
/// logical lines and wrapped to `full_width`. The drawer, the row count,
/// and the terminal-cursor anchor all derive from THIS, so the layout,
/// the glyph, and the IME window can never disagree.
fn composer_body_rows(text: &str, cursor: usize, full_width: usize) -> Vec<String> {
    let mut display = text.to_string();
    display.insert(cursor.min(display.len()), '▏');
    let mut rows = Vec::new();
    for logical in display.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            rows.push(row.iter().map(|s| s.text.as_str()).collect::<String>());
        }
    }
    rows
}

/// The `▏` glyph's position inside the composer body: (body row, display
/// column). Row 0 is the first text row under the top rule.
fn composer_cursor_pos(text: &str, cursor: usize, full_width: usize) -> (usize, usize) {
    let rows = composer_body_rows(text, cursor, full_width);
    for (i, row) in rows.iter().enumerate() {
        if let Some(pos) = row.find('▏') {
            return (i, UnicodeWidthStr::width(&row[..pos]));
        }
    }
    (rows.len().saturating_sub(1), 0)
}

/// Display rows the composer bar occupies (top rule + wrapped input +
/// bottom rule). Must match [`composer_lines`].
fn composer_line_count(text: &str, cursor: usize, full_width: usize) -> usize {
    composer_body_rows(text, cursor, full_width).len().max(1) + 2
}

/// A saved comment as a full-width bar: top and bottom rules only (no side
/// borders, no background fill), so it reads like a speech bubble whose
/// rules run to both screen edges. The rule color shows the state — saved
/// comments are dark gray with a yellow title, composing is cyan.
fn comment_bar_lines(c: &Comment, full_width: usize) -> Vec<Line<'static>> {
    let rule = Style::default().fg(Color::DarkGray);
    let title = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let label = format!(" comment · {} ", c.range_label());
    let fill = "─".repeat(full_width.saturating_sub(label.width()));
    let mut lines = vec![Line::from(vec![
        Span::styled(label, title),
        Span::styled(fill, rule),
    ])];
    for logical in c.text.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            let piece: String = row.iter().map(|s| s.text.as_str()).collect();
            lines.push(Line::from(Span::raw(piece)));
        }
    }
    lines.push(Line::from(Span::styled("─".repeat(full_width), rule)));
    lines
}

/// The inline comment input bar: the same top/bottom rules in cyan while
/// composing. The `▏` cursor glyph sits at the insertion point (the
/// hardware cursor stays hidden — see run(); this is the modern-TUI
/// pattern used by Hermes/pi.dev, language-independent and immune to the
/// IME commit drift of a real cursor). `editing` flips the label to
/// `edit` when the composer is replacing an existing comment.
fn composer_lines(
    text: &str,
    cursor: usize,
    start: usize,
    end: usize,
    full_width: usize,
    editing: bool,
) -> Vec<Line<'static>> {
    let rule = Style::default().fg(Color::Cyan);
    let title = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let word = if editing { "edit" } else { "comment" };
    let label = if start == end {
        format!(" {word} · {} ", start + 1)
    } else {
        format!(" {word} · {}-{} ", start + 1, end + 1)
    };
    let fill = "─".repeat(full_width.saturating_sub(label.width()));
    let mut lines = vec![Line::from(vec![
        Span::styled(label, title),
        Span::styled(fill, rule),
    ])];
    for piece in composer_body_rows(text, cursor, full_width) {
        lines.push(Line::from(Span::raw(piece)));
    }
    lines.push(Line::from(Span::styled("─".repeat(full_width), rule)));
    lines
}

impl App {
    /// Save current per-file state and load the file at `index`.
    fn switch_to_file(&mut self, new_index: usize) {
        if new_index == self.current_file_index || new_index >= self.files.len() {
            return;
        }
        // Save current state into the old slot.
        let old = &mut self.file_states[self.current_file_index];
        old.source = std::mem::take(&mut self.source);
        old.spans = std::mem::take(&mut self.spans);
        old.view = std::mem::take(&mut self.view);
        old.mode = if self.mode == Mode::Input { self.composer_return } else { self.mode };
        old.offset = self.offset;
        old.cursor = self.cursor;
        old.selection = self.selection.take();
        old.line_rows = std::mem::take(&mut self.line_rows);
        old.base_rows = std::mem::take(&mut self.base_rows);
        old.content_width = self.content_width;
        old.gutter_cols = self.gutter_cols;
        old.file_stamp = self.file_stamp;
        old.last_loaded_stamp = self.last_loaded_stamp;
        old.file_changed = self.file_changed;
        old.reload_pending = self.reload_pending.take();
        old.last_change = self.last_change.take();
        old.last_added = std::mem::take(&mut self.last_added);
        old.last_deleted_before = std::mem::take(&mut self.last_deleted_before);

        // Close the composer if it was open.
        self.input.clear();
        self.input_cursor = 0;
        self.editing_comment = None;
        self.mode = if self.mode == Mode::Input { self.composer_return } else { self.mode };
        self.ime_guard = None;

        // Load the new file's state.
        let new = &mut self.file_states[new_index];
        self.source = std::mem::take(&mut new.source);
        self.spans = std::mem::take(&mut new.spans);
        self.view = std::mem::take(&mut new.view);
        self.mode = new.mode;
        self.offset = new.offset;
        self.cursor = new.cursor;
        self.selection = new.selection.take();
        self.line_rows = std::mem::take(&mut new.line_rows);
        self.base_rows = std::mem::take(&mut new.base_rows);
        self.content_width = new.content_width;
        self.gutter_cols = new.gutter_cols;
        self.file_stamp = new.file_stamp;
        self.last_loaded_stamp = new.last_loaded_stamp;
        self.file_changed = new.file_changed;
        self.reload_pending = new.reload_pending.take();
        self.last_change = new.last_change.take();
        self.last_added = std::mem::take(&mut new.last_added);
        self.last_deleted_before = std::mem::take(&mut new.last_deleted_before);

        self.current_file_index = new_index;
        self.confirm_quit = false;
        self.confirm_edit = false;
        self.confirm_reload = false;
        self.drag_anchor = None;
        self.view_dirty = false;
        self.view_dirty_since = None;

        // A resize while this file was in the background left its saved
        // view at the old wrap width (Resize only re-renders the active
        // file). Source mode re-wraps itself every frame via
        // ensure_row_cache; the view needs an explicit re-render.
        if supports_view(self.current_file_path()) {
            let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
            if self.view.width != view_render_width(w) as usize {
                replace_view_preserving_cursor(self);
            }
        }

        let name = self.files[new_index]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.files[new_index].display().to_string());
        self.flash(format!("switched to {name}"));
    }

    fn current_file_path(&self) -> &Path {
        &self.files[self.current_file_index]
    }

    fn terminal_height(&self) -> u16 {
        ratatui::crossterm::terminal::size()
            .map(|s| s.1)
            .unwrap_or(24)
    }

    /// View-mode viewport height in rows — must match `draw_view`'s
    /// `inner.height` (title bar + footer + the frame's two borders take
    /// the other rows), or per-frame `keep_cursor_visible` re-shoves the
    /// offset and wheel scroll stalls. View mode always draws the frame.
    fn view_viewport_rows(&self) -> usize {
        self.terminal_height().saturating_sub(4).max(1) as usize
    }

    /// Source-mode viewport height — must match `draw_source`'s
    /// `inner.height` (title bar + footer only; source mode never draws
    /// a frame, keeping every column for the source).
    fn source_viewport_rows(&self) -> usize {
        self.terminal_height().saturating_sub(2).max(1) as usize
    }

    /// Is the view pane the one on screen (view mode, or the composer
    /// opened from view)? The frame — view-only chrome — hangs off this.
    fn view_active(&self) -> bool {
        self.mode == Mode::View
            || (self.mode == Mode::Input && self.composer_return == Mode::View)
    }

    /// Whether `Esc` may quit the app in normal mode (see [`EscQuit`]):
    /// `always` unconditionally; `auto` when a `--callback` is set — the
    /// app is a step in a loop then, so quitting is a return to the
    /// caller rather than a dead end. `never` keeps Esc a pure cancel.
    fn esc_quit_enabled(&self) -> bool {
        match self.config.esc_quit {
            EscQuit::Always => true,
            EscQuit::Never => false,
            EscQuit::Auto => self.config.callback.is_some(),
        }
    }
}
#[cfg(test)]
mod bar_tests {
    use super::*;
    use crate::comment::Comment;

    fn test_comment(text: &str) -> Comment {
        Comment {
            file_path: "/tmp/x.md".into(),
            start: 1,
            end: 2,
            text: text.into(),
            lines: text.into(),
        }
    }

    #[test]
    fn comment_bar_lines_have_rules_only() {
        // Top rule (title + fill), text row, bottom rule. No side borders,
        // no background fill — the rules run to both screen edges.
        let lines = comment_bar_lines(&test_comment("テスト"), 80);
        assert_eq!(lines.len(), 3, "top rule + text + bottom rule");
        let label = " comment · 1-2 ";
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.starts_with(label), "top rule starts with the title");
        assert_eq!(
            top.chars().filter(|&c| c == '─').count(),
            80 - label.chars().count(),
            "top rule fill spans the rest of the pane"
        );
        let bottom: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            bottom.chars().filter(|&c| c == '─').count(),
            80,
            "bottom rule spans the full pane"
        );
        assert!(lines.iter().all(|l| l.width() <= 80));
    }

    #[test]
    fn composer_lines_have_rules_and_cursor() {
        let text = "テスト";
        let lines = composer_lines(text, text.len(), 0, 0, 80, false);
        assert_eq!(lines.len(), 3, "top rule + input + bottom rule");
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.starts_with(" comment · 1 "), "add label: {top}");
        // Re-edit mode flips the label to `edit`.
        let lines = composer_lines(text, text.len(), 2, 4, 80, true);
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.starts_with(" edit · 3-5 "), "edit label: {top}");
        let text_row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text_row.ends_with('▏'), "cursor glyph on the last text row");
        let bottom: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            bottom.chars().filter(|&c| c == '─').count(),
            80,
            "bottom rule spans the full pane"
        );
    }

    #[test]
    fn composer_glyph_follows_the_cursor() {
        // ▏ renders at the cursor position, not pinned to the end.
        let lines = composer_lines("abc", 1, 0, 0, 80, false);
        let row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(row, "a▏bc");
        assert_eq!(composer_cursor_pos("abc", 1, 80), (0, 1));
        // Multi-line: the glyph (and the IME anchor) lands on the second
        // logical line's row.
        assert_eq!(composer_cursor_pos("ab\ncd", 4, 80), (1, 1));
        // CJK before the cursor counts display width, not chars.
        assert_eq!(composer_cursor_pos("あい", 3, 80), (0, 2));
    }

    #[test]
    fn composer_tabs_expand_so_the_cursor_column_matches() {
        // The ▏ column is measured on the wrapped text; a pasted tab
        // expands to the terminal's 8-column stop, so the cursor lands
        // after the visible text instead of after a 0-width tab.
        let lines = composer_lines("a\tb", 3, 0, 0, 80, false);
        let text_row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text_row, "a       b▏");
    }

    #[test]
    fn line_counts_match_bar_height() {
        let c = test_comment("テスト");
        assert_eq!(card_line_count(&c, 80), comment_bar_lines(&c, 80).len());
        let text = "テスト";
        assert_eq!(
            composer_line_count(text, text.len(), 80),
            composer_lines(text, text.len(), 0, 0, 80, false).len()
        );
        // Mid-text cursor: the count and the render still agree.
        assert_eq!(
            composer_line_count(text, 3, 80),
            composer_lines(text, 3, 0, 0, 80, false).len()
        );
        // Wrapped body: long text adds rows to both the count and the render.
        let c = test_comment(&"あ".repeat(100));
        assert_eq!(card_line_count(&c, 40), comment_bar_lines(&c, 40).len());
    }

    #[test]
    fn index_at_abs_maps_bars_to_their_line() {
        // rows: 行1(1), 行2(1), 行3(1)+バー(3)=4, 行4(1), 行5(1)
        let rows = vec![1, 1, 4, 1, 1];
        // offset=0: absolute rows 0..8
        assert_eq!(index_at_abs(&rows, 0, 0), Some(0), "line 1");
        assert_eq!(index_at_abs(&rows, 0, 1), Some(1), "line 2");
        assert_eq!(index_at_abs(&rows, 0, 2), Some(2), "line 3");
        assert_eq!(index_at_abs(&rows, 0, 3), Some(2), "bar top -> line 3");
        assert_eq!(index_at_abs(&rows, 0, 5), Some(2), "bar bottom -> line 3");
        assert_eq!(index_at_abs(&rows, 0, 6), Some(3), "line 4");
        assert_eq!(index_at_abs(&rows, 0, 7), Some(4), "line 5");
        assert_eq!(index_at_abs(&rows, 0, 8), None, "past the end");
    }

    #[test]
    fn index_at_abs_is_stable_across_scroll_offsets() {
        // Regression: the mapping used to compare the viewport-relative
        // display_row against absolute row spans, so after scrolling
        // (offset > 0) a click landed `offset` lines too high — worst at
        // the scroll stops (top/bottom).
        let rows = vec![1, 1, 4, 1, 1, 1, 1];
        // The same absolute row must map to the same line at any offset.
        for offset in 0..4 {
            // Absolute row 6 is line 4 (offset 0: abs 6 -> idx 3).
            assert_eq!(
                index_at_abs(&rows, offset, 6),
                Some(3),
                "abs row 6 at offset {offset}"
            );
            // Absolute row 4 (bar body) belongs to line 3.
            assert_eq!(
                index_at_abs(&rows, offset, 4),
                Some(2),
                "abs row 4 at offset {offset}"
            );
        }
        // Scrolled to the bottom stop: last viewport row maps to the last
        // line, not `offset` lines above it.
        let last_abs = rows.iter().sum::<usize>() - 1;
        assert_eq!(
            index_at_abs(&rows, 4, last_abs),
            Some(rows.len() - 1),
            "bottom stop maps to the last line"
        );
    }
}
#[cfg(test)]
mod mouse_tests {
    use super::*;
    use crate::comment::Comment;
    use crate::config::{Config, EscQuit};
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::view::ViewState;
    use std::io::Write;

    fn test_app() -> App {
        let path = std::env::temp_dir().join("akapen_row_at_test.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=8 {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::Source;
        app.gutter_cols = 3; // 1 + gutter_width(1) + 1
        app.ensure_row_cache(75);
        app.comments.push(Comment {
            file_path: path.clone(),
            start: 5,
            end: 5,
            text: "テスト".into(),
            lines: "テスト".into(),
        });
        app.refresh_line_rows();
        app
    }

    #[test]
    fn wrapped_continuation_rows_indent_under_the_gutter() {
        // A long first line wraps; continuation rows must start with the
        // gutter width of spaces so text stays aligned under the first
        // row's text instead of under the line number.
        let path = std::env::temp_dir().join("akapen_wrap_indent_test.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", "あ".repeat(40)).unwrap(); // 80 cols -> wraps at 57
        writeln!(f, "short").unwrap();
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 57, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(57);
        app.refresh_line_rows();
        let (text, _) = build_rows(&app, 20, 57);
        let row0: String = text.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let row1: String = text.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            row0.starts_with(">1 "),
            "first row shows the gutter, got {row0:?}"
        );
        assert!(
            row1.starts_with("   "),
            "continuation row indents by the gutter width, got {row1:?}"
        );
        assert!(
            !row1.starts_with('>'),
            "no gutter markers on continuation rows, got {row1:?}"
        );
    }

    #[test]
    fn click_on_bar_maps_to_anchor_line() {
        let app = test_app();
        // 8 lines; line 5 (idx=4) carries a 3-row bar below it:
        // row0..3 = lines 1-4, row4 = line 5, rows 5-7 = the bar,
        // row8 = line 6, row9 = line 7.
        assert_eq!(source_line_at(&app, 75, 4), Some(4), "line 5");
        assert_eq!(source_line_at(&app, 75, 5), Some(4), "bar top rule");
        assert_eq!(source_line_at(&app, 75, 6), Some(4), "bar text");
        assert_eq!(
            source_line_at(&app, 75, 7),
            Some(4),
            "bar bottom rule"
        );
        assert_eq!(source_line_at(&app, 75, 8), Some(5), "line 6");
        assert_eq!(source_line_at(&app, 75, 9), Some(6), "line 7");
    }

    #[test]
    fn click_maps_correctly_without_comments() {
        let mut app = test_app();
        app.comments.clear();
        app.refresh_line_rows();
        assert_eq!(source_line_at(&app, 75, 4), Some(4));
        assert_eq!(source_line_at(&app, 75, 5), Some(5));
    }

    /// A source-mode app with `n` single-line rows (no wrapping at any
    /// sane width), so the scrollbar track exists and `line_rows` sums to
    /// exactly `n`. 1000 lines overflow any reasonable terminal height.
    fn scrollbar_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scroll.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=1000 {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(source_content_width(&app));
        app.refresh_line_rows();
        app
    }

    /// A view-mode app over `n` single-row paragraphs (blank lines keep
    /// the markdown renderer from merging them), so the scrollbar track
    /// exists. The mode is the one `.md` files start in: view.
    fn scrollbar_view_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=1000 {
            writeln!(f, "line{i}\n").unwrap();
        }
        let config = Config {
            files: vec![path],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        App::new(config, source, highlight, view, false)
    }

    #[test]
    fn scrollbar_track_click_jumps_and_grabs() {
        let mut app = scrollbar_app();
        let total: usize = app.line_rows.iter().sum();
        let viewport = app.source_viewport_rows();
        assert!(total > viewport, "the track must exist");
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let col = w - 1;
        let down = |row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let up = |row: u16| MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // A click at content row 5 jumps the viewport there and grabs the
        // thumb (the click row, the offset it landed on).
        let expected = scroll_offset_at(total, viewport, 5).unwrap();
        on_mouse(&mut app, down(1 + 5));
        assert_eq!(app.offset, expected, "click jumps the viewport");
        assert_eq!(app.scrollbar_drag, Some((5, expected)), "thumb grabbed");
        // The title row is not the track: the click falls through.
        on_mouse(&mut app, down(0));
        assert_eq!(app.offset, expected, "title row is not the track");
        // Release ends the grab.
        on_mouse(&mut app, up(1 + 5));
        assert_eq!(app.scrollbar_drag, None, "release clears the grab");
    }

    #[test]
    fn scrollbar_drag_scrubs_and_clamps() {
        let mut app = scrollbar_app();
        let total: usize = app.line_rows.iter().sum();
        let viewport = app.source_viewport_rows();
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let col = w - 1;
        let ev = |kind: MouseEventKind, row: u16, c: u16| MouseEvent {
            kind,
            column: c,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Press at track row 5, drag to track row 15: the offset follows.
        on_mouse(&mut app, ev(MouseEventKind::Down(MouseButton::Left), 6, col));
        let start = app.offset;
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 16, col));
        assert_eq!(
            app.offset,
            scroll_offset_drag(total, viewport, 5, start, 15).unwrap(),
            "drag scrubs the thumb"
        );
        // The pointer may leave the column while grabbed.
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 11, 30));
        assert_eq!(
            app.offset,
            scroll_offset_drag(total, viewport, 5, start, 10).unwrap(),
            "a grabbed drag scrubs from anywhere"
        );
        // Dragging past the track end clamps to the bottom.
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 200, col));
        assert_eq!(
            app.offset,
            scroll_offset_drag(total, viewport, 5, start, viewport - 1).unwrap(),
            "past the track end clamps"
        );
        // Release; a later drag no longer scrubs (and the normal drag
        // selection has no anchor to start from).
        on_mouse(&mut app, ev(MouseEventKind::Up(MouseButton::Left), 16, col));
        let before = app.offset;
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 10, col));
        assert_eq!(app.offset, before, "no grab, no scrub");
    }

    #[test]
    fn scrollbar_works_in_view_mode_too() {
        // View mode: the track rides the frame's right border, one column
        // inside it (the right pad) and one row lower (title + frame top
        // border).
        let mut app = scrollbar_view_app();
        let total = app.view.rows.len();
        let viewport = app.view_viewport_rows();
        assert!(total > viewport, "the track must exist");
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let col = w - 2;
        let ev = |kind: MouseEventKind, row: u16| MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Click at content row 5 (screen row 2 + 5).
        let expected = scroll_offset_at(total, viewport, 5).unwrap();
        on_mouse(&mut app, ev(MouseEventKind::Down(MouseButton::Left), 2 + 5));
        assert_eq!(app.view.offset, expected, "view-mode click jumps the viewport");
        assert!(app.scrollbar_drag.is_some());
        // Release ends the grab.
        on_mouse(&mut app, ev(MouseEventKind::Up(MouseButton::Left), 2 + 5));
        assert_eq!(app.scrollbar_drag, None);
    }
}

/// State-machine tests: mode transitions, selection, composer, deletion,
/// and quit confirmation. These run the real key handlers against a built
/// App (no TTY), so the event-loop logic is exercised the same way a user
/// would — only the rendering is bypassed.
#[cfg(test)]
mod state_tests {
    use super::*;
    use crate::comment::Selection;
    use crate::config::{Config, EscQuit};
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::source::Source;
    use crate::view::ViewState;
    use std::io::Write;

    /// A fresh app over a temp file with `n` lines ("line1"..), in `mode`.
    fn make_app(n: usize, mode: Mode) -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=n {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = mode;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app
    }

    /// Like [`make_app`] but returns the tempdir too, so tests that rewrite
    /// the file on disk can keep it alive (make_app's tempdir is dropped
    /// when it returns, deleting the file).
    fn make_app_keep(n: usize, mode: Mode) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=n {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = mode;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        (app, dir)
    }

    fn add_comment(app: &mut App, start: usize, end: usize, text: &str) {
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: start as u32,
            end: end as u32,
            lines: app.source.snippet(start as u32, end as u32),
            text: text.into(),
        });
    }

    #[test]
    fn quit_without_comments_exits_immediately() {
        let mut app = make_app(5, Mode::Source);
        request_quit(&mut app);
        assert!(!app.running);
    }

    #[test]
    fn quit_with_comments_requires_a_second_q() {
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        assert!(app.running, "first q only arms the confirmation");
        assert!(app.confirm_quit);
        request_quit(&mut app);
        assert!(!app.running, "second q quits");
    }

    #[test]
    fn esc_cancels_quit_confirmation() {
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running);
        assert!(!app.confirm_quit, "Esc clears the confirmation");
    }

    #[test]
    fn esc_quits_with_callback_in_auto_mode() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "auto + callback: Esc quits with no comments");
    }

    #[test]
    fn esc_quits_with_callback_in_view_mode() {
        let mut app = make_app(5, Mode::View);
        app.config.callback = Some("fzf".into());
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "auto + callback: Esc quits from view too");
    }

    #[test]
    fn esc_arms_then_confirms_the_quit_with_callback() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        add_comment(&mut app, 2, 2, "note");
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running);
        assert!(app.confirm_quit, "first Esc arms the confirmation");
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "armed Esc confirms the quit, not cancels");
    }

    #[test]
    fn esc_never_quits_with_callback_in_never_mode() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        app.config.esc_quit = EscQuit::Never;
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running, "never: Esc stays a pure cancel");
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        assert!(app.confirm_quit);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running);
        assert!(!app.confirm_quit, "never: armed Esc still cancels");
    }

    #[test]
    fn esc_always_quits_without_callback() {
        let mut app = make_app(5, Mode::View);
        app.config.esc_quit = EscQuit::Always;
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "always: Esc quits even without callback");
    }

    #[test]
    fn esc_cancels_selection_before_quitting() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        });
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running, "a pending selection cancels first");
        assert!(app.selection.is_none());
    }

    #[test]
    fn overlay_esc_closes_the_overlay_not_the_app() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        open_overlay(&mut app, Overlay::Help, 0);
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.running);
        assert!(app.overlay.is_none(), "overlay Esc never quits");
    }

    #[test]
    fn prompt_message_reflects_the_esc_binding() {
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — q to quit, Esc to cancel")
        );
        app.config.callback = Some("fzf".into());
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — Esc/q to quit")
        );
    }

    #[test]
    fn help_rows_reflect_the_esc_binding() {
        let rows = help_rows(false);
        assert!(
            rows.iter().any(|(l, k)| *l == "quit" && *k == "q quit · Esc cancel"),
            "default help advertises Esc as cancel"
        );
        let rows = help_rows(true);
        assert!(
            rows.iter().any(|(l, k)| *l == "quit" && *k == "Esc/q quit"),
            "esc-quit help advertises Esc/q as quit"
        );
    }

    #[test]
    fn q_in_view_mode_quits_when_there_are_no_comments() {
        let mut app = make_app(5, Mode::View);
        on_view_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE, None);
        assert!(!app.running);
    }

    #[test]
    fn delete_removes_every_comment_covering_the_cursor() {
        let mut app = make_app(10, Mode::Source);
        add_comment(&mut app, 3, 5, "range");
        add_comment(&mut app, 7, 7, "single");
        app.cursor = 4; // inside the 3-5 range
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1);
        assert_eq!(app.comments[0].start, 7);
    }

    #[test]
    fn delete_without_a_comment_on_the_line_flashes() {
        let mut app = make_app(10, Mode::Source);
        add_comment(&mut app, 3, 5, "range");
        app.cursor = 9;
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "nothing deleted");
        assert!(app.status.is_some(), "a flash message explains the miss");
    }

    #[test]
    fn composer_on_the_last_line_stays_on_screen() {
        // A comment on the LAST line used to push its bar off the bottom
        // of the pane: the open-time keep_cursor_visible ran on the
        // pre-composer row layout (the bar was not folded into line_rows
        // yet), so nothing scrolled and the bar rendered below the fold,
        // invisible. The draw pass folds the bar in and nudges the
        // offset so the bar's bottom rule sits at the pane bottom.
        let mut app = make_app(25, Mode::Source);
        app.cursor = 24; // last line
        app.offset = app.max_offset(22); // last line sits at the pane bottom
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        // The draw pass: fold the bar into line_rows, then nudge.
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app.keep_composer_visible(22);
        let start = app.row_of(24);
        let end = start + app.rows_of(24);
        assert_eq!(end, 28, "last line + top rule + body + bottom rule");
        assert_eq!(app.offset, 6, "scrolled so the bar's bottom row is visible");
        assert!(start >= app.offset, "the commented line is still visible");
        assert!(end <= app.offset + 22, "the whole bar is inside the pane");
        // As the input wraps to more rows, the per-frame nudge keeps the
        // bar's bottom rule at the pane bottom (before, a growing bar
        // clipped — nothing re-scrolled while typing).
        app.input = "x".repeat(200); // 3 wrapped body rows at full width 78
        app.input_cursor = app.input.len();
        app.refresh_line_rows();
        app.keep_composer_visible(22);
        let end = app.row_of(24) + app.rows_of(24);
        assert_eq!(end, 30, "5-row bar after wrapping");
        assert_eq!(app.offset, 8);
        assert!(end <= app.offset + 22, "the grown bar still fits");
    }

    #[test]
    fn source_composer_on_the_last_line_renders_in_the_buffer() {
        // End-to-end: with the cursor on the last line and the pane
        // scrolled to the bottom, the composer bar must actually be
        // painted inside the terminal buffer (the user-visible regression:
        // the bar and its text were below the fold, invisible).
        let mut app = make_app(25, Mode::Source);
        app.cursor = 24; // last line
        app.offset = app.max_offset(18); // pane bottom (TestBackend 20 rows − 2)
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "最終行のコメント".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // Wide glyphs (CJK) occupy two cells, the second a spacer; strip
        // spaces so multi-cell sequences compare as one string.
        let content: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .filter(|&c| c != ' ')
            .collect();
        assert!(
            content.contains("comment·25"),
            "the composer bar is visible for the last line"
        );
        assert!(
            content.contains("最終行のコメント▏"),
            "the typed comment text and cursor glyph are visible"
        );
        assert!(
            content.contains("line24"),
            "the commented line itself is still on screen"
        );
    }

    #[test]
    fn enter_confirms_the_comment_and_returns_to_source_mode() {
        let mut app = make_app(8, Mode::Source);
        app.cursor = 1;
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        }); // select 0-based 1..=2
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (1, 2));
        for ch in "テスト".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.input, "テスト");
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.comments.len(), 1);
        let c = &app.comments[0];
        assert_eq!((c.start, c.end), (2, 3), "1-based inclusive range");
        assert_eq!(c.text, "テスト");
        assert_eq!(c.lines, "line2\nline3", "verbatim snippet");
        assert!(app.selection.is_none(), "the selection was consumed");
        assert!(app.ime_guard.is_none(), "the IME guard is dropped");
    }

    #[test]
    fn enter_with_blank_input_is_rejected() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        app.input = "   ".into();
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Input, "stays in the composer");
        assert!(app.comments.is_empty());
    }

    #[test]
    fn esc_cancels_the_composer() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        app.input = "hello".into();
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Source);
        assert!(app.input.is_empty(), "the draft is discarded");
        assert!(app.comments.is_empty());
        assert!(app.ime_guard.is_none());
    }

    #[test]
    fn composer_cursor_moves_and_edits_mid_text() {
        // Arrows move the text cursor; insertion, Backspace, and Delete
        // act at the cursor instead of only at the end (the re-edit
        // prefill was uneditable without this).
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "abc".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(app.input, "abXc", "insert at the cursor");
        on_input_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.input, "abc", "Backspace deletes before the cursor");
        on_input_key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.input, "ab", "Delete removes at the cursor");
        // Home / End and their readline twins Ctrl+a / Ctrl+e.
        on_input_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('>'), KeyModifiers::NONE);
        assert_eq!(app.input, ">ab");
        on_input_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(app.input, ">ab!");
        on_input_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.input, "ab!");
        // Left at the start and Right at the end are clamped no-ops.
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, 0);
        on_input_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn composer_cursor_handles_multibyte_and_lines() {
        // Multibyte chars move as one unit; Up/Down cross logical lines
        // keeping the character column.
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "日本語".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        for ch in "二行目".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.input, "日本語\n二行目");
        // Left steps over one CJK char (3 bytes), never mid-char.
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(app.input, "日本X語\n二行目", "Up kept the char column");
        // Cursor sits after X (col 3). Two Lefts → col 1, Down keeps it.
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('Y'), KeyModifiers::NONE);
        assert_eq!(app.input, "日本X語\n二Y行目", "Down kept the char column");
        // Up at the first line goes to the text start; Down at the last
        // line to the text end.
        on_input_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, 0);
        on_input_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn ctrl_j_and_backspace_edit_the_buffer() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "ab".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(app.input, "ab\nc");
        on_input_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.input, "ab\n");
    }

    #[test]
    fn v_starts_a_selection_and_j_k_extend_it() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 3;
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection::new(3)));
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 4));
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 3));
        // Extending past the top clamps at line 0.
        for _ in 0..5 {
            on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        }
        assert_eq!(app.selection.unwrap().range(), (0, 3));
    }

    #[test]
    fn j_k_move_the_cursor_without_a_selection() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 3;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 4);
        assert!(app.selection.is_none());
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 3);
        // Moving past the last line clamps.
        app.cursor = 9;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 9);
    }

    #[test]
    fn c_anchors_the_composer_to_the_selection_end() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 5;
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (5, 6));
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.cursor, 6, "cursor follows the selection end");
    }

    #[test]
    fn c_on_an_empty_file_flashes_instead_of_opening() {
        let mut app = make_app(0, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert!(app.status.is_some());
    }

    #[test]
    fn esc_cancels_the_selection_and_never_switches_modes() {
        // Tab is the ONE mode toggle: a reflexive Esc must not flip the
        // mode (it used to, and the reading position got lost).
        let mut app = make_app(10, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "Esc only clears the selection");
        assert!(app.selection.is_none());
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "a second Esc stays in source mode");
    }

    #[test]
    fn tab_returns_to_view_on_the_same_source_line() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 7;
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 7, "exact line handoff");
        // The view reveals the cursor (draw_view no longer auto-scrolls).
        let row = app.view.cursor_row();
        let offset = app.view.offset;
        let viewport = app.view_viewport_rows();
        assert!(
            row >= offset && row < offset + viewport,
            "cursor revealed (row {row}, offset {offset}, viewport {viewport})"
        );
    }

    #[test]
    fn view_v_starts_a_selection_in_view_mode() {
        // Headings render one row each (no blank rows, no paragraph merge),
        // so display-row movement maps 1:1 to source lines.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=10 {
            writeln!(f, "# line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::View;
        // Deterministic 1:1 row mapping (headings etc. render with extra
        // blank rows that would skew the display-row walk).
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.move_cursor_display(6);
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(
            app.mode, Mode::View,
            "v starts a selection in view; Tab switches modes"
        );
        assert_eq!(app.selection, Some(Selection::new(6)));
        // j/k extend the selection, parallel to source mode.
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (6, 7));
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (6, 6));
        // Esc clears the selection without leaving view mode.
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
        assert_eq!(app.mode, Mode::View);
    }

    #[test]
    fn view_v_lands_on_the_head_of_a_merged_paragraph_row() {
        // The renderer joins a paragraph's soft-wrapped lines into one rendered
        // row; display-row navigation leaves the view cursor on the row's
        // LAST source line, but the selection must cover the whole row —
        // the head through the row's last line (lines 2-4 merge onto row 1,
        // so the selection is 2-4, 1-based).
        let mut app = make_app(10, Mode::View);
        app.view.rows = vec![vec![]; 4];
        app.view.source_starts = vec![0, 1, 1, 1, 2, 3, 3, 3, 3, 3];
        app.view.cursor = 3; // last source line of the merged group
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        // Model A: the selection is the cursor line (line-granular, same as
        // source mode); the gray may show the whole row, but the comment
        // covers exactly the selected line.
        assert_eq!(app.selection, Some(Selection::new(3)));
    }

    #[test]
    fn view_selection_extends_by_lines_like_source_mode() {
        // j/k step by source lines — byte-for-byte the same semantics as
        // source mode (1:1 rows here, so a line is a display row).
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..8).map(|_| vec![]).collect();
        app.view.source_starts = (0..8).collect();
        app.view.goto_source_line(3);
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection::new(3)), "v selects the cursor line");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 4), "j extends one display row");
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 3), "k shrinks back");
        // k past the anchor extends up.
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (2, 3), "k extends up past the anchor");
        // Esc clears the selection.
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
    }

    #[test]
    fn view_c_comments_the_selection_and_switches_modes() {
        let mut app = make_app(10, Mode::View);
        // A two-line selection, then c: the composer anchors to the whole
        // range and the app lands in Input.
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input_start, 2);
        assert_eq!(app.input_end, 4, "the selection is not overwritten");
        // Without a selection, c falls back to the cursor line (1:1 row
        // mapping so the paragraph head is the line itself).
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(5);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input_start, 5);
        assert_eq!(app.input_end, 5);
    }

    #[test]
    fn view_c_opens_the_composer_directly() {
        let mut app = make_app(10, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input, "c opens the composer right away");
        assert_eq!(
            app.selection,
            Some(Selection::new(0)),
            "the cursor line is selected first"
        );
        assert_eq!(app.input_start, 0);
        assert_eq!(app.input_end, 0);
    }

    #[test]
    fn view_c_on_an_empty_file_flashes_instead_of_entering_input() {
        let mut app = make_app(0, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View, "empty file: stays in view mode");
        assert!(app.status.is_some(), "flashes a message");
    }

    #[test]
    fn view_c_enters_and_returns_without_leaving_view_mode() {
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(3);
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        });
        // c opens the composer inline (still "from view": Enter/Esc return
        // to View, not Comment).
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.composer_return, Mode::View);
        assert_eq!(app.input_start, 2);
        assert_eq!(app.input_end, 4);
        // Esc cancels and returns to view, keeping the selection.
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View, "Esc returns to view mode");
        assert!(app.selection.is_some(), "cancel keeps the selection");
        // c again, this time confirming: the comment lands and view stays.
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View, "Enter returns to view mode");
        assert_eq!(app.comments.len(), 1);
        assert_eq!(app.comments[0].start, 3, "anchored to the selection");
        assert_eq!(app.comments[0].end, 5);
        assert!(app.selection.is_none(), "the selection was consumed");
    }

    #[test]
    fn source_c_still_returns_to_source_mode() {
        let mut app = make_app(10, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.composer_return, Mode::Source);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Source, "source-mode c still lands in comment");
        assert_eq!(app.comments.len(), 1);
    }

    #[test]
    fn view_n_next_jumps_between_commented_lines() {
        let mut app = make_app(10, Mode::View);
        let cur = app.current_file_path().to_path_buf();
        app.comments.push(Comment {
            file_path: cur.clone(),
            start: 3,
            end: 3,
            lines: "line3".into(),
            text: "c1".into(),
        });
        app.comments.push(Comment {
            file_path: cur,
            start: 7,
            end: 7,
            lines: "line7".into(),
            text: "c2".into(),
        });
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 2, "n jumps to the first comment below");
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 6, "n jumps to the next comment");
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 2, "N jumps back");
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 2, "N stays put when no comment above");
        assert!(app.status.is_some(), "and flashes a message");
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 6, "n resumes from where it stopped");
    }

    #[test]
    fn mode_switch_keeps_the_cursor_screen_row() {
        // view: cursor row 20, scrolled to offset 10 → cursor at content
        // row 10, PHYSICAL terminal row 12 (title + the view frame's top
        // border). Source mode draws no frame (content starts at physical
        // row 1), so its offset lands the cursor at content row 11 — the
        // same physical row. The round trip restores the view offset.
        // A 1:1 row mapping (each source line its own rendered row) keeps
        // the math exact; make_app's plain lines would merge into a
        // paragraph in the rendered view.
        let mut app = make_app(60, Mode::View);
        app.view.rows = (0..60).map(|_| vec![]).collect();
        app.view.source_starts = (0..60).collect();
        app.view.goto_source_line(20);
        app.view.offset = 10;
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.cursor, 20);
        assert_eq!(
            app.offset,
            9,
            "comment content row 11 (20 − 9) = the view's physical row"
        );
        // Back to view (Tab) — which mirrors the same physical row.
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 20);
        assert_eq!(app.view.offset, 10, "the round trip restores the view offset");
    }

    #[test]
    fn mode_switch_carries_the_selection_over() {
        // A view selection survives the Tab toggle: selection is one state
        // shared by both modes (the parallel model).
        let mut app = make_app(10, Mode::View);
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 5,
        });
        app.view.goto_source_line(5);
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: 2,
                cursor: 5
            }),
            "the selection is carried into source mode"
        );
        assert_eq!(app.cursor, 5, "the comment cursor sits on the extent");
        // Esc clears the carried selection but never switches modes; Tab
        // returns to view with the selection gone.
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "Esc clears the selection only");
        assert!(app.selection.is_none());
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
    }

    #[test]
    fn mode_switch_clamps_at_the_edges() {
        // Cursor scrolled off-screen above in view (offset 40 > row 30): the
        // preserved screen row would push the offset past the scrollable
        // max, so it clamps (the allowed edge exception).
        let mut app = make_app(60, Mode::View);
        app.view.rows = (0..60).map(|_| vec![]).collect();
        app.view.source_starts = (0..60).collect();
        app.view.goto_source_line(30);
        app.view.offset = 40;
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        let max = app.max_offset(app.source_viewport_rows() as u16);
        assert_eq!(app.offset, max, "offset clamps at the scrollable max");
    }

    #[test]
    fn view_marker_gutter_flags_commented_rows() {
        // Merged rows: lines 2-4 share one rendered row. A comment on line 2
        // must mark the shared row even though it maps to the group's last
        // source line.
        let view = ViewState {
            rows: vec![vec![], vec![], vec![], vec![]],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 1, 1, 1],
            ..Default::default()
        };
        let marked = vec![false, true, false, false]; // line 1 (0-based) commented
        let (_, gutter) = view.visible_text(
            10,
            &marked,
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            ratatui::style::Style::default(),
        );
        // Row 1 (source lines 1-3 merged) carries the marker glyph.
        assert_eq!(gutter[1].glyph, "▌", "merged row shows the marker");
        // Row 0 is unmarked; the cursor (line 0) shows `>` instead.
        assert_ne!(gutter[0].glyph, "▌", "unmarked row shows no marker");
    }

    #[test]
    fn view_marker_flags_cover_the_whole_range() {
        // A multi-line comment (3-5) flags every covered line; a
        // single-line comment (9) flags its own line.
        let comments = vec![
            Comment {
                file_path: "d.md".into(),
                start: 3,
                end: 5,
                lines: String::new(),
                text: "c1".into(),
            },
            Comment {
                file_path: "d.md".into(),
                start: 9,
                end: 9,
                lines: String::new(),
                text: "c2".into(),
            },
        ];
        let marked = view_marker_flags(&comments, 10, Path::new("d.md"));
        assert!(marked[2] && marked[3] && marked[4], "every covered line is flagged");
        assert!(!marked[1] && !marked[5], "lines outside the range are clean");
        assert!(marked[8], "single-line comment flags its own line");
        assert_eq!(marked.len(), 10);
        assert!(view_marker_flags(&comments, 0, Path::new("d.md")).is_empty());
    }

    #[test]
    fn n_n_jump_to_comment_block_heads() {
        // Overlapping ranges stay separate jump targets: A(2-5) and
        // B(4-6) overlap but are jumped to one by one, so the selection
        // never becomes their merged union; C(8-9) is the next target.
        let mut app = make_app(10, Mode::View);
        let cur = app.current_file_path().to_path_buf();
        let comments = vec![
            Comment {
                file_path: cur.clone(),
                start: 2,
                end: 5,
                lines: String::new(),
                text: "c1".into(),
            },
            Comment {
                file_path: cur.clone(),
                start: 4,
                end: 6,
                lines: String::new(),
                text: "c2".into(),
            },
            Comment {
                file_path: cur,
                start: 8,
                end: 9,
                lines: String::new(),
                text: "c3".into(),
            },
        ];
        assert_eq!(
            comment_regions(&comments, 10, app.current_file_path()),
            vec![(1, 4), (3, 5), (7, 8)],
            "overlapping comments stay separate targets"
        );
        app.comments = comments;
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 4, "cursor lands on the first comment's extent");
        assert_eq!(
            app.selection,
            Some(Selection { anchor: 1, cursor: 4 }),
            "only the first comment becomes the selection"
        );
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 5, "n steps into the overlapping comment");
        assert_eq!(app.selection, Some(Selection { anchor: 3, cursor: 5 }));
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 8, "n skips to the last comment");
        assert_eq!(app.selection, Some(Selection { anchor: 7, cursor: 8 }));
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 8, "n at the last comment flashes");
        assert!(app.status.is_some());
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 5, "N returns to the overlapping comment");
        assert_eq!(app.selection, Some(Selection { anchor: 3, cursor: 5 }));
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 4, "N returns to the first comment");
        assert_eq!(app.selection, Some(Selection { anchor: 1, cursor: 4 }));
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 4, "N at the first comment flashes");
        // Clearing the selection falls back to cursor-based navigation.
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 8, "n from a cleared cursor still finds the next comment");
    }

    #[test]
    fn line_change_counts_trim_prefix_and_suffix() {
        assert_eq!(line_change_counts(&[], &[]), (0, 0));
        let old: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        let mut new = old.clone();
        assert_eq!(line_change_counts(&old, &new), (0, 0));
        // Agent truncates the tail.
        new.truncate(7);
        assert_eq!(line_change_counts(&old, &new), (0, 3));
        // Agent inserts lines in the middle.
        new = old.clone();
        new.splice(3..3, ["x".into(), "y".into()]);
        assert_eq!(line_change_counts(&old, &new), (2, 0));
        // Agent replaces a middle line (counts as +1/-1).
        new = old.clone();
        new[4] = "changed".into();
        assert_eq!(line_change_counts(&old, &new), (1, 1));
    }

    #[test]
    fn reload_replaces_source_and_clamps_anchors() {
        let (mut app, _dir) = make_app_keep(10, Mode::Source);
        app.cursor = 9;
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 9,
            end: 10,
            lines: String::new(),
            text: "c".into(),
        });
        // The agent rewrites the file down to 7 lines.
        let mut f = std::fs::File::create(app.current_file_path()).unwrap();
        for i in 1..=7 {
            writeln!(f, "line{i}").unwrap();
        }
        assert!(reload_source(&mut app).is_ok());
        assert_eq!(app.source.len(), 7);
        assert_eq!(app.cursor, 6, "cursor clamps to the new last line");
        assert!(
            app.comments.is_empty(),
            "comments are cleared on reload — anchors are stale"
        );
        assert_eq!(app.last_change, Some((0, 3)), "toast reports the tail cut");
        assert!(!app.file_changed, "the pending prompt clears on reload");
        assert!(app.selection.is_none(), "selection is cleared");
    }

    #[test]
    fn r_reloads_and_i_ignores_a_pending_change() {
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        // Simulate a detected external edit.
        app.file_changed = true;
        app.file_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        // The agent appends a line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(app.current_file_path())
            .unwrap();
        writeln!(f, "line6").unwrap();
        // r reloads and clears the prompt.
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert_eq!(app.source.len(), 6, "r replaces the in-memory source");
        assert!(!app.file_changed, "r clears the pending prompt");
        assert_eq!(app.last_change, Some((1, 0)));
        // A fresh change can be ignored with i.
        app.file_changed = true;
        on_source_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE, None);
        assert!(!app.file_changed, "i marks the change as seen");
        assert_eq!(
            app.last_loaded_stamp, app.file_stamp,
            "ignored state counts as loaded"
        );
    }

    #[test]
    fn r_with_comments_requires_a_second_r() {
        // The reload clears this file's comments, so `r` asks first — the
        // same two-press pattern as `e`/`q`. The agent's rewrite is the
        // most frequent action; the comments it invalidated must not
        // vanish on one stray key.
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        // The agent appends a line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(app.current_file_path())
            .unwrap();
        writeln!(f, "line6").unwrap();
        // Simulate the poll's detection of the external edit.
        app.file_changed = true;
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(app.confirm_reload, "first r only arms the confirmation");
        assert_eq!(app.source.len(), 5, "no reload yet");
        assert_eq!(app.comments.len(), 1, "comments untouched");
        assert!(app.file_changed, "the pending prompt stays up");
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(!app.confirm_reload, "second r confirms");
        assert_eq!(app.source.len(), 6, "second r reloads");
        assert!(app.comments.is_empty(), "comments cleared on reload");
        assert!(!app.file_changed, "the pending prompt clears");
    }

    #[test]
    fn r_without_comments_reloads_in_one_press() {
        // No comments to lose: one `r` reloads immediately (the pre-fix
        // behavior stays for the common no-comment case).
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(app.current_file_path())
            .unwrap();
        writeln!(f, "line6").unwrap();
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(!app.confirm_reload, "no confirmation needed");
        assert_eq!(app.source.len(), 6, "one r reloads with no comments");
    }

    #[test]
    fn esc_cancels_the_reload_confirmation() {
        // Esc must abort the armed reload: comments AND file content stay
        // exactly as they were (nothing reloaded, nothing cleared).
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        let content_before = app.source.content.clone();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(app.current_file_path())
            .unwrap();
        writeln!(f, "line6").unwrap();
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(app.confirm_reload);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.confirm_reload, "Esc clears the confirmation");
        assert_eq!(app.source.content, content_before, "content unchanged");
        assert_eq!(app.comments.len(), 1, "comments kept");
        // Esc in view mode cancels the same way.
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        add_comment(&mut app, 2, 2, "note");
        on_view_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(app.confirm_reload);
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.confirm_reload);
    }

    #[test]
    fn switching_file_cancels_the_reload_confirmation() {
        // ]/[ (and Tab's mode flip) move the user's context away from the
        // pending reload; the armed confirmation must not fire on a later
        // `r` in the new context.
        let (mut app, _dir) = make_session();
        add_comment(&mut app, 1, 1, "note");
        assert_eq!(app.mode, Mode::View, "a.md opens in view mode");
        on_view_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(app.confirm_reload);
        on_view_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        assert!(!app.confirm_reload, "file switch cancels the confirmation");
        assert_eq!(app.current_file_index, 1);
        // Back on a.md (view mode), arm again: Tab's mode flip cancels
        // too — the prompt's context is the pane the user looked at.
        on_source_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 0);
        assert_eq!(app.mode, Mode::View, "a.md restores to view mode");
        on_view_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert!(app.confirm_reload);
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert!(!app.confirm_reload, "mode switch cancels the confirmation");
        assert_eq!(app.mode, Mode::Source);
    }

    #[test]
    fn r_on_non_utf8_file_toasts_an_error_and_keeps_content() {
        // A binary write or a mid-write agent edit breaks UTF-8; `r` must
        // not fail silently (the ⚡ prompt staying up forever with a dead
        // `r` was the bug) — it toasts the reason and keeps the content.
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        std::fs::write(app.current_file_path(), [0xff, 0xfe, b'a', b'\n']).unwrap();
        // Simulate the poll's detection of the external edit.
        app.file_changed = true;
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        let (msg, _, is_error) = app.status.as_ref().expect("a toast fires");
        assert!(*is_error, "the failure is an error toast: {msg}");
        assert!(msg.contains("reload failed"), "toast names the failure: {msg}");
        assert!(msg.contains("reading"), "toast names the file read: {msg}");
        assert_eq!(app.source.len(), 5, "the old content stays");
        assert!(app.file_changed, "the pending prompt stays up for a retry");
        // A successful reload after the file recovers clears the error.
        std::fs::write(
            app.current_file_path(),
            "line1\nline2\nline3\nline4\nline5\nline6\n",
        )
        .unwrap();
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert_eq!(app.source.len(), 6, "r retries after the file recovers");
        assert!(!app.file_changed);
    }

    #[test]
    fn reload_skips_identical_content_but_handles_touches() {
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        // touch the file: same content, new mtime
        let file = app.current_file_path().to_path_buf();
        std::fs::write(&file, std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(reload_source(&mut app).is_ok(), "a touch is handled");
        assert_eq!(app.last_change, None, "no diff to report");
    }

    #[test]
    fn poll_arms_the_reload_on_stamp_mismatch() {
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        // Simulate the startup stamp from an earlier version of the file.
        app.last_loaded_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        app.file_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        poll_file_change(&mut app);
        assert!(
            app.reload_pending.is_some(),
            "a stamp mismatch arms the debounced reload"
        );
        assert!(
            app.file_stamp.is_some_and(|(_, len)| len > 0),
            "the current on-disk stamp is recorded"
        );
    }

    #[test]
    fn esc_cancels_the_quit_confirmation_in_both_modes() {
        // Source mode: q → confirm, Esc cancels it (even with a selection
        // active — the confirmation outranks the selection).
        let mut app = make_app(5, Mode::Source);
        app.selection = Some(Selection::new(2));
        app.comments.push(Comment {
            file_path: "d.md".into(),
            start: 1,
            end: 1,
            lines: String::new(),
            text: "c".into(),
        });
        on_source_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE, None);
        assert!(app.confirm_quit);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.confirm_quit, "Esc cancels the prompt first");
        assert!(
            app.selection.is_some(),
            "the selection survives; Esc did not double as selection-clear"
        );
        // View mode: q → confirm, Esc cancels (view has no other Esc role).
        let mut app = make_app(5, Mode::View);
        app.comments.push(Comment {
            file_path: "d.md".into(),
            start: 1,
            end: 1,
            lines: String::new(),
            text: "c".into(),
        });
        on_view_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE, None);
        assert!(app.confirm_quit);
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.confirm_quit);
        assert_eq!(app.mode, Mode::View, "stays in view mode");
    }

    #[test]
    fn source_mode_n_n_jumps_select_the_block() {
        // Two blocks: [1-3] (one multi-line comment) and [6] (a stacked
        // single-line comment). n/N in source mode land on the block
        // extent and the whole block becomes the selection, exactly like
        // view mode.
        let mut app = make_app(10, Mode::Source);
        let cur = app.current_file_path().to_path_buf();
        app.comments.push(Comment {
            file_path: cur.clone(),
            start: 2,
            end: 4,
            lines: String::new(),
            text: "c1".into(),
        });
        app.comments.push(Comment {
            file_path: cur.clone(),
            start: 7,
            end: 7,
            lines: String::new(),
            text: "c2".into(),
        });
        app.comments.push(Comment {
            file_path: cur,
            start: 7,
            end: 7,
            lines: String::new(),
            text: "c3".into(),
        });
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 3, "cursor lands on the block extent");
        assert_eq!(
            app.selection,
            Some(Selection { anchor: 1, cursor: 3 }),
            "the block [1-3] is selected"
        );
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 6, "stacked comments count as one block");
        assert_eq!(app.selection, Some(Selection { anchor: 6, cursor: 6 }));
        // N returns to the previous block, re-selecting it.
        app.selection = Some(Selection::new(6));
        on_source_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 3, "N returns to the previous block extent");
        assert_eq!(app.selection, Some(Selection { anchor: 1, cursor: 3 }));
    }

    #[test]
    fn view_composer_renders_inline_in_the_rendered_view() {
        // Draw the app while the view-origin composer is open and check the
        // buffer: the composer bar must appear inside the rendered view
        // (title under the cursor line), not a mode switch.
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(3);
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            content.contains("comment · 4"),
            "the composer bar renders inside the view"
        );
        assert!(content.contains('▏'), "the cursor glyph is in the bar");
    }

    #[test]
    fn view_composer_on_the_last_line_stays_on_screen() {
        // Commenting the LAST line used to clamp the view's scroll at the
        // document's last row, pushing the composer bar below the pane —
        // invisible. The bar extends the scrollable extent (max_off grows
        // by the bar's height), so the nudge can always reveal it.
        let mut app = make_app(25, Mode::View);
        app.view.rows = (0..25).map(|_| vec![]).collect();
        app.view.source_starts = (0..25).collect();
        app.view.goto_source_line(24);
        app.view.offset = app.view.rows.len().saturating_sub(20); // pane bottom
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input_end, 24);
        app.keep_composer_visible_view(20);
        assert_eq!(view_composer_anchor(&app), 24, "anchor at the last row");
        let start_row = view_composer_anchor(&app).saturating_sub(app.view.offset) + 1;
        assert_eq!(
            start_row + 3,
            20,
            "the whole 3-row bar fits, ending at the pane bottom (start_row {start_row})"
        );
    }

    #[test]
    fn view_composer_typing_scrolls_to_keep_the_bar_on_screen() {
        // As the input wraps to more rows, the per-frame nudge scrolls the
        // view so the bar's bottom rule stays at the pane bottom (before,
        // only the open-time scroll existed — a growing bar clipped).
        let mut app = make_app(25, Mode::View);
        app.view.rows = (0..25).map(|_| vec![]).collect();
        app.view.source_starts = (0..25).collect();
        app.view.goto_source_line(24);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        app.input = "x".repeat(200);
        app.input_cursor = app.input.len();
        app.keep_composer_visible_view(20);
        let h = composer_line_count(&app.input, app.input_cursor, view_content_width(&app));
        assert_eq!(h, 5, "top rule + 3 wrapped rows + bottom rule");
        let start_row = view_composer_anchor(&app).saturating_sub(app.view.offset) + 1;
        assert_eq!(
            start_row + h,
            20,
            "the bar's bottom rule sits at the pane bottom (start_row {start_row})"
        );
    }

    #[test]
    fn view_composer_on_the_last_line_renders_in_the_buffer() {
        // End-to-end: commenting the LAST line in view mode must paint the
        // composer bar inside the terminal buffer (the regression: the
        // scroll clamp at the document's last row left the bar below the
        // pane, invisible).
        let mut app = make_app(25, Mode::View);
        app.view.rows = (0..25).map(|_| vec![]).collect();
        app.view.source_starts = (0..25).collect();
        app.view.goto_source_line(24);
        app.view.offset = app.view.rows.len().saturating_sub(16); // pane bottom
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "最終行".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // Wide glyphs (CJK) occupy two cells, the second a spacer; strip
        // spaces so multi-cell sequences compare as one string.
        let content: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .filter(|&c| c != ' ')
            .collect();
        assert!(
            content.contains("comment·25"),
            "the composer bar is visible for the last line"
        );
        assert!(
            content.contains("最終行▏"),
            "the typed comment text and cursor glyph are visible"
        );
    }

    #[test]
    fn view_folds_comment_cards_into_the_layout() {
        // Headings render one row each, so the mapping is deterministic: a
        // comment on lines 2-3 inserts its card after line 3's row and
        // shifts the source-line mapping for everything below.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=8 {
            writeln!(f, "# line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let comments = vec![Comment {
            file_path: "d.md".into(),
            start: 2,
            end: 3,
            lines: String::new(),
            text: "card body".into(),
        }];
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let width = view_render_width(w);
        let view = render_view_with_cards(&source, width, &highlight, &comments);
        let card_h = view.card_rows.iter().filter(|&&b| b).count();
        assert!(card_h >= 3, "title + body + rule rows");
        let base = ViewState::render(&source, width, &highlight);
        assert_eq!(
            view.source_starts[3],
            base.source_starts[3] + card_h,
            "rows below the comment end shift by the card height"
        );
        let first_card = view.card_rows.iter().position(|&b| b).unwrap();
        let card_text: String = view
            .card_rows
            .iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .flat_map(|(i, _)| view.rows[i].iter().map(|s| s.text.as_str()))
            .collect::<String>();
        assert!(card_text.contains("card body"), "the card text is in the view");
        // Card rows float in the text column like every other row: one
        // pad on each side (the title keeps its own leading space).
        let (text, gutter) = view.visible_text(
            10,
            &[],
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            ratatui::style::Style::default(),
        );
        let row: String = text.lines[first_card]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            row.trim_start().starts_with("comment · 2-3 "),
            "card row starts with its title: {row:?}"
        );
        assert_eq!(
            gutter[first_card].glyph, "│",
            "card rows keep the plain border"
        );
    }

    #[test]
    fn comment_added_from_view_folds_the_card_in() {
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View);
        assert!(
            app.view.card_rows.iter().any(|&b| b),
            "the new card is folded into the view layout"
        );
        // Deleting the comment removes the card again.
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None); // to source mode
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert!(app.comments.is_empty());
        assert!(
            !app.view.card_rows.iter().any(|&b| b),
            "the card is removed with the comment"
        );
    }

    #[test]
    fn block_last_row_spans_a_merged_paragraph() {
        // Lines a/b/c merge into one paragraph wrapped over rows 0-2, a
        // blank owns row 3, a heading row 4. Any line of the paragraph
        // anchors at the group's last row (2); the blank and the heading
        // are their own blocks.
        let starts = vec![0, 0, 0, 3, 4];
        assert_eq!(block_last_row(&starts, 5, 0), 2);
        assert_eq!(block_last_row(&starts, 5, 2), 2);
        assert_eq!(block_last_row(&starts, 5, 3), 3);
        assert_eq!(block_last_row(&starts, 5, 4), 4);
    }

    #[test]
    fn block_last_row_stays_inside_an_html_block() {
        // 1:1 lines (an HTML block renders one row per source line, no
        // blanks in between): the block of each line is exactly its own
        // row, so a card for a comment ending mid-block sits right under
        // its end line — the same place source mode shows it — instead of
        // dropping below the whole non-blank run (regression: comment
        // 492-498 rendered its card under </div> at line 501).
        let starts = vec![0, 1, 2, 3, 4, 5];
        assert_eq!(block_last_row(&starts, 6, 2), 2);
        assert_eq!(block_last_row(&starts, 6, 5), 5, "last line anchors at the last row");
    }

    #[test]
    fn view_card_sits_under_its_end_line_inside_an_html_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "# t\n\n<div>\n<p>x</p>\n<span>y</span>\n</div>\n",
        )
        .unwrap();
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(None, false);
        // The comment ends on the <p> line (1-based 4, 0-based 3).
        let comments = vec![Comment {
            file_path: "d.md".into(),
            start: 3,
            end: 4,
            lines: String::new(),
            text: "mid-block".into(),
        }];
        let base = ViewState::render(&source, 60, &highlight);
        let view = render_view_with_cards(&source, 60, &highlight, &comments);
        let first_card = view.card_rows.iter().position(|&b| b).unwrap();
        assert_eq!(
            first_card,
            base.source_starts[3] + 1,
            "the card sits directly under the <p> line's row, not under </div>"
        );
    }

    #[test]
    fn composer_anchors_at_the_selection_range_bottom() {
        // Downward selection: anchor 2, cursor 5 → input_end 5.
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(5);
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 5,
        });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input_end, 5);
        assert_eq!(view_composer_anchor(&app), 5, "below line 5's block");
        // Upward selection: anchor 6, cursor 2 → the range max is still 6.
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(2);
        app.selection = Some(Selection {
            anchor: 6,
            cursor: 2,
        });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input_end, 6, "range max, not the cursor");
        assert_eq!(view_composer_anchor(&app), 6);
    }

    #[test]
    fn g_and_g_jump_to_the_ends() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 5;
        app.offset = 4;
        on_source_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE, None);
        assert_eq!((app.cursor, app.offset), (0, 0));
        on_source_key(&mut app, KeyCode::Char('G'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 9);
    }

    #[test]
    fn page_keys_ride_the_cursor_and_clamp() {
        // View-mode spec ported to source mode: the page keys move the
        // CURSOR by a page (Ctrl+u/d a half page) and the viewport follows
        // — no more viewport-only scroll that strands the cursor off
        // screen. Clamps at both document edges.
        let mut app = make_app(50, Mode::Source);
        let viewport = app.source_viewport_rows();
        on_source_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE, None);
        assert_eq!(app.cursor, viewport.min(49), "PageDown jumps the cursor a page");
        assert_cursor_visible(&app, viewport);
        for _ in 0..20 {
            on_source_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE, None);
        }
        assert_eq!(app.cursor, 49, "clamped at the last line");
        assert!(app.offset <= app.max_offset(app.source_viewport_rows() as u16));
        for _ in 0..40 {
            on_source_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE, None);
        }
        assert_eq!(app.cursor, 0, "PageUp clamps at the top");
        assert_eq!(app.offset, 0);
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, viewport / 2, "Ctrl+d jumps half a page");
        on_source_key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, 0, "Ctrl+u jumps back");
    }

    #[test]
    fn view_s_y_d_are_wired() {
        // The export/delete keys work from the view without a mode switch.
        // With no comments they flash instead of exporting (and the early
        // return keeps the test off the real clipboard).
        let mut app = make_app(10, Mode::View);
        on_view_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "s flashes 'no comments yet'");
        app.status = None;
        on_view_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "y flashes 'no comments yet'");
        app.status = None;
        on_view_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "d flashes 'no comment on this line'");
    }

    #[test]
    fn view_jk_skips_blank_lines() {
        // The view's j/k stop only on content lines — blanks render as gap
        // rows and stopping there is a wasted keypress. The mouse, a
        // selection's j/k extension, and source mode still reach blanks.
        let mut app = make_app(7, Mode::View);
        app.source.lines = vec![
            "a".into(),
            String::new(),
            String::new(),
            "b".into(),
            String::new(),
            "c".into(),
            String::new(),
        ];
        app.view.rows = (0..7).map(|_| vec![]).collect();
        app.view.source_starts = (0..7).collect();
        app.view.goto_source_line(0);
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 3, "j skips the blank run onto b");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 5, "j skips onto c");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 5, "only a trailing blank below: stays put");
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 3, "k skips back over the blanks");
        // With a selection active, j/k extend line by line (blanks
        // included — a range must stay contiguous).
        app.selection = Some(Selection::new(3));
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(
            app.selection.unwrap().range(),
            (3, 4),
            "selection extension still walks every line"
        );
    }

    /// The cursor's display-row range under the current layout cache.
    fn cursor_row_range(app: &App) -> (usize, usize) {
        let start = app.row_of(app.cursor);
        (start, start + app.rows_of(app.cursor))
    }

    /// The cursor row range must sit inside the window the renderer paints
    /// (`[offset, offset + viewport)`), or the cursor line is off-screen.
    fn assert_cursor_visible(app: &App, viewport: usize) {
        let (start, end) = cursor_row_range(app);
        assert!(
            start >= app.offset,
            "cursor row {start} above the viewport top {} (cursor {} offset {})",
            app.offset,
            app.cursor,
            app.offset
        );
        assert!(
            end <= app.offset + viewport,
            "cursor row {end} below the viewport bottom {} (cursor {} offset {})",
            app.offset + viewport,
            app.cursor,
            app.offset
        );
    }

    #[test]
    fn j_spam_keeps_the_cursor_row_in_view() {
        // Regression: keep_cursor_visible was fed the raw terminal height,
        // so the down branch (end > offset + height) fired `height - viewport`
        // rows late — the cursor hid in the pane's bottom rows while the
        // viewport never caught up. The up branch (start < offset) is
        // height-independent, which is why only downward movement broke.
        let mut app = make_app(200, Mode::Source);
        let viewport = app.source_viewport_rows() as usize;
        for _ in 0..250 {
            on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
            assert_cursor_visible(&app, viewport);
        }
        assert_eq!(app.cursor, 199, "the cursor reached the last line");
    }

    #[test]
    fn one_j_or_k_yanks_a_scrolled_away_cursor_back_into_view() {
        // Wheel-style scrolling moves the viewport alone — the cursor keeps
        // its absolute file position and can sit far outside the window.
        // The next keyboard move must pull the viewport back in one step.
        let mut app = make_app(100, Mode::Source);
        // Cursor far above the viewport: one j lands it inside.
        app.cursor = 30;
        app.offset = 80;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 31);
        assert_cursor_visible(&app, app.source_viewport_rows() as usize);
        // Cursor far below the viewport: one k lands it inside — this
        // direction is the one that used to fail (height-dependent branch).
        app.cursor = 90;
        app.offset = 10;
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 89);
        assert_cursor_visible(&app, app.source_viewport_rows() as usize);
    }

    /// A session app over two temp files (a.md viewable, b.rs not).
    fn make_session() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("a.md");
        let rs_path = dir.path().join("b.rs");
        std::fs::write(&md_path, "# a\n\nline2\nline3\n").unwrap();
        std::fs::write(&rs_path, "fn main() {}\n").unwrap();
        let config = Config {
            files: vec![md_path, rs_path],
            send_cmd: None,
            send_agent: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(None, false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        // Simulate run()'s per-file modes: a.md → View, b.rs → Source.
        app.file_states = vec![
            FileState {
                mode: Mode::View,
                ..Default::default()
            },
            FileState {
                mode: Mode::Source,
                ..Default::default()
            },
        ];
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        (app, dir)
    }

    #[test]
    fn supports_view_matches_markdown_family_only() {
        assert!(supports_view(Path::new("doc.md")));
        assert!(supports_view(Path::new("doc.MARKDOWN")));
        assert!(supports_view(Path::new("doc.mdx")));
        assert!(!supports_view(Path::new("doc.rs")));
        assert!(!supports_view(Path::new("doc.toml")));
        assert!(!supports_view(Path::new("doc")));
    }

    #[test]
    fn session_switches_files_with_brackets() {
        let (mut app, _dir) = make_session();
        assert_eq!(app.current_file_index, 0);
        assert_eq!(app.mode, Mode::View);
        // ] → next file (b.rs, source-only).
        on_view_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 1);
        assert_eq!(app.mode, Mode::Source, "b.rs opens in source mode");
        // Tab is a no-op on the source-only file.
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "Tab cannot enter view for .rs");
        assert!(app.status.is_some(), "a toast explains the restriction");
        // [ → back to a.md, view mode restored.
        on_source_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 0);
        assert_eq!(app.mode, Mode::View, "per-file mode restored");
    }

    #[test]
    fn ctrl_p_opens_the_file_picker_and_enter_switches() {
        let (mut app, _dir) = make_session();
        // Ctrl+p opens the Files overlay, cursor on the current file.
        on_view_key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL, None);
        assert_eq!(app.overlay, Some(Overlay::Files));
        assert_eq!(app.overlay_cursor, 0, "starts on the current file");
        // j moves to b.rs; Enter switches and closes.
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_cursor, 1);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "Enter closes the picker");
        assert_eq!(app.current_file_index, 1);
        assert_eq!(app.mode, Mode::Source);
    }

    #[test]
    fn esc_and_q_close_the_overlay_without_quitting() {
        let (mut app, _dir) = make_session();
        on_view_key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL, None);
        assert!(app.overlay.is_some());
        on_overlay_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "q closes the picker, not the app");
        assert!(app.running, "the app keeps running");
        // Same for the comments list via l.
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Comments));
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn l_lists_all_comments_across_files_and_d_deletes() {
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 2,
            end: 2,
            lines: "line2".into(),
            text: "on a".into(),
        });
        app.comments.push(Comment {
            file_path: b.clone(),
            start: 1,
            end: 1,
            lines: "fn main".into(),
            text: "on b".into(),
        });
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Comments));
        // The list is sorted by path: b.rs < a.md? No — the list sorts by
        // file_path; the temp dir names are random, so just check both
        // entries are reachable and delete works.
        on_overlay_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 1, "d deletes the selected comment");
        assert_eq!(app.overlay, Some(Overlay::Comments), "the list stays open");
    }

    #[test]
    fn comments_overlay_enter_jumps_to_file_and_line() {
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        app.comments.push(Comment {
            file_path: a,
            start: 3,
            end: 3,
            lines: "line3".into(),
            text: "note".into(),
        });
        // Start on b.rs (index 1); jump via the list back to a.md line 3.
        on_source_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 1);
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "Enter closes the list");
        assert_eq!(app.current_file_index, 0, "jumped back to a.md");
        assert_eq!(app.mode, Mode::View, "a.md restored to view mode");
        assert_eq!(app.view.cursor, 2, "cursor lands on the comment line (0-based)");
    }

    #[test]
    fn reload_clears_only_the_current_files_comments() {
        // `r` re-anchors nothing: it clears the CURRENT file's comments
        // (stale anchors) but must not touch other files' comments.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "on a".into(),
        });
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "y".into(),
            text: "on b".into(),
        });
        std::fs::write(&a, "# a\n\nchanged\n").unwrap();
        assert!(reload_source(&mut app).is_ok());
        assert_eq!(app.comments.len(), 1, "only a.md's comment is cleared");
        assert_eq!(app.comments[0].text, "on b");
    }

    #[test]
    fn switching_back_rerenders_a_stale_width_view() {
        // A resize while a file sits in the background leaves its saved
        // view at the old wrap width; switching back must re-render it.
        let (mut app, _dir) = make_session();
        app.view.width = 10; // simulate a stale render width
        on_view_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        assert_ne!(
            app.view.width, 10,
            "the restored view re-renders at the current width"
        );
    }

    #[test]
    fn unique_suffix_keeps_unique_basenames_short() {
        let all = vec![
            PathBuf::from("testdata/a.md"),
            PathBuf::from("testdata/b.md"),
            PathBuf::from("src/c.md"),
        ];
        assert_eq!(unique_suffix(&all[0], &all), "a.md");
        assert_eq!(unique_suffix(&all[1], &all), "b.md");
        assert_eq!(unique_suffix(&all[2], &all), "c.md");
    }

    #[test]
    fn unique_suffix_disambiguates_colliding_basenames() {
        let all = vec![
            PathBuf::from("docs/design.md"),
            PathBuf::from("src/design.md"),
            PathBuf::from("src/main.rs"),
        ];
        // design.md collides: the parent dir joins the row.
        assert_eq!(unique_suffix(&all[0], &all), "docs/design.md");
        assert_eq!(unique_suffix(&all[1], &all), "src/design.md");
        // main.rs is unique: bare basename.
        assert_eq!(unique_suffix(&all[2], &all), "main.rs");
        // Component-wise matching: `a.md` must not collide with `ba.md`.
        let all2 = vec![PathBuf::from("x/a.md"), PathBuf::from("ba.md")];
        assert_eq!(unique_suffix(&all2[0], &all2), "a.md");
    }

    #[test]
    fn common_parent_finds_the_shared_dir() {
        let same = vec![
            PathBuf::from("testdata/a.md"),
            PathBuf::from("testdata/b.md"),
        ];
        assert_eq!(common_parent(&same).as_deref(), Some("testdata/"));
        // Deeper common ancestor is reported fully.
        let deep = vec![
            PathBuf::from("a/b/x.md"),
            PathBuf::from("a/b/y.md"),
        ];
        assert_eq!(common_parent(&deep).as_deref(), Some("a/b/"));
        // Different dirs: no common parent.
        let mixed = vec![
            PathBuf::from("testdata/a.md"),
            PathBuf::from("src/b.md"),
        ];
        assert_eq!(common_parent(&mixed), None);
        // Cwd-relative single files: parent is empty.
        let flat = vec![PathBuf::from("a.md"), PathBuf::from("b.md")];
        assert_eq!(common_parent(&flat), None);
    }

    #[test]
    fn comments_overlay_groups_rows_by_file() {
        // Two comments on a.md, one on b.rs: the overlay must show each
        // file once as a header (basename + count) and one line per
        // comment — never the full temp path on every row.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "one".into(),
        });
        app.comments.push(Comment {
            file_path: a,
            start: 2,
            end: 2,
            lines: "x".into(),
            text: "two".into(),
        });
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "y".into(),
            text: "three".into(),
        });
        app.overlay = Some(Overlay::Comments);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("a.md (2)"), "group header with count: {content}");
        assert!(content.contains("b.rs (1)"), "second group header: {content}");
        // Rows are short (range + body), not the old full-path location
        // format (`.../a.md:1`).
        assert!(
            !content.contains("a.md:1"),
            "the full-path location is not repeated per comment: {content}"
        );
        // Both comments on a.md still show their line ranges.
        assert!(content.contains("1  one"), "row 1: {content}");
        assert!(content.contains("2  two"), "row 2: {content}");
    }

    #[test]
    fn title_comment_indicator_appears_only_with_comments() {
        // The top-right `▌ N` lights up when the first comment exists and
        // disappears when the comments are cleared (e.g. after `s`).
        let (mut app, _dir) = make_session();
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        assert!(
            !capture(&mut app).contains("▌ 1"),
            "no indicator without comments"
        );
        let a = app.files[0].clone();
        app.comments.push(Comment {
            file_path: a,
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "c".into(),
        });
        assert!(capture(&mut app).contains("▌ 1"), "indicator appears with comments");
        app.comments.clear();
        assert!(
            !capture(&mut app).contains("▌ 1"),
            "indicator disappears when cleared"
        );
    }

    #[test]
    fn toast_floats_over_content_not_footer() {
        // A toast is an overlay at the BOTTOM of the body (row 22, one
        // above the footer — the classic message-line position): the
        // footer keeps the badge + hints, and nothing scrolls.
        let (mut app, _dir) = make_session();
        app.flash("hello toast");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let row22: String = buf.content[22 * 80..23 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            row22.contains("hello toast"),
            "toast floats at the bottom of the body: {row22}"
        );
        let last: String = buf.content[23 * 80..24 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            !last.contains("hello toast"),
            "toast is not in the footer: {last}"
        );
        assert!(last.contains("VIEW"), "footer keeps the mode badge: {last}");
        // An info toast renders yellow.
        let yellow = buf.content[22 * 80..23 * 80]
            .iter()
            .any(|c| c.style().fg == Some(Color::Yellow));
        assert!(yellow, "info toasts are yellow");
    }

    #[test]
    fn error_toast_is_red_and_beeps() {
        // Can't-do feedback: a red toast at the bottom (the BEL beep
        // itself is emitted to stdout at flash time).
        let (mut app, _dir) = make_session();
        app.flash_err("cannot do that");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let row22: String = buf.content[22 * 80..23 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(row22.contains("cannot do that"));
        let red = buf.content[22 * 80..23 * 80]
            .iter()
            .any(|c| c.style().fg == Some(Color::Red));
        assert!(red, "error toasts are red");
    }

    #[test]
    fn title_ys_hint_reads_the_room() {
        // The y/s explainer shows only what is actionable: nothing with
        // no comments, `y copy` only without --send-cmd, both with it.
        // Still low priority: it vanishes when the path needs the room.
        let (mut app, _dir) = make_session();
        let capture = |app: &mut App, width: u16| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .take(width as usize)
                .collect()
        };
        // No comments: neither key does anything — no explainer at all.
        assert!(!capture(&mut app, 80).contains("y copy"));
        assert!(!capture(&mut app, 80).contains("s send"));
        // With comments but no --send-cmd: only the copy key is shown.
        app.comments.push(Comment {
            file_path: app.files[0].clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "c".into(),
        });
        assert!(capture(&mut app, 80).contains("y copy"));
        assert!(!capture(&mut app, 80).contains("s send"));
        // With --send-cmd: both.
        app.config.send_cmd = Some("true".to_string());
        assert!(capture(&mut app, 80).contains("y copy · s send"));
        // The explainer still yields to a narrow screen.
        assert!(!capture(&mut app, 30).contains("y copy"));
    }

    #[test]
    fn title_clicks_open_overlays() {
        // Title bar buttons: `▌ N` → comment list, `1/3 files` → file
        // picker (cursor on the current file). Body clicks do neither.
        let (mut app, _dir) = make_session();
        app.comments.push(Comment {
            file_path: app.files[0].clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "c".into(),
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let m = title_metrics(&app, 80);
        // Comment counter → comment list.
        on_mouse(&mut app, click(0, m.indicator_x + 1));
        assert_eq!(app.overlay, Some(Overlay::Comments));
        app.overlay = None;
        // File counter → file picker, cursor on the current file.
        on_mouse(&mut app, click(0, m.file_count_x + 1));
        assert_eq!(app.overlay, Some(Overlay::Files));
        assert_eq!(app.overlay_cursor, app.current_file_index);
        app.overlay = None;
        // A body click (row 1) opens nothing.
        on_mouse(&mut app, click(1, 5));
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn title_file_counter_shows_position() {
        // `1/3 files` reflects the current file index; hidden for a
        // single-file session.
        let (mut app, _dir) = make_session();
        assert_eq!(
            title_metrics(&app, 80).file_count,
            " 1/2 files",
            "position is 1-based"
        );
        app.current_file_index = 1;
        assert_eq!(title_metrics(&app, 80).file_count, " 2/2 files");
        // Single file: no counter.
        app.files = vec![app.files[0].clone()];
        app.current_file_index = 0;
        assert_eq!(title_metrics(&app, 80).file_count, "");
    }

    #[test]
    fn title_esc_close_badge_tracks_esc_quit() {
        let (mut app, _dir) = make_session();
        // Default (no callback): no badge — Esc is not a close key.
        assert_eq!(title_metrics(&app, 80).esc_close, "");
        // With the esc-quit affordance live, the filled badge appears at
        // the top-right, right of the comment counter.
        app.config.callback = Some("fzf".into());
        let m = title_metrics(&app, 80);
        assert_eq!(m.esc_close, " esc close ");
        assert_eq!(m.esc_close_x + m.esc_close_w, 80, "flush right");
        assert!(
            m.indicator_x + m.indicator_w <= m.esc_close_x,
            "badge sits right of the comment counter"
        );
        // While composing, Esc cancels the composer instead: hide it.
        app.mode = Mode::Input;
        assert_eq!(title_metrics(&app, 80).esc_close, "");
    }

    #[test]
    fn title_esc_close_badge_keeps_room_over_the_path() {
        let (mut app, _dir) = make_session();
        app.config.callback = Some("fzf".into());
        let m = title_metrics(&app, 20);
        assert_eq!(m.esc_close, " esc close ", "badge survives narrow widths");
        assert!(
            m.path_w + m.esc_close_w <= 20,
            "the path yields to the badge: {m:?}"
        );
    }

    #[test]
    fn files_overlay_shows_the_change_bolt() {
        // The picker carries the title bar's ⚡ for files whose on-disk
        // state differs from what was loaded — background files included
        // (the poll loop only watches the current file).
        let (mut app, _dir) = make_session();
        // a.md (current): seen stamp matches disk → no ⚡.
        let meta = std::fs::metadata(&app.files[0]).unwrap();
        app.last_loaded_stamp = Some((meta.modified().unwrap(), meta.len()));
        // b.rs (background): a stale seen stamp → ⚡.
        app.file_states[1].last_loaded_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        assert!(!file_externally_changed(&app, 0));
        assert!(file_externally_changed(&app, 1));
        app.overlay = Some(Overlay::Files);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("b.rs ⚡"), "changed file carries ⚡: {content}");
        assert!(!content.contains("a.md ⚡"), "unchanged file stays clean: {content}");
        // A file with no recorded stamp (load state unknown) shows no ⚡.
        app.file_states[1].last_loaded_stamp = None;
        assert!(!file_externally_changed(&app, 1));
    }

    #[test]
    fn overlay_mouse_selects_entries() {
        // File picker: clicking an entry row moves the overlay cursor;
        // the title row does nothing; the content underneath is untouched.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let view_cursor = app.view.cursor;
        on_mouse(&mut app, click(panel.y + 2, panel.x + 5)); // first entry
        assert_eq!(app.overlay_cursor, 0);
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5)); // second entry
        assert_eq!(app.overlay_cursor, 1);
        on_mouse(&mut app, click(panel.y + 1, panel.x + 5)); // title row
        assert_eq!(app.overlay_cursor, 1, "title row is not an entry");
        assert_eq!(
            app.view.cursor, view_cursor,
            "the content cursor is untouched while an overlay is open"
        );
    }

    #[test]
    fn overlay_mouse_selects_comments_skipping_headers() {
        // Comments list: rows are title, header(a), c0, c1, header(b), c2
        // — clicking a comment row selects it; a header row does nothing.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "one".into(),
        });
        app.comments.push(Comment {
            file_path: a,
            start: 2,
            end: 2,
            lines: "x".into(),
            text: "two".into(),
        });
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "y".into(),
            text: "three".into(),
        });
        app.overlay = Some(Overlay::Comments);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5)); // c0 (first comment row)
        assert_eq!(app.overlay_cursor, 0);
        on_mouse(&mut app, click(panel.y + 4, panel.x + 5)); // c1
        assert_eq!(app.overlay_cursor, 1);
        on_mouse(&mut app, click(panel.y + 6, panel.x + 5)); // c2 (after header b)
        assert_eq!(app.overlay_cursor, 2);
        on_mouse(&mut app, click(panel.y + 2, panel.x + 5)); // header row
        assert_eq!(app.overlay_cursor, 2, "header rows are not selectable");
    }

    #[test]
    fn overlay_wheel_moves_selection() {
        // The wheel moves the overlay cursor (j/k semantics) while an
        // overlay is open, clamped to the entry count.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let wheel = |kind: MouseEventKind| MouseEvent {
            kind,
            column: 0,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.overlay_cursor, 1);
        on_mouse(&mut app, wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.overlay_cursor, 0);
        on_mouse(&mut app, wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.overlay_cursor, 0, "clamped at the top");
    }

    #[test]
    fn footer_hints_are_labeled_and_offer_help() {
        // The footer shows `L{line}/{total}` (1-based, cursor-anchored), a
        // few labeled actions, and `? help` for the full reference.
        let mut app = make_app(10, Mode::View);
        assert!(footer_hints(&app).contains("L1/10"), "cursor line 1 of 10");
        assert!(footer_hints(&app).contains("j/k scroll"));
        assert!(footer_hints(&app).contains("c comment"));
        assert!(footer_hints(&app).contains("? help"));
        app.view.goto_source_line(4);
        assert!(footer_hints(&app).contains("L5/10"));
        app.selection = Some(Selection::new(3));
        assert!(footer_hints(&app).contains("4–4"));
        assert!(footer_hints(&app).contains("j/k extend"));
        assert!(
            footer_hints(&app).contains("Esc cancel"),
            "the selection state spells out the way out: {}",
            footer_hints(&app)
        );
        let mut app2 = make_app(10, Mode::Source);
        assert!(footer_hints(&app2).contains("L1/10"));
        app2.cursor = 9;
        assert!(footer_hints(&app2).contains("L10/10"));
        assert!(footer_hints(&app2).contains("v select"));
        assert!(footer_hints(&app2).contains("? help"));
        // An empty file reports L0/0 instead of an out-of-range line.
        let app3 = make_app(0, Mode::Source);
        assert!(footer_hints(&app3).contains("L0/0"));
    }

    #[test]
    fn footer_badge_shows_the_state_not_just_the_mode() {
        // The badge leads the footer and flips with the transient states:
        // SELECT while a selection is active (both modes — the selection
        // is shared), COMMENT while the composer is open. Esc is the way
        // out of both, spelled out in the hints.
        let footer_badge = |app: &mut App| -> String {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            let row: String = buf.content[23 * 80..24 * 80]
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect();
            row[..8].to_string()
        };
        let mut app = make_app(10, Mode::View);
        assert!(footer_badge(&mut app).contains("VIEW"), "view shows VIEW");
        app.selection = Some(Selection::new(3));
        assert!(footer_badge(&mut app).contains("SELECT"), "selection flips the badge");
        assert!(
            !footer_badge(&mut app).contains("VIEW"),
            "the badge is not the plain mode badge anymore"
        );
        app.mode = Mode::Source;
        assert!(
            footer_badge(&mut app).contains("SELECT"),
            "the selection badge carries over to source mode"
        );
        // While composing, the selection is still held (it is consumed on
        // Enter) — the badge must show COMMENT, not SELECT.
        app.mode = Mode::Input;
        assert!(
            footer_badge(&mut app).contains("COMMENT"),
            "composing shows COMMENT"
        );
        assert!(
            footer_hints(&app).contains("Enter confirm"),
            "the composer hint keeps the confirm/cancel pair"
        );
        assert!(footer_hints(&app).contains("Esc cancel"));
    }

    #[test]
    fn question_mark_opens_help_overlay() {
        let mut app = make_app(10, Mode::View);
        on_view_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Help));
        // The reference fits the panel (24-row terminal): j/k are no-ops
        // — a list that fits never scrolls. Esc / q / ? close it.
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_cursor, 0, "no scroll when the content fits");
        on_overlay_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "? toggles the help closed");
        on_source_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Help));
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn overlay_click_outside_closes() {
        // Modal dismiss: with an overlay open, a click outside the panel
        // closes it; inside on a title row it stays open.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Outside (top-left corner): closes.
        on_mouse(&mut app, click(0, 0));
        assert_eq!(app.overlay, None, "outside click dismisses");
        // Inside on the title row: stays open.
        app.overlay = Some(Overlay::Files);
        on_mouse(&mut app, click(panel.y + 1, panel.x + 5));
        assert_eq!(app.overlay, Some(Overlay::Files), "inside title row keeps it");
        // Outside below the panel: closes again.
        on_mouse(&mut app, click(panel.y + panel.height + 2, 5));
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn prompts_float_at_top_not_footer() {
        // The q-confirmation dialog is a top-center banner (row 1) like
        // the toast: the footer keeps the badge + hints.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "c".into(),
        });
        request_quit(&mut app); // arms the confirmation + a toast
        assert!(app.confirm_quit);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let row1: String = buf.content[80..160]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            row1.contains("unsent comments — q to quit"),
            "prompt banner at the top: {row1}"
        );
        let last: String = buf.content[23 * 80..24 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            !last.contains("unsent comments"),
            "no dialog left in the footer: {last}"
        );
        assert!(last.contains("SOURCE"), "footer keeps the badge: {last}");
    }

    #[test]
    fn prompt_priority_quit_over_edit_over_reload_over_change() {
        let mut app = make_app(10, Mode::Source);
        app.confirm_quit = true;
        app.confirm_edit = true;
        app.confirm_reload = true;
        app.file_changed = true;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — q to quit, Esc to cancel")
        );
        app.confirm_quit = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — e again to edit & clear, Esc to cancel")
        );
        app.confirm_edit = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — r again to reload & clear, Esc to cancel")
        );
        app.confirm_reload = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("file changed — r reload · i ignore")
        );
        app.file_changed = false;
        assert_eq!(prompt_message(&app).as_deref(), None);
    }

    #[test]
    fn overlay_double_click_switches_file() {
        // Double-clicking a file entry = Enter: it switches and closes.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let row = panel.y + 3; // second file entry
        on_mouse(&mut app, click(row, panel.x + 5));
        assert_eq!(app.overlay_cursor, 1);
        assert_eq!(app.overlay, Some(Overlay::Files), "single click selects only");
        on_mouse(&mut app, click(row, panel.x + 5));
        assert_eq!(app.overlay, None, "double-click activates");
        assert_eq!(app.current_file_index, 1, "switched to the second file");
    }

    #[test]
    fn overlay_double_click_jumps_to_comment() {
        // Double-clicking a comment row = Enter: jumps to the file+line.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        app.comments.push(Comment {
            file_path: a,
            start: 3,
            end: 3,
            lines: "line3".into(),
            text: "note".into(),
        });
        // Start on b.rs (index 1), open the comment list.
        on_source_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 1);
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let row = panel.y + 3; // title + header + first comment row
        on_mouse(&mut app, click(row, panel.x + 5));
        on_mouse(&mut app, click(row, panel.x + 5));
        assert_eq!(app.overlay, None);
        assert_eq!(app.current_file_index, 0, "jumped back to a.md");
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 2, "cursor on the comment line");
    }

    #[test]
    fn overlay_double_click_requires_same_row() {
        // Two clicks on different entries never activate.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, click(panel.y + 2, panel.x + 5));
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5)); // different entry
        assert_eq!(app.overlay, Some(Overlay::Files), "different rows do not activate");
        assert_eq!(app.overlay_cursor, 1);
    }

    #[test]
    fn overlay_double_click_state_resets_on_reopen() {
        // Regression: a double-click activates and closes the overlay; a
        // single click in a freshly reopened session on the same row must
        // only select — never inherit the old double-click's second half.
        let (mut app, _dir) = make_session();
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Session 1: double-click the second file → switches there.
        open_overlay(&mut app, Overlay::Files, 0);
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5));
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5));
        assert_eq!(app.current_file_index, 1, "double-click switched");
        assert_eq!(app.overlay, None);
        // Session 2 (within the double-click window): a single click on
        // the same row selects only.
        open_overlay(&mut app, Overlay::Files, 1);
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5));
        assert_eq!(
            app.overlay,
            Some(Overlay::Files),
            "a fresh session's single click only selects"
        );
        assert_eq!(app.current_file_index, 1, "the file did not change");
    }

    #[test]
    fn c_on_the_exact_range_edits_the_comment() {
        // Selecting exactly the range an existing comment covers and
        // pressing c re-edits it: the composer is prefilled and Enter
        // replaces instead of stacking a duplicate.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: "line3\nline4\nline5".into(),
            text: "old".into(),
        });
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        }); // 1-based 3-5
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input, "old", "the comment text is prefilled");
        assert_eq!(app.editing_comment, Some(0));
        // Replace the text and confirm.
        app.input.clear();
        for ch in "new".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 1, "replaced, not added");
        assert_eq!(app.comments[0].text, "new");
        assert_eq!(app.mode, Mode::Source);
        assert!(app.editing_comment.is_none());
    }

    #[test]
    fn c_on_a_different_range_still_adds() {
        // A range that does not exactly match an existing comment adds a
        // new comment, with an empty composer.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: String::new(),
            text: "existing".into(),
        });
        app.selection = Some(Selection {
            anchor: 6,
            cursor: 7,
        }); // 1-based 7-8: no exact match
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input, "", "no prefill for a different range");
        assert_eq!(app.editing_comment, None);
        for ch in "x".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 2, "a second comment is added");
        assert_eq!(app.comments[1].text, "x");
    }

    #[test]
    fn esc_cancels_the_edit_unchanged() {
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 3,
            lines: String::new(),
            text: "old".into(),
        });
        app.selection = Some(Selection::new(2));
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input, "old");
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.comments[0].text, "old", "Esc leaves the comment untouched");
        assert!(app.editing_comment.is_none());
        assert_eq!(app.mode, Mode::Source);
    }

    #[test]
    fn s_with_failed_send_keeps_comments() {
        // A failed send (target unknown / command error) is NOT a
        // delivery: the comments stay for a retry.
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("false".to_string()); // always exits non-zero
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "comments kept on send failure");
        // Quitting now still triggers the unsent-comments confirmation.
        request_quit(&mut app);
        assert!(app.confirm_quit, "kept comments still guard the quit");
    }

    #[test]
    fn s_with_ok_send_clears_the_slate() {
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("cat > /dev/null".to_string()); // succeeds
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert!(app.comments.is_empty(), "delivered → cleared");
    }

    #[test]
    fn s_without_send_cmd_is_a_no_op() {
        // Without a target, `s` has nothing to send: it copies to the
        // clipboard (y's job) and keeps the comments — no clearing, no
        // hidden stdout output after quit.
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "comments kept without a target");
        assert!(app.status.is_some(), "a hint explains why nothing was sent");
    }

    #[test]
    fn y_copies_without_sending() {
        // `y` is copy-only: even with --send-cmd configured it must not
        // deliver, and the comments stay.
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("cat > /dev/null".to_string());
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "y keeps the comments");
    }

    #[test]
    fn overlay_scrolls_only_when_overflowing() {
        // A 24-row terminal gives the panel ~11 visible rows. A short
        // list never scrolls; a long one scrolls to keep the cursor in
        // view.
        let (mut app, _dir) = make_session(); // 2 files
        app.overlay = Some(Overlay::Files);
        on_files_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_offset, 0, "content fits → no scroll");
        // Grow the list past the visible window.
        for i in 0..15 {
            app.files.push(PathBuf::from(format!("f{i:02}.md")));
        }
        app.overlay_cursor = 0;
        for _ in 0..13 {
            on_files_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(app.overlay_cursor, 13);
        assert!(app.overlay_offset > 0, "overflow → the offset follows");
        let visible = overlay_visible_rows();
        assert!(
            app.overlay_offset <= 13 && 13 < app.overlay_offset + visible,
            "the cursor row stays inside the visible window"
        );
        // k scrolls back up and the offset follows.
        for _ in 0..13 {
            on_files_overlay_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        }
        assert_eq!(app.overlay_cursor, 0);
        assert_eq!(app.overlay_offset, 0);
    }

    #[test]
    fn comments_overlay_delete_uses_sorted_order() {
        // The overlay cursor indexes the SORTED list; delete must remove
        // the highlighted comment even when add order interleaves files.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        // Add order: a1, b1, a2 → sorted: a1, a2, b1.
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "a1".into(),
        });
        app.comments.push(Comment {
            file_path: b.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            text: "b1".into(),
        });
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 2,
            end: 2,
            lines: "x".into(),
            text: "a2".into(),
        });
        app.overlay = Some(Overlay::Comments);
        // j × 2 → sorted index 2 = b1 (raw index 1).
        on_comments_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        on_comments_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_cursor, 2);
        on_comments_overlay_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 2);
        assert!(
            app.comments.iter().all(|c| c.text != "b1"),
            "the highlighted (b.rs) comment was deleted, not a.md's"
        );
    }

    #[test]
    fn editing_hides_the_stacked_card() {
        // While re-editing, the old card (`comment · …`) is hidden — the
        // edit composer replaces it, so `comment` and `edit` never stack.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: "line3\nline4\nline5".into(),
            text: "old".into(),
        });
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        });
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.editing_comment, Some(0));
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        let during = capture(&mut app);
        assert!(during.contains("edit · 3-5"), "the edit composer shows: {during}");
        assert!(
            !during.contains("comment · 3-5"),
            "the old card is hidden while editing: {during}"
        );
        // Confirm: the card returns with the new text.
        app.input.clear();
        for ch in "new".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let after = capture(&mut app);
        assert!(after.contains("comment · 3-5"), "the card returns: {after}");
        assert!(after.contains("new"), "with the updated text");
    }

    #[test]
    fn view_mode_editing_hides_the_stacked_card() {
        // Re-editing from VIEW mode: the folded card must disappear under
        // the edit composer (regression: the re-render used to run before
        // the Input flip, so visible_cards never excluded the card), and
        // Esc puts it back.
        let mut app = make_app(10, Mode::View);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: "line3\nline4\nline5".into(),
            text: "old".into(),
        });
        replace_view_preserving_cursor(&mut app); // fold the card in
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        }); // 1-based 3-5: exact match → re-edit
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.editing_comment, Some(0));
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        let during = capture(&mut app);
        assert!(during.contains("edit · 3-5"), "the edit composer shows: {during}");
        assert!(
            !during.contains("comment · 3-5"),
            "the old card is hidden while editing in view: {during}"
        );
        // Esc cancels: the card returns with its original text.
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let after = capture(&mut app);
        assert!(
            after.contains("comment · 3-5"),
            "the card returns on cancel: {after}"
        );
    }

    #[test]
    fn clip_if_needed_only_marks_real_overflows() {
        assert_eq!(clip_if_needed("short", 24), "short");
        assert_eq!(clip_if_needed("testdata", 24), "testdata");
        let long = "a".repeat(30);
        let out = clip_if_needed(&long, 10);
        assert_eq!(UnicodeWidthStr::width(out.as_str()), 10);
        assert!(out.ends_with('…'));
    }
}

#[cfg(test)]
mod width_tests {
    use super::*;

    #[test]
    fn view_render_width_matches_the_pane() {
        // The view always draws its frame: the render width is the terminal
        // minus the page's left margin, the two border columns, and the
        // text column's 1-column pads on each side (the marker column
        // rides the left border, reserving no width). The startup render
        // and the resize re-render both go through this, so they can't
        // disagree.
        assert_eq!(view_render_width(100), 95);
        assert_eq!(view_render_width(1), 0, "clamps at zero");
        assert_eq!(view_render_width(0), 0);
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn short_paths_pass_through() {
        assert_eq!(truncate_path(Path::new("a.md"), 80), "a.md");
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 80),
            "wiki/cases/aozora-plan.md"
        );
    }

    #[test]
    fn keeps_the_basename_whole() {
        // Narrow: drops dirs, never splits the file name.
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 20),
            "…/aozora-plan.md"
        );
    }

    #[test]
    fn adds_dirs_from_the_right() {
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 22),
            "…/cases/aozora-plan.md"
        );
    }

    #[test]
    fn whole_path_when_enough_room() {
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 30),
            "wiki/cases/aozora-plan.md"
        );
    }

    #[test]
    fn clips_when_the_basename_alone_is_too_wide() {
        let out = truncate_path(Path::new("a-very-long-file-name.md"), 10);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn zero_max_is_empty() {
        assert_eq!(truncate_path(Path::new("a.md"), 0), "");
    }

    #[test]
    fn cjk_width_counts_as_wide() {
        // 日本語ドキュメント (9 × 2 cols) + ".md" would overflow 8 cols;
        // the result must stay inside the budget.
        let out = truncate_path(Path::new("日本語ドキュメント.md"), 8);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 8);
    }
}

#[cfg(test)]
mod mouse_view_tests {
    use super::*;
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::view::ViewState;

    fn real_app() -> App {
        let path = "testdata/full.md";
        let config = Config {
            files: vec![path.into()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path.into()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::View;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app
    }

    fn mouse(kind: MouseEventKind, row: u16, col: u16) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn tab_flips_the_pane_under_the_composer() {
        // 入力中の Tab は composer を開いたまま下のペインを切り替える。
        // 対象範囲と入力途中のテキストは無傷。
        let mut app = real_app();
        app.view.cursor = 5;
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        let (start, end) = (app.input_start, app.input_end);
        on_input_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Input, "composing continues");
        assert_eq!(app.composer_return, Mode::Source, "the pane flipped to source");
        assert_eq!(app.input, "x", "typed text survives");
        assert_eq!((app.input_start, app.input_end), (start, end), "target range pinned");
        on_input_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.composer_return, Mode::View, "and back");
        // Enter は現在のペインに戻る。
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.comments.len(), 1);
    }

    #[test]
    fn mouse_cannot_touch_the_selection_while_composing() {
        // c でコメント入力中: クリック/ドラッグはカーソル・選択を変えない
        // （コメント対象範囲は composer を開いた時点で確定している）。
        let mut app = real_app();
        app.view.cursor = 5;
        app.selection = Some(Selection { anchor: 4, cursor: 5 });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        let (sel, cur) = (app.selection, app.view.cursor);
        on_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 10, 0));
        on_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 14, 0));
        assert_eq!(app.selection, sel, "クリック/ドラッグで選択が変わらない");
        assert_eq!(app.view.cursor, cur, "カーソルも動かない");
    }

    #[test]
    fn mouse_click_selects_the_phrase_line_and_drag_extends_by_lines() {
        let app = real_app();
        // インラインリンク行 (0-based 44) の表示行。マウス行 = 表示行 + 2
        // (タイトルバー + view の枠の上辺)。
        let display = app.view.source_starts[44];
        let mouse_row = display as u16 + 2;
        let text: String = app.view.rows[display].iter().map(|s| s.text.as_str()).collect();
        let segs: Vec<(usize, usize, usize)> = app.view.row_segments[display]
            .iter().map(|s| (s.line, s.start, s.end)).collect();
        println!("display {display} text={text:?} segs={segs:?}");
        // 句1（先頭列）→ line 44 / 句2（タイトル付き句内）→ line 45。
        // 列は左枠 1 + ガター 2 の右から数える。
        let mut a = real_app();
        on_mouse(&mut a, mouse(MouseEventKind::Down(MouseButton::Left), mouse_row, 0));
        let first = a.view.cursor;
        let mut b = real_app();
        on_mouse(&mut b, mouse(MouseEventKind::Down(MouseButton::Left), mouse_row, 56));
        let second = b.view.cursor;
        println!("click 句1 → {first}, 句2(col55) → {second}");
        assert_eq!(first, 44, "句1（インラインリンク）は line 44");
        assert_eq!(second, 45, "句2（タイトル付き）は line 45");
        // ドラッグ: 句1 から 2 表示行下まで → 行範囲 (anchor 44, end > 44)。
        let mut c = real_app();
        on_mouse(&mut c, mouse(MouseEventKind::Down(MouseButton::Left), mouse_row, 0));
        on_mouse(&mut c, mouse(MouseEventKind::Drag(MouseButton::Left), mouse_row + 2, 0));
        let sel = c.selection.map(|s| s.range());
        println!("drag 句1→2行下: selection={sel:?} cursor={}", c.view.cursor);
        let (a, b) = sel.unwrap();
        assert_eq!(a, 44);
        assert!(b > a, "ドラッグで複数行が選択される");
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::view::ViewState;

    fn real_app() -> App {
        let path = "testdata/full.md";
        let config = Config {
            files: vec![path.into()],
            send_cmd: None,
            send_agent: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path.into()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::View;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app
    }

    #[test]
    fn tab_round_trip_preserves_the_line() {
        let mut app = real_app();
        // 46行目 (0-based 45, タイトル付き — マージ行の2行目) に移動。
        app.view.goto_source_line(45);
        // View → Comment: 正確な行が引き継がれる（グループ先頭 44 に飛ばない）。
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.cursor, 45, "選択なしでも正確な行");
        // Comment → View → Comment: 行が変わらない。
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 45);
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 45, "ラウンドトリップで行が動かない");
    }
}