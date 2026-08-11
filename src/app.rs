//! The TUI's central state: the [`App`] struct with its per-file
//! [`FileState`], the [`Mode`] enum, overlay state, and shared constants.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ratatui::style::Color;

use crate::comment::{Comment, Selection};
use crate::config::{Config, EscQuit};
use crate::highlight::{Highlighter, Span as HiSpan, wrap_spans};
use crate::history::DocumentHistory;
use crate::ime;
use crate::overlay::Overlay;
use crate::snapshot::SnapshotCache;
use crate::source::Source;
use crate::view::{
    ViewState, border_color, changed_bg, history_border_color, history_frame_flash_color,
    history_glow_bg, scrollbar_thumb, selected_bg,
};
use crate::{
    card_line_count, composer_line_count, replace_view_preserving_cursor, view_composer_anchor,
    view_content_width, view_render_width,
};

/// Whether `path` can be opened in view mode. The native renderer is
/// tui-markdown (pulldown-cmark), so only Markdown-family files render;
/// everything else (`.rs`, `.toml`, …) is source-only.
pub(crate) fn supports_view(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("md" | "markdown" | "mdx"))
}

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The 100ms event-poll/tick cadence.
pub(crate) const TICK_MS: u64 = 100;
/// Transient footer messages live this long.
pub(crate) const STATUS_SECS: Duration = Duration::from_secs(4);
/// Debounce for re-rendering the view after a resize.
pub(crate) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(300);
/// Debounce for reloading the file after an external (agent) edit — the
/// writer may not be atomic, so wait for the dust to settle.
pub(crate) const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);
/// Files above this many lines get a view-mode memory warning (v1: warn only).
pub(crate) const HUGE_FILE: usize = 100_000;
/// The default double-click window (see `App::double_click_ms`).
pub(crate) const DOUBLE_CLICK_MS: Duration = Duration::from_millis(400);
/// The window in which a second key completes a `]`/`[` chord: `]` alone
/// falls back to the file switch after this, `]c` jumps to the next
/// change (the F7 fallback for terminals that do not deliver F-keys).
pub(crate) const CHORD_MS: Duration = Duration::from_millis(400);

/// Per-file state: everything that is unique to each file in the session.
/// Swapped in/out of the active App fields on file switch.
#[derive(Default)]
pub(crate) struct FileState {
    pub(crate) source: Source,
    pub(crate) spans: Vec<Vec<HiSpan>>,
    pub(crate) view: ViewState,
    pub(crate) mode: Mode,
    pub(crate) offset: usize,
    pub(crate) cursor: usize,
    pub(crate) selection: Option<Selection>,
    pub(crate) line_rows: Vec<usize>,
    pub(crate) base_rows: Vec<usize>,
    pub(crate) content_width: u16,
    pub(crate) gutter_cols: u16,
    pub(crate) file_stamp: Option<(SystemTime, u64)>,
    pub(crate) last_loaded_stamp: Option<(SystemTime, u64)>,
    pub(crate) file_changed: bool,
    pub(crate) reload_pending: Option<Instant>,
    pub(crate) review_changed: HashSet<usize>,
    pub(crate) review_deleted_before: HashSet<usize>,
    /// Baseline-relative marks for the generation actually on screen.
    pub(crate) comparison_changed: HashSet<usize>,
    pub(crate) comparison_deleted_before: HashSet<usize>,
}

/// The TUI application state.
pub(crate) struct App {
    pub(crate) config: Config,
    /// All files in the session, in argument order.
    pub(crate) files: Vec<PathBuf>,
    /// Current file index into `files` and `file_states`.
    pub(crate) current_file_index: usize,
    /// Per-file state; index mirrors `files`.
    pub(crate) file_states: Vec<FileState>,
    /// Git-independent, bounded LOCAL document persistence. `None` in unit
    /// tests and reply mode; normal runs discover the per-user cache.
    pub(crate) snapshot_cache: Option<SnapshotCache>,
    /// Complete Markdown snapshots for the time-machine view. This stays
    /// indexed by `files`, so it does not need to be moved through the live
    /// FileState slot when switching files.
    pub(crate) histories: Vec<DocumentHistory>,
    /// Persistent, Git-independent review marks: the cumulative transition
    /// from the last acknowledged document to NOW.
    pub(crate) review_changed: HashSet<usize>,
    pub(crate) review_deleted_before: HashSet<usize>,
    /// Baseline-relative marks for the generation actually rendered. At
    /// NOW these equal the cumulative review marks above; in history they
    /// describe the displayed generation rather than the working tree.
    pub(crate) comparison_changed: HashSet<usize>,
    pub(crate) comparison_deleted_before: HashSet<usize>,
    /// Lines replaced by the most recent time jump. The existing rendered
    /// change color provides a short "block just arrived" afterglow.
    pub(crate) history_changed: HashSet<usize>,
    pub(crate) history_changed_until: Option<Instant>,
    /// Until this instant the rendered view contains dim old blocks that
    /// are about to collapse out of the document.
    pub(crate) history_ghost_until: Option<Instant>,
    /// The history cursor may move ahead of the rendered document while an
    /// arrow is held. Once input settles, this deadline triggers one render
    /// of the final selected revision.
    pub(crate) history_render_due: Option<Instant>,
    /// Brief whole-frame pulse confirming that the selected revision has
    /// finished rendering, even when no changed block is in the viewport.
    pub(crate) history_frame_flash_until: Option<Instant>,
    /// The animation clock for the time-machine frame (`--fx`): the
    /// rotating border's phase derives from this, so the gradient keeps
    /// its pace across frames and redraws.
    pub(crate) fx_clock: Instant,
    /// Set once the new Markdown view is built. The event loop paints that
    /// view once without a pulse, then turns this into `flash_until` so the
    /// completion signal starts on the following frame.
    pub(crate) history_frame_flash_pending: bool,
    /// The overlay currently open (Ctrl+p files / `l` comments / `?`
    /// help), if any.
    pub(crate) overlay: Option<Overlay>,
    /// A pending `]`/`[` chord: the bracket pressed and when. Resolves to
    /// the file switch when the [`CHORD_MS`] window expires, or to a review
    /// mark jump when `c` follows (`]c`/`[c` — the F7 fallback).
    pub(crate) pending_chord: Option<(Instant, char)>,
    /// Cursor row in the overlay (0-based).
    pub(crate) overlay_cursor: usize,
    /// Scroll offset of the overlay's list (rows): moves only when the
    /// content overflows the panel, so a list that fits never scrolls.
    pub(crate) overlay_offset: usize,
    /// Time + entry of the previous overlay click, for double-click
    /// detection (the second click on the same entry within
    /// [`App::double_click_ms`] activates it — Enter-equivalent).
    pub(crate) last_overlay_click: Option<(Instant, usize)>,
    /// The double-click window. A field (not a const) so tests can widen
    /// it and make the second click deterministic: the wall-clock
    /// comparison used to flake under load when the thread stalled
    /// between two clicks.
    pub(crate) double_click_ms: Duration,
    pub(crate) source: Source,
    /// Pre-tokenized spans, one vec per source line.
    pub(crate) spans: Vec<Vec<HiSpan>>,
    /// The theme (re-rendering and view styles resolve colors from it).
    pub(crate) highlight: Highlighter,
    /// Rendered view-mode rows.
    pub(crate) view: ViewState,
    /// Current mode.
    pub(crate) mode: Mode,
    /// Source-mode scroll offset in display rows.
    pub(crate) offset: usize,
    /// Cursor line, 0-based source index.
    pub(crate) cursor: usize,
    /// Active line selection (`v`), extended with j/k.
    pub(crate) selection: Option<Selection>,
    /// Comment text being typed (mode == Input).
    pub(crate) input: String,
    /// Byte offset of the text cursor inside `input` (always on a char
    /// boundary). Arrows/Home/End move it; insertion and deletion happen
    /// here instead of only at the end.
    pub(crate) input_cursor: usize,
    /// When the composer is RE-editing an existing comment (the selection
    /// matched its range exactly): its index into `comments`. `None` =
    /// adding a new comment.
    pub(crate) editing_comment: Option<usize>,
    /// The 0-based range the input box is anchored to (mode == Input).
    pub(crate) input_start: usize,
    pub(crate) input_end: usize,
    /// The mode to return to when the composer closes: source-mode `c`
    /// returns to Comment, view-mode `c` stays in View (the composer is
    /// drawn inline in the rendered view).
    pub(crate) composer_return: Mode,
    /// Comments added this session, in add order.
    pub(crate) comments: Vec<Comment>,
    /// Transient footer message with its expiry; `true` = an operation
    /// that could not be done (rendered red, and a BEL beep was emitted).
    pub(crate) status: Option<(String, Instant, bool)>,
    /// Pending quit confirmation when unsent comments exist.
    pub(crate) confirm_quit: bool,
    /// Pending external-edit confirmation when unsent comments exist (`e`).
    pub(crate) confirm_edit: bool,
    /// Pending reload confirmation when unsent comments exist (`r`).
    pub(crate) confirm_reload: bool,
    /// Export text to print to stdout at the next loop turn (`s`): the TUI
    /// restores the terminal, prints, and re-enters raw mode.
    /// Cached wrapped row count per source line, for the current width.
    pub(crate) line_rows: Vec<usize>,
    /// Per-source-line wrap counts only (width-dependent cache); `line_rows`
    /// is this plus inline card/composer rows, recomputed each frame.
    pub(crate) base_rows: Vec<usize>,
    /// Content width the `base_rows` cache was built for.
    pub(crate) content_width: u16,
    /// View needs re-rendering (resize) and when that was first noticed.
    pub(crate) view_dirty: bool,
    pub(crate) view_dirty_since: Option<Instant>,
    /// Active while the comment composer is open: forces the Japanese input
    /// source for typing, back to ASCII on drop (see [`crate::ime`]).
    pub(crate) ime_guard: Option<ime::ImeGuard>,
    /// Source line where a left-drag selection started (mouse row select).
    pub(crate) drag_anchor: Option<usize>,
    /// Source-mode gutter width in columns (status + number + space).
    pub(crate) gutter_cols: u16,
    /// Session input-source control: ASCII in command mode, restore on
    /// exit (see [`crate::ime::SessionIme`]).
    pub(crate) ime_session: ime::SessionIme,
    /// Whether `ime_session.force_ascii()` has succeeded yet (the helper is
    /// compiled in the background on first run, so it is retried per tick).
    pub(crate) ime_forced: bool,
    /// The file's mtime+size as of the last poll (agent edits are detected
    /// by comparing against `last_loaded_stamp`).
    pub(crate) file_stamp: Option<(SystemTime, u64)>,
    /// The stamp of the content currently loaded in memory.
    pub(crate) last_loaded_stamp: Option<(SystemTime, u64)>,
    /// When a file change was noticed (and toasted); the badge shows ⚡
    /// until the user reloads with `r`.
    pub(crate) file_changed: bool,

    /// When a file change was noticed; the toast fires after
    /// [`RELOAD_DEBOUNCE`] (and never while the composer is open).
    pub(crate) reload_pending: Option<Instant>,
    pub(crate) running: bool,
    /// Resolved UI colors for the current `--light` / dark mode.
    pub(crate) ui_light: bool,
    pub(crate) ui_selected_bg: Color,
    pub(crate) ui_changed_bg: Color,
    pub(crate) ui_history_glow_bg: Color,
    pub(crate) ui_border: Color,
    pub(crate) ui_history_border: Color,
    pub(crate) ui_history_frame_flash: Color,
    pub(crate) ui_scrollbar: Color,
    /// Active scrollbar drag: `(start track row, start scroll offset)` —
    /// set on a thumb press, cleared on release (viewport-only scroll, so
    /// the cursor keeps its absolute position).
    pub(crate) scrollbar_drag: Option<(usize, usize)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mode {
    /// Natively rendered markdown, read-only.
    #[default]
    View,
    /// Raw source with line numbers; comment anchoring.
    Source,
    /// Comment text entry (IME-safe: all keys go to the buffer).
    Input,
}

/// Same error toast still on screen: the identical message was flashed
/// as an error within STATUS_SECS, so [`App::flash_err`] skips the BEL
/// (spamming a key that cannot work beeps once, not per keystroke). An
/// info toast with the same text never counts — the beep is for errors.
pub(crate) fn is_repeat_error(status: &Option<(String, Instant, bool)>, msg: &str) -> bool {
    matches!(
        status,
        Some((prev, at, true)) if prev == msg && at.elapsed() < STATUS_SECS
    )
}

impl App {
    pub(crate) fn new(
        config: Config,
        source: Source,
        highlight: Highlighter,
        view: ViewState,
        light: bool,
    ) -> Self {
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
            snapshot_cache: None,
            histories: Vec::new(),
            review_changed: HashSet::new(),
            review_deleted_before: HashSet::new(),
            comparison_changed: HashSet::new(),
            comparison_deleted_before: HashSet::new(),
            history_changed: HashSet::new(),
            history_changed_until: None,
            history_ghost_until: None,
            history_render_due: None,
            history_frame_flash_until: None,
            fx_clock: Instant::now(),
            history_frame_flash_pending: false,
            overlay: None,
            pending_chord: None,
            overlay_cursor: 0,
            overlay_offset: 0,
            last_overlay_click: None,
            double_click_ms: DOUBLE_CLICK_MS,
            source,
            highlight,
            // The caller (run()) already tokenized every file — this one
            // included — and assigns `app.spans` right after construction;
            // tokenizing again here would double the work for file 0.
            spans: Vec::new(),
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
            running: true,
            ui_light: light,
            ui_selected_bg: selected_bg(light),
            ui_changed_bg: changed_bg(light),
            ui_history_glow_bg: history_glow_bg(light),
            ui_border: border_color(light),
            ui_history_border: history_border_color(light),
            ui_history_frame_flash: history_frame_flash_color(light),
            ui_scrollbar: scrollbar_thumb(light),
            scrollbar_drag: None,
        }
    }

    /// Set a transient footer message.
    /// Set a transient footer message (info: yellow).
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), false));
    }

    /// An operation that could not be done: red toast + a BEL beep (the
    /// terminal's standard "invalid operation" signal — vim beeps on
    /// errors too, and visual-bell settings are the user's choice).
    pub(crate) fn flash_err(&mut self, msg: impl Into<String>) {
        use std::io::Write;
        let msg = msg.into();
        // Beep only for a NEW error: the same message still on screen
        // (e.g. mashing `s`/`d` with nothing to export/delete) must not
        // ring the bell every keystroke — one beep per error is the
        // signal. The toast itself still refreshes either way.
        if !is_repeat_error(&self.status, &msg) {
            // BEL must be FLUSHED: Rust's stdout is line-buffered, so without
            // an explicit flush the beep would sit in the buffer forever
            // (no newline ever arrives while the TUI owns the terminal).
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
        self.status = Some((msg, Instant::now(), true));
    }

    /// Rebuild the per-source-line wrap cache when the content width changes.
    /// Inline card/composer rows are folded in by [`App::refresh_line_rows`], which
    /// runs every frame (composer height grows as you type).
    pub(crate) fn ensure_row_cache(&mut self, width: u16) {
        if width == self.content_width && !self.base_rows.is_empty() {
            return;
        }
        self.content_width = width;
        self.rebuild_base_rows();
    }

    /// Recompute `base_rows` from the tokenized source spans.
    pub(crate) fn rebuild_base_rows(&mut self) {
        let width = self.content_width.max(1) as usize;
        self.base_rows = self
            .spans
            .iter()
            .map(|spans| wrap_spans(spans, width).len())
            .collect();
    }

    /// Fold inline card rows (saved comments) and the composer box (while
    /// typing) into `line_rows`, so scrolling and cursor-following agree
    /// with what the renderer paints.
    pub(crate) fn refresh_line_rows(&mut self) {
        // Comment bars span the whole pane (gutter included). Only the
        // current file's cards count, and never the one being re-edited
        // (its card is hidden under the edit composer).
        let full_width = (self.content_width + self.gutter_cols) as usize;
        let mut extra = vec![0usize; self.base_rows.len()];
        let current = self.current_file_path().to_path_buf();
        for (raw, c) in self.comments.iter().enumerate() {
            if c.file_path != current
                || c.revision != self.current_revision_context()
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
    pub(crate) fn rows_of(&self, idx: usize) -> usize {
        self.line_rows.get(idx).copied().unwrap_or(1)
    }

    /// The row where line `idx` starts, given the current cache.
    pub(crate) fn row_of(&self, idx: usize) -> usize {
        self.line_rows[..idx].iter().sum()
    }

    /// The scrollable maximum offset for a viewport `height` rows tall.
    pub(crate) fn max_offset(&self, height: u16) -> usize {
        let total: usize = self.line_rows.iter().sum();
        total.saturating_sub(height as usize)
    }

    /// Keep the cursor line visible; return nothing, mutate `offset`.
    pub(crate) fn keep_cursor_visible(&mut self, height: u16) {
        if self.source.is_empty() || self.line_rows.is_empty() {
            self.offset = 0;
            return;
        }
        let start = self.row_of(self.cursor);
        let rows = self.rows_of(self.cursor);
        let end = start + rows;
        let height = height.max(1) as usize;
        if start < self.offset {
            self.offset = start;
        } else if end > self.offset + height {
            self.offset = end.saturating_sub(height);
        }
    }

    /// Center a source-line range in the source viewport as far as the
    /// document edges allow. Used by review navigation; ordinary cursor
    /// movement retains the less disruptive keep-visible behavior.
    pub(crate) fn center_source_range(&mut self, start: usize, end: usize, height: u16) {
        if self.source.is_empty() || self.line_rows.is_empty() {
            self.offset = 0;
            return;
        }
        let start = start.min(self.source.len().saturating_sub(1));
        let end = end.max(start).min(self.source.len().saturating_sub(1));
        let first_row = self.row_of(start);
        let after_last = self.row_of(end) + self.rows_of(end);
        let middle = first_row + after_last.saturating_sub(first_row + 1) / 2;
        let height = height.max(1) as usize;
        self.offset = middle
            .saturating_sub(height / 2)
            .min(self.max_offset(height as u16));
    }

    /// While the composer is open, keep its bar on screen. The bar's
    /// height is already folded into `line_rows` (it grows as you type),
    /// so the scroll anchor is the bar's BOTTOM row, not the cursor line:
    /// on the last line the bar cannot fit below the content, and a plain
    /// [`App::keep_cursor_visible`] run on the pre-composer layout leaves
    /// it clipped off-screen. Called every frame while Input is active
    /// (Input mode has no scrolling keys, so a per-frame nudge cannot
    /// fight the user).
    pub(crate) fn keep_composer_visible(&mut self, height: usize) {
        if self.source.is_empty()
            || self.line_rows.is_empty()
            || self.input_end >= self.line_rows.len()
        {
            return;
        }
        let end = self.composer_end_row();
        let height = height.max(1);
        if end > self.offset + height {
            self.offset = end.saturating_sub(height);
        }
    }

    /// The display row just past the composer bar's bottom rule.
    pub(crate) fn composer_end_row(&self) -> usize {
        self.row_of(self.input_end) + self.rows_of(self.input_end)
    }

    /// View-mode pendant of [`App::keep_composer_visible`]: the composer
    /// bar is spliced below the selection's end block, so the view's
    /// scrollable extent is the document PLUS the bar — the bottom stop
    /// extends by the bar's height, or a comment on the LAST line could
    /// never reveal its bar (the old clamp at the document's last row hid
    /// it entirely). Called every frame while the view-origin composer is
    /// open, from [`draw_view`] — before the visible window is built, so
    /// the splice below lands on the adjusted offset.
    pub(crate) fn keep_composer_visible_view(&mut self, height: usize) {
        let height = height.max(1);
        let full_width = view_content_width(self);
        let h = composer_line_count(&self.input, self.input_cursor, full_width);
        let anchor = view_composer_anchor(self);
        let min_offset = (anchor + 1 + h).saturating_sub(height);
        let max_off = self.view.rows.len().saturating_sub(height) + h;
        self.view.offset = self.view.offset.max(min_offset).min(max_off);
    }
}

impl App {
    /// Save current per-file state and load the file at `index`.
    pub(crate) fn switch_to_file(&mut self, new_index: usize) {
        if new_index == self.current_file_index || new_index >= self.files.len() {
            return;
        }
        // A file switch resolves (or drops) any pending `]`/`[` chord.
        self.pending_chord = None;
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
        old.review_changed = std::mem::take(&mut self.review_changed);
        old.review_deleted_before = std::mem::take(&mut self.review_deleted_before);
        old.comparison_changed = std::mem::take(&mut self.comparison_changed);
        old.comparison_deleted_before =
            std::mem::take(&mut self.comparison_deleted_before);

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
        self.review_changed = std::mem::take(&mut new.review_changed);
        self.review_deleted_before = std::mem::take(&mut new.review_deleted_before);
        self.comparison_changed = std::mem::take(&mut new.comparison_changed);
        self.comparison_deleted_before =
            std::mem::take(&mut new.comparison_deleted_before);

        self.current_file_index = new_index;
        self.history_changed.clear();
        self.history_changed_until = None;
        self.history_ghost_until = None;
        self.history_render_due = None;
        self.history_frame_flash_until = None;
        self.history_frame_flash_pending = false;
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

    pub(crate) fn current_file_path(&self) -> &Path {
        &self.files[self.current_file_index]
    }

    pub(crate) fn file_review_count(&self, index: usize) -> usize {
        let (changed, deleted) = if index == self.current_file_index {
            (&self.review_changed, &self.review_deleted_before)
        } else if let Some(state) = self.file_states.get(index) {
            (&state.review_changed, &state.review_deleted_before)
        } else {
            return 0;
        };
        let mut lines: Vec<usize> = changed.iter().copied().collect();
        lines.sort_unstable();
        let mut groups = 0usize;
        let mut previous = None;
        for line in lines {
            if previous.is_none_or(|previous| line != previous + 1) {
                groups += 1;
            }
            previous = Some(line);
        }
        groups
            + deleted
                .iter()
                .filter(|line| !changed.contains(line))
                .count()
    }

    pub(crate) fn history(&self) -> Option<&DocumentHistory> {
        self.histories.get(self.current_file_index)
    }

    pub(crate) fn is_historical(&self) -> bool {
        self.history().is_some_and(|history| history.position > 0)
    }

    pub(crate) fn current_revision_context(&self) -> Option<String> {
        self.history()?.current()?.context()
    }

    pub(crate) fn terminal_height(&self) -> u16 {
        ratatui::crossterm::terminal::size()
            .map(|s| s.1)
            .unwrap_or(24)
    }

    /// View-mode viewport height in rows — must match `draw_view`'s
    /// `inner.height` (title bar + footer + the frame's two borders take
    /// the other rows), or per-frame `keep_cursor_visible` re-shoves the
    /// offset and wheel scroll stalls. View mode always draws the frame.
    pub(crate) fn view_viewport_rows(&self) -> usize {
        self.terminal_height().saturating_sub(4).max(1) as usize
    }

    /// Source-mode viewport height — must match `draw_source`'s
    /// `inner.height` (title bar + footer only; source mode never draws
    /// a frame, keeping every column for the source).
    pub(crate) fn source_viewport_rows(&self) -> usize {
        self.terminal_height().saturating_sub(2).max(1) as usize
    }

    /// Is the view pane the one on screen (view mode, or the composer
    /// opened from view)? The frame — view-only chrome — hangs off this.
    pub(crate) fn view_active(&self) -> bool {
        self.mode == Mode::View
            || (self.mode == Mode::Input && self.composer_return == Mode::View)
    }

    /// Whether `Esc` may quit the app in normal mode (see [`EscQuit`]):
    /// `always` unconditionally; `auto` when a `--callback` is set — the
    /// app is a step in a loop then, so quitting is a return to the
    /// caller rather than a dead end. `never` keeps Esc a pure cancel.
    pub(crate) fn esc_quit_enabled(&self) -> bool {
        match self.config.esc_quit {
            EscQuit::Always => true,
            EscQuit::Never => false,
            EscQuit::Auto => self.config.callback.is_some(),
        }
    }
}
