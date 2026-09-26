//! The overlay stack: the files picker (Ctrl+p), the comments
//! list (`l`), the help reference (`?`), and the marks mode's question
//! picker (`m`) — their key handling, the shared panel/cursor/scroll
//! math, and their drawers.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode, supports_view};
use crate::clip_ellipsis;
use crate::comment::{Comment, Selection};
use crate::reload::file_externally_changed;
use crate::view::CURSOR_GLYPH;
use crate::render_pending_history;
use crate::replace_view_preserving_cursor;
use crate::export_all;

/// The kind of overlay currently open (Ctrl+p = files, `l` = comments,
/// `t` = timeline, `?` = help, `m` = what to mark for).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Overlay {
    /// File picker: switch to another file in the session.
    Files,
    /// All-comments list across every file.
    Comments,
    /// The document timeline (`t`): every revision with provenance and
    /// detail; j/k scrub the document behind it.
    Timeline,
    /// The full key reference (`?`).
    Help,
    /// **marks モードの問いの選択**（`m`）。定型を並べ、最下段に自由入力
    /// （`/`）を置く。`docs/design/marks-only-and-review-mode.md` 0 節。
    ///
    /// **`questions` ではなく `mark for` と名乗る。** marks モードでは
    /// 「question」が**文書の中にある問い**とも読める（`Unsettled` や
    /// `Your call` がまさにそれを光らせる）ので、こちらが持っている問いと
    /// 同じ語になってしまう。ステータス行の `MARK n%` と地続きの言い方に
    /// することで、「何を光らせるか」以外に読みようが無くなる
    /// （2026-09-22 の読み手の指摘）。
    MarkFor,
    /// **Review の候補の一覧**（`R`）。`docs/design/marks-only-and-review-mode.md`
    /// 4 節。
    ///
    /// **窓ではなく、本文の下に据え付ける**（[`crate::review_dock`]）。
    /// 被さる窓だと指摘箇所が窓の下に隠れて読めなかった（2026-09-23 の
    /// 読み手の指摘）。キーは `Comments` と同じ作法（黄色いタイトル・`❯` の
    /// カーソル・j/k と Enter・Esc で閉じる）で、**既にある作法を覚えている
    /// 人が何も覚え直さずに使える**ようにしてある。
    ///
    /// **`MarkFor` の隣ではない。** marks の popup は「何を光らせるか」を
    /// 選ぶ箱で、こちらは「直す候補」の台帳である。共有しているのは
    /// 判定器の一段の問いだけで、UI は別に持つ（読み手の決定、2026-09-22〜23）。
    Review,
}

/// Open an overlay, resetting the double-click tracker: a click in a
/// fresh session must never be mistaken for the tail of an old
/// double-click (and a delete may have shifted the entry indices). The
/// pending `]`/`[` chord is dropped too — overlay keys must never
/// complete it.
pub(crate) fn open_overlay(app: &mut App, kind: Overlay, cursor: usize) {
    app.overlay = Some(kind);
    app.overlay_cursor = cursor;
    app.overlay_offset = 0;
    app.last_overlay_click = None;
    app.pending_chord = None;
}

/// Raw indices of `app.comments` in the overlay's order (file, then
/// start line). The overlay cursor indexes THIS list — Enter, delete,
/// and the drawer all map through it, so they can never disagree with
/// what is highlighted.
pub(crate) fn sorted_comment_indices(app: &App) -> Vec<usize> {
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
pub(crate) fn overlay_rows(app: &App) -> Vec<Option<usize>> {
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
        Some(Overlay::MarkFor) => (0..mark_for_entry_count(app)).map(Some).collect(),
        // 候補は既に文書順に並んでいる（`App::review_candidates`）ので、
        // 並べ替えも見出しの行も無い — 1 候補 = 1 行である。
        Some(Overlay::Review) => (0..app.review_candidates.len()).map(Some).collect(),
        Some(Overlay::Timeline) => (0..app.history().map_or(0, |h| h.revisions.len()))
            .map(Some)
            .collect(),
        Some(Overlay::Help) | None => Vec::new(),
    }
}

/// The number of selectable entries in the open overlay — the j/k and
/// wheel clamps. The comments overlay counts its current tab.
pub(crate) fn overlay_entry_count(app: &App) -> usize {
    match app.overlay {
        Some(Overlay::Files) => app.files.len(),
        Some(Overlay::Comments) => app.comments.len(),
        Some(Overlay::Timeline) => app.history().map_or(0, |h| h.revisions.len()),
        Some(Overlay::MarkFor) => mark_for_entry_count(app),
        Some(Overlay::Review) => app.review_candidates.len(),
        Some(Overlay::Help) | None => 0,
    }
}

/// How many list rows the open overlay's panel can show: its inner
/// height minus the title, the blank, and the footer hint rows.
pub(crate) fn overlay_visible_rows() -> usize {
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
pub(crate) fn visible_cards(app: &App) -> Vec<&Comment> {
    let current = app.current_file_path();
    let revision = app.current_revision_context();
    app.comments
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            c.file_path == *current
                && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
                && !(app.mode == Mode::Input && app.editing_comment == Some(*i))
        })
        .map(|(_, c)| c)
        .collect()
}

/// Keep the overlay cursor's row inside the visible window: the offset
/// scrolls only when the content overflows (a list that fits never
/// scrolls).
pub(crate) fn keep_overlay_cursor_visible(app: &mut App) {
    let rows = overlay_rows(app);
    // Review の一覧は窓ではなく据え付けで、行数は据え付けの配分が決める。
    let visible = if app.overlay == Some(Overlay::Review) {
        crate::review_dock::list_rows(app)
    } else {
        overlay_visible_rows()
    };
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
pub(crate) fn on_overlay_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    match app.overlay {
        Some(Overlay::MarkFor) => on_mark_for_overlay_key(app, key, modifiers),
        Some(Overlay::Files) => on_files_overlay_key(app, key, modifiers),
        Some(Overlay::Comments) => on_comments_overlay_key(app, key, modifiers),
        Some(Overlay::Review) => on_review_overlay_key(app, key, modifiers),
        Some(Overlay::Timeline) => on_timeline_overlay_key(app, key, modifiers),
        Some(Overlay::Help) => on_help_overlay_key(app, key, modifiers),
        None => {}
    }
}

/// Move the history cursor while the timeline overlay is open: the
/// document behind updates through the normal debounced render path
/// (same as `←`/`→`), and the overlay cursor follows the position.
pub(crate) fn timeline_overlay_move(app: &mut App, delta: isize) {
    if crate::select_history(app, delta) {
        app.overlay_cursor = app.history().map_or(0, |h| h.position);
        keep_overlay_cursor_visible(app);
    }
}

/// Snap the history cursor directly to a revision index (mouse click /
/// wheel on the timeline overlay). No toast: the row is already on
/// screen, the document renders behind the panel.
pub(crate) fn timeline_overlay_seek(app: &mut App, position: usize) {
    let moved = app
        .histories
        .get_mut(app.current_file_index)
        .is_some_and(|history| {
            let next = position.min(history.revisions.len().saturating_sub(1));
            let moved = next != history.position;
            history.position = next;
            moved
        });
    app.overlay_cursor = position;
    keep_overlay_cursor_visible(app);
    if moved {
        app.history_render_due = Some(std::time::Instant::now());
    }
}

/// The timeline overlay (`t`): every revision, newest first — NOW on
/// top. j/k (or the arrows, app-wide direction: Left = older) scrub the
/// document behind the panel; Enter / q / `t` close at the selected
/// revision, Esc restores the position the overlay opened at (the fzf
/// cancel contract).
pub(crate) fn on_timeline_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    match key {
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Left => timeline_overlay_move(app, 1),
        KeyCode::Char('k') | KeyCode::Up | KeyCode::Right => timeline_overlay_move(app, -1),
        KeyCode::Enter => app.overlay = None,
        // q or t closes at the selected revision; t also toggles the
        // list closed again, like l for the comments list.
        KeyCode::Char('q') | KeyCode::Char('t') => app.overlay = None,
        KeyCode::Esc => {
            let restore = app.timeline_restore.take();
            app.overlay = None;
            if let Some(position) = restore {
                let index = app.current_file_index;
                if let Some(history) = app.histories.get_mut(index) {
                    history.position =
                        position.min(history.revisions.len().saturating_sub(1));
                }
                app.history_render_due = Some(std::time::Instant::now());
            }
        }
        _ => {}
    }
}

/// The help overlay (`?`): j/k or the wheel scroll the reference — but
/// only when it overflows the panel (content that fits never scrolls).
/// Esc / q / `?` close it.
pub(crate) fn on_help_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let max = help_rows(
        app.esc_quit_enabled(),
        app.config.reply,
        false,
        app.semantic_enabled(),
        app.semantic_enabled() || app.lint.is_some(),
    )
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
pub(crate) fn activate_overlay_selection(app: &mut App) {
    match app.overlay {
        // 問いを選ぶ。**ここが marks モードの遅延の起点の 1 つ**で、
        // 開いただけでは何も走らず、選んだ 1 本だけが Jev に届く。
        // 最下段は自由入力なので、popup を閉じて 1 行プロンプトへ渡す。
        Some(Overlay::MarkFor) => {
            let entry = mark_for_entry(app, app.overlay_cursor);
            app.overlay = None;
            if entry == MarkForEntry::Off {
                // `0 Off` — `Esc` の marks の段と同じ消し方と同じ語
                // （`crate::esc::Layer::Marks`）。沈めていたら、沈める先が
                // 無くなるので一緒に解く。
                app.clear_focus();
                if app.clear_marks_question() {
                    app.flash("marks cleared");
                }
            } else if entry == MarkForEntry::Free {
                crate::open_marks_prompt(app);
            } else if let MarkForEntry::Preset(index) = entry
                && app.ask_marks_preset(index)
            {
                if let Some(label) = app.marks_question_label() {
                    let asking = format!("asking: {label}");
                    app.flash(asking);
                }
            } else {
                crate::refuse_marks_question(app);
            }
        }
        Some(Overlay::Files) => {
            if app.overlay_cursor < app.files.len() {
                app.overlay = None;
                app.switch_to_file(app.overlay_cursor);
            }
        }
        Some(Overlay::Comments) => {
            // Comments tab: jump to the selected comment's file; its
            // whole range becomes the selection (all lines light up),
            // cursor on the extent. The cursor indexes the SORTED list;
            // map through the indices.
            let idx = sorted_comment_indices(app);
            if let Some(&raw) = idx.get(app.overlay_cursor) {
                let c = &app.comments[raw];
                let target_path = c.file_path.clone();
                let target_start = c.start;
                let target_end = c.end;
                let target_revision = c.revision.clone();
                if let Some(pos) = app.files.iter().position(|f| f == &target_path) {
                    app.overlay = None;
                    app.switch_to_file(pos);
                    // A comment anchors to the document it was made
                    // against: a historical one (made while browsing the
                    // timeline) restores that revision before the jump —
                    // its line numbers belong to that document, and its
                    // card only renders under that revision; a live one
                    // returns to NOW when the user happens to be browsing
                    // the past. The target revision is materialized
                    // synchronously here (the overlay is closed above;
                    // the pending-scrub guard means the common at-NOW
                    // jump to a live comment skips the render and its
                    // landing pulse entirely), so the selection below
                    // lands on the comment's actual lines. A historical
                    // revision that no longer exists in the history falls
                    // back to the plain line jump.
                    let restored = app.histories.get_mut(pos).is_some_and(|history| {
                        let target = match target_revision.as_deref() {
                            Some(revision) => history
                                .revisions
                                .iter()
                                .position(|r| {
                                    // 旧形式（`local:<id>` 裸）で保存済みのコメントも
                                    // identity 部だけで照合して復元できる。
                                    crate::history::same_revision(
                                        r.context().as_deref(),
                                        Some(revision),
                                    )
                                }),
                            None => Some(0),
                        };
                        match target {
                            Some(target) => {
                                history.position = target;
                                true
                            }
                            None => false,
                        }
                    });
                    if restored
                        && app
                            .histories
                            .get(pos)
                            .is_some_and(|history| history.position != history.rendered_position)
                    {
                        app.history_render_due = Some(std::time::Instant::now());
                        render_pending_history(app, false);
                    }
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
        // **一覧は開いたまま。** Enter は本文の該当行へ飛ぶだけで、
        // 候補を 1 本ずつ見比べる作業は一覧に戻ってこられないと進まない
        // （Timeline が「箱の後ろで文書が動く」形の先例である）。
        Some(Overlay::Review) => review_overlay_jump(app),
        // Timeline: the position is already live-scrubbed; Enter just
        // confirms it.
        Some(Overlay::Timeline) => app.overlay = None,
        Some(Overlay::Help) | None => {}
    }
}

pub(crate) fn on_files_overlay_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
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
        // picker is open); Ctrl+o toggles it closed again.
        KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => app.overlay = None,
        _ => {}
    }
}

/// The all-comments list (`l`): j/k move, Enter jumps to the comment's
/// file+line, `d` deletes it, and Esc / q / `l` close it.
pub(crate) fn on_comments_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let total = overlay_entry_count(app);
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
            // The cursor indexes the SORTED list; map through the indices
            // to the raw vec.
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
        // The comments' own export lives here, next to the comments: `y`
        // copies them all (kept), `s` sends them (cleared on success —
        // an emptied list closes itself, like `d` deleting the last one).
        KeyCode::Char('y') => export_all(app, false),
        KeyCode::Char('s') => {
            let had_comments = !app.comments.is_empty();
            export_all(app, true);
            if had_comments && app.comments.is_empty() {
                app.overlay = None;
            }
        }
        // Esc or q closes the list; `l` toggles it closed again.
        KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char('l') => app.overlay = None,
        _ => {}
    }
}

/// Draw the all-comments overlay (`l`). Centered panel with sorted comment list.
pub(crate) fn draw_overlay(f: &mut Frame, app: &App) {
    match app.overlay {
        Some(Overlay::MarkFor) => draw_mark_for_overlay(f, app),
        Some(Overlay::Files) => draw_files_overlay(f, app),
        Some(Overlay::Comments) => draw_comments_overlay(f, app),
        // 窓ではない — 本文の下に据え付けてあり、`draw` が本文と並べて
        // 描く（[`crate::review_dock::draw`]）。
        Some(Overlay::Review) => {}
        Some(Overlay::Timeline) => draw_timeline_overlay(f, app),
        Some(Overlay::Help) => draw_help_overlay(f, app),
        None => {}
    }
}

/// The help reference rows (label, keys). Shared by the drawer and the
/// scroll clamp, so the list never scrolls past its own end; scrollable
/// with j/k or the wheel (small screens), closed by Esc / q / `?` or a
/// click outside the panel. The quit row reflects the active Esc binding.
pub(crate) fn help_rows(
    esc_quit: bool,
    reply: bool,
    _in_git: bool,
    semantic: bool,
    review: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut rows = vec![
        ("move", "j/k · g/G · PgUp/PgDn · ^u/^d"),
        ("select", "v · J/K · Shift+↓↑ · Esc cancel"),
        ("comment", "c add · d delete · ^n/^p jump"),
        ("mode", "Tab view⇄source"),
        ("output", "y copy as shown · s send"),
        ("list", "l comments (y/s/d inside) · ? help"),
    ];
    if reply {
        // Reply mode: a single message document — no file navigation, no
        // edit, and reloads are automatic (r stays as a manual retry).
        // ]/[ moves between the recent messages akp materialized.
        rows.push(("msg", "]/[ older/newer"));
        rows.push(("reload", "auto-reload on change · r manual"));
    } else {
        rows.insert(1, ("file", "]/[ · ^o files"));
        rows.insert(2, ("time", "← older · newer → · hold:scrub · t detail"));
        rows.push(("reload", "r reload · i ignore · e edit"));
        rows.push(("compare", "n/N next/prev change · a seen/set baseline"));
    }
    if semantic {
        // Only with `--semantic`: without an annotation these keys refuse,
        // and the help must not offer what the session cannot do.
        //
        // **2 行に割ってある。** 1 行に 4 キーと括弧書きを詰めていた
        // ときは、panel（幅 70 %）の右端で `… (MARK` と切れていた
        // （2026-09-22 の実機）。ヘルプが切れるのは、いちばん読まれる
        // 場面で読めないということである。
        rows.push(("mark", "m what to mark · M previous · / ask your own"));
        rows.push(("amount", "-/+ ±1 · </> ±10 (count and % at the footer's right)"));
        // **トグル 1 つ**（`crate::focus`）。押している間だけ沈む形は
        // 試して捨てたので、端末による違いをここに書くことは無い。
        rows.push(("focus", "f sink everything unmarked (toggle) · Esc off"));
        rows.push(("marks", "]m next mark · [m previous mark"));
        // マークを消す道。Esc は一番手前の層から 1 枚ずつはがすので、
        // 沈めていればそちら、Review が出ていればそちらが先に取る
        // （`crate::esc::ORDER`、`crate::keys::MARKS_CLEAR_HINT`）。
        rows.push(("clear", crate::keys::MARKS_CLEAR_HINT));
    }
    // Review は marks の下にある（**別機能だが、同じ層を使いうる**ことが
    // この並びで読める）。`--lint-cmd` だけのセッションでも出る。
    if review {
        rows.push(("review", crate::keys::REVIEW_HINT));
    }
    // Esc の行（下の quit）のすぐ上。語は段ごとに変わるので、ここでは
    // バッジが何を言う場所なのかだけを言う（`crate::esc::badge`）。
    rows.push(("esc", "top-right badge = what the next Esc does (or click)"));
    rows.push(("quit", if esc_quit { "Esc/q quit" } else { "q quit · Esc cancel" }));
    rows
}

/// The full key reference (`?`): label + keys per category, scrollable
/// with j/k or the wheel (small screens), closed by Esc / q / `?` or a
/// click outside the panel.
pub(crate) fn draw_help_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label_style = Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::BOLD);

    let rows = help_rows(
        app.esc_quit_enabled(),
        app.config.reply,
        false,
        app.semantic_enabled(),
        app.semantic_enabled() || app.lint.is_some(),
    );
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
pub(crate) fn unique_suffix(path: &Path, all: &[PathBuf]) -> String {
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
pub(crate) fn common_parent(files: &[PathBuf]) -> Option<String> {
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
pub(crate) fn clip_if_needed(s: &str, max_cols: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_cols {
        s.to_string()
    } else {
        clip_ellipsis(s, max_cols)
    }
}

/// A centered panel rect (70% width/height) for overlay contents.
pub(crate) fn overlay_panel(area: Rect) -> Rect {
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

/// いま開いている overlay の枠。`MarkFor` だけが小さい箱で、他は 70 %
/// パネルである。**クリックで閉じる境目**（[`crate::on_mouse`]）と当たり
/// 判定がここ 1 か所を見るので、箱の形と「外」の定義がずれない。
pub(crate) fn active_overlay_panel(app: &App, area: Rect) -> Rect {
    match app.overlay {
        Some(Overlay::MarkFor) => mark_for_panel(area, &mark_for_rows(app)),
        _ => overlay_panel(area),
    }
}

/// Map a screen row to the overlay entry under it (the cursor index), or
/// None for the title, group headers, gaps, or outside the panel.
/// Mirrors draw_overlay's layout (border at panel.y, title at +1, entries
/// from +2) so clicks land exactly on what is drawn.
pub(crate) fn overlay_entry_at(app: &App, row: u16) -> Option<usize> {
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
        Overlay::Timeline => {
            (rel < app.history().map_or(0, |h| h.revisions.len())).then_some(rel)
        }
        Overlay::Help => None,
        // **小さい popup は自前で測る。** 他の overlay の 70 % パネルを
        // 当たり判定に使い回すと、箱の外（文書が見えている所）を押しても
        // 行が選ばれる — 7 行の箱で 70 % の当たり判定は事故のもとである。
        Overlay::MarkFor => {
            let rows = mark_for_rows(app);
            let panel = mark_for_panel(
                Rect { x: 0, y: 0, width: w, height: h },
                &rows,
            );
            let first = panel.y + 2; // 枠 + タイトル
            let index = row.checked_sub(first)? as usize;
            (index < rows.len()).then_some(index)
        }
        Overlay::Comments => {
            // Same grouped layout as draw_comments_overlay: per file a
            // header row (not selectable) then one row per comment.
            overlay_rows(app).get(rel).copied().flatten()
        }
        // 据え付けの一覧は自前で測る（窓の 70 % パネルとは位置が違う）。
        Overlay::Review => crate::review_dock::entry_at(app, row),
    }
}

/// The file picker (Ctrl+p): every file in session order with its comment
/// count; the current file is highlighted. Files whose on-disk state
/// changed since their last load carry the title bar's ⚡ badge.
pub(crate) fn draw_files_overlay(f: &mut Frame, app: &App) {
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
        let cursor_mark = if selected { CURSOR_GLYPH } else { " " };
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
        let unseen_count = app.file_unseen_count(i);
        // Unseen spots: the same `! N` the title bar shows.
        let unseen_mark = if unseen_count > 0 {
            format!(" ! {unseen_count}")
        } else {
            String::new()
        };
        // The row: basename when unique, else the shortest unique path
        // suffix; clip only when the panel is too narrow for the right
        // side (⚡ + count + mode tag).
        let suffix = unique_suffix(file, &app.files);
        let reserved = 2
            + UnicodeWidthStr::width(changed_mark)
            + UnicodeWidthStr::width(unseen_mark.as_str())
            + UnicodeWidthStr::width(count_str.as_str())
            + mode_tag.len();
        let name = clip_if_needed(&suffix, inner.saturating_sub(reserved));
        lines.push(Line::from(vec![
            Span::styled(format!("{cursor_mark} {name}"), name_style),
            Span::styled(changed_mark, Style::default().fg(Color::Yellow)),
            Span::styled(unseen_mark, Style::default().fg(Color::Yellow)),
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

/// The document timeline (`t`): every revision, newest first — NOW on
/// top, then LOCAL snapshots by observation time, then COMMITs. The
/// current row carries the `◆` marker, the baseline `▮` with a
/// `· base` tag; j/k (or ←/→) scrub the document behind the panel, Enter
/// confirms, Esc restores the position the list opened at.
pub(crate) fn draw_timeline_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let pink = Style::default().fg(crate::view::TIMELINE_LOCAL_COLOR);

    let Some(history) = app.history() else {
        return;
    };
    let n = history.revisions.len();
    let baseline = history.baseline_position();
    let title_text = format!(" timeline ({n}) ");
    let title_fill =
        "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ])];

    let inner = panel.width.saturating_sub(2) as usize;
    let visible = overlay_visible_rows();
    let offset = app.overlay_offset.min(n.saturating_sub(visible));
    for (i, rev) in history.revisions.iter().enumerate().skip(offset).take(visible) {
        let chron = n - i; // 1..n; n = NOW
        let current = i == history.position;
        let is_baseline = baseline == Some(i);
        // The same marker vocabulary as the browsing bar: ◆ the
        // current point, ▮ baseline, ● local / ◼ commit.
        let (marker, marker_style) = if current {
            ("◆", pink.add_modifier(Modifier::BOLD))
        } else if rev.source == crate::history::RevisionSource::Now {
            // NOW keeps its anchor glyph even when it is also the
            // baseline (the `· base` tag still shows).
            ("●", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
        } else if is_baseline {
            ("▮", yellow)
        } else {
            match rev.source {
                crate::history::RevisionSource::Now => unreachable!(),
                crate::history::RevisionSource::Local => ("●", pink),
                crate::history::RevisionSource::Git => {
                    ("◼", Style::default().fg(crate::view::TIMELINE_COMMIT_COLOR))
                }
            }
        };
        let provenance = match rev.source {
            crate::history::RevisionSource::Now => "NOW",
            crate::history::RevisionSource::Local => "LOCAL",
            crate::history::RevisionSource::Git => "COMMIT",
        };
        let detail = match rev.source {
            // The working tree, exactly as the title label says.
            crate::history::RevisionSource::Now => rev.summary.clone(),
            // A LOCAL generation has no subject and no identity worth
            // showing: it is simply the observed document.
            crate::history::RevisionSource::Local => String::new(),
            crate::history::RevisionSource::Git => {
                format!("{} · {}", rev.short_id, rev.summary)
            }
        };
        let base_tag = if is_baseline { " · base" } else { "" };
        let row_text = if detail.is_empty() {
            format!("{chron:>2}/{n} · {provenance}{base_tag}")
        } else {
            format!("{chron:>2}/{n} · {provenance} · {detail}{base_tag}")
        };
        let row_style = if current {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let budget = inner.saturating_sub(3);
        let text = clip_if_needed(&row_text, budget);
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(text, row_style),
        ]));
    }

    // Footer hints.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:scrub  Enter/q/t:keep·close  Esc:restore&close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// The all-comments list (`l`): every file's comments sorted by path then
/// start line, with jump (Enter) and delete (d).
/// The overlay title's leading spans, plus the shared directory suffix.
pub(crate) fn overlay_title_spans(
    _app: &App,
    comments: usize,
    _changes: usize,
    dir: Option<String>,
) -> Vec<Span<'static>> {
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(format!(" comments ({comments}) "), yellow)];
    if let Some(dir) = dir {
        spans.push(Span::styled(format!("· {dir} "), yellow));
    }
    spans
}

/// The all-comments list (`l`): every file's comments sorted by path then
/// start line, with jump (Enter) and delete (d).
pub(crate) fn draw_comments_overlay(f: &mut Frame, app: &App) {
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
    let dir = (count > 0).then(|| common_parent(&comment_files).unwrap_or_default());
    let mut title = overlay_title_spans(app, count, 0, dir);
    let used: usize = title
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    title.push(Span::styled(
        "─".repeat(panel.width.saturating_sub(used as u16 + 2) as usize),
        dark_gray,
    ));
    lines.push(Line::from(title));

    if count == 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(" no comments yet", dark_gray)));
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
                let cursor_mark = if selected { CURSOR_GLYPH } else { " " };
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
                let revision = c
                    .revision
                    .as_deref()
                    .and_then(|context| context.split_whitespace().next())
                    .map(|short| format!(" [{short}]"))
                    .unwrap_or_default();
                let used = 2
                    + UnicodeWidthStr::width(range.as_str())
                    + UnicodeWidthStr::width(revision.as_str())
                    + 2;
                let budget = inner.saturating_sub(used);
                let body = clip_if_needed(body_first, budget);
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor_mark} {range}"), loc_style),
                    Span::styled(revision, Style::default().fg(Color::Magenta)),
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

// ---- Review の候補の一覧（`R`） --------------------------------------
//
// `docs/design/marks-only-and-review-mode.md` 4 節。キーは `Comments` の
// 一覧の作法の写しである（`❯` のカーソル・j/k と Enter・Esc で閉じる）。
// 違うのは行の中身と、accept / dismiss の 2 本のキーと、**窓ではなく
// 本文の下に据え付ける**ことである — 描くのも高さを決めるのも
// [`crate::review_dock`]。

/// 候補の出どころが 1 本も無いときの 1 行 — **何をすれば候補が出るか**。
pub(crate) const NO_REVIEW_SOURCE: &str = "no review source — set --lint-cmd or enable a rule";

/// 一覧を開く（`R`）。**ここが Review の遅延の起点である。**
///
/// `--semantic <fixture>` では断る（[`App::review_enabled`]）— fixture は
/// 1 つの問いへの固定の答えで、ルールの文面で聞き直す道が無い。
pub(crate) fn open_review(app: &mut App) {
    if !app.review_enabled() {
        app.flash_err(NO_REVIEW_SOURCE);
        return;
    }
    app.arm_review();
    open_overlay(app, Overlay::Review, 0);
    keep_overlay_cursor_visible(app);
    // 候補が手元にあれば（2 度目以降）、本文はすぐ先頭の候補へ。無ければ
    // 答えが届いたときに送る（[`App::accept_review_analysis`]）。
    crate::review_dock::follow(app);
}

/// Enter — 本文の該当行へ飛ぶ。**一覧は開いたままである。**
///
/// 選択を作るのは `Comments` の Enter と同じ作法で、候補の行が丸ごと
/// 光る。飛ぶ先は候補の**最後の行**（範囲の外に出ない位置）である。
fn review_overlay_jump(app: &mut App) {
    let Some(candidate) = app.review_candidates.get(app.overlay_cursor) else {
        return;
    };
    let last = app.source.len().saturating_sub(1);
    let start = (candidate.lines.0.saturating_sub(1) as usize).min(last);
    let end = (candidate.lines.1.saturating_sub(1) as usize).min(last);
    app.selection = Some(Selection { anchor: start, cursor: end });
    if app.mode == Mode::View {
        app.view.goto_source_line(end);
        app.view.keep_cursor_visible(app.view_viewport_rows());
    } else {
        app.cursor = end;
        app.keep_cursor_visible(app.source_viewport_rows() as u16);
    }
}

/// Review の一覧のキー。
///
/// j/k（矢印も）で動き、Enter で本文へ飛び、`a` で accept ⇄ 取り消し、
/// `x` で dismiss ⇄ 戻す、`A` で Pending を全部 accept、q / `R` で閉じる
/// （Esc は層を 1 枚はがす — 選択があればそれ、無ければ一覧）。
///
/// `a` / `x` は**押したキーの状態へ直接行く**切り替えである
/// （[`App::toggle_accept`] / [`App::toggle_dismiss`]）。送った候補には
/// どちらも効かず、そう言う（二重に送らせない）。
///
/// **accept / dismiss でカーソルは動かさない。** 一覧の行は消えず印が
/// 変わるだけなので（`✓` / `–`）、勝手に次へ送ると「いま何を見たか」が
/// 分からなくなる。次へ行くのは j である。
pub(crate) fn on_review_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let total = app.review_candidates.len();
    // `A` の取り消しは**直後の `A`** だけ。ほかのどのキー（移動も含む）でも
    // 戻せなくなる。
    let undo = app.review_accept_all_undo.take();
    match key {
        // 動いたら本文も送る（[`crate::review_dock::follow`]）— 選んだ
        // 候補が本文の中ほどに来るので、`Enter` で飛ばなくても読める。
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 {
                app.overlay_cursor = (app.overlay_cursor + 1).min(total - 1);
                keep_overlay_cursor_visible(app);
                crate::review_dock::follow(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
            keep_overlay_cursor_visible(app);
            crate::review_dock::follow(app);
        }
        KeyCode::Enter => activate_overlay_selection(app),
        // accept はカードを差し込み、本文の行が増える。候補を中ほどに
        // 置き直す（dismiss は下線が消えるだけだが、同じ道を通す）。
        KeyCode::Char(crate::keys::REVIEW_ACCEPT) => {
            let done = app.toggle_accept(app.overlay_cursor);
            after_review_toggle(app, done);
        }
        KeyCode::Char(crate::keys::REVIEW_DISMISS) => {
            let done = app.toggle_dismiss(app.overlay_cursor);
            after_review_toggle(app, done);
        }
        KeyCode::Char(crate::keys::REVIEW_ACCEPT_ALL) => {
            if let Some(made) = undo {
                let removed = app.undo_accept_all(&made);
                replace_view_preserving_cursor(app);
                crate::review_dock::follow(app);
                app.flash(format!("{removed} review comments removed"));
                return;
            }
            let made = app.accept_all_pending();
            if made > 0 {
                replace_view_preserving_cursor(app);
                crate::review_dock::follow(app);
                app.flash(format!("{made} comments from review · A again to undo"));
            }
        }
        // Esc は層の表を通る（`crate::esc`）— 一覧の Enter で作った選択が
        // あれば先にそれがはがれ、次の Esc で一覧が閉じる。`q` / `R` は
        // 今までどおり一覧を閉じるだけ。
        KeyCode::Esc => crate::esc::peel(app),
        KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char(crate::keys::REVIEW_OPEN) => app.overlay = None,
        _ => {}
    }
}

/// `a` / `x` のあと。コメントが増えた・減ったなら本文のカードを描き直し、
/// 候補を中ほどに置き直す（dismiss は下線が消えるだけだが、同じ道を通す）。
///
/// フラッシュは**見えないところで起きたこと**にだけ出す: コメントを消した
/// （`l` の一覧から 1 本減った）と、送った候補で何もしなかった理由。印の
/// 入れ替わり（`✓` / `–` / 空白）は一覧の行そのものが言う。
fn after_review_toggle(app: &mut App, done: crate::app::ReviewToggle) {
    use crate::app::ReviewToggle;
    match done {
        ReviewToggle::Nothing => return,
        ReviewToggle::AlreadySent => {
            app.flash_err(REVIEW_ALREADY_SENT);
            return;
        }
        ReviewToggle::Unaccepted => app.flash("review comment removed"),
        ReviewToggle::Accepted | ReviewToggle::Dismissed | ReviewToggle::Restored => {}
    }
    // コメントが本文にカードとして出る・消える（`l` の一覧にも載る）。
    replace_view_preserving_cursor(app);
    crate::review_dock::follow(app);
}

/// 送った候補で `a` / `x` を押したときの 1 行。書き換わって reload されれば
/// 決め直す（[`App::forget_review_sent`]）ので、待てばよいことを言う。
pub(crate) const REVIEW_ALREADY_SENT: &str = "already sent — decided again when the file changes";

/// **一覧の `e` — カーソル下の候補を人が直す**（4 節「直接編集」）。
///
/// `run` がエディタを開いて戻るまでを受け持つ（本番は
/// [`crate::reload::open_editor_at`]、テストは端末を抜いた同じ道）。開くのは
/// 候補の**先頭行**で、一覧は開いたままにしておく — 戻った reload が
/// 再解析を頼み、答えが揃うまでタイトルが `analyzing…` になる。
///
/// 答えが揃ったら、カーソルは**直した箇所**に着く: 直した候補がまだ
/// 候補なら それ自身、消えていればその位置の次（文書順）。位置は候補の
/// 先頭バイトを差分で新しい版へ写して決める
/// （[`App::review_edit_anchor`] → [`App::review_cursor_target`]）。
///
/// 本文の `e` と同じ断りをする（reply では編集しない・外の書き換えを
/// 読み込む前は開かない）。
pub(crate) fn review_edit_with(app: &mut App, run: impl FnOnce(&mut App, usize)) {
    let Some(candidate) = app.review_candidates.get(app.overlay_cursor) else {
        return;
    };
    if app.config.reply {
        app.flash_err("reply mode — editing disabled");
        return;
    }
    if app.file_changed {
        app.flash_err("file changed — r reload first");
        return;
    }
    let line = candidate.lines.0.max(1) as usize;
    let cursor = app.overlay_cursor;
    app.review_edit_anchor = Some(candidate.range.start);
    app.review_cursor_target = None;
    run(app, line);
    // 見届けた reload が錨を消費していなければ、中身は変わらなかった
    // （閉じただけ・エディタの失敗・読めない書き込み）。候補も答えも前の
    // ままなので、カーソルも元の候補へ戻す。
    if app.review_edit_anchor.take().is_some() {
        app.overlay_cursor = cursor.min(app.review_candidates.len().saturating_sub(1));
    }
    app.overlay = Some(Overlay::Review);
    keep_overlay_cursor_visible(app);
    crate::review_dock::follow(app);
}

/// 一覧に出す Unit の先頭の桁数（読み手の注文、2026-09-23）。
///
/// 40 桁は「どの箇所か思い出せる」いちばん短い長さである。パネルが
/// 狭ければそちらが勝つ。
pub(crate) const REVIEW_HEAD_COLS: usize = 40;

// ---- marks モードの問いの選択（`m`） ----------------------------------
//
// `docs/design/marks-only-and-review-mode.md` 0 節。file picker と `?`
// ヘルプの作法の写しである（枠・黄色いタイトル・`❯` のカーソル・j/k と
// Enter・Esc で閉じる）。新しい語彙を足していないのは、**既にある作法を
// 覚えている人が何も覚え直さずに使えるようにする**ためで、ここだけ違う
// 操作にする理由が無い。

/// popup の行数 — （`0 Off` ＋）定型 ＋ 自由入力の 1 行。
///
/// 自由入力を**行として置く**のが要点である。`/` は今までどこにも書いて
/// おらず（`?` ヘルプにしか無かった）、「自由入力があること」自体が
/// 隠れていた。
pub(crate) fn mark_for_entry_count(app: &App) -> usize {
    app.marks_questions.as_ref().map_or(0, |questions| {
        usize::from(mark_for_has_off(app)) + questions.presets().len() + 1
    })
}

/// 先頭に `0 Off`（マークを消す）の行を置くか — **消すマークがあるときだけ**。
///
/// 読むのは `Esc` の marks の段と同じ問い（[`App::can_clear_marks_question`]）
/// なので、`0 Off` が出ているときは `Esc` を重ねても同じものが消える。
/// 何も光っていないのに `Off` を並べると、押しても何も起きない行になる。
pub(crate) fn mark_for_has_off(app: &App) -> bool {
    app.can_clear_marks_question()
}

/// popup の 1 行が何か。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkForEntry {
    /// `0 Off` — マークを消す。
    Off,
    /// 定型（`presets()` の添字）。
    Preset(usize),
    /// 最下段の `/ Ask...`。
    Free,
}

/// popup の行の添字を、その行の中身へ読む。行の数え方はここ 1 か所。
pub(crate) fn mark_for_entry(app: &App, row: usize) -> MarkForEntry {
    let off = usize::from(mark_for_has_off(app));
    let free_row = mark_for_entry_count(app).saturating_sub(1);
    if off == 1 && row == 0 {
        MarkForEntry::Off
    } else if row >= free_row {
        MarkForEntry::Free
    } else {
        MarkForEntry::Preset(row - off)
    }
}

/// 定型 `index` の行の添字（`0 Off` が出ていれば 1 つ下がる）。
pub(crate) fn mark_for_preset_row(app: &App, index: usize) -> usize {
    usize::from(mark_for_has_off(app)) + index
}

/// popup の中身 1 行ぶん: (先頭のキー, 名前, 英語の 1 行)。
///
/// 最後の 1 行が自由入力で、キーは `/`、名前は `Ask...` である。
/// マークが出ていれば先頭に `0 Off` が立つ（[`mark_for_has_off`]）。
fn mark_for_rows(app: &App) -> Vec<(String, String, String)> {
    let Some(questions) = app.marks_questions.as_ref() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, String, String)> = Vec::new();
    if mark_for_has_off(app) {
        rows.push(("0".to_string(), "Off".to_string(), MARK_FOR_OFF_HINT.to_string()));
    }
    rows.extend(
        questions
            .presets()
            .iter()
            .enumerate()
            .map(|(i, q)| ((i + 1).to_string(), q.label.clone(), q.hint.clone())),
    );
    rows.push((
        "/".to_string(),
        "Ask...".to_string(),
        questions.free_hint().to_string(),
    ));
    rows
}

/// popup の枠。**他の overlay の 70 % パネルではない** — 中身が 6〜7 行
/// しか無いので、70 % の箱に入れると下 2/3 が空白になる。幅は中身に
/// 合わせて測り、端末が狭ければ縮む。
///
/// 位置は**中央やや上**（`overlay_panel` と同じ横中央、縦は 1/4）。
/// 文書の上に短い箱が落ちてくる形で、下に文書が残るので「いま光って
/// いるもの」を見ながら次の問いを選べる。
pub(crate) fn mark_for_panel(area: Rect, rows: &[(String, String, String)]) -> Rect {
    let title = " mark for ".width() as u16;
    let body = rows
        .iter()
        .map(|(key, label, hint)| {
            // `  1 Essential   what you would misread…`
            let name = MARK_FOR_LABEL_COLS.max(label.width());
            (2 + key.width() + 1 + name + hint.width() + 1) as u16
        })
        .max()
        .unwrap_or(0);
    let hintline = " j/k:move  Enter:ask  Esc:close ".width() as u16;
    let inner = body.max(title).max(hintline);
    let w = (inner + 2).min(area.width);
    // 枠 2 行 ＋ タイトル 1 行 ＋ 中身 ＋ キーの案内 1 行。
    let h = (rows.len() as u16 + 4).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + area.height.saturating_sub(h) / 4,
        width: w,
        height: h,
    }
}

/// `0 Off` の行の英語の 1 行。
pub(crate) const MARK_FOR_OFF_HINT: &str = "clear the marks";

/// 名前の欄の幅。`Unsettled` が 9 桁なので、英語の 1 行はここから始まる。
const MARK_FOR_LABEL_COLS: usize = 12;

/// popup のキー: j/k で動き、Enter で聞く。`1`〜`9` は直接、`/` は自由
/// 入力、`0` はマークを消す（`0 Off` の行があるときだけ）。Esc / q / `m` で
/// 閉じる。
///
/// **開いただけでは 1 円もかからない。** 解析が走るのは Enter（または
/// 数字キー）を打った瞬間だけで、そこが巡る形との違いである。
pub(crate) fn on_mark_for_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let count = mark_for_entry_count(app);
    let free_row = count.saturating_sub(1);
    let presets = app.marks_questions.as_ref().map_or(0, |q| q.presets().len());
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if count > 0 {
                app.overlay_cursor = (app.overlay_cursor + 1).min(count - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
        }
        KeyCode::Enter => activate_overlay_selection(app),
        // 数字で直接。`5` までしか無くても `6` を押して何も起きないのは
        // 正しい（無い行を選べない）。
        // `0` は `0 Off`（マークが出ているときだけある行）。
        KeyCode::Char('0') => {
            if mark_for_has_off(app) {
                app.overlay_cursor = 0;
                activate_overlay_selection(app);
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let index = c as usize - '1' as usize;
            if index < presets {
                app.overlay_cursor = mark_for_preset_row(app, index);
                activate_overlay_selection(app);
            }
        }
        KeyCode::Char('/') => {
            app.overlay_cursor = free_row;
            activate_overlay_selection(app);
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => app.overlay = None,
        _ => {}
    }
}

/// popup を描く。file picker と同じ作法（黄色いタイトル ＋ 罫、`❯` の
/// カーソル、下段にキーの案内）。
///
/// **いま聞いている問いは黄色**で、file picker が「いま開いているファイル」
/// を黄色にするのと同じ意味である。
pub(crate) fn draw_mark_for_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let rows = mark_for_rows(app);
    let panel = mark_for_panel(f.area(), &rows);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);

    let title_text = " mark for ".to_string();
    let title_fill =
        "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ])];

    let current = app.marks_question_label();
    let inner = panel.width.saturating_sub(2) as usize;
    for (i, (key, label, hint)) in rows.iter().enumerate() {
        let selected = i == app.overlay_cursor;
        let asking = current.is_some_and(|c| c == label);
        let name_style = if selected {
            cyan.add_modifier(Modifier::BOLD)
        } else if asking {
            yellow
        } else {
            Style::default().fg(Color::White)
        };
        let cursor_mark = if selected { CURSOR_GLYPH } else { " " };
        let name = format!("{label:<MARK_FOR_LABEL_COLS$}");
        let head = format!("{cursor_mark} {key} {name}");
        let room = inner.saturating_sub(head.width());
        lines.push(Line::from(vec![
            Span::styled(head, name_style),
            Span::styled(clip_if_needed(hint, room), dark_gray),
        ]));
    }
    lines.push(Line::from(Span::styled(
        " j/k:move  Enter:ask  Esc:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}
