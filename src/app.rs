//! The TUI's central state: the [`App`] struct with its per-file
//! [`FileState`], the [`Mode`] enum, overlay state, and shared constants.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime};

use ratatui::style::Color;

use semantic_reading::SemanticDocument;

use crate::comment::{Comment, Selection};
use crate::config::{Config, EscQuit};
use crate::decoration::{Decoration, DecorationStyles};
use crate::focus::Focus;
use crate::highlight::{Highlighter, Span as HiSpan, TaggedLine, wrap_spans};
use crate::history::{DeletedBlock, DocumentHistory};
use crate::ime;
use crate::overlay::Overlay;
use crate::chrome::Readout;
use crate::marks_questions::{Question, Questions};
use crate::review::{Candidate, CandidateState, DismissedKey, DismissedStore};
use crate::review_rules::Rules as ReviewRules;
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
    pub(crate) baseline_changed: HashSet<usize>,
    pub(crate) baseline_deleted_before: HashSet<usize>,
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

/// review / lint コメントと候補を結ぶ鍵 — (ファイル, 版, ルール, 行)。
///
/// コメントは候補のバイト範囲を持たず、行と本文の形（ルール）だけを持つ
/// （[`crate::review::candidate_comment_rule`]）。同じ行に同じルールの
/// 候補が 2 本あれば、2 本は同じコメントを指す。
pub(crate) type ReviewCommentKey = (PathBuf, Option<String>, CandidateSpot);

/// review / lint コメントと候補を結ぶ鍵。
///
/// **結び目のあるコメントは (ルール, バイト範囲)** で候補と結ぶ — 同じ行に
/// 同じルールの候補が 2 本あっても取り違えない。結び目の無いコメント（古い
/// 経路）は今までどおり (ルール, 行) で結ぶ。候補の側は両方の鍵を持つ
/// （[`Self::of_candidate`]）ので、どちらの形のコメントとも照合できる。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CandidateSpot {
    /// ルールとバイト範囲。
    Range(String, usize, usize),
    /// ルールと 1 始まりの行（先頭, 末尾）。
    Lines(String, (u32, u32)),
}

impl CandidateSpot {
    /// コメントの鍵。review / lint の形でない本文（人の赤入れ）は `None`。
    ///
    /// ルールは本文から読む — 人が本文を書き換えて review の形でなく
    /// なったら、結び目が残っていても候補とは結ばない（今までと同じ）。
    pub(crate) fn of_comment(comment: &Comment) -> Option<Self> {
        let rule = crate::review::candidate_comment_rule(&comment.text)?;
        Some(match &comment.anchor {
            Some(anchor) => Self::Range(rule, anchor.range.start, anchor.range.end),
            None => Self::Lines(rule, (comment.start, comment.end)),
        })
    }

    /// 候補の鍵 — バイト範囲のものと行のものの 2 つ。
    pub(crate) fn of_candidate(candidate: &crate::review::Candidate) -> impl Iterator<Item = Self> {
        [
            Self::Range(
                candidate.rule.clone(),
                candidate.range.start,
                candidate.range.end,
            ),
            Self::Lines(candidate.rule.clone(), candidate.lines),
        ]
        .into_iter()
    }
}

/// `a` / `x` を押した結果（一覧のキーがフラッシュを選ぶ）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewToggle {
    /// 候補が無い（一覧が空・添字の外）。
    Nothing,
    /// コメントを作った。
    Accepted,
    /// コメントを消して Pending に戻した（Accepted で `a`）。
    Unaccepted,
    /// 捨てた（記録した）。
    Dismissed,
    /// 捨てたのを取り消して Pending に戻した（Dismissed で `x`）。
    Restored,
    /// 送った候補。何もしていない（二重に送らせない）。
    AlreadySent,
}

/// Review の 1 ルール分の答え。
///
/// **marks の [`AnalysisMessage`] とは別の型・別の線である。** 同じ線に
/// 相乗りさせると [`App::accept_analysis`] が Review の答えを
/// `semantic_doc` に入れてしまい、`R` を押しただけでラインマーカーが
/// 別の問いの答えに化ける。「Review 中に marks の状態が変わらない」は
/// 型で担保する。
pub(crate) struct ReviewMessage {
    /// [`App::review_generation`] の値。これが古ければ捨てる。
    pub(crate) generation: u64,
    pub(crate) answer: ReviewAnswer,
}

/// 出どころ 1 本分の答え。**世代だけでは足りない** — 1 世代の中で、有効な
/// ルールの数（＋ `--lint-cmd` の 1 本）だけ答えが返ってくる。
pub(crate) enum ReviewAnswer {
    /// Jev のルール 1 本の答え。
    Rule {
        rule: String,
        result: Result<SemanticDocument, String>,
    },
    /// `--lint-cmd` の答え（[`crate::lint`]）。
    Lint(Result<Vec<crate::lint::Diagnostic>, String>),
}

/// Review のワーカーが答える線。marks と同じく 1 セッション 1 本。
pub(crate) struct ReviewChannel {
    pub(crate) tx: Sender<ReviewMessage>,
    pub(crate) rx: Receiver<ReviewMessage>,
}

impl ReviewChannel {
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
    /// Persistent, Git-independent change marks: the cumulative transition
    /// from the last acknowledged document to NOW.
    pub(crate) baseline_changed: HashSet<usize>,
    pub(crate) baseline_deleted_before: HashSet<usize>,
    /// Baseline-relative marks for the generation actually rendered. At
    /// NOW these equal the cumulative change marks above; in history they
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
    /// **マーカーが引かれる演出**（marks モード）。答えが届いた瞬間に
    /// 立ち、950 ms で終わる（[`crate::effects::marks_reveal_effect`]）。
    ///
    /// **view モードにも source モードにも乗る。** 琥珀は両方で塗られる
    /// ので、演出だけ view 限定だと「source では効かない機能」になる
    /// （他の演出が `view_active()` の中にいるのは、時間旅行の枠のように
    /// view にしか無いものを描いているからである）。`--no-fx` は尊重する。
    pub(crate) marks_fx: Option<tachyonfx::Effect>,
    /// フッタ右下の読み出しが一瞬明るくなる演出（問い・つまみ・答えの
    /// 変化）。座布団があればそこだけが光る
    /// （`crate::effects::readout_flash_effect`、面は
    /// `crate::chrome::FooterLayout::flash_x`）。
    pub(crate) readout_fx: Option<tachyonfx::Effect>,
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
    pub(crate) ui_changed_emph_bg: Color,
    pub(crate) ui_deleted_emph_bg: Color,
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
    /// [`App::semantic_inflight`] が立った時刻。待ちの印のコマと経過秒の
    /// 起点である（[`crate::chrome::busy_text`]）。
    pub(crate) semantic_since: Option<Instant>,
    /// The worker threads' end of the line. Created with the source (so
    /// a session without `--semantic-cmd` never allocates one) and kept
    /// for the whole session: each analysis clones the sender, and the
    /// generation on the message — not the identity of the channel —
    /// decides whether the answer still applies.
    pub(crate) semantic_results: Option<AnalysisChannel>,
    /// **遅延の起点を越えたか。** `--semantic-cmd` は、開いただけでは走らない
    /// — 1 文書 1 回の解析は業務議事録で約 5 円かかるので、素で読むだけの
    /// 文書にそれを払わない（`docs/design/marks-only-and-review-mode.md` の
    /// 1 節の実測）。
    ///
    /// 起点は **問いを決めた最初の 1 打**（`m` / `M` / `/`）で、
    /// [`App::ask_marks`] がここを立てる。それまでは `semantic_doc` も
    /// [`App::semantic_inflight`] も `None` のままなので、表示は
    /// `--semantic-cmd` を渡していないときと 1 文字も変わらない。
    ///
    /// 一度立てたらセッションの終わりまで立ったまま（読み手は「この層を
    /// 使う」と言った）で、以降は文書が入れ替わるたびに解析する。2 度目から
    /// は文書が変わっていなければキャッシュに当たって 0 円である。
    ///
    /// `--semantic <fixture>` と、層が無いセッションはこの値を見ない
    /// （[`App::reanalyze_semantics`] の Command 腕だけが読む）。
    pub(crate) semantic_armed: bool,
    // ---- marks (docs/design/marks-only-and-review-mode.md 0 節) ----
    /// 問いの一式（定型 5 本 ＋ 自由入力の型）。層の無いセッションは
    /// 読まないので `None` のまま。
    pub(crate) marks_questions: Option<Questions>,
    /// **いま聞いている問い。** `None` は「まだ何も聞いていない」で、
    /// marks モードの遅延の起点がここである — 問いが決まるまで判定器は
    /// 1 度も起きない（`m` / `/` の最初の 1 打。`crate::keys`）。
    pub(crate) marks_question: Option<Question>,
    /// 定型を巡っている位置（`m` の環）。自由入力のあとも、ここから続く。
    pub(crate) marks_preset: usize,
    /// **つまみ** — 上から何 % の Unit を光らせるか（1..=100、既定 15）。
    /// 読む側の好みなので、文書をまたいで残る。
    pub(crate) marks_share: u8,
    /// composer が**コメントではなく問いの入力**に使われているか（`/`）。
    ///
    /// `Mode::Input` を借りているだけなので、コメントの経路（アンカー・
    /// 再編集・スナップショット）には触らない。確定と取り消しの 2 か所だけが
    /// この旗を見る。
    pub(crate) marks_prompt: bool,
    /// **フォーカス**（`f`）— 光った文と見出しのほかを沈め、琥珀を消しているか。
    pub(crate) marks_focus: Focus,
    /// 光っている文のソース行（昇順）。**溝の目盛りと `]m` の
    /// ジャンプが同じここを見る。** 装飾を作り直すたびに更新される。
    ///
    /// **フォーカス中も同じ行である。** フォーカスは琥珀を描かないが、
    /// 台帳は描いた装飾ではなく marks の投影から数える
    /// （[`App::refresh_semantic_decorations`]）。
    pub(crate) marks_lines: Vec<usize>,
    /// [`App::semantic_doc`] projected onto the current budget: the
    /// decoration list the paint consumes, cached so a frame does no
    /// policy work. Recomputed by
    /// [`App::refresh_semantic_decorations`] — from the budget and the
    /// document only, never from the provider.
    pub(crate) semantic_decorations: Vec<Decoration>,
    // ---- Review（docs/design/marks-only-and-review-mode.md 4 節） ----
    //
    // **marks の状態とは 1 つも共有していない。** `R` を押しても
    // `marks_question` / `semantic_doc` / `semantic_generation` /
    // `semantic_decorations` / `marks_lines` は 1 バイトも動かない
    // （別機能だからで、テストがそれを押さえている）。
    /// ルール一式。層の無いセッションは読まないので `None` のまま。
    pub(crate) review_rules: Option<ReviewRules>,
    /// `--lint-cmd`。**意味層が無くても** Review を立てる出どころである
    /// （4 節「判定の出どころとしての linter」）。
    pub(crate) lint: Option<crate::lint::LintCommand>,
    /// 直前の lint が失敗した理由（`lint: output is not diagnostics JSON` など）。
    /// 一覧が「0 件」と見せないために持つ。
    pub(crate) review_lint_error: Option<String>,
    /// いま画面に出ている候補（**文書順**）。
    pub(crate) review_candidates: Vec<Candidate>,
    /// 何度 Review の解析を頼んだか。答えはこの値を名乗るものだけ通す
    /// （[`App::semantic_generation`] と同じ仕掛けで、番号は別に持つ）。
    pub(crate) review_generation: u64,
    /// いま走っているルールの本数。`Review · analyzing…` の出所である。
    pub(crate) review_inflight: usize,
    /// Review の解析を頼んだ時刻（[`App::semantic_since`] と同じ役）。
    pub(crate) review_since: Option<Instant>,
    /// **遅延の起点を越えたか。** `R` の最初の 1 打が立てる。marks の
    /// [`App::semantic_armed`] と同じ理由（開いただけの文書に解析費用を
    /// 払わない）で、**別の旗である** — marks を使ったからといって
    /// Review の費用まで払う筋合いは無い。
    pub(crate) review_armed: bool,
    /// ワーカーが答える線。`--semantic-cmd` のセッションだけ持つ。
    pub(crate) review_results: Option<ReviewChannel>,
    /// いまの文書について、いま捨てている (ルール, 範囲)。**`Dismissed` の
    /// 状態の元はここだけ**（[`App::candidate_states`]）。
    pub(crate) review_dismissed: HashSet<DismissedKey>,
    /// `s` で送った review / lint コメントの (ファイル, 版, ルール, 行)。
    /// **`Sent` の状態の元はここだけ**（[`App::candidate_states`]）。
    ///
    /// ディスクには書かない — 送った相手が書き換えれば reload で決め直す
    /// （[`App::forget_review_sent`]）ので、次に開いたときには要らない。
    pub(crate) review_sent: HashSet<ReviewCommentKey>,
    /// **前のセッションまでに送った**候補（`sent.jsonl` のうち、いま画面に
    /// ある文書の sha のもの）。(ルール, 範囲)。[`App::load_dismissed`] が
    /// 捨てた記録と同じ時に読み、[`App::forget_review_sent`] が捨てる。
    pub(crate) review_sent_disk: HashSet<DismissedKey>,
    /// **直前の `A` が作ったコメントの結び目**（ルール, 範囲）。`Some` の
    /// あいだだけ、もう一度 `A` を押すとそれをまとめて Pending に戻す
    /// （[`App::undo_accept_all`]）。一覧のどのキー（移動も含む）・マウス・
    /// reload でも `None` に戻る — 戻せるのは `A` の**直後**だけである。
    pub(crate) review_accept_all_undo: Option<Vec<DismissedKey>>,
    /// 捨てた候補の置き場。決められない環境では `None`（セッション内
    /// だけ消えて、記録は残らない）。
    pub(crate) review_dismissed_store: Option<DismissedStore>,
    /// 一覧から `e` で直しに行った候補の、**旧の版での**先頭バイト。
    /// 見届けた reload が差分で新しい位置へ写し、
    /// [`App::review_cursor_target`] に置き換える（4 節「直接編集」）。
    pub(crate) review_edit_anchor: Option<usize>,
    /// 再解析の答えが揃ったとき、一覧のカーソルを置く新しい版のバイト位置。
    /// そこを覆うか、そこより後ろの最初の候補に着く。
    pub(crate) review_cursor_target: Option<usize>,
    /// 直前の reload で外した review コメントの 1 行（`2 review comments
    /// resolved by edit`）。自動 reload のフラッシュにも同じ 1 行を添える。
    pub(crate) review_reload_note: Option<String>,
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
            baseline_changed: HashSet::new(),
            baseline_deleted_before: HashSet::new(),
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
            marks_fx: None,
            readout_fx: None,
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
            ui_changed_emph_bg: crate::view::changed_emph_bg(light),
            ui_deleted_emph_bg: crate::view::deleted_emph_bg(light),
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
            semantic_since: None,
            semantic_results: None,
            semantic_armed: false,
            marks_questions: None,
            review_rules: None,
            lint: None,
            review_lint_error: None,
            review_candidates: Vec::new(),
            review_generation: 0,
            review_inflight: 0,
            review_since: None,
            review_armed: false,
            review_results: None,
            review_dismissed: HashSet::new(),
            review_sent: HashSet::new(),
            review_sent_disk: HashSet::new(),
            review_accept_all_undo: None,
            review_dismissed_store: None,
            review_edit_anchor: None,
            review_cursor_target: None,
            review_reload_note: None,
            marks_question: None,
            marks_preset: 0,
            marks_share: semantic_reading::marks::DEFAULT_SHARE,
            marks_prompt: false,
            marks_focus: Focus::default(),
            marks_lines: Vec::new(),
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
        self.review_inflight = 0;
        self.semantic_results = match source {
            // A session without `--semantic-cmd` never allocates a
            // channel — the layer stays exactly as cheap as before.
            Some(SemanticSource::Command(_)) => Some(AnalysisChannel::new()),
            _ => None,
        };
        // Review も同じ扱い。fixture のセッションは `R` が断るので線を
        // 張らない（[`App::review_enabled`]）。
        self.review_results = match source {
            Some(SemanticSource::Command(_)) => Some(ReviewChannel::new()),
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
    ///
    /// **`--semantic-cmd` は起点を越えるまで走らない**
    /// （[`App::semantic_armed`]）。開いただけの文書に 1 回分の解析費用を
    /// 払わないためで、起点は [`App::ask_marks`] が立てる。
    pub(crate) fn reanalyze_semantics(&mut self) {
        // **Review を先に蹴る。** この関数は文書が入れ替わるところ
        // （起動・reload・タイムマシン・ファイル切替）すべてから呼ばれる
        // が、下の早期 return は marks のための条件である。Review を
        // その後ろに置くと、reload のあとも古い候補が残って**別の文書の
        // バイト位置に下線が引かれる**。
        self.reanalyze_review();
        if self.semantic_source.is_none() {
            return;
        }
        // 遅延。ここで抜けるので世代も上がらない — 起点前は「解析を 1 度も
        // 頼んでいない」状態そのものである。fixture 経路（`Inline`）は
        // 外部プロセスもネットワークも無いので、遅らせる理由が無い。
        if !self.semantic_armed && matches!(self.semantic_source, Some(SemanticSource::Command(_)))
        {
            return;
        }
        // **問いが無ければ何も聞かない**。問いの無い要求は判定器が
        // 明確なエラーで断るし、そもそもこの投影では使えない。
        if self.marks_question.is_none()
            && matches!(self.semantic_source, Some(SemanticSource::Command(_)))
        {
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
                // ここへ来る時点で問いは必ずある（上の検査を通っている）。
                let question = self
                    .marks_question
                    .clone()
                    .expect("command analysis requires a question");
                let provider = provider.clone();
                let tx = channel.tx.clone();
                let source = self.source.content.clone();
                self.semantic_inflight = Some(generation);
                self.semantic_since = Some(Instant::now());
                std::thread::spawn(move || {
                    let result = provider
                        .analyze(&source, &question)
                        .map_err(|e| e.to_string());
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
                // **演出の起点はここ 1 つ**である。新しい問いも、
                // キャッシュに当たった答えも、文書を開き直したときも、
                // 答えが `semantic_doc` に入るのはこの行だけなので、
                // 「マーカーが引かれる」は必ず 1 回だけ走る。
                //
                // つまみ（`-` `+` `<` `>`）はここを通らない — 意図的で
                // ある。頻繁に打つ操作に 700 ms の演出を付けると邪魔に
                // なるし、「今回新しく光った箇所」を出すには装飾集合の
                // 差分が要って、つまみ 1 打の費用（いまは `marks::mark`
                // 1 回）が上がる。
                self.start_marks_reveal();
                // 読み出しも同じ瞬間に変わる（`analyzing…` → 本数）。
                self.start_readout_flash();
                None
            }
            Err(e) => Some(e),
        };
        self.refresh_semantic_decorations();
        if let Some(message) = refusal {
            self.flash_err(message);
        }
    }

    /// **読み出しが一瞬明るくなる演出を立てる**（`--no-fx` では何もしない）。
    ///
    /// 立つのは 3 か所 — 問いを決めた（[`Self::ask_marks`]）、つまみが
    /// 動いた（[`Self::nudge_marks_share`]）、答えが届いた
    /// （[`Self::accept_analysis`]）。**どれも読み出しの字が変わる瞬間**で、
    /// 変わらない操作（スクロール、選択）では立たない。
    pub(crate) fn start_readout_flash(&mut self) {
        if !self.config.fx {
            return;
        }
        self.readout_fx = Some(crate::effects::readout_flash_effect(
            self.decoration_styles.mark_tick(),
        ));
    }

    /// マーカーが引かれる演出を立てる。`--no-fx` のセッションでは何もしない。
    fn start_marks_reveal(&mut self) {
        if !self.config.fx {
            return;
        }
        self.marks_fx = Some(crate::effects::marks_reveal_effect(
            self.decoration_styles.mark_bg(),
            self.decoration_styles.mark_flash_bg(),
            self.decoration_styles.page_bg(),
        ));
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

    /// 注釈をいまのつまみへ投影する。
    ///
    /// **つまみを動かす費用はこれで全部である** — 手元の Unit に
    /// `marks::mark` を 1 回。解析も描画も provider も通らない。
    pub(crate) fn refresh_semantic_decorations(&mut self) {
        let Some(document) = self.semantic_doc.as_ref() else {
            self.semantic_decorations = Vec::new();
            self.marks_lines = Vec::new();
            return;
        };
        let marks = crate::semantic::marks_decorations_for(document, self.marks_share);
        // 行の台帳はここでだけ作る（装飾が変わった瞬間 = 目盛りとジャンプ先が
        // 変わった瞬間）。フレームごとに数え直さない。
        //
        // **描く列ではなく marks の投影から数える。** フォーカスは琥珀を
        // 描かないので、描く列から数えると目盛りも `]m` も消える。
        self.marks_lines = crate::semantic::marked_lines(&self.source.content, &marks);
        // フォーカス中だけ描き方が変わる。答えもつまみも同じままで、
        // 光った文と見出しのほかを沈め、琥珀は描かない
        // （`crate::semantic::focus_decorations_for`）。
        self.semantic_decorations = if self.marks_focus.is_on() {
            crate::semantic::focus_decorations_for(document, self.marks_share)
        } else {
            marks
        };
    }

    // ---- marks ---------------------------------------------------------
    //
    // `docs/design/semantic-reading-layer.md`。この層の投影はこれ 1 つで
    // ある（2026-09-22 に DIM 版を削除した）。

    /// **つまみを `delta` ポイント動かす**（1..=100 に丸める）。動いたら `true`。
    ///
    /// **下限は無い。** 0 本になるのは足切り（`marks::SCORE_FLOOR`）が
    /// 作るときだけで、それは「この問いに答えている箇所が無い」という
    /// 意味を持つ。
    ///
    /// ここから解析へ到達する経路は無い。つまみを動かす
    /// 費用は [`semantic_reading::marks::mark`] 1 回だけである。
    pub(crate) fn nudge_marks_share(&mut self, delta: i16) -> bool {
        let next = (self.marks_share as i16 + delta).clamp(
            semantic_reading::marks::MIN_SHARE as i16,
            semantic_reading::marks::MAX_SHARE as i16,
        ) as u8;
        if next == self.marks_share {
            return false;
        }
        self.marks_share = next;
        self.refresh_semantic_decorations();
        self.start_readout_flash();
        true
    }

    /// いま光っている Unit の数。注釈がなければ `None`。
    pub(crate) fn marks_lit(&self) -> Option<usize> {
        self.semantic_doc
            .as_ref()
            .map(|document| semantic_reading::marks::lit(document, self.marks_share))
    }

    /// 手元の注釈がスコアを持っているか。`Some(false)` は
    /// 「marks モードで開いたが、判定器がスコアを返していない」。
    pub(crate) fn marks_has_scores(&self) -> Option<bool> {
        self.semantic_doc
            .as_ref()
            .map(semantic_reading::marks::has_scores)
    }

    /// ステータス行に出す問いの名前。
    ///
    /// 自分で選んだ問いがあればその label。無くても、fixture が問いを
    /// 名乗っていればその id を出す（`--semantic <marks fixture>` の経路は
    /// 問いを選ばずに答えが手元にある）。**定型の label は id を大文字で
    /// 始めたものなので、両方の経路がほぼ同じ字面になる**（`Essential` と
    /// `essential`）。
    pub(crate) fn marks_question_label(&self) -> Option<&str> {
        if let Some(question) = self.marks_question.as_ref() {
            return Some(&question.label);
        }
        self.semantic_doc
            .as_ref()
            .and_then(|document| document.question.as_deref())
    }

    /// ステータス行に出す**表示用の**問いの名前。
    ///
    /// 定型はその label をそのまま（英語）。**自由入力だけは違う** —
    /// そこに入っているのは読み手が打った文字そのもの（たいてい日本語）で、
    /// ラベルではない。英語の並びに生で混ざると「また日本語が混ざった」に
    /// 見えるので、`Ask: …` の枠に入れて**`Ask` の引数**として読ませる
    /// （鉤括弧は日本語 UI の名残なので 2026-09-22 に外した。問いの本文は
    /// 読み手が打ったままである）。
    ///
    /// 枠を付けるのがここなのは、[`crate::marks_questions::Questions::free`]
    /// が作る `label` が**要求にも載る値**だからである（`Ask: …` を
    /// Jev 側へ送る理由は無い）。
    pub(crate) fn marks_question_display(&self) -> Option<String> {
        let question = self.marks_question.as_ref();
        if let Some(question) = question
            && question.id == "free"
        {
            return Some(format!("Ask: {}", question.label));
        }
        self.marks_question_label().map(str::to_string)
    }

    // ---- フォーカス（`f`） -------------------------------------------

    /// いま沈めているか（フッタの `FOCUS` バッジ）。
    pub(crate) fn focused(&self) -> bool {
        self.marks_focus.is_on()
    }

    /// 沈める先があるか — 光っている箇所が 1 つ以上ある。
    ///
    /// 0 本のときに沈めると**画面全部が沈む**（正しい答えではあるが、
    /// 読み手が頼んだのは「他を沈める」であって「全部沈める」ではない）。
    pub(crate) fn can_focus(&self) -> bool {
        self.marks_lit().is_some_and(|lit| lit > 0)
    }

    /// `f` の押下。画面が変わったら `true`。
    pub(crate) fn press_focus(&mut self, now: std::time::Instant) -> bool {
        if !self.marks_focus.press(now) {
            return false;
        }
        self.refresh_semantic_decorations();
        true
    }

    /// フォーカスを解く（Esc）。解いたら `true`。
    pub(crate) fn clear_focus(&mut self) -> bool {
        if !self.marks_focus.clear() {
            return false;
        }
        self.refresh_semantic_decorations();
        true
    }

    /// **フッタ右下に出す読み出しの候補**（広い順）— `Essential · 20% ` ＋
    /// 座布団の `13`。
    ///
    /// 縮む順は読み手の決定（2026-09-22）で、`20%` → 問いの名前 と落ち、
    /// **座布団の本数が最後まで残る**。いちばん小さくて意味がある値
    /// だからで、%は問いを覚えていれば足りるし、名前は `?` でも popup でも
    /// 確かめられる。どれも入らなければ呼び手が丸ごと引き下げる
    /// （[`crate::chrome::footer_layout`]）。
    ///
    /// 層の無いセッションでは空。**幅の計算はここにしない** — 収まるかを
    /// 決めるのはフッタの側である。
    pub(crate) fn marks_readouts(&self) -> Vec<Readout> {
        let state = |candidates: Vec<String>| -> Vec<Readout> {
            candidates
                .into_iter()
                .map(|dim| Readout { dim, lit: None })
                .collect()
        };
        let question = self.marks_question_display();
        if self.semantic_inflight.is_some() {
            // 解析中だけは**問いの名前から落とす**。まだ 1 本も光っていない
            // ので「なぜ光っているか」は無く、要るのは「待っている」の方。
            let asking = question.unwrap_or_else(|| "…".to_string());
            let busy = self.busy_text(self.semantic_since);
            return state(vec![format!("{asking} · {busy}"), busy]);
        }
        if self.marks_has_scores() == Some(false) {
            return state(vec![
                "no scores in this answer — the analyser returned none".to_string(),
                "no scores in this answer".to_string(),
                "no scores".to_string(),
            ]);
        }
        let (Some(question), Some(lit)) = (question, self.marks_lit()) else {
            return Vec::new();
        };
        // **末尾の空白 1 桁は薄い方に付ける**（読み手の注文、2026-09-22 の
        // 実機）。座布団の左の余白は琥珀なので、それだけだと `20%` と
        // 琥珀が地続きに見える。薄い空白を 1 桁挟むと離れる。
        // 本数だけまで縮んだ段（`dim` が空）には付けない — 前に何も無い。
        vec![
            Readout {
                dim: format!("{question} · {}% ", self.marks_share),
                lit: Some(lit),
            },
            Readout { dim: format!("{question} "), lit: Some(lit) },
            Readout { dim: String::new(), lit: Some(lit) },
        ]
    }

    /// `max_cols` に収まるいちばん広い候補。
    ///
    /// 描画は通らない — フッタは候補の列をそのまま受け取って、案内を
    /// 落とす所まで含めて配る（[`crate::chrome::footer_layout`]）。
    /// 読み出しを「1 つの値」として見たいテストの口である。
    #[cfg(test)]
    pub(crate) fn marks_readout(&self, max_cols: usize) -> Option<Readout> {
        self.marks_readouts()
            .into_iter()
            .find(|readout| readout.width() <= max_cols)
    }

    /// 問いを消せるか — [`Self::clear_marks_question`] が `true` を返す状態か。
    ///
    /// `Esc` の層の表（[`crate::esc`]）と右上のバッジ、mark for の `0 Off` の
    /// 行が同じ問いを読む。消えるものが無いのにバッジが消すと言うと嘘になる。
    pub(crate) fn can_clear_marks_question(&self) -> bool {
        !self.marks_question_is_fixed()
            && (self.marks_question.is_some() || self.semantic_doc.is_some())
    }

    /// **問いを消す**（Esc の marks の段・mark for の `0 Off`）。消したら `true`。
    ///
    /// マーカーも読み出しも消えて、層を渡す前の画面へ戻る。つまみ
    /// （`marks_share`）は残す — 次に問うたときに前の位置から見たい値で、
    /// 「問いを消す」は量の好みを消す話ではない。
    ///
    /// **走っている解析も切る。** 世代を 1 つ進めるので、あとから届く答えは
    /// [`Self::accept_analysis`] が古い世代として捨てる。切らないと、消した
    /// 数百 ms 後にマーカーが独りでに戻ってくる。
    ///
    /// fixture 経路（`--semantic <fixture.json>`）では**断る**。そこは問いを
    /// 選び直す道が無いので（`m` / `M` / `/` はどれも `false` を返す）、
    /// 消すと二度と戻せない。
    pub(crate) fn clear_marks_question(&mut self) -> bool {
        if !self.can_clear_marks_question() {
            return false;
        }
        self.marks_question = None;
        self.semantic_doc = None;
        self.semantic_generation += 1;
        self.semantic_inflight = None;
        self.refresh_semantic_decorations();
        true
    }

    /// **定型を次へ巡る**（`m`）。巡ったら `true`。
    ///
    /// marks モードの**遅延の起点**でもある。問いが決まった瞬間に、いま
    /// 開いている文書の解析が始まる（キャッシュに当たれば外部プロセスは
    /// 起きない。同じ (文書, 問い) は二度呼ばないので、環を一周して戻れば
    /// 0 円である）。
    ///
    /// fixture 経路では**断る**（`false`）。fixture の答えは 1 つの問いに
    /// 対するもので、問いを変えても答えは変わらない — 変わったふりを
    /// するより、変わらないと言うほうが正しい。
    /// **`step` は環の上の移動量である**（`+1` が次、`-1` が前）。
    ///
    /// 逆回りを「残り全部ぶん進む」で実装してはならない — 1 打ごとに
    /// [`Self::ask_marks`] が走り、**Jev を定型の数だけ呼ぶ**ことになる
    /// （議事録で 0.3 円が 0.9 円になり、スレッドが 3 本競合する）。
    /// 位置を先に決めて、解析は 1 度だけ頼む。
    pub(crate) fn cycle_marks_question(&mut self, step: isize) -> bool {
        if self.marks_question_is_fixed() {
            return false;
        }
        let Some(questions) = self.marks_questions.as_ref() else {
            return false;
        };
        let presets = questions.presets();
        if presets.is_empty() {
            return false;
        }
        let len = presets.len() as isize;
        // いま定型を見ているなら `step` ぶん動く。自由入力のあと（または
        // 最初）は、いまの位置から始める。
        let on_a_preset = self
            .marks_question
            .as_ref()
            .is_some_and(|question| presets.iter().any(|p| p.id == question.id));
        let from = self.marks_preset as isize;
        let next = if on_a_preset {
            (from + step).rem_euclid(len)
        } else {
            from.rem_euclid(len)
        } as usize;
        self.marks_preset = next;
        self.ask_marks(presets[next].clone());
        true
    }

    /// **定型を番号で聞く**（popup の Enter / `1`〜`9`）。聞いたら `true`。
    ///
    /// 巡る（[`Self::cycle_marks_question`]）との違いは**解析の回数**で
    /// ある。環を回ると 1 打ごとに Jev を呼ぶので、4 本めを見るのに 4 回
    /// 呼ぶ。popup は選んだ 1 本しか呼ばない（議事録で 0.2 円 × 3 の差）。
    ///
    /// 巡る位置（`marks_preset`）も動かす — popup で選んでから `M` を
    /// 押したら、その隣へ戻るのが素直である。
    pub(crate) fn ask_marks_preset(&mut self, index: usize) -> bool {
        if self.marks_question_is_fixed() {
            return false;
        }
        let Some(question) = self
            .marks_questions
            .as_ref()
            .and_then(|questions| questions.presets().get(index).cloned())
        else {
            return false;
        };
        self.marks_preset = index;
        self.ask_marks(question);
        true
    }

    /// **自由入力で聞く**（`/` の確定）。聞いたら `true`。
    ///
    /// 入力をそのまま Jev へ渡さない — 「日程」は主張ではないので Noul が
    /// 評価できない。「『<入力>』について述べている箇所である」の型に
    /// 差し込む（[`crate::marks_questions::Questions::free`]）。
    pub(crate) fn ask_marks_free(&mut self, input: &str) -> bool {
        let input = input.trim();
        if input.is_empty() || self.marks_question_is_fixed() {
            return false;
        }
        let Some(question) = self.marks_questions.as_ref().map(|q| q.free(input)) else {
            return false;
        };
        self.ask_marks(question);
        true
    }

    /// 問いを載せて解析を頼む。**ここが marks モードの起点である。**
    fn ask_marks(&mut self, question: Question) {
        self.marks_question = Some(question);
        // 読み出しの字がここで変わる（問いの名前 → `analyzing…`）。
        self.start_readout_flash();
        // **遅延の起点はここ 1 つである。** `--semantic-cmd` は開いた
        // だけでは走らない（`App::semantic_armed`）。
        self.semantic_armed = true;
        self.reanalyze_semantics();
    }

    /// fixture の答えを見ているか — 問いを変えられない経路か。
    ///
    /// `--semantic <fixture.json>` は 1 つの問いへの答えを固定で持っている。
    pub(crate) fn marks_question_is_fixed(&self) -> bool {
        matches!(self.semantic_source, Some(SemanticSource::Inline(_)))
    }

    // ---- Review（`R`）— 校正候補 ----------------------------------
    //
    // `docs/design/marks-only-and-review-mode.md` 4 節。marks と共有して
    // いるのは**判定器の一段の問い・Unit・境界・sha キャッシュ**だけで、
    // ここから marks の状態へ書く経路は 1 本も無い。

    /// `R` が使えるか — 候補の出どころが 1 本でもあるか。
    ///
    /// 出どころは 2 つ: `--lint-cmd`（[`Self::lint`]）と、`--semantic-cmd`
    /// の上で有効な Jev のルール（[`Self::review_rules_enabled`]）。
    /// どちらも無ければ `R` は何をすれば候補が出るかを言って断る。
    pub(crate) fn review_enabled(&self) -> bool {
        self.lint.is_some() || self.review_rules_enabled()
    }

    /// `R` のキーが生きているか — 意味層か `--lint-cmd` のどちらかがあるか。
    ///
    /// [`Self::review_enabled`] より広い: 意味層はあるが有効なルールが
    /// 無いときも `R` は届き、何をすれば候補が出るかを言って断る。
    pub(crate) fn review_key_live(&self) -> bool {
        self.semantic_enabled() || self.lint.is_some()
    }

    /// 走らせる Jev のルールがあるか — 外部コマンドと、有効なルールが
    /// 揃っているか。
    ///
    /// `--semantic <fixture>` では走らない。fixture は 1 つの問いへの
    /// 固定の答えで、ルールの文面で聞き直す道が無いからである
    /// （[`Self::marks_question_is_fixed`] と同じ理由）。
    pub(crate) fn review_rules_enabled(&self) -> bool {
        matches!(self.semantic_source, Some(SemanticSource::Command(_)))
            && self
                .review_rules
                .as_ref()
                .is_some_and(|rules| rules.enabled().next().is_some())
    }

    /// **`R` の押下。** 遅延の起点である。
    ///
    /// 初回は有効なルールの解析が走る（marks の遅延とまったく同じ理由で、
    /// 開いただけの文書に費用を払わない）。2 度目からは同じ (文書, 文面)
    /// がキャッシュに当たって 0 円で、marks が同じ文面を聞いていれば
    /// その答えをそのまま使う。
    pub(crate) fn arm_review(&mut self) {
        if !self.review_enabled() {
            return;
        }
        let first = !self.review_armed;
        self.review_armed = true;
        // 2 度目以降で候補が手元にあるなら、聞き直さない（答えは同じで、
        // accept / dismiss の状態を捨てるだけになる）。
        if first || (self.review_candidates.is_empty() && self.review_inflight == 0) {
            self.reanalyze_review();
        }
    }

    /// 画面に Review の結果（下線・`!`・一覧の中身・解析中の印）があるか。
    ///
    /// [`Self::clear_review`] が消すものがあるか、と同じ問いである。
    pub(crate) fn review_shown(&self) -> bool {
        self.review_armed
            && (!self.review_candidates.is_empty()
                || self.review_inflight > 0
                || self.review_lint_error.is_some())
    }

    /// **Review の結果を画面から消す**（Esc）。消したら `true`。
    ///
    /// 消すのは**画面の候補だけ**である。dismiss の記録（[`DismissedStore`]）・
    /// accept で作ったコメント・判定器のキャッシュは触らない — もう一度 `R` を
    /// 押せば同じ候補がまた出る（キャッシュに当たれば速い）。accept 済みの
    /// 印はコメントから、捨てた候補は記録から、そのとき復元される。
    ///
    /// **起点も戻す**（[`Self::review_armed`]）。戻さないと、次の reload の
    /// [`Self::reanalyze_review`] が頼んでもいない候補を描き直す。走っている
    /// 解析は世代を進めて切る（遅れて届く答えは古い世代として捨てられる）。
    pub(crate) fn clear_review(&mut self) -> bool {
        if !self.review_shown() {
            return false;
        }
        self.review_armed = false;
        self.review_generation += 1;
        self.review_candidates.clear();
        self.review_inflight = 0;
        self.review_lint_error = None;
        self.review_edit_anchor = None;
        self.review_cursor_target = None;
        if self.overlay == Some(crate::overlay::Overlay::Review) {
            self.overlay = None;
        }
        true
    }

    /// 有効なルールごとに、いま画面にある文書を判定器へ渡す。
    ///
    /// **起点を越えるまで走らない**（[`Self::review_armed`]）。走るときは
    /// 候補も下線も一度空にする — 候補は前の文書のバイト位置を指している
    /// ので、残すと別の本文に下線が乗る。
    ///
    /// `--semantic-cmd` は同じ問いを二度呼ばないので、一周して戻れば
    /// 0 円である（キャッシュの鍵は (コマンド行, 文書, 文面)）。
    pub(crate) fn reanalyze_review(&mut self) {
        if !self.review_armed || !self.review_enabled() {
            return;
        }
        // provider は clone して先に手放す（下で `&mut self` を取る）。
        let provider = match self.semantic_source.as_ref() {
            Some(SemanticSource::Command(provider)) if self.review_rules_enabled() => {
                Some(provider.clone())
            }
            _ => None,
        };
        let rules: Vec<crate::review_rules::Rule> = match provider {
            Some(_) => self
                .review_rules
                .as_ref()
                .map(|rules| rules.enabled().cloned().collect())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let lint = self.lint.clone();
        // 線は lint だけのセッション（意味層が無い）でもここで張る。
        let channel = self.review_results.get_or_insert_with(ReviewChannel::new);
        let tx = channel.tx.clone();
        self.review_generation += 1;
        self.review_candidates.clear();
        self.review_inflight = 0;
        self.review_lint_error = None;
        // 一覧が開いたままなら、カーソルを先頭へ戻す。古い添字のままだと、
        // 新しい答えが届いたとき見てもいない候補を指している。
        if self.overlay == Some(crate::overlay::Overlay::Review) {
            self.overlay_cursor = 0;
            self.overlay_offset = 0;
        }
        if rules.is_empty() && lint.is_none() {
            return;
        }
        // 捨てた記録はここで 1 度だけ読む（文書 1 つにつき 1 回）。
        self.load_dismissed();
        let generation = self.review_generation;
        self.review_inflight = rules.len() + usize::from(lint.is_some());
        self.review_since = Some(Instant::now());
        if let Some(provider) = provider {
            for rule in rules {
                let provider = provider.clone();
                let tx = tx.clone();
                let source = self.source.content.clone();
                let question = rule.question();
                std::thread::spawn(move || {
                    let result = provider
                        .analyze(&source, &question)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(ReviewMessage {
                        generation,
                        answer: ReviewAnswer::Rule {
                            rule: question.id,
                            result,
                        },
                    });
                });
            }
        }
        // linter はディスクのファイルを読む。画面の本文と食い違えば
        // [`crate::lint::LintCommand::run`] が断る。
        if let Some(lint) = lint {
            let path = self.current_file_path().to_path_buf();
            let source = self.source.content.clone();
            std::thread::spawn(move || {
                let result = lint.run(&path, &source);
                let _ = tx.send(ReviewMessage {
                    generation,
                    answer: ReviewAnswer::Lint(result),
                });
            });
        }
    }

    /// いま画面にある文書について捨てられた候補を読み直す。
    ///
    /// **文書 1 つにつき 1 回**（[`Self::reanalyze_review`] の先頭）。
    /// 毎回 JSONL を舐めると、候補が届くたびにファイルを読むことになる。
    pub(crate) fn load_dismissed(&mut self) {
        let sha = crate::semantic::source_digest(&self.source.content);
        self.review_dismissed = self
            .review_dismissed_store
            .as_ref()
            .map(|store| store.load(&sha))
            .unwrap_or_default();
        // 送った記録も同じ時に（同じ置き場・同じ sha で）。
        self.review_sent_disk = self
            .review_dismissed_store
            .as_ref()
            .map(|store| store.sent().load(&sha))
            .unwrap_or_default();
    }

    /// Review のワーカーが終えたぶんを引き取る。イベントループから 1 tick
    /// に 1 度。`--semantic-cmd` の無いセッションでは何もしない。
    pub(crate) fn poll_review_analysis(&mut self) {
        loop {
            let received = match self.review_results.as_ref() {
                Some(channel) => channel.rx.try_recv(),
                None => return,
            };
            match received {
                Ok(message) => self.accept_review_analysis(message),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    /// 1 ルール分の答えを取る — **または古い世代として捨てる。**
    ///
    /// **捨てた候補（`dismissed.jsonl`）も落とさない。** 一覧に薄く `–` で
    /// 残り（文書順の位置のまま）、`x` で戻せる。本文には下線も印も出ない
    /// — 状態は [`Self::candidate_states`] が記録から導く。
    pub(crate) fn accept_review_analysis(&mut self, message: ReviewMessage) {
        if message.generation != self.review_generation {
            return; // 古い世代の答え。捨てる。
        }
        self.review_inflight = self.review_inflight.saturating_sub(1);
        let found: Vec<Candidate> = match message.answer {
            ReviewAnswer::Rule { rule, result } => {
                let rule = self
                    .review_rules
                    .as_ref()
                    .and_then(|rules| rules.get(&rule).cloned());
                match (result, rule) {
                    (Ok(document), Some(rule)) => {
                        crate::review::candidates_for(&document, &rule, &self.source.content)
                    }
                    (Err(e), _) => {
                        self.flash_err(e);
                        return;
                    }
                    // ルールが消えている（読み直された）。答えの置き場が無い。
                    (Ok(_), None) => return,
                }
            }
            ReviewAnswer::Lint(Ok(diagnostics)) => {
                crate::lint::candidates(diagnostics, &self.source.content)
            }
            ReviewAnswer::Lint(Err(e)) => {
                // **「0 件」と見せない。** 一覧は理由を出し、フラッシュも同じ 1 行。
                self.review_lint_error = Some(e.clone());
                self.flash_err(e);
                return;
            }
        };
        let fresh = found;
        // 一覧が開いていれば、カーソルの下の候補を覚えておく。
        // 並べ替えで前に候補が差し込まれると、添字のままの
        // カーソルは別の候補を指し、`a` / `x` が見ていない行に
        // 効く（ルールが 2 本以上のときに報告されたずれ）。
        let under_cursor = (self.overlay == Some(crate::overlay::Overlay::Review))
            .then(|| self.review_candidates.get(self.overlay_cursor))
            .flatten()
            .map(|c| (c.rule.clone(), c.range.clone()));
        let before = under_cursor.clone();
        self.review_candidates.extend(fresh);
        // **文書順**に並べ直す。出どころは 1 本ずつ別のスレッドで
        // 返ってくるので、届いた順に足すと順序が run ごとに変わる。
        // Jev のルールと lint の候補も、ここで同じ一覧に文書順で混ざる。
        //
        // **文書全体を範囲にする指摘は末尾へ**（[`Candidate::is_whole_document`]）。
        // 先頭に置くと一覧を開いた最初の候補がいつも総評になり、本文の
        // どこも指さない候補から読み始めることになる。
        let total_lines = self.source.len();
        self.review_candidates.sort_by(|a, b| {
            a.is_whole_document(total_lines)
                .cmp(&b.is_whole_document(total_lines))
                .then(a.range.start.cmp(&b.range.start))
                .then(a.range.end.cmp(&b.range.end))
                .then(a.rule.cmp(&b.rule))
        });
        if let Some((rule, range)) = under_cursor
            && let Some(index) = self
                .review_candidates
                .iter()
                .position(|c| c.rule == rule && c.range == range)
        {
            self.overlay_cursor = index;
            crate::overlay::keep_overlay_cursor_visible(self);
        }
        // 一覧から直しに行って戻ったところなら、答えが**揃った**
        // ときに、直した箇所（か、その次）へカーソルを置く。
        if self.review_inflight == 0
            && let Some(target) = self.review_cursor_target.take()
            && self.overlay == Some(crate::overlay::Overlay::Review)
            && !self.review_candidates.is_empty()
        {
            // 直した行より上から始まる候補は後回しにする。linter には文書
            // 全体を範囲にする指摘があり（文書の品質の総評など）、それが
            // 「直した位置を覆う最初の候補」として毎回カーソルを先頭へ攫う。
            let line_start = self
                .source
                .content
                .get(..target)
                .and_then(|head| head.rfind('\n'))
                .map_or(0, |at| at + 1);
            let covers = |c: &Candidate| c.range.end > target;
            self.overlay_cursor = self
                .review_candidates
                .iter()
                .position(|c| covers(c) && c.range.start >= line_start)
                .or_else(|| self.review_candidates.iter().position(covers))
                .unwrap_or(self.review_candidates.len() - 1);
            crate::overlay::keep_overlay_cursor_visible(self);
        }
        // 一覧のカーソルの下の候補が変わったら（最初の答え・直したあとの
        // 着地）、本文もそこへ送る。同じ候補のままなら本文は動かさない —
        // 2 本目の答えが届くたびに、読んでいた場所から引き戻さない。
        let after = (self.overlay == Some(crate::overlay::Overlay::Review))
            .then(|| self.review_candidates.get(self.overlay_cursor))
            .flatten()
            .map(|c| (c.rule.clone(), c.range.clone()));
        if after.is_some() && after != before {
            crate::review_dock::follow(self);
        }
        self.start_readout_flash();
    }

    /// 候補の下線。**Pending だけが下線を持つ。**
    ///
    /// Accepted はコメントの印（黄色のバー）に変わり、Dismissed は本文
    /// から消える。下線は「まだ見ていない」という意味なので、見たものに
    /// 残っていると一覧を往復するたびに同じ場所を読み直すことになる。
    ///
    /// **持たずに毎回導く**（[`Self::active_decorations`] がフレームごとに
    /// 呼ぶ）。状態がコメントから決まるので、下線を取っておくと本文の
    /// `d` や `s` のたびに作り直しを忘れる経路が残る。
    ///
    /// **文書全体を範囲にする指摘は引かない**（[`Candidate::is_whole_document`]）。
    /// 全文に線が乗ると、本文のどの指摘の線も見分けられなくなる。
    ///
    /// 色は候補の重さ（[`Candidate::severity`]）で分かれる。
    pub(crate) fn review_decorations(&self) -> Vec<Decoration> {
        let total_lines = self.source.len();
        let states = self.candidate_states();
        self.review_candidates
            .iter()
            .zip(states)
            .filter(|(c, state)| state.is_pending() && !c.is_whole_document(total_lines))
            .flat_map(|(c, _)| {
                let kind = crate::decoration::DecorationKind::ReviewCandidate(c.severity());
                c.atoms.iter().cloned().map(move |range| Decoration { range, kind })
            })
            .collect()
    }

    /// 候補の乗っているソース行（0 始まり）— ガターの白抜きの印と、その色の
    /// 重さ（行に乗る候補のうち**いちばん重いもの**）。
    ///
    /// Pending だけを数え、文書全体を範囲にする指摘は数えない（下線と
    /// 同じ理由）。
    pub(crate) fn review_lines(&self) -> Vec<Option<crate::decoration::ReviewSeverity>> {
        let total_lines = self.source.len();
        let mut flags = vec![None; total_lines];
        let states = self.candidate_states();
        for (candidate, _) in self
            .review_candidates
            .iter()
            .zip(states)
            .filter(|(c, state)| state.is_pending() && !c.is_whole_document(total_lines))
        {
            let start = (candidate.lines.0.saturating_sub(1)) as usize;
            let end = (candidate.lines.1.saturating_sub(1)) as usize;
            let severity = candidate.severity();
            for flag in flags.iter_mut().take(end + 1).skip(start) {
                *flag = (*flag).max(Some(severity));
            }
        }
        flags
    }

    /// 候補ごとの状態（`review_candidates` と同じ順・同じ長さ）。
    ///
    /// **状態は候補が持たず、ここで毎回導く**（[`CandidateState`] の表）:
    /// 捨てた記録に載っていれば `Dismissed`、この候補のコメントが今あれば
    /// `Accepted`、送っていれば `Sent`、どれでもなければ `Pending`。
    /// コメントが消える経路（本文の `d`・`l` の一覧の `d`・`s`・reload の
    /// 片付け）のどれを通っても、候補は何もしなくても Pending（か Sent）に
    /// 戻る。
    pub(crate) fn candidate_states(&self) -> Vec<CandidateState> {
        if self.review_candidates.is_empty() {
            return Vec::new();
        }
        let accepted = self.live_review_comments();
        let file = self.current_file_path().to_path_buf();
        let revision = self.current_revision_context();
        let sent: HashSet<&CandidateSpot> = self
            .review_sent
            .iter()
            .filter(|(path, rev, _)| {
                *path == file && crate::history::same_revision(rev.as_deref(), revision.as_deref())
            })
            .map(|(_, _, spot)| spot)
            .collect();
        self.review_candidates
            .iter()
            .map(|c| {
                if self
                    .review_dismissed
                    .contains(&(c.rule.clone(), c.range.start, c.range.end))
                {
                    CandidateState::Dismissed
                } else if CandidateSpot::of_candidate(c).any(|spot| accepted.contains(&spot)) {
                    CandidateState::Accepted
                } else if CandidateSpot::of_candidate(c).any(|spot| sent.contains(&spot))
                    || self
                        .review_sent_disk
                        .contains(&(c.rule.clone(), c.range.start, c.range.end))
                {
                    CandidateState::Sent
                } else {
                    CandidateState::Pending
                }
            })
            .collect()
    }

    /// 1 本の候補の状態（[`Self::candidate_states`]）。添字の外は `None`。
    pub(crate) fn candidate_state(&self, index: usize) -> Option<CandidateState> {
        self.candidate_states().get(index).copied()
    }

    /// **`a` — 切り替え。** Pending / Dismissed → Accepted、Accepted → Pending。
    ///
    /// どの状態からも**押したキーの状態へ直接行く**: Dismissed で押せば、
    /// 捨てたのを取り消してからコメントを作る。送った候補（Sent）には
    /// 何もしない — 同じ指示を二度送らせない。
    pub(crate) fn toggle_accept(&mut self, index: usize) -> ReviewToggle {
        match self.candidate_state(index) {
            None => ReviewToggle::Nothing,
            Some(CandidateState::Sent) => ReviewToggle::AlreadySent,
            Some(CandidateState::Accepted) => {
                self.remove_candidate_comments(index);
                ReviewToggle::Unaccepted
            }
            Some(CandidateState::Dismissed) => {
                self.restore_candidate(index);
                self.accept_candidate(index);
                ReviewToggle::Accepted
            }
            Some(CandidateState::Pending) => {
                self.accept_candidate(index);
                ReviewToggle::Accepted
            }
        }
    }

    /// **`x` — 切り替え。** Pending / Accepted → Dismissed、Dismissed → Pending。
    ///
    /// Accepted で押せば、コメントを消してから捨てる。送った候補（Sent）には
    /// 何もしない（捨てても、指示はもう相手に届いている）。
    pub(crate) fn toggle_dismiss(&mut self, index: usize) -> ReviewToggle {
        match self.candidate_state(index) {
            None => ReviewToggle::Nothing,
            Some(CandidateState::Sent) => ReviewToggle::AlreadySent,
            Some(CandidateState::Dismissed) => {
                self.restore_candidate(index);
                ReviewToggle::Restored
            }
            Some(CandidateState::Accepted) => {
                self.remove_candidate_comments(index);
                self.dismiss_candidate(index);
                ReviewToggle::Dismissed
            }
            Some(CandidateState::Pending) => {
                self.dismiss_candidate(index);
                ReviewToggle::Dismissed
            }
        }
    }

    /// **候補をコメントにする**（`a` の片道）。作ったら `true`。
    ///
    /// コメントは既存の [`Comment`] そのもので、以降は `l` の一覧・
    /// `y copy`・`s send` にそのまま乗る。**コメント ＝ 人が承認した印**
    /// である（4 節の流れの 3）。本文の形は
    /// [`crate::review::comment_text`] の 1 か所にある。
    ///
    /// Pending のときだけ作る（2 本目を作らない・捨てた候補・送った候補は
    /// 作らない）。切り替えは [`Self::toggle_accept`]。
    pub(crate) fn accept_candidate(&mut self, index: usize) -> bool {
        if self.candidate_state(index) != Some(CandidateState::Pending) {
            return false;
        }
        let candidate = &self.review_candidates[index];
        let (start, end) = candidate.lines;
        let text = candidate.comment_text();
        // 本文は該当行そのもの（コメントの `lines` は常に本文の写しで、
        // 候補の範囲ではない — 送り先が行で照合するため）。
        let lines = self
            .source
            .content
            .lines()
            .skip(start.saturating_sub(1) as usize)
            .take((end.saturating_sub(start) + 1) as usize)
            .collect::<Vec<_>>()
            .join("\n");
        let comment = Comment {
            file_path: self.current_file_path().to_path_buf(),
            start,
            end,
            lines,
            revision: self.current_revision_context(),
            anchor: Some(crate::comment::ReviewAnchor::new(
                candidate.rule.clone(),
                candidate.range.clone(),
                &self.source.content,
            )),
            text,
        };
        self.comments.push(comment);
        true
    }

    /// この候補のコメント（いまの版の、同じルール・同じ行の review / lint
    /// コメント）を消す。消した本数を返す。**人の赤入れは触らない。**
    fn remove_candidate_comments(&mut self, index: usize) -> usize {
        let Some(candidate) = self.review_candidates.get(index) else {
            return 0;
        };
        let spots: Vec<CandidateSpot> = CandidateSpot::of_candidate(candidate).collect();
        let current = self.current_file_path().to_path_buf();
        let revision = self.current_revision_context();
        let before = self.comments.len();
        self.comments.retain(|c| {
            !(c.file_path == current
                && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
                && CandidateSpot::of_comment(c).is_some_and(|spot| spots.contains(&spot)))
        });
        before - self.comments.len()
    }

    /// いまの版に生きている review コメントの結び目（[`CandidateSpot`]）。
    fn live_review_comments(&self) -> HashSet<CandidateSpot> {
        let current = self.current_file_path();
        let revision = self.current_revision_context();
        self.comments
            .iter()
            .filter(|c| {
                c.file_path == *current
                    && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
            })
            .filter_map(CandidateSpot::of_comment)
            .collect()
    }

    /// **`s` で送る直前に呼ぶ** — いまある review / lint コメントを「送った」
    /// として覚える。送ったあとコメントは消えるが、その候補は `✓` のまま
    /// （[`CandidateState::Sent`]）で、`a` は二度目を作らない。
    ///
    /// 人の赤入れは覚えない（候補が無い）。ファイルごと・版ごとに持つので、
    /// ほかのファイルのコメントを一緒に送っても混ざらない。
    pub(crate) fn note_review_sent(&mut self) {
        let sent: Vec<ReviewCommentKey> = self
            .comments
            .iter()
            .filter_map(|c| {
                let spot = CandidateSpot::of_comment(c)?;
                Some((c.file_path.clone(), c.revision.clone(), spot))
            })
            .collect();
        self.review_sent.extend(sent);
        self.persist_review_sent();
    }

    /// 送った review / lint コメントのうち、**いまのファイルの NOW に結び目で
    /// 結ばれたもの**を `sent.jsonl` へ追記する。
    ///
    /// ほかのファイル・過去の版のコメントは書かない — 鍵になる文書の sha が
    /// 手元に無い（画面にあるのは今のファイルの NOW だけ）。結び目の無い
    /// コメントも書かない（範囲が無い）。書けなくても送信は成功のまま
    /// （[`Self::dismiss_candidate`] と同じ扱い）。
    fn persist_review_sent(&mut self) {
        let Some(store) = self.review_dismissed_store.as_ref().map(DismissedStore::sent) else {
            return;
        };
        let current = self.current_file_path().to_path_buf();
        let sha = crate::semantic::source_digest(&self.source.content);
        for comment in &self.comments {
            let Some(anchor) = comment.anchor.as_ref() else {
                continue;
            };
            if comment.file_path != current
                || comment.revision.is_some()
                || crate::review::candidate_comment_rule(&comment.text).is_none()
            {
                continue;
            }
            let _ = store.append(&sha, &anchor.rule, &anchor.range);
        }
    }

    /// **送った印を忘れる** — いまのファイルの NOW が書き換わったとき
    /// （[`crate::reload::reload_source`]）。送った相手が直したかどうかは、
    /// 新しい版で候補が出るかどうかで決め直す（出れば Pending）。
    pub(crate) fn forget_review_sent(&mut self) {
        let current = self.current_file_path().to_path_buf();
        self.review_sent
            .retain(|(path, revision, _)| !(*path == current && revision.is_none()));
        // ディスクの記録は消さない（sha で鍵をかけてあるので、書き換わった
        // 版には効かない）。読んだぶんだけを捨てる。
        self.review_sent_disk.clear();
    }

    /// **見届けた reload で、いまの版の review コメントを片付ける。**
    /// 外した本数を返す（4 節「直接編集」）。
    ///
    /// 範囲の中が 1 バイトでも変わったコメントは外す — 直した箇所への
    /// 指示を `s` で LLM に届けないためである。変わっていないものは
    /// `new_content` の新しい位置へ付け直し、NOW のコメントのまま残す。
    /// **人の書いた赤入れは触らない**（[`crate::review::parse_comment_text`]
    /// が形で見分ける。留め置きは呼び出し側の今の挙動のまま）。
    ///
    /// 範囲は、accept した候補がまだ手元にあればその Unit のバイト範囲、
    /// 無ければコメントの行そのものである。Unit の範囲の方が狭いので、
    /// 同じ行の別の文を直しただけなら外れない。
    pub(crate) fn settle_review_comments(
        &mut self,
        map: &crate::edit_map::EditMap,
        new_content: &str,
    ) -> usize {
        let current = self.current_file_path().to_path_buf();
        let old_starts = tui_markdown::line_starts(&self.source.content);
        let new_starts = tui_markdown::line_starts(new_content);
        let old_len = self.source.content.len();
        let mut resolved = 0;
        let mut kept = Vec::with_capacity(self.comments.len());
        for mut comment in std::mem::take(&mut self.comments) {
            let rule = match crate::review::candidate_comment_rule(&comment.text) {
                Some(rule) if comment.file_path == current && comment.revision.is_none() => rule,
                _ => {
                    kept.push(comment);
                    continue;
                }
            };
            // 結び目があればその範囲。無ければ（結び目の無い古いコメント）、
            // 同じルール・同じ行の候補の範囲か、行そのもの。
            let range = comment
                .anchor
                .as_ref()
                .map(|anchor| anchor.range.clone())
                .or_else(|| {
                    self.review_candidates
                        .iter()
                        .find(|c| c.rule == rule && c.lines == (comment.start, comment.end))
                        .map(|c| c.range.clone())
                })
                .unwrap_or_else(|| {
                    // 行の頭から、末尾の行の終わり（改行の手前）まで。
                    let line = |n: u32| n.saturating_sub(1) as usize;
                    let start = old_starts.get(line(comment.start)).copied().unwrap_or(old_len);
                    let end = old_starts
                        .get(line(comment.end) + 1)
                        .map_or(old_len, |next| next.saturating_sub(1));
                    start..end.max(start)
                });
            let Some(moved) = map.map_range(&range) else {
                resolved += 1;
                continue;
            };
            let (start, end) = crate::review::lines_at(&new_starts, &moved);
            comment.start = start;
            comment.end = end;
            comment.lines = new_content
                .lines()
                .skip(start.saturating_sub(1) as usize)
                .take((end.saturating_sub(start) + 1) as usize)
                .collect::<Vec<_>>()
                .join("\n");
            // 結び目も同じ差分で写す（位置と抜粋は新しい版で作り直す）。
            if let Some(anchor) = comment.anchor.take() {
                comment.anchor = Some(crate::comment::ReviewAnchor::new(
                    anchor.rule,
                    moved.clone(),
                    new_content,
                ));
            }
            kept.push(comment);
        }
        self.comments = kept;
        resolved
    }

    /// **候補を捨てる**（`x` の片道）。捨てたら `true`。
    ///
    /// セッション内は印が変わるだけ（一覧からは消えない）で、本文からは
    /// 下線が消える。加えて `dismissed.jsonl` に追記し、同じ文書を開いても
    /// 一覧に薄く `–` で出る。**書けなくても捨てる** — ディスクが一杯でも
    /// 画面の操作は通る（次に開くと戻ってくるだけ）。
    ///
    /// Pending のときだけ捨てる。切り替えは [`Self::toggle_dismiss`]。
    pub(crate) fn dismiss_candidate(&mut self, index: usize) -> bool {
        if self.candidate_state(index) != Some(CandidateState::Pending) {
            return false;
        }
        let candidate = &self.review_candidates[index];
        let sha = crate::semantic::source_digest(&self.source.content);
        if let Some(store) = self.review_dismissed_store.as_ref() {
            let _ = store.append(&sha, candidate);
        }
        let key = (
            candidate.rule.clone(),
            candidate.range.start,
            candidate.range.end,
        );
        self.review_dismissed.insert(key);
        true
    }

    /// **捨てたのを取り消す**（Dismissed で `x`）。取り消したら `true`。
    ///
    /// 記録は**追記**で取り消す（`"undo": true` の行 — 同じ鍵は最後の行が
    /// 勝つ）。書けなくても画面では戻す（[`Self::dismiss_candidate`] と同じ）。
    pub(crate) fn restore_candidate(&mut self, index: usize) -> bool {
        let Some(candidate) = self.review_candidates.get(index) else {
            return false;
        };
        let key = (
            candidate.rule.clone(),
            candidate.range.start,
            candidate.range.end,
        );
        if !self.review_dismissed.remove(&key) {
            return false;
        }
        let sha = crate::semantic::source_digest(&self.source.content);
        if let Some(store) = self.review_dismissed_store.as_ref() {
            let _ = store.append_undo(&sha, candidate);
        }
        true
    }

    /// **`A` — Pending を全部 accept する。** 作ったコメントの数を返す。
    ///
    /// 捨てた候補・送った候補・accept 済みには触らない。1 本でも作ったら、
    /// 作ったぶんの結び目を [`Self::review_accept_all_undo`] に覚える —
    /// **直後にもう一度 `A`** を押せばまとめて戻る（`a` / `x` の「同じキーを
    /// もう一度押せば戻る」と揃えてある）。
    pub(crate) fn accept_all_pending(&mut self) -> usize {
        let pending: Vec<usize> = self
            .candidate_states()
            .iter()
            .enumerate()
            .filter(|(_, state)| state.is_pending())
            .map(|(i, _)| i)
            .collect();
        let mut made = Vec::new();
        for index in pending {
            if self.accept_candidate(index) {
                let c = &self.review_candidates[index];
                made.push((c.rule.clone(), c.range.start, c.range.end));
            }
        }
        let count = made.len();
        self.review_accept_all_undo = (count > 0).then_some(made);
        count
    }

    /// **`A` の直後の `A`** — その `A` で作ったコメントだけを消し、候補を
    /// Pending に戻す。消した本数を返す。
    ///
    /// 消すのは、いまのファイル・いまの版で、結び目が `made` にある review /
    /// lint コメントだけ。`A` より前に `a` で作ったコメントと人の赤入れは
    /// 触らない。
    pub(crate) fn undo_accept_all(&mut self, made: &[DismissedKey]) -> usize {
        let current = self.current_file_path().to_path_buf();
        let revision = self.current_revision_context();
        let before = self.comments.len();
        self.comments.retain(|c| {
            let ours = c.file_path == current
                && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
                && crate::review::candidate_comment_rule(&c.text).is_some()
                && c.anchor.as_ref().is_some_and(|a| {
                    made.contains(&(a.rule.clone(), a.range.start, a.range.end))
                });
            !ours
        });
        before - self.comments.len()
    }

    /// フッタの `A` の案内 — 戻せるあいだ（[`Self::review_accept_all_undo`]）は
    /// `A undo all`、それ以外は `A accept all`。
    pub(crate) fn accept_all_hint(&self) -> &'static str {
        if self.review_accept_all_undo.is_some() {
            "A undo all"
        } else {
            "A accept all"
        }
    }

    /// **見た本数と一覧の行数**（読み出しの `3/9`、一覧の題の `(3/9)`）。
    ///
    /// 分母は一覧の行の全部 — 捨てた候補も一覧に残るので数える。分子は
    /// Pending でない行（`✓` の accept 済み・送った候補と、`–` の捨てた候補）。
    /// `9/9` は「残っている候補が無い」である。
    pub(crate) fn review_counts(&self) -> (usize, usize) {
        let decided = self
            .candidate_states()
            .iter()
            .filter(|state| !state.is_pending())
            .count();
        (decided, self.review_candidates.len())
    }

    /// **フッタの読み出しの候補**（広い順）— marks の読み出しの**左**に
    /// 出る。
    ///
    /// 縮む順は marks の後である（読み手の決定、2026-09-23）: 狭くなったら
    /// **Review が先に引き下がる**。marks はいま光っている箇所の本数で、
    /// 画面に見えているものの説明になっているが、Review は一覧を開けば
    /// 同じ数が読める。
    pub(crate) fn review_readouts(&self) -> Vec<String> {
        if !self.review_armed {
            return Vec::new();
        }
        if self.review_inflight > 0 {
            return vec![format!("Review · {}", self.busy_text(self.review_since))];
        }
        let (decided, total) = self.review_counts();
        if total == 0 {
            return Vec::new();
        }
        vec![format!("Review · {decided}/{total}")]
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
        // **Review の下線は最後に足す。** 下線は前景も地色も書かないので
        // （`crate::decoration::DecorationStyles::patch`）、marks の琥珀と
        // 同じ range に乗っても打ち消し合わない — 後勝ちになるのは同じ
        // ものを書く kind 同士だけである。Review と marks は同時に出る。
        let review = self.review_decorations();
        if self.semantic_decorations.is_empty()
            && review.is_empty()
            && self.config.decorations.is_empty()
        {
            return Vec::new();
        }
        let mut all = self.config.decorations.clone();
        all.extend_from_slice(&self.semantic_decorations);
        all.extend(review);
        crate::decoration::sanitize(&all, &self.source.content)
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

    /// 行 `idx`（表示中の文書の行）の行内の強調: 書き換えペアの新側で、変わった
    /// バイト範囲。強調の無い行は空。
    pub(crate) fn new_line_emphasis(&self, idx: usize) -> &[std::ops::Range<usize>] {
        self.comparison_deleted_blocks
            .iter()
            .find_map(|b| {
                idx.checked_sub(b.anchor)
                    .and_then(|i| b.new_emphasis.get(i))
            })
            .map_or(&[], Vec::as_slice)
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
            // `❯` glyph lives on the band's first text row — over its
            // end, or the cursor itself scrolls off the top edge.
            if start < self.offset {
                self.offset = start;
            }
        }
    }

    /// Center a source-line range in the source viewport as far as the
    /// document edges allow. Used by change navigation; ordinary cursor
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
        old.baseline_changed = std::mem::take(&mut self.baseline_changed);
        old.baseline_deleted_before = std::mem::take(&mut self.baseline_deleted_before);
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
        self.baseline_changed = std::mem::take(&mut new.baseline_changed);
        self.baseline_deleted_before = std::mem::take(&mut new.baseline_deleted_before);
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

    pub(crate) fn file_unseen_count(&self, index: usize) -> usize {
        let (changed, deleted) = if index == self.current_file_index {
            (&self.baseline_changed, &self.baseline_deleted_before)
        } else if let Some(state) = self.file_states.get(index) {
            (&state.baseline_changed, &state.baseline_deleted_before)
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

    /// **外部コマンドの答えを待っているか**（意味解析・Review の linter と
    /// ルール）。待っているあいだ、イベントループは待ちの印が回る間隔
    /// （[`crate::chrome::SPINNER_FRAME_MS`]）で描き直す。
    ///
    /// `--no-fx` でも止めない — 印は飾りではなく「まだ終わっていない」と
    /// いう状態の表示である。
    pub(crate) fn waiting(&self) -> bool {
        self.semantic_inflight.is_some() || (self.review_armed && self.review_inflight > 0)
    }

    /// 待ちの印（`⠹ analyzing… 3s`）。起点が無ければ今から数える。
    pub(crate) fn busy_text(&self, since: Option<Instant>) -> String {
        let now = Instant::now();
        crate::chrome::busy_text(since.unwrap_or(now), now)
    }

    /// Whether any tachyonfx effect is currently animating (toast fade,
    /// appear/ghost scatter, the rotating time-machine frame). The event
    /// loop raises its tick rate while this is true.
    pub(crate) fn has_active_fx(&self) -> bool {
        if !self.config.fx {
            return false;
        }
        self.toast_fx.is_some()
            // マーカーが引かれる演出（marks）。これが無いと、toast の
            // 出ない経路（文書の再読み込みなど）で 700 ms が 100 ms の
            // 刻みに落ちて 7 枚の飛び飛びになる。
            || self.marks_fx.is_some()
            || self.readout_fx.is_some()
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
        terminal_size().1
    }

    /// View-mode viewport height in rows — must match `draw_view`'s
    /// `inner.height` (title bar + footer + the frame's two borders take
    /// the other rows), or per-frame `keep_cursor_visible` re-shoves the
    /// offset and wheel scroll stalls. View mode always draws the frame.
    ///
    /// **Review の一覧が据え付けてあれば、そのぶん低い**
    /// （[`crate::review_dock::body_rows_taken`]）。端末の高さのままだと
    /// カーソルが一覧の下に潜る。
    pub(crate) fn view_viewport_rows(&self) -> usize {
        self.terminal_height()
            .saturating_sub(4)
            .saturating_sub(crate::review_dock::body_rows_taken(self))
            .max(1) as usize
    }

    /// Source-mode viewport height — must match `draw_source`'s
    /// `inner.height` (title bar + footer only; source mode never draws
    /// a frame, keeping every column for the source). Review の据え付けの
    /// ぶんは view と同じく引く。
    pub(crate) fn source_viewport_rows(&self) -> usize {
        self.terminal_height()
            .saturating_sub(2)
            .saturating_sub(crate::review_dock::body_rows_taken(self))
            .max(1) as usize
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

#[cfg(test)]
thread_local! {
    /// テストで端末の大きさを決める（`TestBackend` と同じ大きさにする）。
    /// `cargo test` は端末の中で走ると本物の大きさを拾うので、高さを
    /// 前提にするテストはこれで固定する。
    pub(crate) static TEST_TERMINAL_SIZE: std::cell::Cell<Option<(u16, u16)>> =
        const { std::cell::Cell::new(None) };
}

/// 端末の大きさ `(幅, 高さ)`。取れなければ 80×24。
pub(crate) fn terminal_size() -> (u16, u16) {
    #[cfg(test)]
    if let Some(size) = TEST_TERMINAL_SIZE.with(|cell| cell.get()) {
        return size;
    }
    ratatui::crossterm::terminal::size().unwrap_or((80, 24))
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
