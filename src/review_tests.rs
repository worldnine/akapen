//! Review の end-to-end — **校正候補が、直す道まで繋がっているか。**
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節。ここで固定するのは 7 つ:
//!
//! 1. **accept** — 正しい行にコメントができ、既存の `l` / `y` / `s` に乗る
//! 2. **dismiss** — 本文から消え、`dismissed.jsonl` に残り、再読込でも `–` のまま
//!    （取り消し・送った候補・遷移表は [`crate::review_undo_tests`]）
//! 3. **一覧の描画** — `L42 · Filler 0.87 · …` と、`✓` / `–` の印
//! 4. **下線** — Pending だけに引かれ、**marks の琥珀とは別の色**である
//! 5. **marks が動かない** — `R` も accept も dismiss も marks の状態を触らない
//! 6. **`R` の断り** — fixture 経路では使えない
//! 7. **lint が出どころ** — `--lint-cmd` の指摘が同じ流れ（一覧・accept・dismiss・
//!    引き継ぎ・`e`）に乗り、形の違う出力を「0 件」と見せない
//!
//! 判定器は呼ばない。[`App::accept_review_analysis`] に手で組んだ
//! [`SemanticDocument`] を渡す — 本番と同じ入口で、外部プロセスだけが居ない。

use crate::*;

use crate::app::{ReviewAnswer, ReviewMessage};
use crate::config::{Config, EscQuit};
use crate::decoration::{DecorationKind, ReviewSeverity};
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
pub(crate) const DOC: &str = "\
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
pub(crate) fn app_with_rules(dir: &std::path::Path, rules: Rules) -> App {
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
        lint_cmd: None,
        undercurl: Default::default(),
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
    // **`source_from_config` を通さない。** あれは実ユーザーの
    // `~/.cache/akapen/semantic` を provider に付ける（読むだけだが、
    // テストが実環境の置き場に触る理由が無い）。キャッシュ無しの
    // `CommandProvider` は同じ経路を同じように通る。
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Command(
        crate::semantic::CommandProvider::new("false"),
    )));
    // **起点は立てるが解析は頼まない**（頼むとコマンドが走る）。
    app.review_armed = true;
    app.load_dismissed();
    app
}

/// 既定のルールで `Filler` だけを有効にした App（**既定では 1 本も
/// 有効でない** — `--review-rules` で有効にした読み手の状態である）。
pub(crate) fn built_in(dir: &std::path::Path) -> App {
    app_with_rules(dir, Rules::built_in_enabling(&["filler"]))
}

/// `DOC` の段落を Atom にした注釈。`scores` は段落ごとのスコア。
pub(crate) fn answer(scores: [Option<f32>; 3]) -> SemanticDocument {
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
pub(crate) fn deliver(app: &mut App, rule: &str, document: SemanticDocument) {
    app.review_inflight = app.review_inflight.max(1);
    app.accept_review_analysis(ReviewMessage {
        generation: app.review_generation,
        answer: ReviewAnswer::Rule {
            rule: rule.to_string(),
            result: Ok(document),
        },
    });
}

pub(crate) fn underlined(app: &App) -> Vec<std::ops::Range<usize>> {
    app.active_decorations()
        .into_iter()
        .filter(|d| matches!(d.kind, DecorationKind::ReviewCandidate(_)))
        .map(|d| d.range)
        .collect()
}

pub(crate) fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
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
        answer: ReviewAnswer::Rule {
            rule: "filler".into(),
            result: Ok(answer([Some(0.9), None, None])),
        },
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
    assert_eq!(app.candidate_state(0).unwrap(), CandidateState::Accepted);
    assert_eq!(app.candidate_state(0).unwrap().mark(), "✓");
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
    assert_eq!(app.candidate_state(1).unwrap(), CandidateState::Dismissed);
    assert_eq!(app.review_counts(), (3, 3), "見た本数は ✓ 2 本と – 1 本");
}

// ---- 2. dismiss ---------------------------------------------------------

#[test]
fn a_dismissed_candidate_leaves_the_document_and_stays_dismissed_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    assert_eq!(underlined(&app).len(), 2, "前提: 2 本に下線");

    assert!(app.dismiss_candidate(0));
    // 一覧には残る（印が変わるだけ）が、本文からは消える。
    assert_eq!(app.review_candidates.len(), 2);
    assert_eq!(app.candidate_state(0).unwrap().mark(), "–");
    assert_eq!(underlined(&app).len(), 1, "捨てた候補の下線は消える");

    // **同じ文書を開き直しても捨てたまま。** 置き場を共有した別の App が、
    // 本番と同じ経路（`load_dismissed` → `accept_review_analysis`）で
    // 候補を受け取る。一覧には文書順の位置のまま `–` で残り、本文には
    // 下線も印も出ない。
    let mut reopened = built_in(dir.path());
    assert_eq!(reopened.review_dismissed.len(), 1, "記録が読めている");
    deliver(&mut reopened, "filler", answer([Some(0.9), Some(0.8), None]));
    assert_eq!(reopened.review_candidates.len(), 2, "捨てた 1 本も一覧に残る");
    assert_eq!(
        reopened.candidate_states(),
        [CandidateState::Dismissed, CandidateState::Pending],
        "文書順の位置のまま"
    );
    assert_eq!(underlined(&reopened).len(), 1, "捨てた候補に下線は無い");
    assert_eq!(reopened.review_lines().iter().flatten().count(), 1, "ガターの印も無い");
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
    assert!(screen.contains("a accept"), "キーの案内（フッタ）:\n{screen}");
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
    assert!(screen.contains("review (2/2)"), "数（✓ も – も見た本数）:\n{screen}");
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
///
/// 出どころからして違う: 琥珀は紙から作る RGB（本文に重ねる印）、下線は
/// ターミナルのパレットの名前（枠まわりの合図 — [`ReviewSeverity::color`]）。
#[test]
fn the_underline_is_a_different_colour_from_the_marks_amber() {
    for light in [false, true] {
        let highlight = Highlighter::new(None, light);
        let styles = crate::decoration::DecorationStyles::from_theme(&highlight, Default::default());
        let underline = styles.review_underline(ReviewSeverity::Info);
        assert_eq!(underline, ratatui::style::Color::Cyan, "light={light}: パレットの青緑でない");
        assert!(
            matches!(styles.mark_bg(), ratatui::style::Color::Rgb(..))
                && matches!(styles.mark_tick(), ratatui::style::Color::Rgb(..)),
            "light={light}: 琥珀は紙から作る RGB のまま"
        );
        for severity in [ReviewSeverity::Error, ReviewSeverity::Warning, ReviewSeverity::Info] {
            let c = styles.review_underline(severity);
            assert_ne!(c, styles.mark_bg(), "light={light}: {severity:?} が琥珀の地色と同じ");
            assert_ne!(c, styles.mark_tick(), "light={light}: {severity:?} が目盛りの琥珀と同じ");
        }
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
        DecorationKind::ReviewCandidate(ReviewSeverity::Warning),
    );
    assert_eq!(both.bg, Some(styles.mark_bg()), "琥珀の地色が残る");
    assert_eq!(both.underline_color, Some(styles.review_underline(ReviewSeverity::Warning)));
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
        lint_cmd: None,
        undercurl: Default::default(),
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
        lint_cmd: None,
        undercurl: Default::default(),
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    let app = App::new(config, source, highlight, view, false);
    assert!(!app.review_enabled());
    assert!(app.review_readouts().is_empty());
    assert!(app.review_lines().iter().all(Option::is_none));
    // `?` ヘルプにも出ない。
    let rows = crate::overlay::help_rows(false, false, false, false, false);
    assert!(
        !rows.iter().any(|(label, _)| *label == "review"),
        "層の無いセッションに Review の案内が出ている"
    );
}

#[test]
fn the_help_carries_one_review_row_with_the_layer() {
    let rows = crate::overlay::help_rows(false, false, false, true, true);
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

// ---- 7. 一覧のカーソルは候補を指し続ける ---------------------------------

/// `filler` と `preamble` の 2 本が有効なルール一式。
fn two_rules(dir: &std::path::Path) -> Rules {
    let path = dir.join("two-rules.json");
    std::fs::write(
        &path,
        r#"{"version":1,"rules":[
          {"id":"filler","label":"Filler","action":"delete","threshold":0.5,"enabled":true,"text":"f"},
          {"id":"preamble","label":"Preamble","action":"delete","threshold":0.5,"enabled":true,"text":"p"}
        ]}"#,
    )
    .unwrap();
    Rules::discover(Some(&path)).unwrap()
}

/// **2 本目のルールの答えが一覧を開いたまま届いても、カーソルは同じ候補を
/// 指す。** 答えは文書順に並べ直されるので、前に候補が差し込まれると
/// 添字のままのカーソルは別の候補を指し、`a` / `x` が見ていない行に効く
/// （報告されたずれ）。
#[test]
fn the_list_cursor_stays_on_its_candidate_when_another_rule_answers() {
    let dir = tempfile::tempdir().unwrap();
    let rules = two_rules(dir.path());
    let mut app = app_with_rules(dir.path(), rules);
    app.review_inflight = 2;
    // 1 本目: 3 段落目だけ。
    deliver(&mut app, "filler", answer([None, None, Some(0.9)]));
    crate::overlay::open_overlay(&mut app, crate::overlay::Overlay::Review, 0);
    assert_eq!(app.review_candidates[app.overlay_cursor].lines, (7, 7));

    // 2 本目: 1 段落目と 2 段落目。文書順では 3 段落目の前に入る。
    deliver(&mut app, "preamble", answer([Some(0.8), Some(0.7), None]));
    assert_eq!(app.review_candidates.len(), 3);
    let under = &app.review_candidates[app.overlay_cursor];
    assert_eq!(
        (under.lines, under.rule.as_str()),
        ((7, 7), "filler"),
        "カーソルは開いたときに見ていた候補に留まる"
    );

    // `a` は見えている候補に効く。
    crate::overlay::on_review_overlay_key(
        &mut app,
        ratatui::crossterm::event::KeyCode::Char(crate::keys::REVIEW_ACCEPT),
        ratatui::crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(app.comments.len(), 1);
    assert_eq!((app.comments[0].start, app.comments[0].end), (7, 7));
    assert_eq!(app.comments[0].text, "review: filler (0.90)");
}

/// 一覧を開いたまま文書が差し替わったら、カーソルは先頭へ戻る。古い添字
/// のままだと、次に届いた答えの見てもいない候補を指す。
#[test]
fn a_reanalysis_under_the_open_list_puts_the_cursor_back_on_top() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    crate::overlay::open_overlay(&mut app, crate::overlay::Overlay::Review, 2);
    app.reanalyze_semantics();
    assert_eq!(app.overlay_cursor, 0);
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    assert_eq!(app.review_candidates[app.overlay_cursor].lines, (3, 3));
}

// ---- 8. 送る文面に書き換えの契約が乗る ------------------------------------

/// accept したコメントを送ると、文面の先頭に契約と Filler の定義が乗り、
/// その後ろは既存の整形そのままである。
#[test]
fn an_accepted_candidate_sends_the_contract_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([None, Some(0.87), None]));
    assert!(app.accept_candidate(0));
    let text = crate::export_text(&app);
    let contract = crate::review_contract::contract_text();
    assert!(text.starts_with(contract.trim_end()), "契約が先頭に無い");
    let filler = app.review_rules.as_ref().unwrap().get("filler").unwrap().text.clone();
    assert!(text.contains(&filler), "Filler の定義が無い");
    assert!(text.ends_with(&crate::export::format_all(&app.comments)));
    assert!(text.contains("review: filler (0.87)"));
}

/// `--reply` でも同じく先頭に乗る。
#[test]
fn the_reply_mode_sends_the_contract_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    app.config.reply = true;
    deliver(&mut app, "filler", answer([None, Some(0.87), None]));
    assert!(app.accept_candidate(0));
    let text = crate::export_text(&app);
    let contract = crate::review_contract::contract_text();
    assert!(text.starts_with(contract.trim_end()));
    assert!(text.ends_with(&crate::export::format_all_reply(&app.comments)));
}

/// **review コメントが無い送信は 1 バイトも変わらない。** 候補が届いて
/// いても、accept していなければ契約は乗らない。
#[test]
fn a_send_without_an_accepted_candidate_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    app.comments.push(crate::comment::Comment {
        file_path: app.current_file_path().to_path_buf(),
        start: 5,
        end: 5,
        lines: "ふたつめの段落。".into(),
        revision: None,
        text: "ここは言い過ぎ".into(),
    });
    assert_eq!(crate::export_text(&app), crate::export::format_all(&app.comments));
    app.config.reply = true;
    assert_eq!(crate::export_text(&app), crate::export::format_all_reply(&app.comments));
}

// ---- 9. 直接編集 — 見届けた reload を越える ------------------------------
//
// `docs/design/marks-only-and-review-mode.md` 4 節「直接編集」。候補を
// LLM に送らず人が `e` で直す流れと、開いたまま LLM が書き換えた流れの
// どちらも、akapen が前後の版を見届けた reload（`reload_source`）を通る。

/// ディスクの文書を `text` に書き換えて、見届けた reload を通す。
pub(crate) fn rewrite_and_reload(app: &mut App, text: &str) {
    std::fs::write(app.current_file_path(), text).unwrap();
    crate::reload::reload_source(app, false).unwrap();
}

/// 文書の中の `paragraphs` を 1 段落 = 1 Atom = 1 Unit にした答え。
pub(crate) fn answer_for(doc: &str, paragraphs: &[&str], scores: &[Option<f32>]) -> SemanticDocument {
    let atoms: Vec<Atom> = paragraphs
        .iter()
        .map(|text| {
            let at = doc.find(text).unwrap();
            Atom::new(at..at + text.len(), AtomKind::Sentence)
        })
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

/// 2 段落目だけを直し、3 段落目の前に 1 段落足した版。1 段落目は
/// 1 バイトも変わらず、3 段落目は中身が同じままバイト位置だけが動く。
pub(crate) const EDITED: &str = "\
# みだし

ひとつめの段落。

ふたつめを直した。

足した段落。

みっつめの段落。
";

/// **dismiss は、中身が変わっていない範囲に限って差分を越える。**
///
/// 修正前は sha が変わるので捨てた判断が全部外れ、外れがまた出ていた。
#[test]
fn a_dismissal_follows_its_unchanged_range_across_a_watched_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.9), Some(0.9)]));
    // 1 段落目（動かない）・2 段落目（直す）・3 段落目（位置だけ動く）を捨てる。
    for index in 0..3 {
        assert!(app.dismiss_candidate(index));
    }

    rewrite_and_reload(&mut app, EDITED);

    let sha = crate::semantic::source_digest(EDITED);
    let store = app.review_dismissed_store.clone().unwrap();
    let carried = store.load(&sha);
    let range_of = |text: &str| {
        let at = EDITED.find(text).unwrap();
        ("filler".to_string(), at, at + text.len())
    };
    assert!(carried.contains(&range_of("ひとつめの段落。")), "{carried:?}");
    assert!(carried.contains(&range_of("みっつめの段落。")), "位置が動いた方も写る: {carried:?}");
    assert_eq!(carried.len(), 2, "直した 2 段落目は写さない: {carried:?}");
    // 記録の形は変えない — 本文は 1 バイトも書かない。
    let raw = std::fs::read_to_string(store.path()).unwrap();
    for word in ["ひとつめ", "みっつめ", "直した"] {
        assert!(!raw.contains(word), "{raw}");
    }

    // 新しい版の答えが届くと、写した 2 本は `–` のまま、直した段落だけが
    // Pending で出る。
    deliver(
        &mut app,
        "filler",
        answer_for(
            EDITED,
            &["ひとつめの段落。", "ふたつめを直した。", "みっつめの段落。"],
            &[Some(0.9), Some(0.9), Some(0.9)],
        ),
    );
    let got: Vec<((u32, u32), CandidateState)> = app
        .review_candidates
        .iter()
        .map(|c| c.lines)
        .zip(app.candidate_states())
        .collect();
    assert_eq!(
        got,
        [
            ((3, 3), CandidateState::Dismissed),
            ((5, 5), CandidateState::Pending),
            ((9, 9), CandidateState::Dismissed)
        ],
        "直した段落だけが判定し直される"
    );
}

/// 閉じている間に変わったファイル（起動時の読み込み・ファイル切替）は
/// 見届けていないので、何も写さない。写すのは `reload_source` だけである。
#[test]
fn a_dismissal_is_not_carried_when_the_document_is_only_reanalysed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    assert!(app.dismiss_candidate(0));
    let store = app.review_dismissed_store.clone().unwrap();
    let before = std::fs::read_to_string(store.path()).unwrap();
    app.reanalyze_semantics();
    assert_eq!(std::fs::read_to_string(store.path()).unwrap(), before);
}

/// `review_tests` の App に、NOW だけの履歴を持たせる（人の赤入れが
/// 古い版へ留め置かれるのを確かめるため — 履歴が無いと留め置く先が無い）。
fn with_history(app: &mut App) {
    let mut history =
        crate::history::DocumentHistory::load(&app.files[0], &app.source.content, 0);
    history.acknowledge_in_memory(&app.source.content);
    app.histories = vec![history];
}

/// **直した箇所の accept 済み review コメントは外れ、変わっていない方は
/// 新しい位置に付け直され、人の赤入れは古い版に留め置かれる。**
///
/// 修正前は 3 本とも古い版に留め置かれ、`s` で直した箇所への指示が
/// LLM に届いていた。
#[test]
fn a_watched_reload_resolves_the_review_comments_whose_range_changed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    with_history(&mut app);
    deliver(&mut app, "filler", answer([None, Some(0.87), Some(0.9)]));
    assert!(app.accept_candidate(0), "2 段落目（直す）");
    assert!(app.accept_candidate(1), "3 段落目（位置だけ動く）");
    // 人の赤入れ。1 段落目は変わらないが、それでも触らない。
    app.comments.push(crate::comment::Comment {
        file_path: app.current_file_path().to_path_buf(),
        start: 3,
        end: 3,
        lines: "ひとつめの段落。".into(),
        revision: None,
        text: "ここは言い過ぎ".into(),
    });

    rewrite_and_reload(&mut app, EDITED);

    let review: Vec<&crate::comment::Comment> = app
        .comments
        .iter()
        .filter(|c| crate::review::parse_comment_text(&c.text).is_some())
        .collect();
    assert_eq!(review.len(), 1, "直した 2 段落目のコメントは外れる");
    let moved = review[0];
    assert_eq!(moved.text, "review: filler (0.90)");
    assert_eq!((moved.start, moved.end), (9, 9), "新しい位置へ付け直す");
    assert_eq!(moved.lines, "みっつめの段落。");
    assert_eq!(moved.revision, None, "新しい版（NOW）のコメントとして残る");

    let human = app
        .comments
        .iter()
        .find(|c| c.text == "ここは言い過ぎ")
        .expect("人の赤入れは消さない");
    assert!(human.revision.is_some(), "人の赤入れは古い版に留め置く（今の挙動）");
    assert_eq!((human.start, human.end), (3, 3));

    let (message, _, err) = app.status.clone().unwrap();
    assert!(!err);
    assert!(message.contains("1 review comment resolved by edit"), "{message}");
}

/// 付け直したコメントの候補は、新しい版の答えが届いても Pending に戻らない
/// （もう一度 `a` を押すと同じ指示が 2 本になる）。
#[test]
fn a_moved_review_comment_keeps_its_candidate_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    with_history(&mut app);
    deliver(&mut app, "filler", answer([None, None, Some(0.9)]));
    assert!(app.accept_candidate(0));
    rewrite_and_reload(&mut app, EDITED);
    deliver(
        &mut app,
        "filler",
        answer_for(EDITED, &["みっつめの段落。"], &[Some(0.9)]),
    );
    assert_eq!(app.review_candidates.len(), 1);
    assert_eq!(app.candidate_state(0).unwrap(), CandidateState::Accepted);
    assert!(!app.accept_candidate(0), "同じ指示を 2 本作らない");
}

// ---- 10. 一覧から直接 `e` ------------------------------------------------

/// テスト用の `$EDITOR`。**名前は `vi`** — 行ジャンプ（`+N FILE`）を渡す
/// エディタの一覧に入っている名前なので、本番と同じ引数で呼ばれる。
/// 受けた `+N` を隣の `args` に書き、`awk` の `program` で N 行目を
/// 書き換えて終わる。
fn editor_script(dir: &std::path::Path, program: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("vi");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             echo \"$1\" > \"$(dirname \"$0\")/args\"\n\
             n=${{1#+}}\n\
             awk -v n=\"$n\" '{program}' \"$2\" > \"$2.tmp\" && mv \"$2.tmp\" \"$2\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// 一覧から `e` を押したのと同じ道を、端末の切り替えだけ抜いて通す。
fn edit_from_the_list(app: &mut App, editor: &std::path::Path) {
    let editor = editor.display().to_string();
    crate::overlay::review_edit_with(app, |app, line| {
        let path = app.current_file_path().to_path_buf();
        let status = crate::reload::run_editor(&editor, line, &path);
        crate::reload::after_editor(app, &editor, status);
    });
}

fn list_screen(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, app)).unwrap();
    buffer_text(terminal.backend().buffer())
}

/// **`e` はカーソル下の候補の先頭行でエディタを開き、戻ったら一覧を
/// 開き直す。** 答えが揃ったら、カーソルは直した箇所の次の候補に着く
/// （直した候補は消えている）。
#[test]
fn e_in_the_list_edits_the_candidate_and_lands_on_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.9), Some(0.9)]));
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 1; // ふたつめ（5 行目）
    // N 行目を消す。
    let editor = editor_script(dir.path(), "NR != n");

    edit_from_the_list(&mut app, &editor);

    let args = std::fs::read_to_string(editor.parent().unwrap().join("args")).unwrap();
    assert_eq!(args.trim(), "+5", "候補の先頭行で開く");
    assert!(!app.source.content.contains("ふたつめ"), "{}", app.source.content);
    assert_eq!(app.overlay, Some(Overlay::Review), "一覧を開き直す");
    assert!(app.review_inflight > 0, "再解析を頼んでいる");
    let screen = list_screen(&mut app);
    assert!(screen.contains("analyzing"), "解析中と分かる:\n{screen}");

    let doc = app.source.content.clone();
    deliver(
        &mut app,
        "filler",
        answer_for(&doc, &["ひとつめの段落。", "みっつめの段落。"], &[Some(0.9), Some(0.9)]),
    );
    let under = &app.review_candidates[app.overlay_cursor];
    assert_eq!(
        app.source.content.get(under.range.clone()),
        Some("みっつめの段落。"),
        "直した箇所の次の候補に着く"
    );
}

/// 直した候補がまだ候補に残っていれば、カーソルはそれ自身に着く。
#[test]
fn e_in_the_list_lands_on_the_candidate_itself_when_it_is_still_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.9), Some(0.9)]));
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 1;
    // N 行目の末尾に足す（直したが、まだ候補に出る）。
    let editor = editor_script(dir.path(), "NR == n { print $0 \"まだ足りない。\"; next } { print }");

    edit_from_the_list(&mut app, &editor);

    let doc = app.source.content.clone();
    assert!(doc.contains("ふたつめの段落。まだ足りない。"), "{doc}");
    // 1 段落目はもう候補に出ない。前に 1 本減るので、添字のままでは
    // 3 段落目を指してしまう。
    deliver(
        &mut app,
        "filler",
        answer_for(
            &doc,
            &["ひとつめの段落。", "ふたつめの段落。まだ足りない。", "みっつめの段落。"],
            &[None, Some(0.9), Some(0.9)],
        ),
    );
    let under = &app.review_candidates[app.overlay_cursor];
    assert_eq!(
        doc.get(under.range.clone()),
        Some("ふたつめの段落。まだ足りない。"),
        "直した候補そのものに着く"
    );
}

/// 何も変えずにエディタを閉じたら、一覧も候補もカーソルもそのまま。
#[test]
fn e_in_the_list_without_a_change_leaves_the_list_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.9), Some(0.9)]));
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 2;
    let generation = app.review_generation;
    let editor = editor_script(dir.path(), "{ print }");

    edit_from_the_list(&mut app, &editor);

    assert_eq!(app.overlay, Some(Overlay::Review));
    assert_eq!(app.overlay_cursor, 2);
    assert_eq!(app.review_candidates.len(), 3);
    assert_eq!(app.review_generation, generation, "聞き直していない");
    assert_eq!(app.review_edit_anchor, None);
    assert_eq!(app.review_cursor_target, None);
}

/// 外からの書き換えを読み込む前は、一覧からもエディタを開かない
/// （本文の `e` と同じ断り）。
#[test]
fn e_in_the_list_refuses_while_a_file_change_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    crate::overlay::open_review(&mut app);
    app.file_changed = true;
    let mut ran = false;
    crate::overlay::review_edit_with(&mut app, |_, _| ran = true);
    assert!(!ran, "エディタを開いていない");
    let (message, _, err) = app.status.clone().unwrap();
    assert!(err && message.contains("r reload first"), "{message}");
    assert_eq!(app.overlay, Some(Overlay::Review));
}

/// 一覧の案内とヘルプに `e` が載っている。
#[test]
fn the_list_and_the_help_mention_e() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    crate::overlay::open_review(&mut app);
    let screen = list_screen(&mut app);
    assert!(screen.contains("e edit"), "{screen}");
    assert!(crate::keys::REVIEW_HINT.contains("e edit"), "{}", crate::keys::REVIEW_HINT);
}

// ---- 10. linter が出どころ（`--lint-cmd`） --------------------------------
//
// `docs/design/marks-only-and-review-mode.md` 4 節「判定の出どころとしての
// linter」。判定は linter がして、akapen はその後ろの流れを受け持つ。
// **後ろの流れは Jev の候補と同じ道を通る**ことを、ここで押さえる。

/// 意味層の無い、`--lint-cmd` だけのセッション。
pub(crate) fn lint_only(dir: &std::path::Path, cmd: &str) -> App {
    let mut app = built_in(dir);
    app.set_semantic_source(None);
    app.review_rules = None;
    app.lint = Some(crate::lint::LintCommand::new(cmd));
    app
}

/// `DOC` の `text` の先頭 `chars` 字に当たる Diagnostic の JSON。
pub(crate) fn diagnostic(doc: &str, text: &str, chars: usize, code: &str, message: &str) -> serde_json::Value {
    let at = doc.find(text).unwrap();
    let line = doc[..at].matches('\n').count();
    let line_start = doc[..at].rfind('\n').map_or(0, |i| i + 1);
    let character: usize = doc[line_start..at].chars().map(char::len_utf16).sum();
    let width: usize = text.chars().take(chars).map(char::len_utf16).sum();
    serde_json::json!({
        "range": {"start": {"line": line, "character": character},
                  "end": {"line": line, "character": character + width}},
        "message": message, "source": "textlint", "code": code, "severity": 2,
    })
}

pub(crate) fn deliver_lint(app: &mut App, diagnostics: Vec<serde_json::Value>) {
    let json = serde_json::json!({ "diagnostics": diagnostics }).to_string();
    let parsed = crate::lint::parse(&json, &app.source.content).expect("形どおり");
    app.review_inflight = app.review_inflight.max(1);
    app.accept_review_analysis(ReviewMessage {
        generation: app.review_generation,
        answer: ReviewAnswer::Lint(Ok(parsed)),
    });
}

#[test]
fn a_lint_only_session_opens_review_and_lists_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_only(dir.path(), "true");
    assert!(app.review_enabled(), "意味層が無くても R が使える");
    assert!(
        crate::overlay::help_rows(false, false, false, false, true)
            .iter()
            .any(|(label, _)| *label == "review"),
        "lint だけでもヘルプに Review の行が出る"
    );
    crate::overlay::open_review(&mut app);
    assert_eq!(app.overlay, Some(Overlay::Review));
    deliver_lint(
        &mut app,
        vec![diagnostic(DOC, "ふたつめの段落。", 4, "ja-no-weak-phrase", "弱い表現: \"かも\" が使われています。")],
    );
    let c = &app.review_candidates[0];
    assert_eq!(c.rule, "textlint/ja-no-weak-phrase");
    assert_eq!(app.source.content.get(c.range.clone()), Some("ふたつめ"), "範囲は指摘そのまま");
    assert_eq!(underlined(&app), vec![c.range.clone()], "下線もその範囲");

    let screen = list_screen(&mut app);
    let packed: String = screen.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        packed.contains("L5·ja-no-weak-phrase·弱い表現:\"かも\"が使われています。"),
        "一覧の 1 行は理由を見せる:\n{screen}"
    );
}

/// **キーの段でも通る。** `R` の腕が意味層だけを見ていて、lint だけの
/// セッションでは押しても何も起きなかった（実機の測定で見つけた）。
#[test]
fn the_r_key_reaches_the_list_in_a_lint_only_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_only(dir.path(), "true");
    assert!(!app.semantic_enabled());
    on_view_key(&mut app, KeyCode::Char(crate::keys::REVIEW_OPEN), KeyModifiers::NONE, None);
    assert_eq!(app.overlay, Some(Overlay::Review), "VIEW");
    app.overlay = None;
    on_source_key(&mut app, KeyCode::Char(crate::keys::REVIEW_OPEN), KeyModifiers::NONE, None);
    assert_eq!(app.overlay, Some(Overlay::Review));
}

#[test]
fn an_accepted_lint_candidate_becomes_a_lint_comment_with_the_lint_paragraph() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_only(dir.path(), "true");
    deliver_lint(
        &mut app,
        vec![diagnostic(DOC, "みっつめの段落。", 4, "ja-no-mixed-period", "文末が\"。\"で終わっていません。\n理由: 句点")],
    );
    assert!(app.accept_candidate(0));
    let comment = app.comments.last().unwrap();
    assert_eq!(
        comment.text,
        "lint: textlint/ja-no-mixed-period — 文末が\"。\"で終わっていません。 理由: 句点"
    );
    assert_eq!((comment.start, comment.end), (7, 7));
    let text = crate::export_text(&app);
    let lint = crate::review_contract::lint_contract_text();
    assert!(text.starts_with(lint.trim_end()), "lint の段落が先頭に無い:\n{text}");
    assert!(!text.contains("[要: 具体例]"), "Jev の契約は足さない");
    assert!(text.ends_with(&crate::export::format_all(&app.comments)));
}

/// dismiss・差分越しの引き継ぎ・付け直しは、(rule, 範囲) の鍵のまま
/// lint の候補にも効く。
#[test]
fn lint_candidates_ride_the_dismiss_and_the_carry_across_a_watched_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_only(dir.path(), "true");
    deliver_lint(
        &mut app,
        vec![
            diagnostic(DOC, "ひとつめの段落。", 4, "a", "m1"),
            diagnostic(DOC, "ふたつめの段落。", 8, "b", "m2"),
            diagnostic(DOC, "みっつめの段落。", 4, "c", "m3"),
        ],
    );
    assert!(app.dismiss_candidate(0)); // 動かない
    assert!(app.dismiss_candidate(1)); // 直される
    assert!(app.accept_candidate(2)); // 位置だけ動く

    rewrite_and_reload(&mut app, EDITED);

    let store = app.review_dismissed_store.clone().unwrap();
    let carried = store.load(&crate::semantic::source_digest(EDITED));
    let at = EDITED.find("ひとつめ").unwrap();
    assert_eq!(
        carried.into_iter().collect::<Vec<_>>(),
        vec![("textlint/a".to_string(), at, at + "ひとつめ".len())],
        "直した方は写さない"
    );
    let live: Vec<&str> = app.comments.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(live, ["lint: textlint/c — m3"], "中身の変わらない lint コメントは残る");
    assert_eq!(app.comments[0].start, 9, "新しい行へ付け直す");

    // 新しい版の答え: 写した dismiss は `–` で残り、付け直したコメントは ✓ のまま。
    deliver_lint(
        &mut app,
        vec![
            diagnostic(EDITED, "ひとつめの段落。", 4, "a", "m1"),
            diagnostic(EDITED, "ふたつめを直した。", 4, "b", "m2"),
            diagnostic(EDITED, "みっつめの段落。", 4, "c", "m3"),
        ],
    );
    let got: Vec<(&str, CandidateState)> = app
        .review_candidates
        .iter()
        .map(|c| c.rule.as_str())
        .zip(app.candidate_states())
        .collect();
    assert_eq!(
        got,
        [
            ("textlint/a", CandidateState::Dismissed),
            ("textlint/b", CandidateState::Pending),
            ("textlint/c", CandidateState::Accepted)
        ]
    );
}

#[test]
fn a_lint_that_prints_no_diagnostics_is_not_shown_as_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_only(dir.path(), "true");
    crate::overlay::open_review(&mut app);
    app.review_inflight = 1;
    app.accept_review_analysis(ReviewMessage {
        generation: app.review_generation,
        answer: ReviewAnswer::Lint(Err(crate::lint::NOT_DIAGNOSTICS.into())),
    });
    let screen = list_screen(&mut app);
    assert!(screen.contains("lint: output is not diagnostics JSON"), "{screen}");
    assert!(!screen.contains("nothing to fix"), "0 件と見せている:\n{screen}");
}

#[test]
fn without_a_lint_or_an_enabled_rule_r_says_what_to_set() {
    let dir = tempfile::tempdir().unwrap();
    // 既定のルール（1 本も有効でない）と `--semantic-cmd` だけ。
    let mut app = app_with_rules(dir.path(), Rules::built_in().unwrap());
    assert!(!app.review_enabled());
    crate::overlay::open_review(&mut app);
    assert_eq!(app.overlay, None);
    let (message, _, _) = app.status.clone().unwrap();
    assert_eq!(message, "no review source — set --lint-cmd or enable a rule");
}

#[test]
fn jev_and_lint_candidates_share_one_list_in_document_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    app.lint = Some(crate::lint::LintCommand::new("true"));
    deliver(&mut app, "filler", answer([Some(0.9), None, Some(0.9)]));
    crate::overlay::open_review(&mut app);
    deliver_lint(&mut app, vec![diagnostic(DOC, "ふたつめの段落。", 4, "x", "m")]);
    let rules: Vec<&str> = app.review_candidates.iter().map(|c| c.rule.as_str()).collect();
    assert_eq!(rules, ["filler", "textlint/x", "filler"]);
    let screen = list_screen(&mut app);
    assert!(screen.contains("L3 · Filler 0.90"), "Jev の行の形は変わらない:\n{screen}");
}

/// 本物のコマンドを走らせる道。**exit 1 でも stdout が JSON なら成功**で、
/// 引数の最後は文書の絶対パスである。
#[test]
fn r_runs_the_lint_command_and_its_answer_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("lint.sh");
    let json = serde_json::json!({
        "diagnostics": [diagnostic(DOC, "ふたつめの段落。", 4, "weak", "m")]
    });
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ntest -f \"$1\" || exit 3\ncat <<'JSON'\n{json}\nJSON\nexit 1\n"
        ),
    )
    .unwrap();
    let mut app = lint_only(dir.path(), &format!("sh '{}'", script.display()));
    app.review_armed = false;
    crate::overlay::open_review(&mut app);
    assert_eq!(app.review_inflight, 1);
    let started = std::time::Instant::now();
    while app.review_inflight > 0 && started.elapsed() < std::time::Duration::from_secs(10) {
        app.poll_review_analysis();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.review_candidates.len(), 1, "{:?}", app.status);
    assert_eq!(app.review_candidates[0].rule, "textlint/weak");
}

/// 一覧から `e` で直して戻ったとき、**文書全体を範囲にする指摘には
/// 着かない**（実機の測定で、毎回 L1 の総評に攫われていた）。
#[test]
fn a_whole_document_diagnostic_does_not_steal_the_cursor_after_an_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = lint_only(dir.path(), "true");
    let whole = serde_json::json!({
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 7, "character": 0}},
        "message": "総評", "source": "textlint", "code": "whole",
    });
    deliver_lint(
        &mut app,
        vec![
            whole.clone(),
            diagnostic(DOC, "ふたつめの段落。", 4, "b", "m2"),
            diagnostic(DOC, "みっつめの段落。", 4, "c", "m3"),
        ],
    );
    assert_eq!(app.review_candidates.len(), 3, "総評も候補");
    assert_eq!(app.review_candidates[2].rule, "textlint/whole", "総評は末尾");
    crate::overlay::open_review(&mut app);
    app.review_inflight = 0;
    app.overlay_cursor = 0; // ふたつめ
    let editor = editor_script(dir.path(), "NR != n");
    edit_from_the_list(&mut app, &editor);
    let doc = app.source.content.clone();
    deliver_lint(&mut app, vec![whole, diagnostic(&doc, "みっつめの段落。", 4, "c", "m3")]);
    assert_eq!(app.review_candidates[app.overlay_cursor].rule, "textlint/c");
}

// ---- 8. Esc は一番手前の層から 1 枚ずつはがす（`crate::esc`） -------------

/// `Esc` を 1 回押す。キーの届き先は本番と同じ（据え付けの一覧が開いて
/// いれば一覧、無ければいまのモード）。
fn press_esc(app: &mut App) {
    if app.overlay.is_some() {
        on_overlay_key(app, KeyCode::Esc, KeyModifiers::NONE);
    } else if app.mode == Mode::Source {
        on_source_key(app, KeyCode::Esc, KeyModifiers::NONE, None);
    } else {
        on_view_key(app, KeyCode::Esc, KeyModifiers::NONE, None);
    }
}

fn flashed(app: &App) -> Option<&str> {
    app.status.as_ref().map(|(msg, _, _)| msg.as_str())
}

/// marks を出す（問いと答えを手で置く。判定器は呼ばない）。
fn show_marks(app: &mut App) {
    app.marks_question = Some(app.marks_questions.as_ref().unwrap().presets()[0].clone());
    app.semantic_doc = Some(answer([Some(0.9), Some(0.8), Some(0.7)]));
    app.refresh_semantic_decorations();
}

/// **全部の層を重ねて、1 枚ずつはがす。** 1 回ごとに、フッタの予告が
/// 言ったものが実際に消え、同じ語でフラッシュされる。
#[test]
fn esc_peels_every_layer_in_the_order_the_footer_announces() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    assert!(app.accept_candidate(0), "accept で作ったコメントは消えないこと");
    show_marks(&mut app);
    assert!(app.press_focus(std::time::Instant::now()), "沈める");
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 1;
    on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.selection.is_some(), "一覧の Enter が選択を作る");
    app.confirm_quit = true;

    let steps = [
        ("Esc: cancel quit", "quit cancelled"),
        ("Esc: cancel selection", "selection cancelled"),
        ("Esc: close list", "list closed"),
        ("Esc: focus off", "focus off"),
        ("Esc: clear review", "review cleared"),
        ("Esc: clear marks", "marks cleared"),
    ];
    for (preview, flash) in steps {
        assert_eq!(crate::esc::preview(&app).as_deref(), Some(preview));
        // 予告は描かれるフッタにも出ている（同じ表を読む）。
        let footer = crate::chrome::footer_metrics(&app, 200);
        assert_eq!(footer.esc.as_deref(), Some(preview), "フッタの予告");
        press_esc(&mut app);
        assert_eq!(flashed(&app), Some(flash), "{preview} の後のフラッシュ");
    }
    // 何が消えたか。
    assert!(!app.confirm_quit && app.selection.is_none() && app.overlay.is_none());
    assert!(!app.focused());
    assert!(app.review_candidates.is_empty() && app.review_lines().iter().all(Option::is_none));
    assert!(app.semantic_doc.is_none() && app.marks_question.is_none());
    // **何も消えないもの。** accept で作ったコメントは残る。
    assert_eq!(app.comments.len(), 1);
    // もう消すものが無い。`--esc-quit` でもないので、予告は出ず、Esc は
    // 何もしない（終了しない）。
    assert_eq!(crate::esc::preview(&app), None);
    assert_eq!(crate::chrome::footer_metrics(&app, 200).esc, None);
    press_esc(&mut app);
    assert!(app.running);
    assert_eq!(flashed(&app), Some("marks cleared"), "何も起きていない");
}

/// source モードでは選択の次に**削除のフォーカス**が入る。view には無い段。
#[test]
fn source_mode_peels_the_deletion_focus_right_after_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    show_marks(&mut app);
    app.mode = Mode::Source;
    app.focused_deletion = Some(app.cursor);
    app.selection = Some(crate::comment::Selection::new(app.cursor));
    assert_eq!(crate::esc::preview(&app).as_deref(), Some("Esc: cancel selection"));
    press_esc(&mut app);
    assert_eq!(crate::esc::preview(&app).as_deref(), Some("Esc: cancel deletion focus"));
    press_esc(&mut app);
    assert_eq!(flashed(&app), Some("deletion focus cancelled"));
    assert_eq!(crate::esc::preview(&app).as_deref(), Some("Esc: clear marks"));
    // view では同じ値が残っていても段にならない（以前から view の Esc は見ていない）。
    app.focused_deletion = Some(app.cursor);
    app.mode = Mode::View;
    assert_eq!(crate::esc::top(&app), Some(crate::esc::Layer::Marks));
}

/// **消した Review は `R` でまた出る。** 消したのは画面の候補だけで、
/// dismiss の記録と accept のコメントは残っている — 戻ってきた候補は
/// 前と同じ印を付けている。
#[test]
fn a_cleared_review_comes_back_on_r_with_its_accepts_and_dismissals() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    let scores = || answer([Some(0.9), Some(0.8), Some(0.7)]);
    deliver(&mut app, "filler", scores());
    assert_eq!(app.review_candidates.len(), 3);
    assert!(app.accept_candidate(0));
    assert!(app.dismiss_candidate(1));
    let generation = app.review_generation;

    assert!(app.clear_review());
    assert!(app.review_candidates.is_empty());
    assert!(!app.review_armed, "起点も戻す");
    assert!(app.review_generation > generation, "走っている答えは古い世代になる");
    assert!(!app.review_shown());
    assert!(!app.clear_review(), "2 度目は消すものが無い");
    // 次の reload（再解析）が頼んでもいない候補を描き直さない。
    app.reanalyze_review();
    assert_eq!(app.review_inflight, 0);

    // `R` — 起点が立ち直り、同じ答えが届けば同じ印で戻る。
    app.arm_review();
    assert!(app.review_armed);
    deliver(&mut app, "filler", scores());
    assert_eq!(
        app.candidate_states(),
        [CandidateState::Accepted, CandidateState::Dismissed, CandidateState::Pending],
        "accept はコメントから、dismiss は記録から戻る（捨てた候補は `–` で残る）"
    );
}

/// `--esc-quit` のとき、最後の段は終了。未送信のコメントがあれば `q` と
/// 同じく確認を挟み、確認中の予告は `Esc: quit` になる。
#[test]
fn with_esc_quit_the_last_layer_is_quit() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    app.config.esc_quit = EscQuit::Always;
    show_marks(&mut app);
    assert_eq!(crate::esc::preview(&app).as_deref(), Some("Esc: clear marks"));
    press_esc(&mut app);
    assert!(app.running, "marks を消しただけ");
    assert_eq!(crate::esc::preview(&app).as_deref(), Some("Esc: quit"));
    app.comments.push(crate::comment::Comment {
        file_path: app.current_file_path().to_path_buf(),
        start: 1,
        end: 1,
        lines: String::new(),
        revision: None,
        text: "c".into(),
    });
    press_esc(&mut app);
    assert!(app.confirm_quit && app.running, "未送信のコメントがあるので確認");
    assert_eq!(crate::esc::preview(&app).as_deref(), Some("Esc: quit"));
    press_esc(&mut app);
    assert!(!app.running);
}

/// popup と composer は `Esc` を自分で取る。そこで表の予告を出すと嘘になる。
#[test]
fn the_preview_is_silent_while_a_popup_or_the_composer_owns_esc() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    show_marks(&mut app);
    assert!(crate::esc::preview(&app).is_some());
    app.overlay = Some(crate::overlay::Overlay::MarkFor);
    assert_eq!(crate::esc::preview(&app), None);
    app.overlay = None;
    app.mode = Mode::Input;
    assert_eq!(crate::esc::preview(&app), None);
}

/// 据え付けの一覧のフッタは右端の予告で出口を言う（`Esc close` を
/// 案内に重ねない）。80 桁でも予告は残る。
#[test]
fn the_list_footer_names_its_way_out_at_the_right_edge() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), None]));
    crate::overlay::open_review(&mut app);
    let footer = crate::chrome::footer_metrics(&app, 80);
    assert_eq!(footer.esc.as_deref(), Some("Esc: close list"));
    assert!(!footer.hints.contains("Esc close"), "{}", footer.hints);
    assert!(footer.hints.contains("e edit"), "{}", footer.hints);
}

// ---- 9. mark for の `0 Off` ---------------------------------------------

fn mark_for_screen(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, app)).unwrap();
    buffer_text(terminal.backend().buffer())
}

/// マークが出ているときだけ、先頭に `0 Off` が立つ。`0` でも選べて、
/// `Esc` の marks の段と同じ語でフラッシュする。
#[test]
fn mark_for_offers_off_only_while_marks_are_shown() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    let presets = app.marks_questions.as_ref().unwrap().presets().len();

    // マークが無い: `0 Off` は無く、`0` は何もしない。
    crate::overlay::open_overlay(&mut app, crate::overlay::Overlay::MarkFor, 0);
    assert_eq!(crate::overlay::mark_for_entry_count(&app), presets + 1);
    let screen = mark_for_screen(&mut app);
    assert!(!screen.contains("0 Off"), "{screen}");
    assert!(screen.contains("1 Essential"), "{screen}");
    assert!(screen.contains("4 Your call"), "新しい名前: {screen}");
    on_overlay_key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(app.overlay, Some(crate::overlay::Overlay::MarkFor), "閉じない");
    app.overlay = None;

    // マークが出ている: 先頭に `0 Off`、定型は 1 行下がる。
    show_marks(&mut app);
    assert!(app.press_focus(std::time::Instant::now()));
    crate::overlay::open_overlay(&mut app, crate::overlay::Overlay::MarkFor, 0);
    assert_eq!(crate::overlay::mark_for_entry_count(&app), presets + 2);
    let screen = mark_for_screen(&mut app);
    let off_row = screen.lines().position(|l| l.contains("0 Off")).expect("0 Off が出る");
    let first_row = screen.lines().position(|l| l.contains("1 Essential")).unwrap();
    assert_eq!(off_row + 1, first_row, "0 Off は先頭:\n{screen}");
    assert!(screen.contains("clear the marks"), "{screen}");
    assert_eq!(
        crate::overlay::mark_for_entry(&app, 0),
        crate::overlay::MarkForEntry::Off
    );
    assert_eq!(
        crate::overlay::mark_for_entry(&app, 1),
        crate::overlay::MarkForEntry::Preset(0)
    );
    on_overlay_key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(app.overlay, None);
    assert!(app.semantic_doc.is_none() && app.marks_question.is_none(), "マークが消える");
    assert!(!app.focused(), "沈める先が無くなるので一緒に解く");
    assert_eq!(flashed(&app), Some("marks cleared"));
}

/// `0 Off` が出ている popup でも、数字キーと開いたときのカーソルは
/// 定型を指す（1 行ずれない）。
#[test]
fn with_off_shown_the_digits_and_the_cursor_still_point_at_the_presets() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = built_in(dir.path());
    show_marks(&mut app);
    app.marks_preset = 2;
    on_view_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE, None);
    assert_eq!(app.overlay, Some(crate::overlay::Overlay::MarkFor));
    assert_eq!(
        crate::overlay::mark_for_entry(&app, app.overlay_cursor),
        crate::overlay::MarkForEntry::Preset(2),
        "いま聞いている定型にカーソル"
    );
    // `/` は最下段の自由入力。
    let free = crate::overlay::mark_for_entry_count(&app) - 1;
    assert_eq!(crate::overlay::mark_for_entry(&app, free), crate::overlay::MarkForEntry::Free);
}
