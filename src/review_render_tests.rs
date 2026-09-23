//! **Review の指摘の描き方**（下線の範囲・重さの色・波線・文書全体の指摘）。
//!
//! 実際に `crate::draw` で `TestBackend` に描き、セルを見て固定する:
//!
//! 1. **下線は範囲の文字だけに乗る** — 引用の行の 1 字（view / source）、
//!    折り返しをまたぐ範囲（行頭の余白には乗らない）、表のセル（上位集合 —
//!    断片ごと）
//! 2. **重なりの順序** — marks の琥珀・カーソル帯・選択帯・コメントの印の
//!    下でも下線は残り、地色は帯が勝つ（琥珀の句は帯の上で濃い琥珀）
//! 3. **重さの色** — 下線はパレットの赤・黄・青緑（`58;5;N`）、ガターは
//!    同じ色の地に紙の色で抜いた `E` / `W` / `I`（行ではいちばん重いもの）。
//!    範囲の行だけに立つ（1 段落に畳まれた引用で、範囲の無い行には立たない）
//! 4. **文書全体を範囲にする指摘** — 一覧の末尾、本文に下線も印も無い
//! 5. **波線** — 描いたセルを出口（[`crate::undercurl::draw`]）に通すと
//!    `CSI 4:3 m` が範囲の字の直前に出る
//!
//! 文書は作り物（業務文書を使わない）。

use crate::*;

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use crate::app::{ReviewAnswer, ReviewMessage, TEST_TERMINAL_SIZE};
use crate::config::{Config, EscQuit};
use crate::decoration::{CURL_CARRIER, Decoration, DecorationKind, ReviewSeverity};
use crate::highlight::Highlighter;
use crate::ime::ImeMode;
use crate::source::Source;
use crate::view::ViewState;

const W: u16 = 60;
const H: u16 = 40;

/// 引用・折り返す段落・表・後ろの段落。尻を伸ばして 20 行を超えさせる
/// （文書全体の指摘の閾値を、1 文の指摘がまたがない大きさにする）。
fn doc() -> String {
    let mut doc = String::from(
        "# 見出し\n\
         \n\
         > 主任「半分は、通知に気づいていないだけ。\n\
         > 通知の出し方を変える方が効くかもしれない」\n\
         \n\
         折り返しの段落である。ここから先は長い一文で、幅六十の端末では一行に収まらないので二行目へ送られ、その先でようやく終わる。\n\
         \n\
         | 列 | 説明 |\n\
         | --- | --- |\n\
         | a | セルの中の弱い表現かも |\n\
         \n",
    );
    for i in 0..16 {
        doc.push_str(&format!("後ろの段落 {i:02}。\n\n"));
    }
    doc
}

struct Size;
impl Size {
    fn set() -> Size {
        TEST_TERMINAL_SIZE.with(|cell| cell.set(Some((W, H))));
        Size
    }
}
impl Drop for Size {
    fn drop(&mut self) {
        TEST_TERMINAL_SIZE.with(|cell| cell.set(None));
    }
}

fn lint_app(dir: &std::path::Path, doc: &str, mode: Mode, decorations: Vec<Decoration>) -> App {
    let path = dir.join("doc.md");
    std::fs::write(&path, doc).unwrap();
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: Some("base16-ocean.dark".into()),
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: false,
        semantic: None,
        semantic_cmd: None,
        marks_questions: None,
        review_rules: None,
        review_json: false,
        lint_cmd: None,
        undercurl: Default::default(),
        decoration_blend: Default::default(),
        decorations,
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, view_render_width(W), &highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.spans = app.highlight.highlight_with(&app.source.content, syntax_for(&app.files[0]));
    app.review_dismissed_store = Some(crate::review::DismissedStore::at(dir.join("store")));
    app.lint = Some(crate::lint::LintCommand::new("true"));
    app.review_armed = true;
    app.mode = mode;
    app
}

/// `needle` の中の `pick` の範囲（1 行の中）を指す Diagnostic。
fn diag(doc: &str, needle: &str, pick: &str, code: &str, severity: u8) -> serde_json::Value {
    let at = doc.find(needle).unwrap() + needle.find(pick).unwrap();
    diag_bytes(doc, at, at + pick.len(), code, Some(severity))
}

/// バイト範囲 `start..end` の Diagnostic（行をまたいでよい）。
fn diag_bytes(doc: &str, start: usize, end: usize, code: &str, severity: Option<u8>) -> serde_json::Value {
    let pos = |at: usize| {
        let line = doc[..at].matches('\n').count();
        let line_start = doc[..at].rfind('\n').map_or(0, |i| i + 1);
        let character: usize = doc[line_start..at].chars().map(char::len_utf16).sum();
        serde_json::json!({"line": line, "character": character})
    };
    let mut d = serde_json::json!({
        "range": {"start": pos(start), "end": pos(end)},
        "message": format!("{code} の理由"), "source": "textlint", "code": code,
    });
    if let Some(severity) = severity {
        d["severity"] = severity.into();
    }
    d
}

fn deliver(app: &mut App, diagnostics: Vec<serde_json::Value>) {
    let json = serde_json::json!({ "diagnostics": diagnostics }).to_string();
    let parsed = crate::lint::parse(&json, &app.source.content).expect("形どおり");
    app.review_inflight = app.review_inflight.max(1);
    app.accept_review_analysis(ReviewMessage {
        generation: app.review_generation,
        answer: ReviewAnswer::Lint(Ok(parsed)),
    });
}

fn paint(app: &mut App) -> Buffer {
    let backend = ratatui::backend::TestBackend::new(W, H);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    // 1 度目で source の行の cache が温まる。
    terminal.draw(|f| crate::draw(f, app)).unwrap();
    terminal.draw(|f| crate::draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// 1 行の字とそのセルの x（全角の 2 桁目は飛ばす）。
fn glyphs(buf: &Buffer, y: u16) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    let mut x = 0;
    while x < buf.area().width {
        let s = buf[(x, y)].symbol().to_string();
        let w = unicode_width::UnicodeWidthStr::width(s.as_str()).max(1) as u16;
        out.push((x, s));
        x += w;
    }
    out
}

/// **Review の下線**の乗ったセル（`(x, y)` と字）。見出しの下線と
/// 見分けるため、波線の印（[`CURL_CARRIER`]）で数える。
fn underlined(buf: &Buffer) -> Vec<(u16, u16, String)> {
    let mut out = Vec::new();
    for y in 0..buf.area().height {
        for (x, s) in glyphs(buf, y) {
            let cell = &buf[(x, y)];
            if cell.modifier.contains(Modifier::UNDERLINED | CURL_CARRIER) {
                out.push((x, y, s));
            }
        }
    }
    out
}

fn underlined_text(buf: &Buffer) -> String {
    underlined(buf).into_iter().map(|(_, _, s)| s).collect()
}

/// 画面の字のうち `needle` の最初の字のセル。
fn cell_of(buf: &Buffer, needle: &str) -> (u16, u16) {
    for y in 0..buf.area().height {
        let row = glyphs(buf, y);
        let text: String = row.iter().map(|(_, s)| s.as_str()).collect();
        if let Some(at) = text.find(needle) {
            // バイト位置 → セル: 前の字を足していって一致したところ。
            let mut acc = 0;
            for (x, s) in &row {
                if acc == at {
                    return (*x, y);
                }
                acc += s.len();
            }
        }
    }
    panic!("画面に {needle:?} が無い");
}

fn row_text(buf: &Buffer, y: u16) -> String {
    glyphs(buf, y).into_iter().map(|(_, s)| s).collect()
}

/// 本文のカーソルを文書の頭へ戻す（候補を選ぶと本文がその行へ送られ、
/// カーソル帯や `>` がその行に乗るため）。
fn park_cursor(app: &mut App) {
    app.view.goto_source_line(0);
    app.cursor = 0;
}

// ---- 1. 範囲の文字だけ ------------------------------------------------------

/// **72 行目の「かも」の形** — linter が 1 字（「か」）を返したら、引用の
/// 塊ではなくその 1 字だけに線が乗る。view と source の両方。
#[test]
fn a_one_character_diagnostic_underlines_one_character() {
    let _size = Size::set();
    for mode in [Mode::View, Mode::Source] {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc();
        let mut app = lint_app(dir.path(), &doc, mode, Vec::new());
        paint(&mut app);
        crate::overlay::open_review(&mut app);
        deliver(&mut app, vec![diag(&doc, "効くかもしれない", "か", "ja-no-weak-phrase", 2)]);
        let buf = paint(&mut app);
        assert_eq!(underlined_text(&buf), "か", "{mode:?}");
    }
}

/// **折り返しをまたぐ範囲** — 範囲の字だけに乗り、2 行目の行頭の余白
/// （hanging pad・ガター）には乗らない。
#[test]
fn a_range_across_the_wrap_underlines_exactly_its_characters() {
    let _size = Size::set();
    for mode in [Mode::View, Mode::Source] {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc();
        let mut app = lint_app(dir.path(), &doc, mode, Vec::new());
        paint(&mut app);
        crate::overlay::open_review(&mut app);
        let start = doc.find("ここから先は").unwrap();
        let end = doc.find("ようやく").unwrap();
        deliver(&mut app, vec![diag_bytes(&doc, start, end, "sentence-length", Some(2))]);
        let buf = paint(&mut app);
        let cells = underlined(&buf);
        let rows: std::collections::BTreeSet<u16> = cells.iter().map(|(_, y, _)| *y).collect();
        assert!(rows.len() >= 2, "{mode:?}: 折り返しをまたいでいない {rows:?}");
        let want: String = doc[start..end].chars().filter(|c| !c.is_whitespace()).collect();
        let got: String = underlined_text(&buf).chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(got, want, "{mode:?}");
        // どの行でも、線の始まりは字である（余白から線が始まらない）。
        for y in rows {
            let first = cells.iter().find(|(_, cy, _)| *cy == y).unwrap();
            assert!(!first.2.trim().is_empty(), "{mode:?}: 行 {y} の線が余白から始まる: {:?}", row_text(&buf, y));
        }
    }
}

/// **表のセル**（上位集合の span）— 範囲の字だけには切れないので、触れた
/// 断片ごと引く（見失うよりよい）。隣のセルには乗らない。
#[test]
fn a_diagnostic_inside_a_table_cell_underlines_that_fragment_only() {
    let _size = Size::set();
    let dir = tempfile::tempdir().unwrap();
    let doc = doc();
    let mut app = lint_app(dir.path(), &doc, Mode::View, Vec::new());
    paint(&mut app);
    crate::overlay::open_review(&mut app);
    deliver(&mut app, vec![diag(&doc, "弱い表現かも", "か", "ja-no-weak-phrase", 2)]);
    let buf = paint(&mut app);
    let text = underlined_text(&buf);
    assert!(text.contains('か'), "セルの中の指摘が本文に出ない: {text:?}");
    assert!(!text.contains('a'), "隣のセルに乗った: {text:?}");
    assert!(!text.contains("列") && !text.contains("説明"), "見出しの行に乗った: {text:?}");
}

// ---- 2. 重なりの順序 ------------------------------------------------------

/// marks の琥珀・カーソル帯・選択帯・コメントの印。どれの下でも下線は
/// 残り、地色は後から塗る側（琥珀 → 帯）が勝つ。琥珀の句は帯の上で
/// 濃い琥珀（`mark_band_bg`）になり、確定色ではなくなる。
#[test]
fn the_underline_survives_the_amber_the_bands_and_a_comment() {
    let _size = Size::set();
    let doc = doc();
    let quote = doc.find("通知の出し方").unwrap();
    let line_end = quote + doc[quote..].find('\n').unwrap();
    let amber = vec![Decoration { range: quote..line_end, kind: DecorationKind::SemanticMark }];
    let quote_line = doc[..quote].matches('\n').count();
    for mode in [Mode::View, Mode::Source] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = lint_app(dir.path(), &doc, mode, amber.clone());
        paint(&mut app);
        crate::overlay::open_review(&mut app);
        deliver(&mut app, vec![diag(&doc, "効くかもしれない", "か", "ja-no-weak-phrase", 2)]);
        let styles = app.decoration_styles;
        park_cursor(&mut app);

        // 琥珀の上: 地色は琥珀、線も残る。
        let buf = paint(&mut app);
        let (x, y) = cell_of(&buf, "かもしれない");
        let ka = &buf[(x, y)];
        assert!(ka.modifier.contains(Modifier::UNDERLINED), "{mode:?}: 琥珀の上で線が消えた");
        assert_eq!(ka.bg, styles.mark_bg(), "{mode:?}: 琥珀の地色が消えた");
        assert_eq!(ka.underline_color, styles.review_underline(ReviewSeverity::Warning));
        let (mx, my) = cell_of(&buf, "もしれない");
        assert!(!buf[(mx, my)].modifier.contains(Modifier::UNDERLINED), "{mode:?}: 範囲の外に線");
        assert_eq!(buf[(mx, my)].bg, styles.mark_bg(), "{mode:?}: 琥珀は範囲の外にも残る");

        // カーソル帯: 琥珀の句は帯の上の濃い琥珀になり、線は残る。
        match mode {
            Mode::View => app.view.goto_source_line(quote_line),
            _ => app.cursor = quote_line,
        }
        let buf = paint(&mut app);
        let (x, y) = cell_of(&buf, "かもしれない");
        let ka = &buf[(x, y)];
        assert!(ka.modifier.contains(Modifier::UNDERLINED), "{mode:?}: カーソル帯の下で線が消えた");
        assert_eq!(ka.bg, styles.mark_band_bg(), "{mode:?}: カーソル帯の上で琥珀の句が濃い琥珀でない");

        // 選択帯。
        app.selection = Some(crate::comment::Selection::new(quote_line));
        let buf = paint(&mut app);
        let (x, y) = cell_of(&buf, "かもしれない");
        assert!(buf[(x, y)].modifier.contains(Modifier::UNDERLINED), "{mode:?}: 選択帯の下で線が消えた");
        assert_eq!(buf[(x, y)].bg, styles.mark_band_bg(), "{mode:?}: 選択帯の上で琥珀の句が濃い琥珀でない");
        app.selection = None;

        // コメントの印: 同じ行にコメントがあっても、Pending の線は残る。
        app.comments.push(crate::comment::Comment {
            file_path: app.files[0].clone(),
            start: quote_line as u32 + 1,
            end: quote_line as u32 + 1,
            lines: doc[quote..line_end].to_string(),
            revision: None,
            anchor: None,
            text: "人の赤入れ".into(),
        });
        park_cursor(&mut app);
        let buf = paint(&mut app);
        let (x, y) = cell_of(&buf, "かもしれない");
        assert!(buf[(x, y)].modifier.contains(Modifier::UNDERLINED), "{mode:?}: コメントの行で線が消えた");
        app.comments.clear();
    }
}

// ---- 3. 重さの色 ----------------------------------------------------------

/// 下線の色は重さごとにパレットの色（赤・黄・青緑）で、ガターは同じ色の地に
/// 紙の色で字を抜いた `E` / `W` / `I`（行でいちばん重いもの）。
#[test]
fn severity_colours_the_underline_and_the_gutter_mark() {
    use ratatui::style::Color;
    let _size = Size::set();
    for mode in [Mode::View, Mode::Source] {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc();
        let mut app = lint_app(dir.path(), &doc, mode, Vec::new());
        paint(&mut app);
        crate::overlay::open_review(&mut app);
        deliver(
            &mut app,
            vec![
                diag(&doc, "効くかもしれない", "か", "weak", 3),
                diag(&doc, "効くかもしれない", "効", "worse", 1),
                diag(&doc, "後ろの段落 03", "段落", "warn", 2),
                diag(&doc, "後ろの段落 05", "段落", "none", 0),
            ],
        );
        let paper = app.decoration_styles.page_bg();
        park_cursor(&mut app);
        let buf = paint(&mut app);
        let color_of = |needle: &str| buf[cell_of(&buf, needle)].underline_color;
        assert_eq!(color_of("効くかも"), Color::Red, "{mode:?}");
        assert_eq!(color_of("かもしれ"), Color::Cyan, "{mode:?}");
        assert_eq!(color_of("段落 03"), Color::Yellow, "{mode:?}");
        // 知らない重さは Info（色分けの前の青緑）。
        assert_eq!(color_of("段落 05"), Color::Cyan, "{mode:?}");

        // ガターの白抜き: 引用の行は Error と Info が乗る → 赤地の `E`。
        for (needle, letter, bg) in
            [("効くかも", "E", Color::Red), ("段落 03", "W", Color::Yellow), ("段落 05", "I", Color::Cyan)]
        {
            let (_, y) = cell_of(&buf, needle);
            let (x, cell) = gutter_badge(&buf, y).unwrap_or_else(|| {
                panic!("{mode:?}: {needle:?} の行に白抜きが無い: {:?}", row_text(&buf, y))
            });
            assert_eq!(cell.symbol(), letter, "{mode:?}: {needle:?}");
            assert_eq!(cell.bg, bg, "{mode:?}: {needle:?} の地");
            assert_eq!(cell.fg, paper, "{mode:?}: {needle:?} の字は紙の色");
            assert!(cell.modifier.contains(Modifier::BOLD), "{mode:?}: {needle:?}");
            // `!` はもうガターに出ない（`!` は「まだ見ていない変更」だけ）。
            assert!((0..W).all(|x| buf[(x, y)].symbol() != "!"), "{mode:?}: {needle:?} の行に ! が残った");
            let _ = x;
        }

        // 一覧の行頭にも同じ白抜きが立つ（`✓` の桁）。
        let listed = (0..H)
            .find(|&y| row_text(&buf, y).contains("· warn ·"))
            .unwrap_or_else(|| panic!("{mode:?}: 一覧に warn の行が無い"));
        let state = &buf[(3, listed)];
        assert_eq!((state.symbol(), state.bg, state.fg), ("W", Color::Yellow, paper), "{mode:?}: 一覧の行頭");
    }
}

/// 白抜きの地は重さの色のまま — 選択帯の上でも塗り替わらない。印の優先順
/// （カーソルの `>`・選択の `▌` が上）は今のまま。
#[test]
fn the_badge_keeps_its_ground_under_the_bands() {
    use ratatui::style::Color;
    let _size = Size::set();
    let doc = doc();
    let para = doc.find("後ろの段落 03").unwrap();
    let para_line = doc[..para].matches('\n').count();
    for mode in [Mode::View, Mode::Source] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = lint_app(dir.path(), &doc, mode, Vec::new());
        paint(&mut app);
        crate::overlay::open_review(&mut app);
        deliver(&mut app, vec![diag(&doc, "後ろの段落 03", "段落", "warn", 2)]);
        park_cursor(&mut app);
        app.selection = Some(crate::comment::Selection::new(para_line));
        let buf = paint(&mut app);
        let (_, y) = cell_of(&buf, "段落 03");
        match mode {
            // view: 選択の行は選択の `▌` が勝つ（今の優先順）。
            Mode::View => assert!(gutter_badge(&buf, y).is_none(), "view: 選択の印より上に出た"),
            // source: 選択に印は無く白抜きが立つ — 地は選択帯の色に塗り替わらない。
            _ => {
                let (_, cell) = gutter_badge(&buf, y).expect("source: 選択の行で白抜きが消えた");
                assert_eq!(cell.bg, Color::Yellow, "source: 選択帯に塗り替わった");
            }
        }
        app.selection = None;
        let buf = paint(&mut app);
        let (_, cell) = gutter_badge(&buf, y).expect("白抜きが無い");
        assert_eq!(cell.bg, Color::Yellow, "{mode:?}");
    }
}

/// 本文の左端（ガター）の白抜きの印 — 重さの色の地に `E` / `W` / `I`。
/// 一覧の行頭（x = 3）は含めない。塗りが 1 マスで終わるものだけ — フッタの
/// `REVIEW` バッジ（青緑の地が 8 マス続く）の `E` を拾わないため。
fn gutter_badge(buf: &Buffer, y: u16) -> Option<(u16, ratatui::buffer::Cell)> {
    use ratatui::style::Color;
    (0..3).find_map(|x| {
        let cell = &buf[(x, y)];
        (["E", "W", "I"].contains(&cell.symbol())
            && [Color::Red, Color::Yellow, Color::Cyan].contains(&cell.bg)
            && buf[(x + 1, y)].bg != cell.bg)
            .then(|| (x, cell.clone()))
    })
}

// ---- 4. 文書全体を範囲にする指摘 ----------------------------------------

/// 総評（L1–末尾）は一覧の末尾で「文書全体」と名乗り、本文には下線も
/// 白抜きも出さない。ほかの指摘の線はそのまま。
#[test]
fn a_whole_document_diagnostic_goes_last_and_leaves_the_text_clean() {
    let _size = Size::set();
    for mode in [Mode::View, Mode::Source] {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc();
        let mut app = lint_app(dir.path(), &doc, mode, Vec::new());
        paint(&mut app);
        crate::overlay::open_review(&mut app);
        deliver(
            &mut app,
            vec![
                diag_bytes(&doc, 0, doc.len(), "ai-tech-writing-guideline", Some(1)),
                diag(&doc, "効くかもしれない", "か", "ja-no-weak-phrase", 2),
            ],
        );
        assert_eq!(app.review_candidates.len(), 2);
        assert_eq!(app.review_candidates[0].rule, "textlint/ja-no-weak-phrase", "{mode:?}: 総評が先頭");
        assert_eq!(app.review_candidates[1].rule, "textlint/ai-tech-writing-guideline");
        park_cursor(&mut app);
        let buf = paint(&mut app);
        assert_eq!(underlined_text(&buf), "か", "{mode:?}: 総評が本文に線を引いた");
        // 白抜きは「か」の乗る行だけ。view では 2 行の引用が 1 段落に畳まれて
        // 2 行に折り返すが、範囲の無い 1 行目（「主任」）には立たない。
        let badges: Vec<String> = (0..H)
            .filter(|&y| gutter_badge(&buf, y).is_some())
            .map(|y| row_text(&buf, y))
            .collect();
        assert_eq!(badges.len(), 1, "{mode:?}: 白抜きの行の数: {badges:?}");
        assert!(badges[0].contains("効く"), "{mode:?}: 範囲の行でない: {}", badges[0]);
        let listed: Vec<String> = (0..H).map(|y| row_text(&buf, y)).collect();
        let whole = listed
            .iter()
            .position(|row| row.contains(crate::review_dock::WHOLE_DOCUMENT))
            .unwrap_or_else(|| panic!("{mode:?}: 一覧に「文書全体」が無い:\n{}", listed.join("\n")));
        let weak = listed.iter().position(|row| row.contains("L4 · ja-no-weak-phrase")).unwrap();
        assert!(weak < whole, "{mode:?}: 総評が末尾でない");
        assert!(!listed[whole].contains(" L1 "), "{mode:?}: L1 と名乗った: {}", listed[whole]);
    }
}

/// 閾値の境: 9 割を覆えば文書全体、そうでなければ普通の指摘。
#[test]
fn the_whole_document_threshold_is_nine_tenths_of_the_lines() {
    use crate::review::{Candidate, Finding};
    let candidate = |lines: (u32, u32)| Candidate {
        range: 0..1,
        lines,
        rule: "t/r".into(),
        finding: Finding::Lint { message: String::new(), severity: None },
        atoms: std::iter::once(0..1).collect(),
    };
    assert!(candidate((1, 192)).is_whole_document(192));
    assert!(candidate((1, 173)).is_whole_document(192), "173/192 = 0.901");
    assert!(!candidate((1, 172)).is_whole_document(192), "172/192 = 0.896");
    assert!(!candidate((8, 10)).is_whole_document(192));
    assert!(!candidate((1, 2)).is_whole_document(2), "2 行の文書の 1 文は総評ではない");
    assert!(candidate((1, 3)).is_whole_document(3));
}

// ---- 5. 波線 --------------------------------------------------------------

/// 描いたセルを出口に通すと、範囲の字の直前に `CSI 4:3 m` が出る。
/// `--undercurl off` にあたる出口では 1 本線で、どちらでも点滅は出ない。
#[test]
fn the_painted_underline_leaves_as_a_curl_through_the_exit() {
    use ratatui::style::Color;
    let _size = Size::set();
    let dir = tempfile::tempdir().unwrap();
    let doc = doc();
    let mut app = lint_app(dir.path(), &doc, Mode::View, Vec::new());
    paint(&mut app);
    crate::overlay::open_review(&mut app);
    deliver(&mut app, vec![diag(&doc, "効くかもしれない", "か", "ja-no-weak-phrase", 1)]);
    let buf = paint(&mut app);
    let (x, y) = cell_of(&buf, "かもしれない");
    assert!(buf[(x, y)].modifier.contains(CURL_CARRIER), "下線のセルに波線の印が無い");
    let area = buf.area();
    let cells: Vec<(u16, u16, &ratatui::buffer::Cell)> = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .map(|(x, y)| (x, y, &buf[(x, y)]))
        .collect();
    let exit = |undercurl: bool| {
        let mut out = Vec::new();
        crate::undercurl::draw(&mut out, cells.iter().copied(), undercurl).unwrap();
        String::from_utf8(out).unwrap()
    };
    let curly = exit(true);
    // 波線の開始から「か」までに字が挟まらない。
    let curl = curly.find("\x1b[4:3m").expect("波線が出ていない");
    let tail = &curly[curl..];
    let printed: String = strip_escapes(tail).chars().take(1).collect();
    assert_eq!(printed, "か", "波線の直後の字が「か」でない");
    assert_eq!(curly.matches("\x1b[4:3m").count(), 1, "波線は 1 か所だけ");
    // 下線の色はパレット番号（error = 赤 = 1）。RGB（`58;2;…`）では出さない。
    assert!(curly.contains("\x1b[58;5;1m"), "下線の色がパレット番号でない");
    assert!(!curly.contains("58;2;"), "下線の色が RGB で出た");
    // ガターの白抜き: 赤の地（`48;5;1`）に紙の色の字で `E`。
    let Color::Rgb(r, g, b) = app.decoration_styles.page_bg() else { panic!("紙は RGB") };
    let badge = format!("\x1b[38;2;{r};{g};{b};48;5;1mE");
    assert!(curly.contains(&badge), "白抜きの色と字が出ていない: {badge:?}");
    let plain = exit(false);
    assert!(!plain.contains("4:3"));
    for out in [&curly, &plain] {
        assert!(!out.contains("\x1b[5m") && !out.contains("\x1b[6m"), "印が点滅として漏れた");
    }
}

/// エスケープ列（CSI … 終端文字）を落とした字だけ。
fn strip_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}
