//! **Review の一覧を本文の下に据え付ける**（`R`）。
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節「UI」。一覧は窓で
//! 本文に被さらない — 端末を縦に割って、本文の領域の下に置く。本文は
//! 一覧のぶん低くなるだけで、字は 1 つも隠れない。
//!
//! ```text
//! title
//! ┌ 本文 ──────────────────────────────┐
//! │ …                                  │   ← 選んだ候補が中ほどに来るよう送る
//! └ review (3/13) ─────────────────────┘   ← 本文の枠の下辺に題（view）
//!  ❯   L72 · ja-no-weak-phrase · 弱い表現…   ← 一覧（候補の数だけ、上限あり）
//!  ─ textlint/ja-no-weak-phrase · L72 ──   ← 選んだ候補の出どころ
//!    弱い表現: "かも" が使われています。      ← 理由の全文（折り返す）
//! REVIEW  L72/192 · j/k move · a accept …  ← フッタが一覧のキーを案内する
//! ```
//!
//! **高さを決めるのはここ 1 か所である**（[`dock_rows`]）。描画
//! （[`split`]）、本文の高さ（`App::view_viewport_rows` /
//! `App::source_viewport_rows`）、一覧の行の当たり判定（[`entry_at`]）が
//! 同じ計算を通るので、「カーソルが一覧の下に潜る」「クリックが 1 行
//! ずれる」が起きようがない。

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::overlay::Overlay;
use crate::review::{Candidate, Finding};
use crate::view::CURSOR_GLYPH;

/// 一覧の行の上限。これより多い候補は一覧の中でスクロールする。
pub(crate) const LIST_MAX: u16 = 10;

/// 理由の欄の上限。これより長い理由は末尾を `…` で切る。
pub(crate) const DETAIL_MAX: u16 = 6;

/// 端末の高さのうち、据え付けが取ってよい割合（分子 / 分母）。
///
/// **本文が主である。** 一覧は本文を読みながら使う台帳なので、画面の
/// 半分を超えると「一覧を見るために本文を狭める」逆転が起きる。4 割で
/// 切ると、高さ 20 でも本文の中身が 9 行残る。
const SHARE: (u16, u16) = (2, 5);

/// 本文の領域に最低限残す行数（view の枠 2 行込み）。
const BODY_MIN: u16 = 6;

/// 据え付けの行の配分。`list == 0` なら据え付けは無い（端末が低すぎる）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DockRows {
    /// 一覧の行数。
    pub(crate) list: u16,
    /// 理由の欄の行数。0 なら区切りの行も無い。
    pub(crate) detail: u16,
}

impl DockRows {
    /// 据え付けの全高 — 題の 1 行 ＋ 一覧 ＋（区切り ＋ 理由）。
    pub(crate) fn height(self) -> u16 {
        if self.list == 0 {
            return 0;
        }
        1 + self.list + if self.detail > 0 { 1 + self.detail } else { 0 }
    }
}

/// 据え付けの矩形。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dock {
    /// 全体（題の行から理由の欄の最後まで）。
    pub(crate) area: Rect,
    /// 題の行。view では本文の枠の下辺と同じ行である。
    pub(crate) title: Rect,
    /// 一覧。
    pub(crate) list: Rect,
    /// 選んだ候補の出どころの行（高さ 0 なら無い）。
    pub(crate) rule: Rect,
    /// 理由の欄（高さ 0 なら無い）。
    pub(crate) detail: Rect,
}

/// Review の一覧が開いているか。**開いていれば据え付けである**（窓は無い）。
pub(crate) fn is_open(app: &App) -> bool {
    app.overlay == Some(Overlay::Review)
}

/// 理由の欄の文字の幅。一覧の頭（`❯ ✓ `）の下に揃える。
fn detail_width(width: u16) -> usize {
    width.saturating_sub(5).max(1) as usize
}

/// 行の配分を決める — **ここが唯一の式である。**
///
/// `width` / `height` は端末全体。一覧は候補の数（最低 1 行 — 0 件でも
/// 「何をしているか」の 1 行を出す）と [`LIST_MAX`]、理由の欄はいちばん
/// 長い理由の行数と [`DETAIL_MAX`] で頭打ちになり、両方を合わせて端末の
/// 4 割（[`SHARE`]）と「本文に [`BODY_MIN`] 行残す」の狭い方に収める。
///
/// **理由の欄の高さは選んだ候補ではなく全候補で決める。** j/k のたびに
/// 本文の高さが変わると、本文が上下に揺れて読んでいた場所を見失う
/// （パネルは利用者の操作なしに動かない）。候補が届いたときだけ変わる。
pub(crate) fn dock_rows(app: &App, width: u16, height: u16) -> DockRows {
    if !is_open(app) {
        return DockRows::default();
    }
    let middle = height.saturating_sub(2); // タイトルとフッタ
    let share = height * SHARE.0 / SHARE.1;
    let cap = share.min(middle.saturating_sub(BODY_MIN));
    if cap < 2 {
        return DockRows::default(); // 題 ＋ 1 行も取れない
    }
    let room = cap - 1; // 題の行
    let count = app.review_candidates.len() as u16;
    let list_want = count.clamp(1, LIST_MAX);
    let detail_want = if count == 0 {
        0
    } else {
        let w = detail_width(width);
        app.review_candidates
            .iter()
            .map(|c| detail_lines(app, c, w).len() as u16)
            .max()
            .unwrap_or(1)
            .clamp(1, DETAIL_MAX)
    };
    // 区切り ＋ 理由 1 行 ＋ 一覧 1 行が入らなければ、理由の欄は諦める。
    if detail_want == 0 || room < 3 {
        return DockRows { list: list_want.min(room), detail: 0 };
    }
    let detail = detail_want.min((room - 1) / 2).max(1);
    let list = list_want.min(room - 1 - detail).max(1);
    DockRows { list, detail }
}

/// いまの端末で据え付けが取る行数と、本文が失う行数。
///
/// view では題の行が本文の枠の下辺に乗る（1 行重なる）ので、本文が
/// 失うのは全高より 1 行少ない。source には枠が無いので全高を失う。
pub(crate) fn body_rows_taken(app: &App) -> u16 {
    let (w, h) = crate::app::terminal_size();
    let height = dock_rows(app, w, h).height();
    if height > 0 && app.view_active() {
        height - 1
    } else {
        height
    }
}

/// 一覧の行数（一覧の中のスクロールの窓）。
pub(crate) fn list_rows(app: &App) -> usize {
    let (w, h) = crate::app::terminal_size();
    dock_rows(app, w, h).list.max(1) as usize
}

/// タイトルとフッタに挟まれた `middle` を、本文と据え付けに割る。
pub(crate) fn split(app: &App, middle: Rect) -> (Rect, Option<Dock>) {
    let rows = dock_rows(app, middle.width, middle.height + 2);
    let height = rows.height();
    if height == 0 {
        return (middle, None);
    }
    let dock_y = middle.y + middle.height - height;
    let overlap = u16::from(app.view_active());
    let body = Rect { height: dock_y - middle.y + overlap, ..middle };
    let row = |y: u16, h: u16| Rect { x: middle.x, y, width: middle.width, height: h };
    let title = row(dock_y, 1);
    let list = row(dock_y + 1, rows.list);
    let (rule, detail) = if rows.detail > 0 {
        let y = dock_y + 1 + rows.list;
        (row(y, 1), row(y + 1, rows.detail))
    } else {
        (row(dock_y + 1 + rows.list, 0), row(dock_y + 1 + rows.list, 0))
    };
    let area = row(dock_y, height);
    (body, Some(Dock { area, title, list, rule, detail }))
}

/// いまの端末での据え付けの矩形（マウスの当たり判定用）。
pub(crate) fn current(app: &App) -> Option<Dock> {
    let (w, h) = crate::app::terminal_size();
    let middle = Rect { x: 0, y: 1, width: w, height: h.saturating_sub(2) };
    split(app, middle).1
}

/// 画面の行 `row` の下にある一覧の候補。一覧の外なら `None`。
pub(crate) fn entry_at(app: &App, row: u16) -> Option<usize> {
    let dock = current(app)?;
    let rel = row.checked_sub(dock.list.y)?;
    if rel >= dock.list.height {
        return None;
    }
    let index = rel as usize + app.overlay_offset;
    (index < app.review_candidates.len()).then_some(index)
}

/// **一覧のカーソルの下の候補へ本文を送る** — 候補を本文の中ほどに置き、
/// 本文のカーソルもその候補の先頭の行へ動かす（`Enter` で飛ぶ必要を無くす）。
///
/// 候補が本文の高さより長い（文書全体を範囲にする lint の総評など）
/// ときは、範囲の真ん中ではなく先頭の行を中ほどに置く — 真ん中に寄せると
/// カーソルの行が画面の外に出る。
///
/// 前に `Enter` で作った選択は落とす。選択は別の候補の行を指したまま
/// 残り、フッタが `SELECT` と名乗り続けるからである。
pub(crate) fn follow(app: &mut App) {
    if !is_open(app) {
        return;
    }
    let Some(candidate) = app.review_candidates.get(app.overlay_cursor) else {
        return;
    };
    app.selection = None;
    // **文書全体を範囲にする指摘では本文を動かさない。** 本文には下線も
    // 白抜きの印も無く（[`crate::review::Candidate::is_whole_document`]）、送っても
    // 指す場所が無い。先頭の行へ送ると、一覧の末尾まで読んできた読み手が
    // 文書の頭へ飛ばされる。
    if candidate.is_whole_document(app.source.len()) {
        return;
    }
    let last = app.source.len().saturating_sub(1);
    let start = (candidate.lines.0.saturating_sub(1) as usize).min(last);
    let end = (candidate.lines.1.saturating_sub(1) as usize).min(last).max(start);
    if app.mode == Mode::View {
        let viewport = app.view_viewport_rows();
        app.view.goto_source_line(start);
        app.view.center_source_range(start, end, viewport);
        let row = app.view.cursor_row();
        if row < app.view.offset || row >= app.view.offset + viewport {
            app.view.center_source_range(start, start, viewport);
        }
    } else {
        let viewport = app.source_viewport_rows() as u16;
        app.cursor = start;
        app.center_source_range(start, end, viewport);
        let row = app.row_of(start);
        if row < app.offset || row >= app.offset + viewport as usize {
            app.center_source_range(start, start, viewport);
        }
    }
}

/// 候補の理由を、幅 `width` で折り返した行にする。
///
/// - lint: `message` の全文。linter の複数行の理由は改行のまま行を分ける
/// - Jev のルール: 定義（`review-rules.json` の `text`）の**最初の 1 文**。
///   定義は「下の「対象」は、〜な箇所である。」で始まり、その 1 文が
///   ルールの要点になっている。**短い説明を別に持たない** — 持てば Jev に
///   聞いた文面と人に見せる文面が 2 か所になり、片方だけ直す日が来る
///   （段階 2 が LLM 向けの言い換えを持たなかったのと同じ理由）
pub(crate) fn detail_lines(app: &App, candidate: &Candidate, width: usize) -> Vec<String> {
    let text = match &candidate.finding {
        Finding::Lint { message, .. } => message.trim_end().to_string(),
        Finding::Rule { .. } => app
            .review_rules
            .as_ref()
            .and_then(|rules| rules.get(&candidate.rule))
            .map(|rule| first_sentence(&rule.text).to_string())
            .unwrap_or_default(),
    };
    let mut out = Vec::new();
    for line in text.split('\n') {
        let span = crate::highlight::Span { text: line.to_string(), style: Style::default() };
        for row in crate::highlight::wrap_spans(&[span], width) {
            out.push(row.into_iter().map(|s| s.text).collect::<String>().trim_end().to_string());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 文書全体を範囲にする指摘の、一覧の行と出どころの行での呼び名。
pub(crate) const WHOLE_DOCUMENT: &str = "文書全体";

/// 最初の `。` までの 1 文（無ければ全体）。
fn first_sentence(text: &str) -> &str {
    match text.find('。') {
        Some(at) => &text[..at + '。'.len_utf8()],
        None => text,
    }
}

/// 選んだ候補の出どころの 1 行 — 一覧では落とした `<source>/` と、範囲の行。
fn rule_line(app: &App, candidate: &Candidate) -> String {
    let lines = if candidate.is_whole_document(app.source.len()) {
        format!("{WHOLE_DOCUMENT} L{}–{}", candidate.lines.0, candidate.lines.1)
    } else if candidate.lines.1 > candidate.lines.0 {
        format!("L{}–{}", candidate.lines.0, candidate.lines.1)
    } else {
        format!("L{}", candidate.lines.0)
    };
    match &candidate.finding {
        Finding::Rule { action, score } => {
            let label = app
                .review_rules
                .as_ref()
                .and_then(|rules| rules.get(&candidate.rule))
                .map_or(candidate.rule.clone(), |rule| rule.label.clone());
            format!("{label} {score:.2} · {} · {lines}", action.as_str())
        }
        Finding::Lint { .. } => format!("{} · {lines}", candidate.rule),
    }
}

/// 据え付けを描く。本文と フッタを描いた**あと**に呼ぶ（view では題が
/// 本文の枠の下辺に乗る）。
pub(crate) fn draw(f: &mut Frame, app: &App, dock: Dock) {
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let width = dock.area.width;

    // 題。`comments (3)` と同じ形で、`3/9` は見た本数（✓ と –）/ 一覧の行数（フッタの
    // 読み出しと同じ数）。view では本文の枠の下辺に字だけを乗せる — 罫を
    // 引き直すと枠の色（履歴の色・演出）と食い違う。
    let (decided, total) = app.review_counts();
    let title_text = if app.review_inflight > 0 {
        format!(" review · {} ", app.busy_text(app.review_since))
    } else {
        format!(" review ({decided}/{total}) ")
    };
    let buf = f.buffer_mut();
    if app.view_active() {
        let x = dock.title.x + 2; // 余白 1 ＋ 枠の角 1
        if x < dock.title.x + width {
            buf.set_stringn(x, dock.title.y, &title_text, (width - (x - dock.title.x)) as usize, yellow);
        }
    } else {
        let fill = "─".repeat(width.saturating_sub(title_text.width() as u16 + 1) as usize);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("─", dark_gray),
                Span::styled(title_text, yellow),
                Span::styled(fill, dark_gray),
            ])),
            dock.title,
        );
    }

    // 一覧の面を先に消す。本文は低くなっているので下敷きは無いが、前の
    // フレームの字（窓だった頃・高さが変わる前）を残さない。
    f.render_widget(ratatui::widgets::Clear, Rect {
        y: dock.list.y,
        height: dock.area.height - 1,
        ..dock.area
    });

    if total == 0 {
        // lint が失敗したなら**「0 件」とは言わない**（形の違う出力は
        // 「指摘が無い」ではない）。
        let message = if app.review_inflight > 0 {
            "  asking the analyser…".to_string()
        } else if let Some(error) = app.review_lint_error.as_deref() {
            format!("  {error}")
        } else {
            "  nothing to fix in this document".to_string()
        };
        f.render_widget(Paragraph::new(Span::styled(message, dark_gray)), dock.list);
        return;
    }

    draw_list(f, app, dock.list);

    let Some(candidate) = app.review_candidates.get(app.overlay_cursor) else {
        return;
    };
    if dock.rule.height > 0 {
        let head = format!(" ─ {} ", rule_line(app, candidate));
        let fill = "─".repeat(width.saturating_sub(head.width() as u16) as usize);
        f.render_widget(
            Paragraph::new(Span::styled(
                crate::overlay::clip_if_needed(&format!("{head}{fill}"), width as usize),
                dark_gray,
            )),
            dock.rule,
        );
    }
    if dock.detail.height > 0 {
        let rows = detail_lines(app, candidate, detail_width(width));
        let shown = dock.detail.height as usize;
        let mut lines: Vec<Line> = rows
            .iter()
            .take(shown)
            .map(|text| Line::from(format!("   {text}")))
            .collect();
        // 入り切らない理由は最後の行を `…` で切る（省いたことが分かるように）。
        if rows.len() > shown
            && let Some(last) = lines.last_mut()
        {
            let text = format!("   {}", rows[shown - 1]);
            *last = Line::from(crate::clip_ellipsis(&text, width.saturating_sub(1) as usize));
        }
        f.render_widget(Paragraph::new(lines), dock.detail);
    }
}

/// 一覧。1 行 = `❯ ✓ L42 · Filler 0.87 · <Unit の先頭>`（窓だった頃と同じ）。
/// 残っている候補の `✓` の位置には、ガターと同じ白抜きの重さ（`E` など）が立つ。
///
/// **選んだ行は本文のカーソル帯と同じ背景で塗る。** 一覧の行と本文の
/// 行が同じ色で繋がって見え、「どの候補が本文のどこか」を目で結べる。
/// `❯` は色が無い端末のための印として残す。
fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow);
    let cyan = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let inner = area.width.saturating_sub(1) as usize;
    let visible = area.height as usize;
    let total = app.review_candidates.len();
    let offset = app.overlay_offset.min(total.saturating_sub(visible));
    let states = app.candidate_states();
    let mut lines = Vec::with_capacity(visible);
    for ((entry, candidate), candidate_state) in app
        .review_candidates
        .iter()
        .enumerate()
        .zip(states.iter().copied())
        .skip(offset)
        .take(visible)
    {
        let selected = entry == app.overlay_cursor;
        // Jev のルールは `Filler 0.87`、lint は `<code>` だけ（`<source>/` は
        // 落とす。一覧の幅は理由の文に回し、全体は下の出どころの行に出す）。
        let tag = match &candidate.finding {
            Finding::Rule { score, .. } => {
                let label = app
                    .review_rules
                    .as_ref()
                    .and_then(|rules| rules.get(&candidate.rule))
                    .map(|rule| rule.label.clone())
                    .unwrap_or_else(|| candidate.rule.clone());
                format!("{label} {score:.2}")
            }
            Finding::Lint { .. } => candidate
                .rule
                .split_once('/')
                .map_or(candidate.rule.as_str(), |(_, code)| code)
                .to_string(),
        };
        // 文書全体を範囲にする指摘は `L1` ではなく「文書全体」と名乗る —
        // `L1` と書くと 1 行目の指摘に読める。
        let place = if candidate.is_whole_document(app.source.len()) {
            WHOLE_DOCUMENT.to_string()
        } else {
            format!("L{}", candidate.lines.0)
        };
        let lead = format!(" {} ", if selected { CURSOR_GLYPH } else { " " });
        let head = format!(" {place} · {tag} · ");
        let cols = match candidate.finding {
            Finding::Rule { .. } => crate::overlay::REVIEW_HEAD_COLS,
            Finding::Lint { .. } => usize::MAX,
        };
        let body = crate::review::head_of(
            &app.source.content,
            candidate,
            inner.saturating_sub(lead.width() + 1 + head.width()).min(cols),
        );
        // 見たものは沈める（`✓` も `–` も）— 残っているのは記録であって作業ではない。
        let done = !candidate_state.is_pending();
        let (head_style, body_style) = if selected {
            (cyan, Style::default().fg(Color::White))
        } else if done {
            (dark_gray, dark_gray)
        } else {
            (yellow, Style::default().fg(Color::Gray))
        };
        // **状態の 1 マス**: 残っている候補は本文のガターと同じ白抜きの
        // 重さ（`E` / `W` / `I`、[`crate::decoration::ReviewSeverity::badge`]）、
        // 見たものは `✓` / `–`。1 マスを入れ替えるだけなので桁は増えない。
        let state = if candidate_state.is_pending() {
            candidate.severity().badge(app.decoration_styles.page_bg())
        } else {
            (candidate_state.mark(), head_style)
        };
        lines.push(Line::from(vec![
            Span::styled(lead.clone(), head_style),
            Span::styled(state.0, state.1),
            Span::styled(head, head_style),
            Span::styled(body, body_style),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
    // 帯は行の端まで（字の無いところも）塗る。白抜きの地は重さなので、
    // 帯の上から塗り直す。
    if let Some(rel) = app.overlay_cursor.checked_sub(offset)
        && rel < visible
    {
        let band = Rect { y: area.y + rel as u16, height: 1, ..area };
        f.buffer_mut().set_style(band, Style::default().bg(app.ui_selected_bg));
        if let Some(candidate) = app.review_candidates.get(app.overlay_cursor)
            && states.get(app.overlay_cursor).is_some_and(|state| state.is_pending())
            && let Some(cell) = f.buffer_mut().cell_mut((area.x + LIST_STATE_COL, band.y))
        {
            cell.set_style(candidate.severity().badge(app.decoration_styles.page_bg()).1);
        }
    }
}

/// 一覧の行で状態の 1 マス（白抜きの重さ / `✓` / `–`）が立つ桁 —
/// 行頭の余白 1 と `❯ ` の 2 のあと。
const LIST_STATE_COL: u16 = 3;

#[cfg(test)]
mod tests {
    use super::first_sentence;

    #[test]
    fn the_first_sentence_ends_at_the_first_full_stop() {
        assert_eq!(first_sentence("一文目である。二文目。"), "一文目である。");
        assert_eq!(first_sentence("句点が無い"), "句点が無い");
    }
}
