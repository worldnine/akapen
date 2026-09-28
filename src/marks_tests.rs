//! 意味層の end-to-end — **投影そのものを fixture から見る。**
//!
//! `docs/design/semantic-reading-layer.md`。ここで固定するのは 5 つ:
//!
//! 1. **投影** — スコアのある注釈が光り、つまみ以外は沈めない
//! 2. **スコアの保持** — 応答の `score` が注釈に残り、つまみが読む
//! 3. **つまみの単調性** — N を上げると光る集合は入れ子で広がる
//! 4. **0 本の許容** — 該当の無い問いは 0 本で、理由が言える
//! 5. **fixture 経路** — `--semantic <fixture>` で Jev を呼ばずに動く

use crate::*;

use crate::config::{Config, EscQuit};
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

/// `demo.md` を `--semantic <fixture>` で開いた App。
fn app_with(fixture: &str) -> App {
    let path = demo("demo.md");
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: crate::config::ThemePair::both("base16-ocean.dark"),
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: true,
        semantic: Some(demo(fixture)),
        semantic_cmd: None,
        marks_questions: None,
        review_rules: None,
        review_json: false,
        lint_cmd: None,
        undercurl: Default::default(),
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path.clone()).unwrap();
    let highlight = Highlighter::new(config.theme.for_background(false), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.marks_questions = Some(Questions::built_in().unwrap());
    let source = crate::semantic::source_from_config(&app.config).unwrap();
    app.set_semantic_source(source);
    app.reanalyze_semantics();
    app
}

/// **層を渡さない既定の起動。** `--semantic` も `--semantic-cmd` も無い。
fn app_without_a_layer() -> App {
    let path = demo("demo.md");
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: crate::config::ThemePair::both("base16-ocean.dark"),
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: true,
        semantic: None,
        semantic_cmd: None,
        // **既定をそのまま使う。** ここを書くとこのテストの意味が消える。
        marks_questions: None,
        review_rules: None,
        review_json: false,
        lint_cmd: None,
        undercurl: Default::default(),
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.for_background(false), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    App::new(config, source, highlight, view, false)
}

/// **スコアを 1 つも持たない注釈。** 「スコアの無い答え」と「該当の無い
/// 問い」は別のことで、akapen はそれを言い分ける（`App::marks_readout`）。
///
/// fixture をもう 1 つ置くのではなく、`demo-marks.json` から `score` を
/// 落として作る — 2 つのファイルが同じ文書の同じ切り方を指していないと、
/// 片方を直したときにもう片方が黙ってずれる。
fn scoreless() -> semantic_reading::SemanticDocument {
    let mut document: semantic_reading::SemanticDocument =
        serde_json::from_str(&std::fs::read_to_string(demo("demo-marks.json")).unwrap()).unwrap();
    for unit in &mut document.units {
        unit.score = None;
    }
    document
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

// ---- 1. 投影 ------------------------------------------------------------

#[test]
fn the_fixture_lights_something_up() {
    let app = app_with("demo-marks.json");
    assert!(!marked(&app).is_empty(), "スコアのある fixture は光る");
}

#[test]
fn the_projection_never_dims_anything_on_its_own() {
    // 沈めるのは `f`（フォーカス）だけである。つまみは沈めない。
    for share in [1, 20, 50, 100] {
        let mut app = app_with("demo-marks.json");
        app.marks_share = share;
        app.refresh_semantic_decorations();
        assert_eq!(dimmed(&app), 0, "つまみは DIM を出さない (share {share})");
    }
    // `f` を押したときだけ沈む（対照）。
    let mut focused = app_with("demo-marks.json");
    assert!(focused.press_focus(std::time::Instant::now()));
    assert!(dimmed(&focused) > 0, "フォーカスは沈める（対照）");
}

// ---- 2. スコアの保持 ----------------------------------------------------

#[test]
fn the_scores_survive_into_the_annotation() {
    let app = app_with("demo-marks.json");
    let doc = app.semantic_doc.as_ref().expect("注釈が載っている");
    assert!(marks::has_scores(doc), "スコアが残っている");
    assert_eq!(doc.units[0].score, Some(0.94), "u1 のスコア");
    assert_eq!(doc.question.as_deref(), Some("settled"), "問いも残る");
}

#[test]
fn a_scoreless_annotation_says_why_it_is_empty() {
    // **黙った 0 本にしない。** スコアの無い注釈と、該当の無い問いは
    // 別のことである。**読み出しの文字列まで見る** — 「光っていない」
    // だけを見ていると、理由を言わない実装でも通ってしまう。
    let mut app = app_with("demo-marks.json");
    app.semantic_doc = Some(scoreless());
    app.refresh_semantic_decorations();
    assert_eq!(app.marks_has_scores(), Some(false));
    assert!(marked(&app).is_empty());
    let readout = app.marks_readout(200).expect("理由は必ず出る").text();
    assert!(readout.contains("no scores"), "理由が出ていない: {readout}");
    // 誰のせいかまで言う。**逃げ道は無い** — DIM 版は消えたので、
    // 直すのは判定器の側である。
    assert!(
        readout.contains("the analyser returned none"),
        "誰のせいかが出ていない: {readout}"
    );
    // 理由はフッタ右下の読み出しに出る。**キー案内の側には混ざらない**
    // （打てるキーの一覧と、押しても何も起きない値は別物である）。
    let hints = crate::chrome::footer_hints(&app);
    assert!(!hints.contains("no scores"), "案内に混ざっている: {hints}");
    assert!(!hints.contains("MARK"), "案内に混ざっている: {hints}");
}

#[test]
fn the_readout_counts_what_is_on_screen() {
    let mut app = app_with("demo-marks.json");
    for share in [1, 20, 50, 100] {
        app.marks_share = share;
        app.refresh_semantic_decorations();
        let readout = app.marks_readout(200).expect("問いも答えもある");
        let lit = app.marks_lit().unwrap();
        assert!(readout.dim.contains(&format!("{share}%")), "{}", readout.text());
        // 本数は**座布団に乗る**ので、薄い文の側には出ない（2026-09-22 に
        // フッタ右下へ移した形）。前後 1 桁の余白は座布団の一部である。
        assert_eq!(readout.lit, Some(lit));
        assert_eq!(readout.count(), format!(" {lit} "));
        assert!(
            readout.dim.contains("settled"),
            "問いの名前が出ている: {}",
            readout.text()
        );
        // 画面の本数と一致する（読み出しの数字が嘘をつかない）。
        assert_eq!(lit, marked(&app).len());
    }
}

/// **自由入力の問いは `Ask: …` で出る。**
///
/// 鉤括弧（`Ask 「…」`）は日本語 UI の名残で、2026-09-22 に外した。
/// 外したのは枠だけで、**問いの本文は読み手が打ったまま**である。
#[test]
fn a_free_question_reads_as_ask_colon() {
    let mut app = app_with("demo-marks.json");
    // fixture 経路は問いを変えないので、自由入力の問いを直に載せる
    // （`ask_marks_free` は fixture では断る。上のテストで固定してある）。
    let questions = Questions::built_in().unwrap();
    app.marks_question = Some(questions.free("費用の話"));

    assert_eq!(app.marks_question_display().as_deref(), Some("Ask: 費用の話"));
    let readout = app.marks_readout(200).expect("問いも答えもある").text();
    assert!(readout.starts_with("Ask: 費用の話 · "), "{readout}");
    assert!(!readout.contains('「'), "鉤括弧が残っている: {readout}");
}

#[test]
fn the_readout_drops_from_the_right_when_the_room_runs_out() {
    // **最後まで残るのは座布団の本数である**（読み手の決定、2026-09-22。
    // フッタへ移したときに変わった）。いちばん小さくて意味がある値で、
    // % は問いを覚えていれば足り、名前は popup でも `?` でも確かめられる。
    let app = app_with("demo-marks.json");
    let lit = app.marks_lit().unwrap();
    let full = app.marks_readout(200).unwrap();
    assert!(full.dim.contains('%'), "広ければ全部出る: {}", full.text());
    let narrower = app.marks_readout(full.width() - 1).unwrap();
    assert!(!narrower.dim.contains('%'), "まず % が落ちる: {}", narrower.text());
    assert!(narrower.dim.contains("settled"), "{}", narrower.text());
    let narrowest = app.marks_readout(narrower.width() - 1).unwrap();
    assert_eq!(narrowest.dim, "", "最後は本数だけ: {}", narrowest.text());
    assert_eq!(narrowest.lit, Some(lit));
    // それも入らなければ、何も言わずに引き下がる。
    assert_eq!(app.marks_readout(narrowest.width() - 1), None);
}

// ---- 3. つまみの単調性 --------------------------------------------------

#[test]
fn raising_the_knob_grows_a_nested_set() {
    let mut app = app_with("demo-marks.json");
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
    let mut app = app_with("demo-marks.json");
    let generation = app.semantic_generation;
    for _ in 0..20 {
        app.nudge_marks_share(10);
        app.nudge_marks_share(-10);
    }
    assert_eq!(app.semantic_generation, generation, "解析は 1 度も起きない");
}

#[test]
fn the_knob_stops_at_both_ends() {
    let mut app = app_with("demo-marks.json");
    app.marks_share = marks::MIN_SHARE;
    assert!(!app.nudge_marks_share(-1), "下限より下へは回らない");
    app.marks_share = marks::MAX_SHARE;
    assert!(!app.nudge_marks_share(1), "上限より上へは回らない");
}

#[test]
fn the_default_knob_is_the_measured_one() {
    // Unit = 散文の Atom 1 つにしたときに決め直した値
    // （examples/semantic/measurements/unit-per-atom.md）。
    assert_eq!(marks::DEFAULT_SHARE, 15);
    let app = app_with("demo-marks.json");
    assert_eq!(app.marks_share, 15);
    assert_eq!(app.marks_lit(), Some(2), "13 Unit の 15 % = 2 本");
}

// ---- 4. 0 本の許容 ------------------------------------------------------

#[test]
fn a_question_nothing_answers_lights_nothing_at_any_knob() {
    let mut app = app_with("demo-marks.json");
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
    let mut app = app_with("demo-marks.json");
    assert!(app.marks_question_is_fixed());
    assert!(!app.cycle_marks_question(1), "fixture では巡らない");
    assert!(!app.cycle_marks_question(-1), "逆回りも断る");
    assert!(!app.ask_marks_free("費用の話"), "自由入力も受けない");
    // 問いの名前は fixture が名乗っているものが出る。
    assert_eq!(app.marks_question_label(), Some("settled"));
}

#[test]
fn only_the_core_of_a_unit_lights() {
    let app = app_with("demo-marks.json");
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
    let mut app = app_with("demo-marks.json");
    // fixture は問いを固定するので、巡回を見るために外部コマンドへ差し替える。
    // `true` は黙って終わるので Jev もネットワークも出てこないが、
    // **解析を頼んだ回数は世代に出る**。
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Command(
        crate::semantic::CommandProvider::new("true"),
    )));
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
    let mut app = app_with("demo-marks.json");
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Command(
        crate::semantic::CommandProvider::new("true"),
    )));
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
fn the_amount_keys_move_the_knob() {
    use crate::keys::{SemanticKey, semantic_key};
    let mut app = app_with("demo-marks.json");
    let before = app.marks_share;
    let Some(SemanticKey::Amount(delta)) = semantic_key('>') else {
        panic!("`>` は量のつまみ");
    };
    app.nudge_marks_share(delta);
    assert_eq!(app.marks_share, before + 10);
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
        )],
    );
    document.source_sha256 = Some(crate::semantic::source_digest(source));
    document.question = Some("settled".into());
    cache
        .put("cmd", source, "決まったことか。", &document)
        .unwrap();

    assert!(
        cache.get("cmd", source, "決まったことか。").is_some(),
        "同じ問いは当たる"
    );
    assert!(
        cache.get("cmd", source, "決まっていないことか。").is_none(),
        "別の問いは当たらない"
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
    cache.put("cmd", source, before, &document).unwrap();
    assert!(cache.get("cmd", source, before).is_some());
    assert!(
        cache.get("cmd", source, &after).is_none(),
        "文面を 1 文足しただけで外れる"
    );
}


// ---- 6. 既定が marks でも、層が無ければ何も起きない --------------------

#[test]
fn the_default_marks_mode_binds_nothing_without_a_layer() {
    //
    // `examples/semantic/README.md` が散文で約束していること —
    // 「`--semantic` を渡さなければ、これらのキーは束縛されない。読み出しも
    // `?` ヘルプの行も出ず、この層が無かったときと完全に同じ動きをする」。
    let app = app_without_a_layer();
    assert!(!app.semantic_enabled(), "層は立っていない");

    let hints = crate::chrome::footer_hints(&app);
    assert!(!hints.contains("MARK"), "つまみの読み出しが出ている: {hints}");
    assert_eq!(app.marks_readout(60), None, "読み出しが出ている");
    assert!(
        crate::chrome::footer_metrics(&app, 120).readout.is_none(),
        "層が無いのにフッタが読み出しの場所を取っている"
    );

    let rows = crate::overlay::help_rows(false, false, false, app.semantic_enabled(), app.semantic_enabled());
    assert!(
        !rows.iter().any(|(label, _)| *label == "amount"),
        "? ヘルプが使えないキーを宣伝している"
    );

    // **キーの門番は `semantic_enabled()` の側にある。** 割り当ての方は
    // 7 本とも `Some` を返す — だから門番が閉じていることがそのまま
    // 「束縛されない」の中身である（`main` の `KeyCode::Char(c)` の腕は
    // 2 つの条件の AND）。
    for c in ['-', '+', '=', '<', '>', 'm', 'M'] {
        assert!(
            crate::keys::semantic_key(c).is_some(),
            "{c} は割り当てにある（門番だけが止めている）"
        );
    }
    assert!(!app.semantic_enabled(), "その門番が閉じている");
}

// ---- 問いの入力は composer を借りない -----------------------------------

/// **画面のセルを見る。** 「composer を弾いた」を旗の値で確かめると、
/// `composing` の定義が 2 か所ある（`draw` の演出の門と `draw_view` の
/// 描画）ので、片方だけ直して通ってしまう — **実際にそれを踏んだ**
/// （2026-09-22。1 行プロンプトと `comment · 53` の吹き出しが同時に出て
/// いるのを実機の GIF で見つけた）。`docs/gotchas/rendering.md`
/// 「片方だけを見て『直った』と判断しないこと」。
#[test]
fn the_question_prompt_does_not_open_the_comment_composer() {
    let mut app = app_with("demo-marks.json");
    app.marks_questions = Some(Questions::built_in().unwrap());
    // `/` を押した状態を作る（fixture 経路は問いを固定するので、旗と
    // モードだけを直に置く — ここで見たいのは描画である）。
    app.mode = Mode::Input;
    app.composer_return = Mode::View;
    app.marks_prompt = true;
    app.input = "費用".to_string();
    app.input_cursor = app.input.len();

    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    assert!(
        !screen.contains("comment ·"),
        "コメントの吹き出しが出ている（1 行プロンプトのはず）:\n{screen}"
    );
    // 全角は 2 セルを占め、後ろ半分は空白のセルになる。字面ではなく
    // **空白を落とした形**で見る。
    let packed: String = screen.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        packed.contains("ASK❯費用"),
        "1 行プロンプトが出ていない:\n{screen}"
    );
    assert!(
        screen.contains("Enter ask"),
        "フッタが問いの入力のヒントになっていない:\n{screen}"
    );
}

/// コメントの composer の方は**今までどおり**吹き出しで出る（`c`）。
/// 上のテストだけだと「composer が壊れた」でも通ってしまう。
#[test]
fn the_comment_composer_still_opens_its_bubble() {
    let mut app = app_with("demo-marks.json");
    app.mode = Mode::Input;
    app.composer_return = Mode::View;
    app.marks_prompt = false;
    app.input = "ここ".to_string();
    app.input_cursor = app.input.len();

    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    assert!(screen.contains("comment ·"), "吹き出しが消えた:\n{screen}");
    assert!(!screen.contains("ASK ❯"), "問いのプロンプトが出ている:\n{screen}");
}

/// TestBackend のセルを行ごとの文字列にする。
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

// ---- 6. フォーカス（`f`） ----------------------------------------------

/// **沈むもの・沈まないものの線引き**（読み手の決定、2026-09-24）。
///
/// 光った文（MARKED の Atom）は**ふつうの明るさで、琥珀は描かない**。見出しは
/// 残す。それ以外は全部沈む — 光った Unit の核でない文も沈む（2026-09-24
/// までは沈めずに残していた）。ここが投影の全部である。
#[test]
fn focus_drops_the_amber_and_sinks_everything_but_the_lit_sentences_and_headings() {
    let mut app = app_with("demo-marks.json");
    let lit_before = marked(&app);
    assert!(!lit_before.is_empty(), "前提: 光っている");
    assert_eq!(dimmed(&app), 0, "前提: marks は沈めない");

    assert!(app.press_focus(std::time::Instant::now()), "沈んだ");
    assert!(app.focused());
    assert!(
        marked(&app).is_empty(),
        "**琥珀は描かない**（線は消してよい、が読み手の注文）"
    );
    assert!(dimmed(&app) > 0, "他所が沈んでいない");

    // Atom ごとに: 光った文と見出しには装飾が無く、それ以外は沈む。
    let document = app.semantic_doc.as_ref().unwrap();
    let states = semantic_reading::marks::mark(document, app.marks_share);
    let decorated = |range: &std::ops::Range<usize>| {
        app.semantic_decorations
            .iter()
            .find(|d| &d.range == range)
            .map(|d| d.kind)
    };
    let mut sunk_in_a_lit_unit = 0;
    for (index, atom) in document.atoms.iter().enumerate() {
        let lit = matches!(
            states.get(index),
            Some((_, semantic_reading::DisplayState::Marked))
        );
        let got = decorated(&atom.range);
        if lit {
            assert_eq!(
                got, None,
                "光った文に装飾が付いた（ふつうの明るさのはず）: {:?}",
                atom.range
            );
        } else if atom.kind == semantic_reading::AtomKind::Heading {
            assert_eq!(
                got, None,
                "見出しが沈んだ（沈んだ本文の中で現在地が読めなくなる）"
            );
        } else {
            assert_eq!(
                got,
                Some(crate::decoration::DecorationKind::Dim),
                "光っていない文が沈んでいない: {:?}",
                atom.range
            );
            let in_a_lit_unit = document.units.iter().any(|unit| {
                unit.atoms.iter().any(|a| a.0 == index)
                    && unit.atoms.iter().any(|a| {
                        matches!(
                            states.get(a.0),
                            Some((_, semantic_reading::DisplayState::Marked))
                        )
                    })
            });
            sunk_in_a_lit_unit += usize::from(in_a_lit_unit);
        }
    }
    assert!(
        sunk_in_a_lit_unit > 0,
        "前提: この fixture には光った Unit の核でない文があり、それも沈む"
    );
}

/// **溝の目盛りと `]m` はフォーカス中も残る。** 琥珀を描かないだけで、
/// 台帳は marks の投影から数える（`App::refresh_semantic_decorations`）。
#[test]
fn focus_keeps_the_ruler_ticks_and_the_mark_jumps() {
    let mut app = app_with("demo-marks.json");
    app.marks_share = 20;
    app.refresh_semantic_decorations();
    let lines = app.marks_lines.clone();
    assert!(lines.len() >= 2, "前提: マーク行が 2 本以上");

    // 溝が立つ高さで、沈める前と後の目盛りを数える。
    app.config.fx = false;
    app.marks_fx = None;
    app.readout_fx = None;
    let backend = ratatui::backend::TestBackend::new(120, 12);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let ticks_before = ticks_in_the_track(terminal.backend().buffer());
    assert!(ticks_before > 0, "前提: 目盛りが出ている");

    assert!(app.press_focus(std::time::Instant::now()));
    assert!(marked(&app).is_empty(), "前提: 琥珀は描いていない");
    assert_eq!(app.marks_lines, lines, "沈めたら台帳が変わった");
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    assert_eq!(
        ticks_in_the_track(terminal.backend().buffer()),
        ticks_before,
        "沈めたら目盛りが消えた"
    );

    // `]m` も同じ台帳へ飛ぶ。
    app.mode = Mode::View;
    app.view.cursor = lines[0];
    crate::jump_mark(&mut app, 1);
    assert_eq!(app.view.cursor, lines[1], "沈めたまま次のマークへ飛べない");
    assert!(app.focused(), "ジャンプでフォーカスが解けてはいけない");

    // つまみを動かしても、台帳は沈めていないときと同じものになる。
    assert!(app.nudge_marks_share(30));
    let focused_lines = app.marks_lines.clone();
    assert!(app.clear_focus());
    assert_eq!(
        app.marks_lines, focused_lines,
        "沈めているときだけ台帳がずれる"
    );
}

/// フォーカスを解けば元に戻る — **元の投影と 1 バイトも違わない**。
#[test]
fn leaving_focus_restores_the_plain_marks_projection() {
    let mut app = app_with("demo-marks.json");
    let before = app.semantic_decorations.clone();
    app.press_focus(std::time::Instant::now());
    assert_ne!(app.semantic_decorations, before, "沈んだ");
    assert!(app.clear_focus(), "解けた");
    assert_eq!(app.semantic_decorations, before, "戻っていない");
}

/// **沈んだまま問いとつまみを動かせる**（注文の要）。
#[test]
fn the_knob_and_the_question_still_work_while_focused() {
    let mut app = app_with("demo-marks.json");
    app.press_focus(std::time::Instant::now());
    // 沈めている間は琥珀を描かないので、光る本数は台帳と読み出しで数える。
    let lit_before = app.marks_lit().unwrap();
    let dimmed_before = dimmed(&app);
    assert!(app.nudge_marks_share(50), "つまみが効かない");
    assert!(app.focused(), "つまみでフォーカスが解けてはいけない");
    assert!(
        app.marks_lit().unwrap() > lit_before,
        "光る本数が増えていない"
    );
    assert!(dimmed(&app) > 0, "沈んだままであること");
    assert!(
        dimmed(&app) < dimmed_before,
        "光った文が増えたぶん、沈む文が減っていない"
    );
}

/// 光る箇所が 1 つも無いときは沈めない — 画面が全部沈むのは
/// 「他を沈める」ではない。
#[test]
fn focus_refuses_when_nothing_is_marked() {
    let mut app = app_with("demo-marks.json");
    app.semantic_doc = Some(scoreless());
    app.refresh_semantic_decorations();
    let app = app;
    assert_eq!(app.marks_lit(), Some(0), "前提: 0 本");
    assert!(!app.can_focus());
}

/// **画面のセルで見る。** 沈めたら本文の前景が動き、琥珀の背景は消える。
/// 旗（`focused()`）だけを見ていると、投影を切り替え忘れても通る。
#[test]
fn focus_changes_what_is_painted_not_just_a_flag() {
    let mut app = app_with("demo-marks.json");
    // **演出を止めてから撮る。** マーカーが引かれる演出は琥珀のセルを
    // ページ色から立ち上げるので（`effects::marks_reveal_effect`）、
    // 立った直後の 1 枚は
    // 「確定色の琥珀が 1 セルも無い」画面になる。
    // 見たいのは静止した絵である。
    app.config.fx = false;
    app.marks_fx = None;
    app.readout_fx = None;
    // 文書が丸ごと入る高さで見る（マークが画面の外にいると、沈んだか
    // どうかを画面から読めない）。
    let backend = ratatui::backend::TestBackend::new(120, (app.view.rows.len() + 4) as u16);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let before = terminal.backend().buffer().clone();

    app.press_focus(std::time::Instant::now());
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let after = terminal.backend().buffer().clone();
    assert!(app.focused(), "前提: 沈んでいる");

    let area = *before.area();
    let dimmed_cells = (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| before[(x, y)].fg != after[(x, y)].fg)
        .count();
    assert!(dimmed_cells > 0, "沈んだセルが 1 つも無い");
    let amber = app.decoration_styles.mark_bg();
    let amber_before = (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| before[(x, y)].bg == amber)
        .count();
    let amber_after = (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| after[(x, y)].bg == amber)
        .count();
    assert!(amber_before > 0, "前提: 沈める前は琥珀が画面に出ている");
    assert_eq!(amber_after, 0, "沈めたら琥珀は 1 セルも残らない");
}

/// `FOCUS` はフッタのモードバッジのスロットに出る。**選択が優先する**。
#[test]
fn the_focus_badge_sits_in_the_mode_slot_and_yields_to_select() {
    let mut app = app_with("demo-marks.json");
    app.press_focus(std::time::Instant::now());
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("FOCUS"), "バッジが出ていない:\n{screen}");
    assert!(!screen.contains(" VIEW "), "VIEW と二重に出ている:\n{screen}");

    app.selection = Some(crate::Selection::new(0));
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("SELECT"), "選択が優先していない:\n{screen}");
    assert!(!screen.contains("FOCUS"), "両方出ている:\n{screen}");
}

// ---- 7. 読み出しの移設 --------------------------------------------------

/// **読み出しはフッタの右下に出て、タイトル行には出ない**（2026-09-22 に
/// 移した）。画面のセルで見る — `footer_metrics` と `title_metrics` を
/// 別々に呼んでも、実際に描かれているかは分からない。
#[test]
fn the_readout_is_drawn_at_the_footers_right_and_the_title_keeps_the_path() {
    let mut app = app_with("demo-marks.json");
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());
    let title = screen.lines().next().unwrap();
    assert!(!title.contains("settled"), "タイトルに読み出しが残っている: {title}");
    assert!(!title.contains('%'), "タイトルにつまみが残っている: {title}");
    let footer = screen.lines().last().unwrap();
    assert!(footer.contains("settled"), "問いがフッタに無い:\n{screen}");
    assert!(footer.contains('%'), "つまみがフッタに無い:\n{screen}");
    assert!(footer.contains("j/k scroll"), "操作案内が消えた: {footer}");
    assert!(footer.contains("f focus"), "f の案内が無い: {footer}");
    // 読み出しは**右端**に寄る（本数の座布団の右の余白が最後の桁）。
    let lit = app.marks_lit().unwrap();
    assert!(
        footer.trim_end().ends_with(&format!(" {lit}")),
        "本数が右端に無い: {footer}"
    );
}

/// **本数はマーカーと同じ琥珀の座布団に乗る。** セルの色で見る — 字面
/// だけ見ていると、薄い灰のまま出ていても通ってしまう。
#[test]
fn the_count_sits_on_an_amber_cushion() {
    let mut app = app_with("demo-marks.json");
    // 静止した絵を見る（`docs/gotchas/rendering.md`「演出の立っている
    // 1 枚目には琥珀が無い」）。
    app.config.fx = false;
    app.marks_fx = None;
    app.readout_fx = None;
    let amber = app.decoration_styles.mark_tick();
    let lit = app.marks_lit().unwrap();
    assert!(lit > 0, "座布団を敷く前提が崩れている");
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = 23;
    let cushion: String = (0..120)
        .filter(|x| buffer[(*x, row)].style().bg == Some(amber))
        .map(|x| buffer[(x, row)].symbol().to_string())
        .collect();
    // 前後 1 桁の余白込み。`FOCUS` バッジも同じ琥珀なので、そちらが
    // 出ていない VIEW のときだけこの形になる。
    assert_eq!(cushion, format!(" {lit} "), "座布団が琥珀で敷かれていない");
    let digit_x = (0..120u16)
        .find(|x| buffer[(*x, row)].style().bg == Some(amber))
        .unwrap()
        + 1;
    // **`Rgb(0,0,0)` であって `Color::Black` ではない** — フラッシュの
    // lerp は RGB 同士でしか混ざらない（下のテストが理由を書いている）。
    assert_eq!(
        buffer[(digit_x, row)].style().fg,
        Some(ratatui::style::Color::Rgb(0, 0, 0)),
        "座布団の字が黒くない"
    );
}

/// **300 ms のフラッシュは座布団の上にだけ乗る。**
///
/// 実機では速すぎて captured frame に写らない（CLI の往復が 300 ms より
/// 遅い）ので、演出を途中まで進めた 1 枚をここで見る。`last_draw` を
/// 過去にずらすと、`draw` が出す差分がそのまま演出の進み方になる。
#[test]
fn the_flash_lands_on_the_cushion_and_nowhere_else() {
    use std::time::{Duration, Instant};
    let mut app = app_with("demo-marks.json");
    app.marks_fx = None;
    let amber = app.decoration_styles.mark_tick();
    let lit = app.marks_lit().unwrap();
    assert!(lit > 0, "座布団を敷く前提が崩れている");

    // 演出を立て、300 ms のうち 150 ms ぶん進んだところで 1 枚描く。
    app.start_readout_flash();
    assert!(app.readout_fx.is_some(), "演出が立っていない");
    app.last_draw = Some(Instant::now() - Duration::from_millis(150));
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = 23;

    // 座布団の桁。地色は琥珀のままで、**字が動いている**
    // （静止時は黒。琥珀から黒へ落ちてくる途中なので、まだ黒ではない）。
    let cushion: Vec<u16> = (0..120)
        .filter(|x| buffer[(*x, row)].style().bg == Some(amber))
        .collect();
    // 前後 1 桁の余白込み（本数の桁数で幅は変わる）。
    assert_eq!(cushion.len(), format!(" {lit} ").len(), "座布団の幅が違う");
    let digit = buffer[(cushion[1], row)].style().fg.unwrap();
    assert_ne!(
        digit,
        ratatui::style::Color::Rgb(0, 0, 0),
        "座布団が光っていない（静止時の黒のまま）"
    );
    assert_ne!(digit, amber, "演出が始まっていない（alpha が 0 のまま）");
    // **名前付きの色だと演出が素通りする。** `crate::view::lerp_color` は
    // RGB 同士でしか混ぜず、それ以外は行き先をそのまま返す — 座布団の字を
    // `Color::Black` にしていた最初の実装は、ここで 1 フレームも光らな
    // かった（`docs/gotchas/rendering.md`）。
    assert!(
        matches!(digit, ratatui::style::Color::Rgb(..)),
        "混ざった結果が RGB でない: {digit:?}"
    );

    // **左の薄い字には乗らない。** 面で掴んでいるので、同じ行に並ぶ
    // `Essential · 20%` やキー案内は 1 桁も触られない。
    let dim = buffer[(cushion[0] - 2, row)].style().fg;
    assert_eq!(
        dim,
        Some(ratatui::style::Color::DarkGray),
        "座布団の外まで光っている"
    );
}

/// **タイムマシン中は読み出しが出ない**（過去の世代ではフッタを timeline
/// bar が覆っている）。**マーカーは引かれたまま**である — 消えるのは
/// 値の方だけ。
#[test]
fn the_readout_steps_aside_in_the_past() {
    let mut app = app_with("demo-marks.json");
    assert!(!app.is_historical(), "fixture は NOW から始まる");
    let now = crate::chrome::footer_metrics(&app, 120);
    assert!(now.readout.is_some(), "NOW では読み出しが出る");
    assert!(now.flash_w > 0, "NOW では演出の面がある");

    // 1 世代前へ入る（この fixture の App は git の外なので、履歴を
    // 手で組む — 見たいのは「過去にいる」だけである）。
    let revision = |short: &str| crate::history::Revision {
        id: Some(format!("local:{short}")),
        short_id: short.into(),
        summary: "local snapshot".into(),
        content: app.source.content.clone(),
        source: crate::history::RevisionSource::Local,
        timestamp_ms: None,
    };
    app.histories = vec![crate::history::DocumentHistory {
        revisions: vec![revision("now"), revision("old")],
        position: 1,
        rendered_position: 1,
        baseline_id: None,
        baseline_content: None,
        git: None,
    }];
    assert!(app.is_historical());

    let past = crate::chrome::footer_metrics(&app, 120);
    assert!(past.readout.is_none(), "過去の世代で読み出しが出ている");
    assert_eq!(past.flash_w, 0, "覆われた行で演出が走る");

    // **立てた演出は 1 枚描いた時点で捨てられる。** 描かずに見送ると
    // tachyonfx のタイマーが進まず、`done()` にならないまま
    // `has_active_fx` が速いティックを掴み続ける（`crate::draw`）。
    app.start_readout_flash();
    assert!(app.readout_fx.is_some(), "演出が立っていない");
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    assert!(app.readout_fx.is_none(), "面の無い演出が残った");
    // マーカーは残る（`refresh_semantic_decorations` は通っていない）。
    assert!(!app.semantic_decorations.is_empty(), "マーカーまで消えた");
    // 余った幅はキー案内に回る。
    assert!(past.hints.len() >= now.hints.len(), "案内が戻っていない");
}

/// 層の無いセッションのタイトル行は**1 バイトも変わらない**。
#[test]
fn a_session_without_the_layer_has_no_readout() {
    let mut app = app_without_a_layer();
    assert_eq!(app.marks_readout(200), None);
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());
    let footer = screen.lines().last().unwrap();
    assert!(!footer.contains("f focus"), "使えないキーを案内している: {footer}");
}

// ---- 8. 目盛りとジャンプ ------------------------------------------------

/// **溝の目盛りとジャンプ先は同じ台帳を見る。**
#[test]
fn the_ruler_and_the_jump_read_the_same_ledger() {
    let app = app_with("demo-marks.json");
    assert!(!app.marks_lines.is_empty(), "マーク行が無い");
    let lines = app.marks_lines.clone();
    assert!(lines.windows(2).all(|w| w[0] < w[1]), "昇順で重複なし");
    // 台帳の行は、実際に光っている範囲の行である。
    let starts = tui_markdown::line_starts(&app.source.content);
    for decoration in &app.semantic_decorations {
        if decoration.kind != crate::decoration::DecorationKind::SemanticMark {
            continue;
        }
        let line = tui_markdown::line_at(&starts, decoration.range.start);
        assert!(lines.contains(&line), "L{line} が台帳に無い");
    }
}

/// 環である（最後の次は最初）。
#[test]
fn the_mark_jump_wraps_around() {
    let mut app = app_with("demo-marks.json");
    let lines = app.marks_lines.clone();
    app.mode = Mode::View;
    // **いまいる行より先**のマークへ飛ぶ（同じ行には留まらない）。
    app.view.cursor = lines[0];
    crate::jump_mark(&mut app, 1);
    assert_eq!(app.view.cursor, lines[1], "次のマークへ");
    app.view.cursor = *lines.last().unwrap();
    crate::jump_mark(&mut app, 1);
    assert_eq!(app.view.cursor, lines[0], "末尾の次は先頭へ回る");
    crate::jump_mark(&mut app, -1);
    assert_eq!(app.view.cursor, *lines.last().unwrap(), "逆回りも環");
}

/// 溝の桁に立っている点の数。**`·` は読み出しの区切りにも使われている**
/// ので、画面全部から数えると読み出しを数えてしまう（実際に踏んだ）。
fn ticks_in_the_track(buf: &ratatui::buffer::Buffer) -> usize {
    let area = *buf.area();
    // view モードのスクロールバーは右の枠線の 1 つ内側。
    let column = area.right().saturating_sub(2);
    (area.y..area.bottom())
        .filter(|&y| buf[(column, y)].symbol() == "·")
        .count()
}

/// 溝が無い（本文が収まっている）ときは目盛りを打たない。
#[test]
fn no_track_no_ticks() {
    let mut app = app_with("demo-marks.json");
    // 3 本目（結論の一文）まで光らせる。既定の 15 % では 2 本とも冒頭に
    // あって、12 行の溝ではつまみの影に入る。
    app.marks_share = 20;
    app.refresh_semantic_decorations();
    // 本文が全部入る高さ。
    let tall = (app.view.rows.len() + 10) as u16;
    let backend = ratatui::backend::TestBackend::new(120, tall);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    assert_eq!(
        ticks_in_the_track(terminal.backend().buffer()),
        0,
        "溝が無いのに点が出ている"
    );

    // 狭くすればスクロールバーが立ち、目盛りが出る。
    let backend = ratatui::backend::TestBackend::new(120, 12);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    assert!(
        ticks_in_the_track(terminal.backend().buffer()) > 0,
        "目盛りが出ていない"
    );
}

/// **タイトル行は層の有無で 1 桁も変わらない**（2026-09-22 に読み出しを
/// フッタへ移したので、path の取り分を削る物がもう無い）。
///
/// 測り方は「層の無いセッションと同じ path が出ること」である。path の
/// 短縮には `~` 表記や `…/` があるので（`truncate_path`）、字面を焼き
/// 込むと path の作法を変えたときに落ちる。
///
/// **これが移した理由そのものである。** 前は読み出しがタイトルに同居
/// していて、ファイル名が長いだけで丸ごと引き下がっていた。
#[test]
fn the_title_never_yields_the_path_to_the_layer() {
    let app = app_with("demo-marks.json");
    // 同じ文書を、層の無いセッションで開いたもの。
    let bare = app_without_a_layer();
    for width in [200u16, 120, 100, 90, 84, 80, 76, 72, 68, 60, 40] {
        let m = crate::chrome::title_metrics(&app, width);
        let without = crate::chrome::title_metrics(&bare, width);
        assert_eq!(m.path, without.path, "幅 {width}: path が層に削られた");
        assert_eq!(m.path_w, without.path_w, "幅 {width}");
    }
}

/// **沈んだまま 0 本の問いへ移っても、`f` で戻れる。**
///
/// 断るのは「沈める先が無いのに沈めようとした」ときだけである。沈んだ
/// あとに 0 本になった画面で `f` を断ると、Esc しか出口が無くなる。
#[test]
fn focus_can_always_be_turned_off_even_when_nothing_is_lit() {
    let mut app = app_with("demo-marks.json");
    // 1 秒前に押した形にする（auto-repeat よけの門は 120 ms なので、
    // テストの中で 2 打が同じ瞬間になると 2 打目が捨てられる）。
    app.press_focus(std::time::Instant::now() - std::time::Duration::from_secs(1));
    assert!(app.focused());
    // 問いが変わって 0 本になった画面（スコアを持たない注釈で作る）。
    app.semantic_doc = Some(scoreless());
    app.refresh_semantic_decorations();
    assert_eq!(app.marks_lit(), Some(0), "前提: 0 本");
    assert!(!app.can_focus(), "前提: 新しく沈めることはできない");
    crate::press_focus(&mut app);
    assert!(!app.focused(), "沈んだまま出られない");
}

/// **沈んだ本文の上にカーソル帯が乗っても、字は沈んだままにならない。**
///
/// `docs/gotchas/rendering.md`「`Dim` は前景色を書く — 帯の下では装飾
/// する前に落とす」。既存の門（`view.rs` の `banded`）が **フォーカスの
/// 投影にもそのまま効く**ことを、画面のセルで確かめる。効いていないと、
/// カーソルを置いた行の字だけが霞んで読めなくなる。
#[test]
fn the_cursor_band_over_a_sunken_line_is_not_dim() {
    let mut app = app_with("demo-marks.json");
    app.config.fx = false;
    app.marks_fx = None;
    app.readout_fx = None;
    app.press_focus(std::time::Instant::now());

    // 沈んでいる装飾の、最初のソース行。
    let starts = tui_markdown::line_starts(&app.source.content);
    let sunken_line = app
        .semantic_decorations
        .iter()
        .find(|d| d.kind == crate::decoration::DecorationKind::Dim)
        .map(|d| tui_markdown::line_at(&starts, d.range.start))
        .expect("沈んだ行がある");

    let backend = ratatui::backend::TestBackend::new(120, (app.view.rows.len() + 4) as u16);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();

    // カーソルが他所にある間は沈んでいる（前提）。
    app.view.cursor = 0;
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let row = app.view.source_starts[sunken_line] as u16 + 2; // 枠 1 行 + 先頭行
    let dim_fg = app.decoration_styles.dim_fg(None);
    let buf = terminal.backend().buffer().clone();
    let area = *buf.area();
    let dim_cells = |buf: &ratatui::buffer::Buffer| {
        (area.x..area.right())
            .filter(|&x| buf[(x, row)].fg == dim_fg)
            .count()
    };
    assert!(dim_cells(&buf) > 0, "前提: その行は沈んでいる");

    // カーソルを置くと帯が乗り、字は沈んだ色をやめる。
    app.view.cursor = sunken_line;
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let banded = terminal.backend().buffer().clone();
    assert_eq!(
        dim_cells(&banded),
        0,
        "帯の下で字が沈んだまま（gotchas の「帯の下では装飾する前に落とす」）"
    );
    assert!(
        (area.x..area.right()).any(|x| banded[(x, row)].bg == app.ui_selected_bg),
        "帯そのものが出ていない"
    );
}

/// 沈んだ本文の上でもコメントの吹き出しは普通に出る（沈まない）。
#[test]
fn a_comment_bubble_over_the_sunken_body_is_not_dim() {
    let mut app = app_with("demo-marks.json");
    app.config.fx = false;
    app.marks_fx = None;
    app.readout_fx = None;
    app.press_focus(std::time::Instant::now());
    app.mode = Mode::Input;
    app.composer_return = Mode::View;
    app.marks_prompt = false;
    app.input = "ここは".to_string();
    app.input_cursor = app.input.len();

    let backend = ratatui::backend::TestBackend::new(120, (app.view.rows.len() + 6) as u16);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let screen = buffer_text(&buf);
    assert!(screen.contains("comment ·"), "吹き出しが出ていない:\n{screen}");

    // 吹き出しの行に沈んだ色が 1 セルも無いこと（吹き出しは source の
    // 範囲を持たないので装飾に掴まれない ——「掴まれないはず」ではなく
    // 掴まれていないことを見る）。
    let dim_fg = app.decoration_styles.dim_fg(None);
    let area = *buf.area();
    let bubble_rows: Vec<u16> = (area.y..area.bottom())
        .filter(|&y| {
            (area.x..area.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .contains("comment ·")
        })
        .collect();
    assert!(!bubble_rows.is_empty());
    for y in bubble_rows {
        assert_eq!(
            (area.x..area.right())
                .filter(|&x| buf[(x, y)].fg == dim_fg)
                .count(),
            0,
            "吹き出しの行が沈んでいる"
        );
    }
}
