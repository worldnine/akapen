//! The TUI's central state: the [`App`] struct with its per-file
//! [`FileState`], the [`Mode`] enum, overlay state, and shared constants.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime};

use ratatui::style::Color;

use semantic_reading::{Provider, SemanticDocument};

use crate::comment::{Comment, Selection};
use crate::config::{Config, EscQuit};
use crate::decoration::{Decoration, DecorationStyles};
use crate::highlight::{Highlighter, Span as HiSpan, TaggedLine, wrap_spans};
use crate::history::{DeletedBlock, DocumentHistory};
use crate::ime;
use crate::overlay::Overlay;
use crate::semantic::SemanticSource;
use crate::snapshot::SnapshotCache;
use crate::source::Source;
use crate::view::{
    ViewState, border_color, changed_bg, deleted_bg, history_border_color, landing_pulse_color,
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
/// The event-loop tick while a tachyonfx effect is animating: ~60 fps so
/// streaming reveals and backspace deletions render smoothly instead of
/// jumping in 100 ms steps.
pub(crate) const FX_TICK_MS: u64 = 16;
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
    pub(crate) spans: Vec<TaggedLine>,
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
    pub(crate) comparison_deleted_blocks: Vec<DeletedBlock>,
}

/// One provider answer on its way back to the event loop.
///
/// The generation rides ON THE MESSAGE rather than being implied by the
/// channel it arrived on: several analyses can be in flight at once (a
/// file that keeps changing while a slow command runs), and the one that
/// answers first is not necessarily the one that was asked first.
pub(crate) struct AnalysisMessage {
    /// The value [`App::semantic_generation`] had when this analysis was
    /// started. An answer is applied only while that is still current.
    pub(crate) generation: u64,
    /// The annotation, or the reason there is none (already rendered to
    /// a string: the error type does not cross the thread boundary).
    pub(crate) result: Result<SemanticDocument, String>,
}

/// The line the worker threads answer on: one channel for the whole
/// session, cloned per analysis.
pub(crate) struct AnalysisChannel {
    pub(crate) tx: Sender<AnalysisMessage>,
    pub(crate) rx: Receiver<AnalysisMessage>,
}

impl AnalysisChannel {
    pub(crate) fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx }
    }
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
    /// The baseline text of every change (rewrites included), anchored at
    /// its position in the displayed document. Source mode renders these
    /// inline as red deleted rows above their anchor line — the old side
    /// of the baseline → displayed-generation diff. Display-only: cursor,
    /// selection, and comments never address these rows.
    pub(crate) comparison_deleted_blocks: Vec<DeletedBlock>,
    /// Set when `n`/`N` lands on a pure deletion in source mode: the
    /// anchor line whose inline deleted rows hold the focus (they render
    /// bright instead of DIM, and `c` comments the deletion). The raw
    /// value can go stale; [`App::deletion_focus`] is the live check —
    /// moving the cursor off the line or starting a selection dissolves
    /// the focus with no extra bookkeeping.
    pub(crate) focused_deletion: Option<usize>,
    /// Until this instant the rendered view contains dim old blocks that
    /// are about to collapse out of the document.
    pub(crate) history_ghost_until: Option<Instant>,
    /// The history cursor may move ahead of the rendered document while an
    /// arrow is held. Once input settles, this deadline triggers one render
    /// of the final selected revision.
    pub(crate) history_render_due: Option<Instant>,
    /// The landing pulse (tachyonfx): a brief brighten-and-settle beat on
    /// the frame border when the selected revision finishes rendering and
    /// the transition effects (warp, ghost, stream) have settled. It rides
    /// ON TOP of the rotating time-machine gradient — the rotation never
    /// pauses — and drops itself when the 400 ms flight completes. `None`
    /// with `--no-fx` or while the transition is still moving.
    pub(crate) landing_pulse_fx: Option<tachyonfx::Effect>,
    /// The landing pulse's time window, kept for source mode: source has
    /// no frame border to flare, so the line-number gutter brightens for
    /// the same window instead (see `build_rows`). Armed together with
    /// [`App::landing_pulse_fx`] and cleared with it.
    pub(crate) landing_pulse_until: Option<Instant>,
    /// The rotating time-machine frame effect (tachyonfx), created at
    /// startup when `--fx` is on. Rendered by `draw` while browsing the
    /// past in view mode; `None` with `--no-fx`. Its rotation clock
    /// lives inside the effect itself.
    pub(crate) time_machine_fx: Option<tachyonfx::Effect>,
    /// The twinkling starfield behind the page while browsing the past
    /// (tachyonfx), created at startup alongside
    /// [`App::time_machine_fx`] when `--fx` is on: stars live in the
    /// page's empty cells, the space the frame floats in. Its twinkle
    /// clock lives inside the effect too.
    pub(crate) starfield_fx: Option<tachyonfx::Effect>,
    /// The generation-warp zoom (tachyonfx): window outlines flying
    /// through the frame when a selected revision finishes rendering —
    /// approaching from the depth going DEEPER into the past, receding
    /// coming BACK toward NOW. Created by `render_pending_history` in
    /// view mode with `--fx` on; dropped when the flight completes (or
    /// the view goes away mid-flight).
    pub(crate) warp_fx: Option<tachyonfx::Effect>,
    /// The timeline bar's slide-in/out effect (tachyonfx), created when
    /// the bar appears or disappears and rendered over its rows until
    /// it completes.
    pub(crate) timeline_fx: Option<tachyonfx::Effect>,
    /// While Some the timeline bar lingers to play its slide-out (the
    /// history cursor has already returned to NOW).
    pub(crate) timeline_exit_until: Option<Instant>,
    /// How long the slide-out linger lasts. A field (not a const) so
    /// tests can widen it and make the linger assertions deterministic:
    /// the wall-clock default (200 ms) raced under parallel test load,
    /// when the thread could stall between arming the window and
    /// asserting it.
    pub(crate) timeline_exit_ms: Duration,
    /// The history position to restore when the timeline overlay closes
    /// with Esc (the fzf-style cancel contract).
    pub(crate) timeline_restore: Option<usize>,
    /// While Some the scrubber tooltip rides above the timeline axis,
    /// centered over the `◆` and clamped to the edges: provenance, id,
    /// relative age, and summary of the revision at `◆`. Refreshed by
    /// every history step, gone after the hold
    /// window — the ephemeral half of the time-machine readout (the
    /// title keeps only the tiny `◆ 3/7` badge).
    pub(crate) timeline_tooltip_until: Option<Instant>,
    /// The active toast's fade-in/hold/fade-out effect (tachyonfx),
    /// created by `flash`/`flash_err` and dropped when the status
    /// expires. Rendered on the message row only while the toast is the
    /// top message (a prompt suppresses it).
    pub(crate) toast_fx: Option<tachyonfx::Effect>,
    /// Scatter-in effects for the blocks that appeared in the selected
    /// history revision: `(first display row, height, effect)` in
    /// view-relative coordinates, mapped to the screen at draw time.
    /// Rebuilt on every history render; cleared when the view is
    /// re-rendered (the row mapping would be stale).
    pub(crate) appear_fx: Vec<(usize, usize, tachyonfx::Effect)>,
    /// Scatter-out effects for the deletion ghosts (same view-relative
    /// rect shape as [`App::appear_fx`]), timed to complete exactly when
    /// the ghosts expire.
    pub(crate) ghost_fx: Vec<(usize, usize, tachyonfx::Effect)>,
    /// When the last frame was drawn: the real per-frame delta that
    /// advances the effects' timers.
    pub(crate) last_draw: Option<Instant>,
    /// Set once the new Markdown view is built. The event loop paints that
    /// view once without a pulse, then arms the landing pulse (see
    /// [`App::landing_pulse_fx`]) so the completion signal starts on the
    /// following frame.
    pub(crate) landing_pulse_pending: bool,
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
    /// Pre-tokenized spans, one entry per source line, each carrying the
    /// source byte range every span came from (see [`TaggedLine`]). The
    /// attribution is what lets source mode run the same range-decoration
    /// layer the rendered view does.
    pub(crate) spans: Vec<TaggedLine>,
    /// The theme (re-rendering and view styles resolve colors from it).
    pub(crate) highlight: Highlighter,
    /// Rendered view-mode rows.
    pub(crate) view: ViewState,
    /// The last full terminal buffer akapen drew. The wide-char residue
    /// pass ([`crate::clear_wide_char_residue`]) compares it with the
    /// newly drawn frame to find wide characters whose blanked right half
    /// ratatui's diff skipped (a real terminal keeps the halved glyph —
    /// the backspace afterimage). `None` before the first frame.
    pub(crate) last_frame: Option<ratatui::buffer::Buffer>,
    /// The frame drawn BEFORE the current one (`last_frame` is updated at
    /// the end of every draw).
    pub(crate) prior_frame: Option<ratatui::buffer::Buffer>,
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
    pub(crate) ui_selected_bg: Color,
    pub(crate) ui_changed_bg: Color,
    pub(crate) ui_deleted_bg: Color,
    pub(crate) ui_history_glow_bg: Color,
    pub(crate) ui_border: Color,
    pub(crate) ui_history_border: Color,
    /// The landing pulse's bright pole: the frame flares toward this color
    /// and settles back when a history transition completes.
    pub(crate) ui_landing_pulse: Color,
    pub(crate) ui_scrollbar: Color,
    /// The raw `--light` / dark flag the ui colors were resolved from,
    /// kept for effects created after startup (the generation warp
    /// picks its ring colors at flight time).
    pub(crate) ui_light: bool,
    /// Active scrollbar drag: `(start track row, start scroll offset)` —
    /// set on a thumb press, cleared on release (viewport-only scroll, so
    /// the cursor keeps its absolute position).
    pub(crate) scrollbar_drag: Option<(usize, usize)>,

    // ---- Semantic Reading Layer (docs/design/semantic-reading-layer.md) ----
    /// Where semantic annotation comes from: `--semantic` installs a
    /// fixture provider, `--semantic-cmd` an external command, and a
    /// future in-process Jev provider drops in beside them unchanged.
    /// `None` = the layer is absent entirely.
    ///
    /// **This is the seam.** Everything slow and non-deterministic lives
    /// behind it; everything outside is the deterministic local
    /// calculation the layer exists for.
    pub(crate) semantic_source: Option<SemanticSource>,
    /// The annotation for the document CURRENTLY on screen, or `None`
    /// when there is no provider, the provider refused this document
    /// (a fixture whose `source_sha256` names another file), or an
    /// external command is still running for a document this one does
    /// not describe. Produced only by [`App::reanalyze_semantics`] and
    /// [`App::accept_analysis`] — the two places that take a provider's
    /// answer.
    pub(crate) semantic_doc: Option<SemanticDocument>,
    /// How many times a document has been handed to the provider. Every
    /// call to [`App::reanalyze_semantics`] bumps it, and an answer is
    /// only accepted if it names the CURRENT value.
    ///
    /// **This is what keeps a slow answer off a document it never saw.**
    /// An external command is a process launch plus a network round
    /// trip, so it can still be running when `reload.rs` notices
    /// the file changed while one is running, the older answer describes
    /// byte positions in text that is no longer on screen. Applying it
    /// would decorate the wrong places — and silently, since every range
    /// still lands on a character boundary. So it is dropped by number
    /// rather than by hope ([`App::accept_analysis`]).
    pub(crate) semantic_generation: u64,
    /// The generation of the external analysis currently running, if
    /// any. Drives the `analyzing…` readout and is cleared when THAT
    /// generation's answer (or a newer one) arrives.
    pub(crate) semantic_inflight: Option<u64>,
    /// The worker threads' end of the line. Created with the source (so
    /// a session without `--semantic-cmd` never allocates one) and kept
    /// for the whole session: each analysis clones the sender, and the
    /// generation on the message — not the identity of the channel —
    /// decides whether the answer still applies.
    pub(crate) semantic_results: Option<AnalysisChannel>,
    /// Reading Budget: "how much attention can I spend on this
    /// document", 1..=100 %, default 100. A pure reading preference, so
    /// it is NOT per-file state — switching files keeps it.
    pub(crate) reading_budget: u8,
    /// [`App::semantic_doc`] projected onto the current budget: the
    /// decoration list the paint consumes, cached so a frame does no
    /// policy work. Recomputed by
    /// [`App::refresh_semantic_decorations`] — from the budget and the
    /// document only, never from the provider.
    pub(crate) semantic_decorations: Vec<Decoration>,
    /// The decoration styles resolved from the session's theme, for the
    /// SOURCE-mode paint. The rendered view carries its own copy on
    /// [`ViewState`]; source mode has no `ViewState` of its own to hang
    /// them on, and `app.view` is not a safe stand-in — a file that has
    /// never been shown in view mode holds a `ViewState::default()`,
    /// whose styles are the dark-theme fallbacks. Theme-independent of
    /// the file, so it is resolved once in [`App::new`].
    pub(crate) decoration_styles: DecorationStyles,
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
        let time_machine_fx = config
            .fx
            .then(|| crate::effects::time_machine_border_effect(light));
        let starfield_fx = config.fx.then(|| crate::effects::starfield_effect(light));
        let decoration_styles =
            DecorationStyles::from_theme(&highlight, config.decoration_blend);
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
            comparison_deleted_blocks: Vec::new(),
            focused_deletion: None,
            history_ghost_until: None,
            history_render_due: None,
            landing_pulse_fx: None,
            landing_pulse_until: None,
            time_machine_fx,
            starfield_fx,
            warp_fx: None,
            timeline_fx: None,
            timeline_exit_until: None,
            timeline_tooltip_until: None,
            timeline_exit_ms: Duration::from_millis(crate::effects::TIMELINE_SLIDE_MS as u64),
            timeline_restore: None,
            toast_fx: None,
            appear_fx: Vec::new(),
            ghost_fx: Vec::new(),
            last_draw: None,
            landing_pulse_pending: false,
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
            last_frame: None,
            prior_frame: None,
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
            ui_selected_bg: selected_bg(light),
            ui_changed_bg: changed_bg(light),
            ui_deleted_bg: deleted_bg(light),
            ui_history_glow_bg: history_glow_bg(light),
            ui_border: border_color(light),
            ui_history_border: history_border_color(light),
            ui_landing_pulse: landing_pulse_color(light),
            ui_scrollbar: scrollbar_thumb(light),
            ui_light: light,
            scrollbar_drag: None,
            semantic_source: None,
            semantic_doc: None,
            semantic_generation: 0,
            semantic_inflight: None,
            semantic_results: None,
            reading_budget: crate::semantic::DEFAULT_BUDGET,
            semantic_decorations: Vec::new(),
            decoration_styles,
        }
    }

    /// Whether this session has a Semantic Reading Layer at all — i.e.
    /// whether a provider was installed from the command line.
    ///
    /// **Everything the layer adds to the UI hangs off this.** Without a
    /// provider akapen behaves exactly as it did before the layer
    /// existed: the budget keys are not bound (they fall through to the
    /// same `_ => {}` as any unbound key), the `?` help has no row for
    /// them, the status line has no `READ %`, and nothing is ever
    /// flashed about a feature that is not there. A user with no API key
    /// — and no fixture — must not be shown a disabled feature.
    ///
    /// This is deliberately about the PROVIDER, not the annotation: a
    /// session that asked for the layer and got a document the provider
    /// refused keeps its keys and is told why.
    pub(crate) fn semantic_enabled(&self) -> bool {
        self.semantic_source.is_some()
    }

    /// Install a source and, for an external command, the channel its
    /// worker threads answer on. **The one place the layer is turned on.**
    pub(crate) fn set_semantic_source(&mut self, source: Option<SemanticSource>) {
        // Nothing can answer on the old channel any more (its receiving
        // end goes with it), so a run that was in flight is over as far
        // as this session is concerned — otherwise the status line would
        // say analyzing… for the rest of the session.
        self.semantic_inflight = None;
        self.semantic_results = match source {
            // A session without `--semantic-cmd` never allocates a
            // channel — the layer stays exactly as cheap as before.
            Some(SemanticSource::Command(_)) => Some(AnalysisChannel::new()),
            _ => None,
        };
        self.semantic_source = source;
    }

    /// Re-run the provider over the document now on screen.
    ///
    /// **The only place akapen asks its provider anything.** It is called
    /// where `App::source` is REPLACED — startup, `r`/auto reload, a
    /// time-machine revision, a file switch — which is the position the
    /// design document's 「編集時」 chapter asks for: a real provider
    /// (Jev) re-reads the document when the document changes, and at no
    /// other time. In particular **no budget key reaches this function**.
    ///
    /// A provider that refuses the document (the fixture belongs to
    /// another file) drops the annotation and says so, rather than
    /// painting confident nonsense at positions that mean nothing here.
    pub(crate) fn reanalyze_semantics(&mut self) {
        if self.semantic_source.is_none() {
            return;
        }
        // Whatever happens next, the annotation in hand is only still
        // valid if it describes the text now on screen. An annotation
        // that names ANOTHER document is dropped here — before the new
        // answer exists — so nothing paints old judgements onto new
        // bytes, not even for the seconds an external command runs.
        self.drop_stale_annotation();
        self.semantic_generation += 1;
        let generation = self.semantic_generation;
        match self.semantic_source.as_ref() {
            // 即答する provider: その場で呼ぶ。世代は必ず現在値なので
            // 捨てられることはない（非同期経路と同じ入口を通すためだけ
            // に番号を付けている）。
            Some(SemanticSource::Inline(provider)) => {
                let analyzed = provider.analyze(&self.source.content);
                self.accept_analysis(AnalysisMessage {
                    generation,
                    result: analyzed.map_err(|e| e.to_string()),
                });
            }
            // 外部コマンド: 別スレッドへ。**ここで待たない。** イベント
            // ループは `event::poll` のポーリングで回っているので、
            // プロセス起動とネットワーク往復を挟む呼び出しをこの場で
            // 待つと UI が固まる。
            Some(SemanticSource::Command(provider)) => {
                let Some(channel) = self.semantic_results.as_ref() else {
                    // set_semantic_source を通らずに組み立てられた App。
                    self.flash_err("--semantic-cmd: no channel to receive the answer");
                    return;
                };
                let provider = provider.clone();
                let tx = channel.tx.clone();
                let source = self.source.content.clone();
                self.semantic_inflight = Some(generation);
                std::thread::spawn(move || {
                    let result = provider.analyze(&source).map_err(|e| e.to_string());
                    // 受け手が先に消えていても（終了・受信側の drop）
                    // ここは静かに終わる。
                    let _ = tx.send(AnalysisMessage { generation, result });
                });
            }
            None => unreachable!("checked above"),
        }
    }

    /// Drop the annotation in hand when it names a document other than
    /// the one on screen.
    ///
    /// An annotation is a set of byte ranges, and byte ranges only mean
    /// something against the text they were computed from. Both provider
    /// kinds stamp the text they read (`source_sha256`), so this is a
    /// comparison rather than a guess; an annotation that names nothing
    /// is left alone (a hand-written fixture without the field — the
    /// same backwards compatibility [`crate::semantic::DigestChecked`]
    /// keeps).
    fn drop_stale_annotation(&mut self) {
        let stale = self.semantic_doc.as_ref().is_some_and(|document| {
            document.source_digest().is_some_and(|named| {
                let here = crate::semantic::source_digest(&self.source.content);
                !named.eq_ignore_ascii_case(&here)
            })
        });
        if stale {
            self.semantic_doc = None;
            self.refresh_semantic_decorations();
        }
    }

    /// Take one answer from a provider — **or refuse it as stale.**
    ///
    /// An answer is applied only when it names the current generation.
    /// A slower analysis whose document has since been replaced is
    /// dropped on the floor: its ranges describe text that is no longer
    /// on screen, and painting them would decorate the wrong places
    /// without any symptom to notice (every range still lands on a
    /// character boundary, so nothing panics and nothing looks broken).
    ///
    /// A failure keeps whatever annotation survived
    /// [`App::drop_stale_annotation`] — an external command that
    /// crashed, timed out or answered nonsense is a reason to say so,
    /// not a reason to throw away a valid annotation for the document
    /// the user is looking at.
    pub(crate) fn accept_analysis(&mut self, message: AnalysisMessage) {
        if message.generation != self.semantic_generation {
            return; // 古い世代の答え。捨てる。
        }
        if self.semantic_inflight == Some(message.generation) {
            self.semantic_inflight = None;
        }
        let refusal = match message.result {
            Ok(document) => {
                self.semantic_doc = Some(document);
                None
            }
            Err(e) => Some(e),
        };
        self.refresh_semantic_decorations();
        if let Some(message) = refusal {
            self.flash_err(message);
        }
    }

    /// Collect whatever the worker threads have finished. Called once per
    /// event-loop tick; does nothing (and allocates nothing) without
    /// `--semantic-cmd`.
    pub(crate) fn poll_semantic_analysis(&mut self) {
        loop {
            let received = match self.semantic_results.as_ref() {
                Some(channel) => channel.rx.try_recv(),
                None => return,
            };
            match received {
                Ok(message) => self.accept_analysis(message),
                // Disconnected は起こらない（送信端を App 自身が持って
                // いる）が、起きても待ち続けるより抜ける方が正しい。
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Project the annotation onto the current budget.
    ///
    /// This is the whole cost of moving the budget: `policy::decorate`
    /// over the units already in hand. No parsing, no rendering, no
    /// provider.
    pub(crate) fn refresh_semantic_decorations(&mut self) {
        self.semantic_decorations = match self.semantic_doc.as_ref() {
            Some(document) => crate::semantic::decorations_for(document, self.reading_budget),
            None => Vec::new(),
        };
    }

    /// Move the Reading Budget by `delta` percentage points, clamped to
    /// 1..=100. Returns whether it actually moved (already at an end is
    /// not an error — the readout simply does not change).
    ///
    /// 設計書「Budget 変更では Jev を呼ばない」: the body is a clamp plus
    /// [`App::refresh_semantic_decorations`], and neither reaches
    /// `Provider::analyze` or `render::render`. That is the property of
    /// this layer, and it is held here by the call graph rather than by
    /// a comment.
    pub(crate) fn nudge_reading_budget(&mut self, delta: i16) -> bool {
        let next = (self.reading_budget as i16 + delta).clamp(
            crate::semantic::MIN_BUDGET as i16,
            crate::semantic::MAX_BUDGET as i16,
        ) as u8;
        if next == self.reading_budget {
            return false;
        }
        self.reading_budget = next;
        self.refresh_semantic_decorations();
        true
    }

    /// Set a transient footer message.
    /// Set a transient footer message (info: yellow).
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), false));
        if self.config.fx {
            self.toast_fx = Some(crate::effects::toast_effect());
        }
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
        if self.config.fx {
            self.toast_fx = Some(crate::effects::toast_effect());
        }
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

    /// The range decorations that apply to the document currently on
    /// screen, ready to hand to [`crate::decoration::decorate_row`].
    ///
    /// `--decorations` は手で置く開発用のレンジ、semantic の方は Budget から
    /// 導かれるレンジ。併用されたら連結する（decoration は patch を重ねる
    /// ので、後ろに置いた semantic 側が同じ range では後勝ちになる）。
    ///
    /// The `--decorations` ranges come from OUTSIDE, so they are checked
    /// against the document that is actually on screen — which the time
    /// machine and a reload both change under them. Empty (the normal
    /// case) allocates nothing.
    ///
    /// **Both paint paths call this**: the rendered view and source mode
    /// decorate the same ranges, so they cannot disagree about which
    /// bytes are MARKED or DIM.
    pub(crate) fn active_decorations(&self) -> Vec<Decoration> {
        if self.semantic_decorations.is_empty() {
            crate::decoration::sanitize(&self.config.decorations, &self.source.content)
        } else {
            let mut both = self.config.decorations.clone();
            both.extend_from_slice(&self.semantic_decorations);
            crate::decoration::sanitize(&both, &self.source.content)
        }
    }

    /// Recompute `base_rows` from the tokenized source spans.
    pub(crate) fn rebuild_base_rows(&mut self) {
        let width = self.content_width.max(1) as usize;
        self.base_rows = self
            .spans
            .iter()
            .map(|line| wrap_spans(&line.spans, width).len())
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
        // 世代照合は「id — 説明文」の identity（「 — 」の左）だけで行う。
        let revision = self.current_revision_context();
        for (raw, c) in self.comments.iter().enumerate() {
            if c.file_path != current
                || !crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
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
        // Inline deleted rows (the baseline text of every change) belong
        // to their anchor line's band, like cards do — the whole scroll /
        // cursor / mouse stack keeps working off one row count per line.
        // A block past the last line clamps to it (rendered below its
        // text; see `deleted_blocks_at`).
        for block in &self.comparison_deleted_blocks {
            let i = block.anchor.min(extra.len().saturating_sub(1));
            if i < extra.len() {
                extra[i] += deleted_block_rows(&block.content, self.content_width.max(1) as usize);
            }
        }
        self.line_rows = self
            .base_rows
            .iter()
            .zip(extra)
            .map(|(b, e)| b + e)
            .collect();
    }

    /// The deleted blocks that render inside line `idx`'s band, split into
    /// (above the line's text, below it). Blocks anchor BEFORE their line;
    /// a block whose anchor fell past the last line (content deleted at
    /// EOF) clamps to the last line and renders below its text instead.
    /// The live deletion focus: the anchor line `n`/`N` landed on, as long
    /// as the cursor is still there with no selection. Leaving the line or
    /// starting a selection dissolves the focus; returning to the line
    /// (before the next mark refresh clears the raw value) revives it.
    pub(crate) fn deletion_focus(&self) -> Option<usize> {
        let line = self.focused_deletion?;
        (self.selection.is_none() && self.cursor == line).then_some(line)
    }

    /// The baseline text of the focused deletion: every block anchored at
    /// the focused line, joined in document order. This is the snippet a
    /// `c` comment quotes while the focus is live — the deleted content
    /// itself, not the anchor line that happens to sit below it.
    pub(crate) fn focused_deletion_content(&self) -> Option<String> {
        self.deleted_content_at(self.deletion_focus()?)
    }

    /// The baseline text deleted at `line` (every block anchored there),
    /// joined in document order — regardless of any focus. `None` when no
    /// deletion anchors at the line.
    pub(crate) fn deleted_content_at(&self, line: usize) -> Option<String> {
        let last = self.source.len().saturating_sub(1);
        let parts: Vec<&str> = self
            .comparison_deleted_blocks
            .iter()
            .filter(|block| block.anchor.min(last) == line)
            .map(|block| block.content.as_str())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    pub(crate) fn deleted_blocks_at(&self, idx: usize) -> (Vec<&DeletedBlock>, Vec<&DeletedBlock>) {
        let last = self.source.len().saturating_sub(1);
        let mut above = Vec::new();
        let mut below = Vec::new();
        for block in &self.comparison_deleted_blocks {
            if block.anchor.min(last) != idx {
                continue;
            }
            if block.anchor > last {
                below.push(block);
            } else {
                above.push(block);
            }
        }
        (above, below)
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
            // A band taller than the viewport (a long wrapped line, a
            // tall card stack) cannot fit whole: prefer its START — the
            // `>` glyph lives on the band's first text row — over its
            // end, or the cursor itself scrolls off the top edge.
            if start < self.offset {
                self.offset = start;
            }
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
        old.comparison_deleted_blocks =
            std::mem::take(&mut self.comparison_deleted_blocks);

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
        self.comparison_deleted_blocks =
            std::mem::take(&mut new.comparison_deleted_blocks);

        self.current_file_index = new_index;
        // Another document is live now. The budget is a reading
        // preference and rides along unchanged; the ANNOTATION does not
        // — it describes byte positions in the file we just left.
        self.reanalyze_semantics();
        self.focused_deletion = None;
        self.history_ghost_until = None;
        self.history_render_due = None;
        self.landing_pulse_pending = false;
        self.landing_pulse_fx = None;
        self.landing_pulse_until = None;
        self.timeline_fx = None;
        self.timeline_exit_until = None;
        self.timeline_tooltip_until = None;
        self.timeline_restore = None;
        self.confirm_quit = false;
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

    /// Whether any tachyonfx effect is currently animating (toast fade,
    /// appear/ghost scatter, the rotating time-machine frame). The event
    /// loop raises its tick rate while this is true.
    pub(crate) fn has_active_fx(&self) -> bool {
        if !self.config.fx {
            return false;
        }
        self.toast_fx.is_some()
            // The scrubber tooltip's exit dissolve is drawn by the bar
            // drawer itself (not a tachyonfx shader — hidden cells must
            // reveal the document, which a post-hoc shader cannot do),
            // but it still needs fast ticks to animate smoothly.
            || self.timeline_tooltip_until.is_some()
            || !self.appear_fx.is_empty()
            || !self.ghost_fx.is_empty()
            || self.warp_fx.is_some()
            || self.landing_pulse_fx.is_some()
            || (self.is_historical()
                && (self.time_machine_fx.is_some() || self.starfield_fx.is_some()))
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

/// Display rows one deleted block occupies at `width` content columns:
/// each baseline line wraps exactly like a source line does (an empty
/// line still takes one row — `wrap_spans` never returns zero rows).
/// `build_rows` paints the same wrap per line, so the row-count caches
/// and the renderer cannot disagree.
pub(crate) fn deleted_block_rows(content: &str, width: usize) -> usize {
    content
        .split('\n')
        .map(|line| {
            wrap_spans(
                &[HiSpan {
                    text: line.to_string(),
                    style: ratatui::style::Style::default(),
                }],
                width,
            )
            .len()
        })
        .sum()
}
