//! **Review の一覧を本文の下に据え付ける**（[`crate::review_dock`]）。
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節「UI」。ここで固定するのは:
//!
//! 1. **一覧の下に字が隠れない** — 本文の高さの計算と、描いた本文の領域が
//!    同じ行数である（view / source、高さ 20 / 40 / 60）
//! 2. **j で選んだ候補の行が本文に見えている**（中ほどに来る）
//! 3. **クリックが正しい行に当たる** — 本文の行も、一覧の行も
//! 4. **狭い端末で崩れない** — 高さ 20 で本文が残り、それより低くても、
//!    幅を変えても落ちない
//! 5. **理由の欄** — lint は `message` の全文（改行はそのまま）、Jev は定義の
//!    最初の 1 文
//! 6. **閉じれば全画面の本文に戻る**
//!
//! 端末の大きさは [`crate::app::TEST_TERMINAL_SIZE`] で `TestBackend` と
//! 揃える（`cargo test` は端末の中で走ると本物の大きさを拾う）。

use crate::*;

use crate::app::{ReviewAnswer, ReviewMessage, TEST_TERMINAL_SIZE};
use crate::config::{Config, EscQuit};
use crate::highlight::Highlighter;
use crate::ime::ImeMode;
use crate::source::Source;
use crate::view::ViewState;

/// 1 段落 = 1 行の長い文書。`p007` のような ASCII の札で行を探せる。
fn long_doc() -> String {
    (0..120)
        .map(|i| format!("p{i:03} 本文の段落である。\n\n"))
        .collect()
}

/// 候補を置く段落の番号（文書の頭から尻まで散らす）。
const HITS: [usize; 12] = [2, 9, 17, 30, 44, 51, 63, 70, 82, 95, 108, 119];

/// 端末の大きさを `TestBackend` と揃えて、テストの間だけ固定する。
struct Size;
impl Size {
    fn set(w: u16, h: u16) -> Size {
        TEST_TERMINAL_SIZE.with(|cell| cell.set(Some((w, h))));
        Size
    }
}
impl Drop for Size {
    fn drop(&mut self) {
        TEST_TERMINAL_SIZE.with(|cell| cell.set(None));
    }
}

fn lint_app(dir: &std::path::Path, doc: &str, mode: Mode) -> App {
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
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let (w, _) = crate::app::terminal_size();
    let view = ViewState::render(&source, view_render_width(w), &highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.spans = app.highlight.highlight_with(&app.source.content, syntax_for(&app.files[0]));
    app.review_dismissed_store = Some(crate::review::DismissedStore::at(dir.join("store")));
    app.lint = Some(crate::lint::LintCommand::new("true"));
    app.review_armed = true;
    app.mode = mode;
    app
}

fn diagnostic(doc: &str, text: &str, code: &str, message: &str) -> serde_json::Value {
    let at = doc.find(text).unwrap();
    let line = doc[..at].matches('\n').count();
    let width: usize = text.chars().map(char::len_utf16).sum();
    serde_json::json!({
        "range": {"start": {"line": line, "character": 0},
                  "end": {"line": line, "character": width}},
        "message": message, "source": "textlint", "code": code, "severity": 2,
    })
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

fn hits(doc: &str) -> Vec<serde_json::Value> {
    HITS.iter()
        .map(|i| diagnostic(doc, &format!("p{i:03}"), "ja-no-weak-phrase", "弱い表現: \"かも\" が使われています。"))
        .collect()
}

fn screen_rows(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect()
}

/// 開いて答えを渡し、1 度描いた App（source の行の cache も温まる）。
fn opened(dir: &std::path::Path, mode: Mode, w: u16, h: u16) -> (App, String) {
    let doc = long_doc();
    let mut app = lint_app(dir, &doc, mode);
    screen_rows(&mut app, w, h);
    crate::overlay::open_review(&mut app);
    deliver(&mut app, hits(&doc));
    assert_eq!(app.review_candidates.len(), HITS.len());
    (app, doc)
}

fn dock_of(app: &App, w: u16, h: u16) -> crate::review_dock::Dock {
    let middle = Rect { x: 0, y: 1, width: w, height: h - 2 };
    crate::review_dock::split(app, middle).1.expect("据え付けがある")
}

/// 本文の中身の行（view は枠の上辺の下から、source はタイトルの下から）。
fn body_rows(app: &App, dock: crate::review_dock::Dock) -> std::ops::Range<u16> {
    if app.view_active() { 2..dock.title.y } else { 1..dock.title.y }
}

// ---- 1. 字が隠れない ----------------------------------------------------

/// 本文の高さの計算（スクロール・`keep_cursor_visible` が使う）と、描いた
/// 本文の領域が同じ行数である。ずれれば、カーソルが一覧の下に潜る。
#[test]
fn the_viewport_is_exactly_the_body_that_is_drawn() {
    for mode in [Mode::View, Mode::Source] {
        for h in [20u16, 24, 40, 60] {
            let _size = Size::set(80, h);
            let dir = tempfile::tempdir().unwrap();
            let (mut app, _) = opened(dir.path(), mode, 80, h);
            let rows = screen_rows(&mut app, 80, h);
            let dock = dock_of(&app, 80, h);
            let viewport = if mode == Mode::View {
                app.view_viewport_rows()
            } else {
                app.source_viewport_rows()
            };
            let body = body_rows(&app, dock);
            assert_eq!(viewport, body.len(), "{mode:?} 高さ {h}");
            // 本文の最後の行は本文の字で、一覧ではない。
            let last = &rows[body.end as usize - 1];
            assert!(!last.contains('▸') && !last.contains("L5"), "{mode:?} {h}: {last}");
            assert!(rows[dock.title.y as usize].contains("review (0/12)"), "{mode:?} {h}: 題\n{}", rows.join("\n"));
        }
    }
}

// ---- 2. 選んだ候補が本文に見えている ---------------------------------------

#[test]
fn each_j_brings_the_candidate_into_the_middle_of_the_body() {
    for mode in [Mode::View, Mode::Source] {
        for h in [20u16, 40, 60] {
            let _size = Size::set(80, h);
            let dir = tempfile::tempdir().unwrap();
            let (mut app, _) = opened(dir.path(), mode, 80, h);
            for (step, hit) in HITS.iter().enumerate() {
                if step > 0 {
                    on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
                }
                assert_eq!(app.overlay_cursor, step);
                let rows = screen_rows(&mut app, 80, h);
                let dock = dock_of(&app, 80, h);
                let body = body_rows(&app, dock);
                let tag = format!("p{hit:03}");
                let at = body
                    .clone()
                    .find(|y| rows[*y as usize].contains(&tag))
                    .unwrap_or_else(|| panic!("{mode:?} 高さ {h} の {step} 回目: {tag} が本文に無い\n{}", rows.join("\n")));
                // 本文のカーソルも候補の行へ（`L` の位置）。
                let line = 2 * hit; // 1 段落 = 本文 1 行 ＋ 空行
                let cursor = if mode == Mode::View { app.view.cursor } else { app.cursor };
                assert_eq!(cursor, line, "{mode:?} {h} {step}");
                // 文書の端でなければ中ほど（±1 行）に来る。
                let middle = body.start + body.len() as u16 / 2;
                let (offset, max) = if mode == Mode::View {
                    let v = app.view_viewport_rows();
                    (app.view.offset, app.view.rows.len().saturating_sub(v))
                } else {
                    (app.offset, app.max_offset(app.source_viewport_rows() as u16))
                };
                let edge = offset == 0 || offset == max;
                if !edge {
                    assert!(at.abs_diff(middle) <= 1, "{mode:?} {h} {step}: {at} vs {middle}");
                }
                // 一覧でも選んだ行が見えている。
                let row = &rows[(dock.list.y as usize)..(dock.list.y + dock.list.height) as usize];
                assert!(row.iter().any(|r| r.contains('▸') && r.contains(&format!("L{}", line + 1))), "{mode:?} {h} {step}");
            }
        }
    }
}

// ---- 3. クリックが正しい行に当たる -----------------------------------------

fn click(app: &mut App, row: u16, col: u16) {
    on_mouse(
        app,
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE },
    );
    on_mouse(
        app,
        MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE },
    );
}

#[test]
fn a_click_on_the_body_lands_on_the_line_drawn_there_and_keeps_the_list() {
    for mode in [Mode::View, Mode::Source] {
        let (w, h) = (80u16, 24u16);
        let _size = Size::set(w, h);
        let dir = tempfile::tempdir().unwrap();
        let (mut app, _) = opened(dir.path(), mode, w, h);
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        let rows = screen_rows(&mut app, w, h);
        let dock = dock_of(&app, w, h);
        // 本文の最後の段落の行（一覧のすぐ上の辺り）を押す。
        let body = body_rows(&app, dock);
        let y = body.clone().rev().find(|y| rows[*y as usize].contains('p')).unwrap();
        let tag: String = rows[y as usize].split('p').nth(1).unwrap()[..3].to_string();
        let n: usize = tag.parse().unwrap();
        click(&mut app, y, 10);
        let cursor = if mode == Mode::View { app.view.cursor } else { app.cursor };
        assert_eq!(cursor, 2 * n, "{mode:?}: 押した行 p{tag}");
        assert_eq!(app.overlay, Some(Overlay::Review), "{mode:?}: 本文を押しても一覧は閉じない");
    }
}

#[test]
fn a_click_on_a_list_row_selects_that_candidate_and_follows_it() {
    let (w, h) = (80u16, 40u16);
    let _size = Size::set(w, h);
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = opened(dir.path(), Mode::View, w, h);
    screen_rows(&mut app, w, h);
    let dock = dock_of(&app, w, h);
    click(&mut app, dock.list.y + 3, 10);
    assert_eq!(app.overlay_cursor, 3);
    assert_eq!(app.view.cursor, 2 * HITS[3], "本文も送る");
    // 題・出どころ・理由の行を押しても何も変わらない。
    for y in [dock.title.y, dock.rule.y, dock.detail.y] {
        click(&mut app, y, 10);
        assert_eq!(app.overlay_cursor, 3, "行 {y}");
    }
    // ホイールは一覧を動かす。
    on_mouse(
        &mut app,
        MouseEvent { kind: MouseEventKind::ScrollDown, column: 10, row: dock.list.y, modifiers: KeyModifiers::NONE },
    );
    assert_eq!(app.overlay_cursor, 4);
}

// ---- 4. 狭い端末 --------------------------------------------------------

#[test]
fn at_twenty_rows_the_body_keeps_its_share() {
    let (w, h) = (80u16, 20u16);
    let _size = Size::set(w, h);
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = opened(dir.path(), Mode::View, w, h);
    let rows = crate::review_dock::dock_rows(&app, w, h);
    assert_eq!((rows.list, rows.detail, rows.height()), (5, 1, 8), "{rows:?}");
    assert_eq!(app.view_viewport_rows(), 9, "本文の中身は 9 行残る");
}

#[test]
fn the_share_grows_with_the_terminal_and_stops_at_the_caps() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = opened(dir.path(), Mode::View, 80, 24);
    let at = |h| crate::review_dock::dock_rows(&app, 80, h);
    assert_eq!((at(40).list, at(40).detail), (10, 1));
    assert_eq!((at(60).list, at(60).detail), (10, 1));
}

#[test]
fn low_and_narrow_terminals_never_panic() {
    for (w, h) in [(80u16, 12u16), (80, 9), (80, 8), (80, 7), (80, 4), (80, 3), (40, 20), (20, 20), (8, 20), (1, 20), (0, 0)] {
        let _size = Size::set(w, h);
        let dir = tempfile::tempdir().unwrap();
        let doc = long_doc();
        let mut app = lint_app(dir.path(), &doc, Mode::View);
        crate::overlay::open_review(&mut app);
        deliver(&mut app, hits(&doc));
        for _ in 0..3 {
            on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        }
        screen_rows(&mut app, w.max(1), h.max(1));
        app.mode = Mode::Source;
        screen_rows(&mut app, w.max(1), h.max(1));
    }
}

/// 端末の大きさを変えながら描く（横幅変更で落ちた前例がある —
/// `docs/gotchas/rendering.md`「横幅変更のフレーム比較」）。
#[test]
fn resizing_under_the_open_list_keeps_the_candidate_in_view() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = {
        let _size = Size::set(120, 40);
        opened(dir.path(), Mode::View, 120, 40)
    };
    for (w, h) in [(120u16, 40u16), (80, 20), (40, 24), (120, 60), (80, 24)] {
        let _size = Size::set(w, h);
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        // 幅が変わると view は描き直す（本番は Resize → 描き直し → follow）。
        replace_view_preserving_cursor(&mut app);
        crate::review_dock::follow(&mut app);
        let rows = screen_rows(&mut app, w, h);
        let dock = dock_of(&app, w, h);
        let hit = HITS[app.overlay_cursor];
        let tag = format!("p{hit:03}");
        assert!(
            body_rows(&app, dock).any(|y| rows[y as usize].contains(&tag)),
            "{w}×{h}: {tag}\n{}",
            rows.join("\n")
        );
    }
}

// ---- 5. 理由の欄 --------------------------------------------------------

#[test]
fn the_detail_shows_the_whole_message_with_its_line_breaks() {
    let (w, h) = (60u16, 40u16);
    let _size = Size::set(w, h);
    let dir = tempfile::tempdir().unwrap();
    let doc = long_doc();
    let mut app = lint_app(dir.path(), &doc, Mode::View);
    screen_rows(&mut app, w, h);
    crate::overlay::open_review(&mut app);
    let long = "【具体性】曖昧な判断表現が使われています。何をもってそう判断したのかを書くと、読み手が同じ結論に辿り着けます。\n例: 数字・比較の相手・期限";
    deliver(&mut app, vec![diagnostic(&doc, "p010", "ai-tech-writing-guideline", long)]);
    let rows = screen_rows(&mut app, w, h);
    let dock = dock_of(&app, w, h);
    assert!(dock.detail.height >= 3, "{dock:?}");
    let detail: String = rows[dock.detail.y as usize..(dock.detail.y + dock.detail.height) as usize]
        .iter()
        .map(|r| r.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let packed: String = detail.chars().filter(|c| !c.is_whitespace()).collect();
    let want: String = long.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(packed, want, "全文が折り返されて出る:\n{detail}");
    let last: String = detail.lines().last().unwrap().chars().filter(|c| !c.is_whitespace()).collect();
    assert!(last.starts_with("例:"), "改行はそのまま:\n{detail}");
    // 出どころの行は `<source>/<code>` を落とさない。
    assert!(rows[dock.rule.y as usize].contains("textlint/ai-tech-writing-guideline · L21"), "{}", rows[dock.rule.y as usize]);
}

#[test]
fn a_jev_rule_shows_the_first_sentence_of_its_definition() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_app(dir.path(), "a\n", Mode::View);
    app.review_rules = Some(crate::review_rules::Rules::built_in_enabling(&["filler"]));
    let candidate = crate::review::Candidate {
        range: 0..1,
        lines: (1, 1),
        rule: "filler".into(),
        finding: crate::review::Finding::Rule { action: crate::review_rules::Action::Delete, score: 0.87 },
        atoms: std::iter::once(0..1).collect(),
        state: crate::review::CandidateState::Pending,
    };
    let lines = crate::review_dock::detail_lines(&app, &candidate, 200);
    assert_eq!(lines, vec!["下の「対象」は、具体的なことを何も述べていない箇所である。".to_string()]);
}

// ---- 6. 閉じれば元どおり -------------------------------------------------

#[test]
fn closing_the_list_gives_the_body_back_its_full_height() {
    let (w, h) = (80u16, 30u16);
    let _size = Size::set(w, h);
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = opened(dir.path(), Mode::View, w, h);
    assert!(app.view_viewport_rows() < 26);
    let rows = screen_rows(&mut app, w, h);
    assert!(rows[h as usize - 1].contains("REVIEW"), "バッジ: {}", rows[h as usize - 1]);
    assert!(rows[h as usize - 1].contains("a accept"), "フッタが一覧のキーを案内する");
    // 80 桁でも `e edit` まで残る（題と同じ数の読み出しを出さないぶん）。
    assert!(rows[h as usize - 1].contains("e edit"), "{}", rows[h as usize - 1]);
    assert!(!rows[h as usize - 1].contains("Review ·"), "数は題に 1 か所: {}", rows[h as usize - 1]);
    on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.view_viewport_rows(), 26);
    let rows = screen_rows(&mut app, w, h);
    assert!(!rows.iter().any(|r| r.contains("review (")), "据え付けは消える");
    assert!(rows[h as usize - 2].starts_with(" └"), "本文の枠がフッタの上まで戻る: {}", rows[h as usize - 2]);
}
