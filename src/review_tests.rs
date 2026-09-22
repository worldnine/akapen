//! Review の end-to-end — **校正候補が、直す道まで繋がっているか。**
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節。ここで固定するのは 6 つ:
//!
//! 1. **accept** — 正しい行にコメントができ、既存の `l` / `y` / `s` に乗る
//! 2. **dismiss** — セッション内で消え、`dismissed.jsonl` に残り、再読込で出ない
//! 3. **一覧の描画** — `L42 · Filler 0.87 · …` と、`✓` / `–` の印
//! 4. **下線** — Pending だけに引かれ、**marks の琥珀とは別の色**である
//! 5. **marks が動かない** — `R` も accept も dismiss も marks の状態を触らない
//! 6. **`R` の断り** — fixture 経路では使えない
//!
//! 判定器は呼ばない。[`App::accept_review_analysis`] に手で組んだ
//! [`SemanticDocument`] を渡す — 本番と同じ入口で、外部プロセスだけが居ない。

use crate::*;

use crate::app::ReviewMessage;
use crate::config::{Config, EscQuit};
use crate::decoration::DecorationKind;
use crate::highlight::Highlighter;
use crate::ime::ImeMode;
use crate::marks_questions::Questions;
use crate::review::CandidateState;
use crate::review_rules::Rules;
use crate::source::Source;
use crate::view::ViewState;
use semantic_reading::{Atom, AtomIndex, AtomKind, SemanticDocument, SemanticUnit};
use std::path::PathBuf;

/// 3 つの段落を持つ小さな文書。1 段落 = 1 Atom = 1 Unit である。
const DOC: &str = "\
# みだし

ひとつめの段落。

ふたつめの段落。

みっつめの段落。
";

/// テスト用の文書をディスクに置いて `--semantic-cmd` の App を組む。
///
/// **コマンドは 1 度も走らない。** `reanalyze_review` を呼ばず、答えは
/// [`App::accept_review_analysis`] に手で渡すからである（`false` を置いて
/// あるのは、走ってしまったら失敗すると分かるようにするため）。
fn app_with_rules(dir: &std::path::Path, rules: Rules) -> App {
    let path = dir.join("doc.md");
    std::fs::write(&path, DOC).unwrap();
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
        semantic_cmd: Some("false".into()),
        marks_questions: None,
        review_rules: None,
        review_json: false,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.marks_questions = Some(Questions::built_in().unwrap());
    app.review_rules = Some(rules);
    app.review_dismissed_store = Some(crate::review::DismissedStore::at(dir.join("store")));
    let semantic = crate::semantic::source_from_config(&app.config).unwrap();
    app.set_semantic_source(semantic);
    // **起点は立てるが解析は頼まない**（頼むとコマンドが走る）。
    app.review_armed = true;
    app.load_dismissed();
    app
}

fn built_in(dir: &std::path::Path) -> App {
    app_with_rules(dir, Rules::built_in().unwrap())
}

/// `DOC` の段落を Atom にした注釈。`scores` は段落ごとのスコア。
fn answer(scores: [Option<f32>; 3]) -> SemanticDocument {
    let paragraphs: Vec<(usize, usize)> = ["ひとつめの段落。", "ふたつめの段落。", "みっつめの段落。"]
        .iter()
        .map(|text| {
            let at = DOC.find(text).unwrap();
            (at, at + text.len())
        })
        .collect();
    let atoms: Vec<Atom> = paragraphs
        .iter()
        .map(|(a, b)| Atom::new(*a..*b, AtomKind::Sentence))
        .collect();
    let units: Vec<SemanticUnit> = scores
        .iter()
        .enumerate()
        .map(|(i, score)| {
            let mut unit = SemanticUnit::new(format!("u{i}"), [AtomIndex(i)]);
            unit.score = *score;
            unit
        })
        .collect();
    let mut document = SemanticDocument::new(atoms, units);
    document.question = Some("filler".into());
    document
}

/// 1 ルール分の答えを `App` へ渡す（本番の入口）。
fn deliver(app: &mut App, rule: &str, document: SemanticDocument) {
    app.review_inflight = app.review_inflight.max(1);
    app.accept_review_analysis(ReviewMessage {
        generation: app.review_generation,
        rule: rule.to_string(),
        result: Ok(document),
    });
}

fn underlined(app: &App) -> Vec<std::ops::Range<usize>> {
    app.active_decorations()
        .into_iter()
        .filter(|d| d.kind == DecorationKind::ReviewCandidate)
        .map(|d| d.range)
        .collect()
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area();
    let mut out = String::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

// ---- 0. 候補が届く ------------------------------------------------------

#[test]
fn only_the_units_over_the_threshold_become_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.49), Some(0.5)]));
    // 0.49 は落ち、0.5 は**境界で拾う**。
    assert_eq!(app.review_candidates.len(), 2);
    assert_eq!(app.review_candidates[0].lines, (3, 3));
    assert_eq!(app.review_candidates[1].lines, (7, 7));
    assert_eq!(app.review_counts(), (0, 2));
}

#[test]
fn an_answer_from_an_older_generation_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    app.review_generation = 3;
    app.accept_review_analysis(ReviewMessage {
        generation: 2,
        rule: "filler".into(),
        result: Ok(answer([Some(0.9), None, None])),
    });
    assert!(app.review_candidates.is_empty(), "古い世代の答えは捨てる");
}

// ---- 1. accept ----------------------------------------------------------

#[test]
fn accept_makes_a_comment_on_the_candidates_own_lines() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([None, Some(0.87), None]));
    assert_eq!(app.review_candidates.len(), 1);
    let lines = app.review_candidates[0].lines;
    assert_eq!(lines, (5, 5), "`ふたつめの段落。` は 5 行目");

    assert!(app.accept_candidate(0));
    assert_eq!(app.comments.len(), 1);
    let comment = &app.comments[0];
    assert_eq!((comment.start, comment.end), lines);
    // **本文の形は 1 か所**（`crate::review::comment_text`）。段階 2 の
    // 送り先がこれを読む。
    assert_eq!(comment.text, "review: filler (0.87)");
    // `lines` は本文の写しである（送り先が行で照合する）。
    assert_eq!(comment.lines, "ふたつめの段落。");
    assert_eq!(comment.file_path, *app.current_file_path());
    // 一覧からは消えず、印が変わる。
    assert_eq!(app.review_candidates[0].state, CandidateState::Accepted);
    assert_eq!(app.review_candidates[0].mark(), "✓");
    assert_eq!(app.review_counts(), (1, 1));
    // 二度 accept しても 2 つにはならない。
    assert!(!app.accept_candidate(0));
    assert_eq!(app.comments.len(), 1);
}

#[test]
fn accept_all_takes_every_pending_candidate_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    assert!(app.dismiss_candidate(1), "真ん中を先に捨てておく");
    assert_eq!(app.accept_all_pending(), 2);
    assert_eq!(app.comments.len(), 2);
    assert_eq!(app.review_candidates[1].state, CandidateState::Dismissed);
    assert_eq!(app.review_counts(), (2, 3));
}

// ---- 2. dismiss ---------------------------------------------------------

#[test]
fn a_dismissed_candidate_is_gone_from_the_document_and_comes_back_never() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    assert_eq!(underlined(&app).len(), 2, "前提: 2 本に下線");

    assert!(app.dismiss_candidate(0));
    // 一覧には残る（印が変わるだけ）が、本文からは消える。
    assert_eq!(app.review_candidates.len(), 2);
    assert_eq!(app.review_candidates[0].mark(), "–");
    assert_eq!(underlined(&app).len(), 1, "捨てた候補の下線は消える");

    // **同じ文書を開き直すと出ない。** 置き場を共有した別の App が、
    // 本番と同じ経路（`load_dismissed` → `accept_review_analysis`）で
    // 候補を受け取る。
    let mut reopened = built_in(dir.path());
    assert_eq!(reopened.review_dismissed.len(), 1, "記録が読めている");
    deliver(&mut reopened, "filler", answer([Some(0.9), Some(0.8), None]));
    assert_eq!(reopened.review_candidates.len(), 1, "捨てた 1 本が出ていない");
    assert_eq!(reopened.review_candidates[0].lines, (5, 5));
}

#[test]
fn the_dismissal_record_carries_no_document_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    assert!(app.dismiss_candidate(0));
    let raw = std::fs::read_to_string(dir.path().join("store/dismissed.jsonl")).unwrap();
    assert!(!raw.contains("ひとつめ"), "本文が書かれている:\n{raw}");
    assert!(raw.contains("\"rule\":\"filler\""), "{raw}");
}

// ---- 3. 一覧の描画 ------------------------------------------------------

#[test]
fn the_list_shows_the_line_the_rule_the_score_and_the_head() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.91), Some(0.55), None]));
    crate::overlay::open_review(&mut app);
    assert_eq!(app.overlay, Some(Overlay::Review));

    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    assert!(screen.contains("review (0/2)"), "タイトル:\n{screen}");
    assert!(screen.contains("L3 · Filler 0.91"), "1 行目:\n{screen}");
    assert!(screen.contains("L5 · Filler 0.55"), "2 行目:\n{screen}");
    assert!(screen.contains("a:accept"), "キーの案内:\n{screen}");
    // 全角は 2 セルを占めるので、空白を落とした形で見る。
    let packed: String = screen.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(packed.contains("ひとつめの段落。"), "Unit の先頭:\n{screen}");
}

#[test]
fn the_list_marks_what_was_accepted_and_what_was_dismissed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    assert!(app.accept_candidate(0));
    assert!(app.dismiss_candidate(1));
    crate::overlay::open_review(&mut app);

    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    // **どちらの行も消えていない。** 印だけが変わる。
    assert!(screen.contains("✓ L3"), "accept の印:\n{screen}");
    assert!(screen.contains("– L5"), "dismiss の印:\n{screen}");
    assert!(screen.contains("review (1/2)"), "数:\n{screen}");
}

#[test]
fn the_footer_shows_review_to_the_left_of_the_marks_readout() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    assert!(app.accept_candidate(0));
    // marks の読み出しも同時に出す（「同時に出る」が注文である）。
    app.marks_question = Some(app.marks_questions.as_ref().unwrap().presets()[0].clone());
    app.semantic_doc = Some(answer([Some(0.9), Some(0.8), Some(0.7)]));
    app.refresh_semantic_decorations();

    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());
    assert!(
        screen.contains("Review · 1/2 · Essential"),
        "Review が marks の左に出ていない:\n{screen}"
    );

    // **縮む順** — 狭くなると Review が先に引き下がり、marks の本数は残る。
    let readouts = crate::chrome::footer_metrics(&app, 44).readout;
    let narrow = readouts.expect("44 桁でも読み出しは出る").text();
    assert!(!narrow.contains("Review"), "Review が先に引き下がっていない: {narrow:?}");
    assert!(narrow.contains("Essential"), "marks が先に消えた: {narrow:?}");
}

// ---- 4. 下線 -----------------------------------------------------------

#[test]
fn only_the_pending_candidates_are_underlined() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    assert_eq!(underlined(&app).len(), 3);
    assert!(app.accept_candidate(0));
    assert!(app.dismiss_candidate(1));
    let left = underlined(&app);
    assert_eq!(left.len(), 1, "残るのは Pending の 1 本だけ");
    // 3 つめの段落の範囲である。
    let third = DOC.find("みっつめの段落。").unwrap();
    assert_eq!(left[0].start, third);
}

/// **marks の琥珀とは別の色である。** 同じ色だと、読め（marks）と
/// 直せ（Review）が同じ印で出る。
#[test]
fn the_underline_is_a_different_colour_from_the_marks_amber() {
    for light in [false, true] {
        let highlight = Highlighter::new(None, light);
        let styles = crate::decoration::DecorationStyles::from_theme(&highlight, Default::default());
        let underline = styles.review_underline();
        assert_ne!(underline, styles.mark_bg(), "light={light}: 琥珀の地色と同じ");
        assert_ne!(underline, styles.mark_tick(), "light={light}: 目盛りの琥珀と同じ");
        // 色相まで見る — 琥珀は赤 > 青、Review の青緑は青 > 赤である。
        let (ratatui::style::Color::Rgb(ur, _, ub), ratatui::style::Color::Rgb(mr, _, mb)) =
            (underline, styles.mark_tick())
        else {
            panic!("両方とも RGB で出るはず");
        };
        assert!(ub > ur, "light={light}: Review の下線が青緑でない ({ur},_,{ub})");
        assert!(mr > mb, "light={light}: marks の目盛りが琥珀でない ({mr},_,{mb})");
    }
}

/// 下線は**前景も地色も書かない**ので、marks の琥珀と重なっても両方残る。
#[test]
fn the_underline_and_the_amber_survive_each_other() {
    let highlight = Highlighter::new(None, false);
    let styles = crate::decoration::DecorationStyles::from_theme(&highlight, Default::default());
    let base = ratatui::style::Style::default();
    let both = styles.patch(
        styles.patch(base, DecorationKind::SemanticMark),
        DecorationKind::ReviewCandidate,
    );
    assert_eq!(both.bg, Some(styles.mark_bg()), "琥珀の地色が残る");
    assert_eq!(both.underline_color, Some(styles.review_underline()));
    assert!(both.add_modifier.contains(ratatui::style::Modifier::UNDERLINED));
}

// ---- 5. marks の状態は動かない ------------------------------------------

/// **これが「別機能」の担保である。** `R` も accept も dismiss も、
/// marks の問い・注釈・世代・装飾に 1 バイトも触らない。
#[test]
fn review_never_touches_the_marks_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    // marks 側にも答えを入れておく（何も無い状態の比較では弱い）。
    app.marks_question = Some(app.marks_questions.as_ref().unwrap().presets()[0].clone());
    app.semantic_doc = Some(answer([Some(0.9), Some(0.8), Some(0.7)]));
    app.refresh_semantic_decorations();

    let question = app.marks_question.clone();
    let doc = app.semantic_doc.clone();
    let generation = app.semantic_generation;
    let decorations = app.semantic_decorations.clone();
    let lines = app.marks_lines.clone();
    let share = app.marks_share;
    assert!(!decorations.is_empty(), "前提: marks が光っている");

    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    crate::overlay::open_review(&mut app);
    assert!(app.accept_candidate(0));
    assert!(app.dismiss_candidate(1));
    app.accept_all_pending();

    assert_eq!(app.marks_question, question, "問いが動いた");
    assert_eq!(app.semantic_doc, doc, "注釈が動いた");
    assert_eq!(app.semantic_generation, generation, "世代が動いた");
    assert_eq!(app.semantic_decorations, decorations, "marks の装飾が動いた");
    assert_eq!(app.marks_lines, lines, "目盛りの行が動いた");
    assert_eq!(app.marks_share, share, "つまみが動いた");
}

/// 逆も見る — marks を動かしても Review の候補は消えない。
#[test]
fn the_marks_knob_never_touches_the_review_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    let before = app.review_candidates.clone();
    app.semantic_doc = Some(answer([Some(0.9), Some(0.8), Some(0.7)]));
    assert!(app.nudge_marks_share(10));
    assert!(app.press_focus(std::time::Instant::now()));
    assert_eq!(app.review_candidates, before);
    assert_eq!(underlined(&app).len(), 2);
}

// ---- 6. 断り・遅延 ------------------------------------------------------

#[test]
fn review_refuses_the_fixture_route() {
    // fixture は 1 つの問いへの固定の答えで、ルールの文面で聞き直す道が
    // 無い（`--semantic <file>`）。
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/semantic/demo.md");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/semantic/demo-marks.json");
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
        semantic: Some(fixture),
        semantic_cmd: None,
        marks_questions: None,
        review_rules: None,
        review_json: false,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.review_rules = Some(Rules::built_in().unwrap());
    let semantic = crate::semantic::source_from_config(&app.config).unwrap();
    app.set_semantic_source(semantic);

    assert!(!app.review_enabled());
    crate::overlay::open_review(&mut app);
    assert_eq!(app.overlay, None, "一覧は開かない");
    assert!(!app.review_armed, "起点も立たない");
}

#[test]
fn a_session_without_the_layer_has_no_review_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.md");
    std::fs::write(&path, DOC).unwrap();
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
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    let app = App::new(config, source, highlight, view, false);
    assert!(!app.review_enabled());
    assert!(app.review_readouts().is_empty());
    assert!(app.review_lines().iter().all(|f| !f));
    // `?` ヘルプにも出ない。
    let rows = crate::overlay::help_rows(false, false, false, false);
    assert!(
        !rows.iter().any(|(label, _)| *label == "review"),
        "層の無いセッションに Review の案内が出ている"
    );
}

#[test]
fn the_help_carries_one_review_row_with_the_layer() {
    let rows = crate::overlay::help_rows(false, false, false, true);
    let review: Vec<&(&str, &str)> = rows.iter().filter(|(l, _)| *l == "review").collect();
    assert_eq!(review.len(), 1, "1 行だけ");
    assert!(review[0].1.starts_with("R "), "{}", review[0].1);
}

// ---- 7. 文書が入れ替わったら候補は捨てる --------------------------------

/// **reload / タイムマシン / ファイル切替**は `reanalyze_semantics` を
/// 通る。候補は前の文書のバイト位置を指しているので、そこで捨てないと
/// 別の本文に下線が乗る。
#[test]
fn replacing_the_document_clears_the_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    assert_eq!(app.review_candidates.len(), 1);
    let generation = app.review_generation;

    // 本番と同じ入口。marks は問いが無いので何もせず、Review だけが動く。
    app.reanalyze_semantics();
    assert!(app.review_candidates.is_empty(), "候補が残っている");
    assert!(underlined(&app).is_empty(), "下線が残っている");
    assert!(app.review_generation > generation, "世代が上がっていない");
}
