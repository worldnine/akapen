//! marks モードの end-to-end — **DIM 版とは別のファイルに置いてある。**
//!
//! `docs/design/marks-only-and-review-mode.md` 0 節。ここで固定するのは
//! 5 つ:
//!
//! 1. **モードの切り替え** — 同じ注釈でも投影が変わる
//! 2. **スコアの保持** — 応答の `score` が注釈に残り、つまみが読む
//! 3. **つまみの単調性** — N を上げると光る集合は入れ子で広がる
//! 4. **0 本の許容** — 該当の無い問いは 0 本で、理由が言える
//! 5. **fixture 経路** — `--semantic <marks fixture>` で Jev を呼ばずに動く
//!
//! `state_tests.rs` には 1 行も足していない（DIM 版を触っていない証拠）。

use crate::*;

use crate::config::{Config, EscQuit, SemanticMode};
use crate::highlight::Highlighter;
use crate::ime::ImeMode;
use crate::marks_questions::Questions;
use crate::source::Source;
use crate::view::ViewState;
use semantic_reading::marks;
use std::path::PathBuf;

/// リポジトリの中の demo 一式。
fn demo(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/semantic")
        .join(name)
}

/// `demo.md` を `--semantic-mode <mode> --semantic <fixture>` で開いた App。
fn app_with(fixture: &str, mode: SemanticMode) -> App {
    let path = demo("demo.md");
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
        fx: true,
        semantic: Some(demo(fixture)),
        semantic_cmd: None,
        semantic_mode: mode,
        marks_questions: None,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path.clone()).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.semantic_mode = mode;
    app.marks_questions = Some(Questions::built_in().unwrap());
    let source = crate::semantic::source_from_config(&app.config).unwrap();
    app.set_semantic_source(source);
    app.reanalyze_semantics();
    app
}

/// 光っている（MARKED の）range の始まり。
fn marked(app: &App) -> Vec<usize> {
    app.semantic_decorations
        .iter()
        .filter(|d| d.kind == crate::decoration::DecorationKind::SemanticMark)
        .map(|d| d.range.start)
        .collect()
}

fn dimmed(app: &App) -> usize {
    app.semantic_decorations
        .iter()
        .filter(|d| d.kind == crate::decoration::DecorationKind::Dim)
        .count()
}

// ---- 1. モードの切り替え ------------------------------------------------

#[test]
fn the_same_fixture_projects_differently_in_the_two_modes() {
    // marks の fixture は Tier が全部 detail なので、budget モードでは
    // 何も光らない（`protocol.rs` の版の表の 2 行目そのもの）。
    let budget = app_with("demo-marks.json", SemanticMode::Budget);
    assert!(marked(&budget).is_empty(), "budget モードでは光らない");

    let marks = app_with("demo-marks.json", SemanticMode::Marks);
    assert!(!marked(&marks).is_empty(), "marks モードでは光る");
}

#[test]
fn marks_mode_never_dims_anything() {
    let app = app_with("demo-marks.json", SemanticMode::Marks);
    for share in [1, 20, 50, 100] {
        let mut app = app_with("demo-marks.json", SemanticMode::Marks);
        app.marks_share = share;
        app.refresh_semantic_decorations();
        assert_eq!(dimmed(&app), 0, "marks モードは DIM を出さない (share {share})");
    }
    // DIM 版は同じ文書を沈める（比較のための対照）。
    let budget = app_with("demo.json", SemanticMode::Budget);
    let mut budget = budget;
    budget.reading_budget = 20;
    budget.refresh_semantic_decorations();
    assert!(dimmed(&budget) > 0, "budget モードは沈める（対照）");
    let _ = app;
}

// ---- 2. スコアの保持 ----------------------------------------------------

#[test]
fn the_scores_survive_into_the_annotation() {
    let app = app_with("demo-marks.json", SemanticMode::Marks);
    let doc = app.semantic_doc.as_ref().expect("注釈が載っている");
    assert!(marks::has_scores(doc), "スコアが残っている");
    assert_eq!(doc.units[0].score, Some(0.94), "u1 のスコア");
    assert_eq!(doc.question.as_deref(), Some("settled"), "問いも残る");
}

#[test]
fn a_budget_fixture_in_marks_mode_says_why_it_is_empty() {
    // **黙った 0 本にしない。** スコアの無い注釈と、該当の無い問いは
    // 別のことである。**ステータス行の文字列まで見る** — 「光っていない」
    // だけを見ていると、理由を言わない実装でも通ってしまう。
    let app = app_with("demo.json", SemanticMode::Marks);
    assert_eq!(app.marks_has_scores(), Some(false));
    assert!(marked(&app).is_empty());
    let footer = crate::chrome::footer_hints(&app);
    assert!(
        footer.contains("no scores"),
        "理由がステータス行に出ていない: {footer}"
    );
}

#[test]
fn the_footer_counts_what_is_on_screen() {
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    for share in [1, 20, 50, 100] {
        app.marks_share = share;
        app.refresh_semantic_decorations();
        let footer = crate::chrome::footer_hints(&app);
        let lit = app.marks_lit().unwrap();
        assert!(footer.contains(&format!("MARK {share}%")), "{footer}");
        assert!(footer.contains(&format!("{lit}本")), "{footer}");
        assert!(footer.contains("settled"), "問いの名前が出ている: {footer}");
        // 画面の本数と一致する（footer の数字が嘘をつかない）。
        assert_eq!(lit, marked(&app).len());
    }
}

// ---- 3. つまみの単調性 --------------------------------------------------

#[test]
fn raising_the_knob_grows_a_nested_set() {
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    let mut previous: Vec<usize> = Vec::new();
    let mut counts = Vec::new();
    for share in marks::MIN_SHARE..=marks::MAX_SHARE {
        app.marks_share = share;
        app.refresh_semantic_decorations();
        let now = marked(&app);
        assert!(
            previous.iter().all(|start| now.contains(start)),
            "share {share} で光っていた箇所が消えた"
        );
        counts.push(now.len());
        previous = now;
    }
    assert!(counts.first() < counts.last(), "つまみで本数が変わる");
    // 足切りが上限を作る。fixture は 13 Unit 中 7 本が 0.20 を越える。
    assert_eq!(*counts.last().unwrap(), 7);
}

#[test]
fn the_knob_never_reaches_the_provider() {
    // つまみを動かすのに要るのは `marks::mark` だけ。世代が上がらない
    // ことでそれを言う（世代が上がる = 解析を頼んだ）。
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    let generation = app.semantic_generation;
    for _ in 0..20 {
        app.nudge_marks_share(10);
        app.nudge_marks_share(-10);
    }
    assert_eq!(app.semantic_generation, generation, "解析は 1 度も起きない");
}

#[test]
fn the_knob_stops_at_both_ends() {
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    app.marks_share = marks::MIN_SHARE;
    assert!(!app.nudge_marks_share(-1), "下限より下へは回らない");
    app.marks_share = marks::MAX_SHARE;
    assert!(!app.nudge_marks_share(1), "上限より上へは回らない");
}

#[test]
fn the_default_knob_is_the_measured_one() {
    // 段 1 の 6 節: ラン間でいちばん動かないのが上位 20 %。
    assert_eq!(marks::DEFAULT_SHARE, 20);
    let app = app_with("demo-marks.json", SemanticMode::Marks);
    assert_eq!(app.marks_share, 20);
    assert_eq!(app.marks_lit(), Some(3), "13 Unit の 20 % = 3 本");
}

// ---- 4. 0 本の許容 ------------------------------------------------------

#[test]
fn a_question_nothing_answers_lights_nothing_at_any_knob() {
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    // 該当の無い問いの形（段 1 の `demo` に「判断が要る」= 最大 0.11）を
    // 注釈の上で作る。
    if let Some(doc) = app.semantic_doc.as_mut() {
        for (n, unit) in doc.units.iter_mut().enumerate() {
            unit.score = Some(0.11 - (n as f32) * 0.005);
        }
    }
    for share in [marks::MIN_SHARE, 20, 50, marks::MAX_SHARE] {
        app.marks_share = share;
        app.refresh_semantic_decorations();
        assert!(marked(&app).is_empty(), "share {share} でも 0 本");
        assert_eq!(app.marks_lit(), Some(0));
    }
    // **スコアはある。** 「答えが無い」と「スコアを受け取っていない」は
    // 別で、ステータス行の言い方も違う。
    assert_eq!(app.marks_has_scores(), Some(true));
}

// ---- 5. fixture 経路と問いの選択 ----------------------------------------

#[test]
fn the_fixture_path_refuses_to_change_the_question() {
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    assert!(app.marks_question_is_fixed());
    assert!(!app.cycle_marks_question(1), "fixture では巡らない");
    assert!(!app.cycle_marks_question(-1), "逆回りも断る");
    assert!(!app.ask_marks_free("費用の話"), "自由入力も受けない");
    // 問いの名前は fixture が名乗っているものが出る。
    assert_eq!(app.marks_question_label(), Some("settled"));
}

#[test]
fn only_the_core_of_a_unit_lights() {
    let app = app_with("demo-marks.json", SemanticMode::Marks);
    let doc = app.semantic_doc.as_ref().unwrap();
    // fixture は Unit ごとに核を 1 つだけ持つ。光った range の数が
    // 光った Unit の数と一致する = 核だけが光っている。
    assert_eq!(marked(&app).len(), app.marks_lit().unwrap());
    assert!(doc.units.iter().all(|u| u.core_atoms.as_ref().unwrap().len() == 1));
}

#[test]
fn cycling_backwards_asks_jev_exactly_once() {
    // **1 打 = 解析 1 回。** 逆回りを「残り全部ぶん進む」で書くと、
    // 定型 4 本なら 1 打で Jev を 3 回呼ぶ（議事録で 0.3 円が 0.9 円）。
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    // fixture は問いを固定するので、巡回を見るために外部コマンドへ差し替える。
    // `true` は黙って終わるので Jev もネットワークも出てこないが、
    // **解析を頼んだ回数は世代に出る**。
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Command(
        crate::semantic::CommandProvider::new("true"),
    )));
    app.semantic_mode = SemanticMode::Marks;
    let presets = app.marks_questions.as_ref().unwrap().presets().len();
    assert!(presets >= 2);

    // 最初の 1 打は向きによらず「いまの位置」を選ぶ（まだ問いが無い）。
    assert!(app.cycle_marks_question(-1));
    assert_eq!(app.marks_preset, 0);
    let after_first = app.semantic_generation;

    // そこからの 1 打で、環の反対側へ**1 つだけ**動く。
    assert!(app.cycle_marks_question(-1));
    assert_eq!(app.marks_preset, presets - 1, "環の反対側へ 1 つだけ動く");
    assert_eq!(
        app.marks_question.as_ref().map(|q| q.id.as_str()),
        Some("numbers")
    );
    // **解析を頼んだのは 1 回だけ。** 3 回なら世代が 3 つ上がる。
    assert_eq!(
        app.semantic_generation,
        after_first + 1,
        "1 打で解析 1 回（定型の数だけ呼んでいない）"
    );

    // 次へ 1 つで先頭へ戻る。
    assert!(app.cycle_marks_question(1));
    assert_eq!(app.marks_preset, 0);
}

#[test]
fn cycling_forward_walks_the_ring_in_order() {
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Command(
        crate::semantic::CommandProvider::new("true"),
    )));
    app.semantic_mode = SemanticMode::Marks;
    let ids: Vec<String> = app
        .marks_questions
        .as_ref()
        .unwrap()
        .presets()
        .iter()
        .map(|q| q.id.clone())
        .collect();
    // 最初の 1 打はいまの位置（先頭）、以降は 1 つずつ進む。
    let mut seen = Vec::new();
    for _ in 0..ids.len() + 1 {
        assert!(app.cycle_marks_question(1));
        seen.push(app.marks_question.as_ref().unwrap().id.clone());
    }
    let mut expected = ids.clone();
    expected.push(ids[0].clone());
    assert_eq!(seen, expected, "環を 1 周して戻る");
}

// ---- キーの割り当て ------------------------------------------------------

#[test]
fn the_question_keys_do_nothing_in_budget_mode() {
    // budget モードのセッションで `m` `/` は層に触らない（`crate::keys`）。
    for c in [
        crate::keys::MARKS_CYCLE,
        crate::keys::MARKS_CYCLE_BACK,
        crate::keys::MARKS_FREE,
    ] {
        assert!(crate::keys::semantic_key(c, false).is_none());
    }
}

#[test]
fn the_amount_keys_move_the_knob_in_marks_mode() {
    use crate::keys::{SemanticKey, semantic_key};
    let mut app = app_with("demo-marks.json", SemanticMode::Marks);
    let before = app.marks_share;
    let Some(SemanticKey::Amount(delta)) = semantic_key('>', true) else {
        panic!("`>` は量のつまみ");
    };
    app.nudge_marks_share(delta);
    assert_eq!(app.marks_share, before + 10);
    // Reading Budget は動いていない（2 つの投影は互いを呼ばない）。
    assert_eq!(app.reading_budget, crate::semantic::DEFAULT_BUDGET);
}

// ---- キャッシュの鍵に問いが入る -----------------------------------------

#[test]
fn a_different_question_is_a_different_cache_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = crate::semantic_cache::SemanticCache::at(dir.path().to_path_buf());
    let source = "# 見出し\n\n本文である。\n";
    let mut document = semantic_reading::SemanticDocument::new(
        semantic_reading::atomize(source),
        vec![semantic_reading::SemanticUnit::new(
            "u1",
            [semantic_reading::AtomIndex(0)],
            semantic_reading::ReadingTier::Detail,
        )],
    );
    document.source_sha256 = Some(crate::semantic::source_digest(source));
    document.question = Some("settled".into());
    cache
        .put_asking("cmd", source, Some("決まったことか。"), &document)
        .unwrap();

    assert!(
        cache.get_asking("cmd", source, Some("決まったことか。")).is_some(),
        "同じ問いは当たる"
    );
    assert!(
        cache.get_asking("cmd", source, Some("決まっていないことか。")).is_none(),
        "別の問いは当たらない"
    );
    assert!(
        cache.get("cmd", source).is_none(),
        "DIM 版の項目とは混ざらない"
    );
}

#[test]
fn editing_a_preset_misses_the_cache_on_its_own() {
    // `docs/gotchas/semantic-reading.md`「同じコマンド行のままプロンプト
    // だけ変えると当たる」が、marks モードでは塞がっている。
    let dir = tempfile::tempdir().unwrap();
    let cache = crate::semantic_cache::SemanticCache::at(dir.path().to_path_buf());
    let source = "# 見出し\n\n本文である。\n";
    let mut document = semantic_reading::SemanticDocument::new(
        semantic_reading::atomize(source),
        Vec::new(),
    );
    document.source_sha256 = Some(crate::semantic::source_digest(source));
    document.question = Some("settled".into());
    let before = "下の「対象」は、決定・合意・確定した事柄を述べている箇所である。";
    let after = format!("{before}なお、見出しは当てはまらない。");
    cache.put_asking("cmd", source, Some(before), &document).unwrap();
    assert!(cache.get_asking("cmd", source, Some(before)).is_some());
    assert!(
        cache.get_asking("cmd", source, Some(&after)).is_none(),
        "文面を 1 文足しただけで外れる"
    );
}
