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

use crate::config::{Config, EscQuit};
use crate::highlight::Highlighter;
use crate::ime::ImeMode;
use crate::marks_questions::Questions;
use crate::source::Source;
use unicode_width::UnicodeWidthStr;
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
        theme: Some("base16-ocean.dark".into()),
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: true,
        semantic: Some(demo(fixture)),
        semantic_cmd: None,
        marks_questions: None,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path.clone()).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
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
        theme: Some("base16-ocean.dark".into()),
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
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, 75, &highlight, Default::default());
    App::new(config, source, highlight, view, false)
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
fn a_budget_fixture_in_marks_mode_says_why_it_is_empty() {
    // **黙った 0 本にしない。** スコアの無い注釈と、該当の無い問いは
    // 別のことである。**読み出しの文字列まで見る** — 「光っていない」
    // だけを見ていると、理由を言わない実装でも通ってしまう。
    //
    // 読み出しは 2026-09-22 にフッタからタイトル行の右へ引っ越した。
    // 文言は 1 字も変わっていない。
    let app = app_with("demo.json");
    assert_eq!(app.marks_has_scores(), Some(false));
    assert!(marked(&app).is_empty());
    let readout = app.marks_readout(200).expect("理由は必ず出る");
    assert!(readout.contains("no scores"), "理由が出ていない: {readout}");
    // **逃げ道まで固定する。** 既定が marks になった（2026-09-22）ので、
    // ここに来るのは「スコアの無い fixture を既定のまま開いた」人である。
    // 理由だけ言って次の一手を言わないと、画面は光らないままになる。
    assert!(
        readout.contains("--semantic-mode budget"),
        "逃げ道が出ていない: {readout}"
    );
    // フッタはもう読み出しを持たない（操作案内だけ）。
    let footer = crate::chrome::footer_hints(&app);
    assert!(!footer.contains("no scores"), "フッタに残っている: {footer}");
    assert!(!footer.contains("MARK"), "フッタに残っている: {footer}");
}

#[test]
fn the_readout_counts_what_is_on_screen() {
    let mut app = app_with("demo-marks.json");
    for share in [1, 20, 50, 100] {
        app.marks_share = share;
        app.refresh_semantic_decorations();
        let readout = app.marks_readout(200).expect("問いも答えもある");
        let lit = app.marks_lit().unwrap();
        assert!(readout.contains(&format!("{share}%")), "{readout}");
        // 単位の「本」は 2026-09-22 に落とした（UI の言葉は全部英語）。
        // 区切りごと見て、`20%` の `20` を拾ってしまわないようにする。
        assert!(readout.contains(&format!("· {lit} ·")), "{readout}");
        assert!(readout.contains("settled"), "問いの名前が出ている: {readout}");
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
    let readout = app.marks_readout(200).expect("問いも答えもある");
    assert!(readout.starts_with("Ask: 費用の話 · "), "{readout}");
    assert!(!readout.contains('「'), "鉤括弧が残っている: {readout}");
}

#[test]
fn the_readout_drops_from_the_right_when_the_room_runs_out() {
    // **最後まで残るのは問いの名前である**（読み手の決定、2026-09-22）。
    // % と本数はつまみを動かせば分かるが、名前が無いと「なぜここが
    // 光っているのか」が読めない。
    let app = app_with("demo-marks.json");
    let full = app.marks_readout(200).unwrap();
    assert!(full.contains('%'), "広ければ全部出る: {full}");
    let narrower = app.marks_readout(full.width() - 1).unwrap();
    assert!(!narrower.contains('%'), "まず % が落ちる: {narrower}");
    assert!(narrower.contains("settled"), "{narrower}");
    let narrowest = app.marks_readout(narrower.width() - 1).unwrap();
    assert_eq!(narrowest, "settled", "最後は問いの名前だけ: {narrowest}");
    // それも入らなければ、何も言わずに引き下がる（path を削らない）。
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
    // 段 1 の 6 節: ラン間でいちばん動かないのが上位 20 %。
    assert_eq!(marks::DEFAULT_SHARE, 20);
    let app = app_with("demo-marks.json");
    assert_eq!(app.marks_share, 20);
    assert_eq!(app.marks_lit(), Some(3), "13 Unit の 20 % = 3 本");
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


// ---- 6. 既定が marks でも、層が無ければ何も起きない --------------------

#[test]
fn the_default_marks_mode_binds_nothing_without_a_layer() {
    // **既定が marks になった（2026-09-22）ことで開いた穴を塞ぐ。**
    // `state_tests` 側の同じ趣旨のテストは `--semantic-mode budget` を
    // 明示するようになったので、**出荷される既定を通るのはここだけ**である。
    //
    // `examples/semantic/README.md` が散文で約束していること —
    // 「`--semantic` を渡さなければ、これらのキーは束縛されない。読み出しも
    // `?` ヘルプの行も出ず、この層が無かったときと完全に同じ動きをする」。
    let app = app_without_a_layer();
    assert!(!app.semantic_enabled(), "層は立っていない");

    let footer = crate::chrome::footer_hints(&app);
    assert!(!footer.contains("MARK"), "つまみの読み出しが出ている: {footer}");
    assert_eq!(app.marks_readout(60), None, "読み出しが出ている");

    let rows = crate::overlay::help_rows(false, false, false, app.semantic_enabled());
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
        packed.contains("ASK▸費用"),
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
    assert!(!screen.contains("ASK ▸"), "問いのプロンプトが出ている:\n{screen}");
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

/// **沈むもの・沈まないものの線引き**（読み手の決定、2026-09-22）。
///
/// 光った Unit は**丸ごと**残す（核は琥珀、残りの文はそのまま）、見出しは
/// 残す、それ以外が沈む。ここが投影の全部である。
#[test]
fn focus_sinks_the_others_but_spares_the_lit_unit_and_the_headings() {
    let mut app = app_with("demo-marks.json");
    let lit_before = marked(&app);
    assert!(!lit_before.is_empty(), "前提: 光っている");
    assert_eq!(dimmed(&app), 0, "前提: marks は沈めない");

    assert!(app.press_focus(std::time::Instant::now()), "沈んだ");
    assert!(app.focused());
    assert_eq!(
        marked(&app),
        lit_before,
        "**琥珀は 1 本も動かない。** 沈めるのは他所であって、答えではない"
    );
    assert!(dimmed(&app) > 0, "他の Unit が沈んでいない");

    // 沈んだ範囲に、光った Unit の Atom と見出しが 1 つも入っていないこと。
    let document = app.semantic_doc.as_ref().unwrap();
    let states = semantic_reading::marks::mark(document, app.marks_share);
    let dim_ranges: Vec<_> = app
        .semantic_decorations
        .iter()
        .filter(|d| d.kind == crate::decoration::DecorationKind::Dim)
        .map(|d| d.range.clone())
        .collect();
    for unit in &document.units {
        let lit = unit.atoms.iter().any(|atom| {
            matches!(
                states.get(atom.0),
                Some((_, semantic_reading::DisplayState::Marked))
            )
        });
        if !lit {
            continue;
        }
        for atom in &unit.atoms {
            let range = &document.atoms[atom.0].range;
            assert!(
                !dim_ranges.contains(range),
                "光った Unit の文が沈んだ（核だけ浮いて段落が割れる）: {range:?}"
            );
        }
    }
    for atom in &document.atoms {
        if atom.kind == semantic_reading::AtomKind::Heading {
            assert!(
                !dim_ranges.contains(&atom.range),
                "見出しが沈んだ（沈んだ本文の中で現在地が読めなくなる）"
            );
        }
    }
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
    let lit_at_20 = marked(&app).len();
    assert!(app.nudge_marks_share(50), "つまみが効かない");
    assert!(app.focused(), "つまみでフォーカスが解けてはいけない");
    assert!(marked(&app).len() > lit_at_20, "光る本数が増えていない");
    assert!(dimmed(&app) > 0, "沈んだままであること");
}

/// 光る箇所が 1 つも無いときは沈めない — 画面が全部沈むのは
/// 「他を沈める」ではない。
#[test]
fn focus_refuses_when_nothing_is_marked() {
    let app = app_with("demo.json");
    assert_eq!(app.marks_lit(), Some(0), "前提: 0 本");
    assert!(!app.can_focus());
}

/// **画面のセルで見る。** 沈めたら本文の前景が動き、琥珀の背景は残る。
/// 旗（`focused()`）だけを見ていると、投影を切り替え忘れても通る。
#[test]
fn focus_changes_what_is_painted_not_just_a_flag() {
    let mut app = app_with("demo-marks.json");
    // **演出を止めてから撮る。** マーカーが引かれる演出は琥珀のセルを
    // ページ色から立ち上げるので（`effects::marks_reveal_effect`）、
    // 立った直後の 1 枚は「琥珀が 1 セルも無い」画面になる。
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
    assert_eq!(amber_before, amber_after, "琥珀のセル数が変わった");
    assert!(amber_after > 0, "前提: 琥珀が画面に出ている");
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

/// **読み出しはタイトル行に出て、フッタには出ない。** 画面のセルで見る
/// （`title_metrics` と `footer_hints` を別々に呼ぶと、描かれているかは
/// 分からない）。
#[test]
fn the_readout_is_drawn_in_the_title_row_and_the_footer_only_hints_keys() {
    let mut app = app_with("demo-marks.json");
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut app)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());
    let mut rows = screen.lines();
    let title = rows.next().unwrap();
    assert!(title.contains("settled"), "問いがタイトルに無い:\n{screen}");
    assert!(title.contains('%'), "つまみがタイトルに無い:\n{screen}");
    let footer = screen.lines().last().unwrap();
    assert!(!footer.contains("MARK"), "フッタに読み出しが残っている: {footer}");
    assert!(footer.contains("j/k scroll"), "操作案内が消えた: {footer}");
    assert!(footer.contains("f focus"), "f の案内が無い: {footer}");
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

/// **タイトル行では読み出しが先に譲る**（案 1。読み手の決定 2026-09-22）。
///
/// 測り方は「層の無いセッションと同じ path が出ること」である。path の
/// 短縮には `~` 表記や `…/` があるので（`truncate_path`）、字面を焼き
/// 込むと path の作法を変えたときに落ちる。見たいのは **読み出しが
/// path の取り分を削っていない**ことだけ。
///
/// `marks_readout` 単体のテスト（上）では足りない — そちらは渡された幅に
/// 収める関数で、**幅をどう配るか**はタイトルの側にある。実際にこの
/// 配り方を間違えていた（path を先に削って読み出しを丸ごと残していた
/// ＝ 案 2 の振る舞い）。
#[test]
fn the_title_shrinks_the_readout_before_the_path() {
    let app = app_with("demo-marks.json");
    // 同じ文書を、層の無い（＝読み出しの無い）セッションで開いたもの。
    let bare = app_without_a_layer();

    let wide = crate::chrome::title_metrics(&app, 200);
    assert!(wide.readout.contains('%'), "広ければ読み出しは丸ごと");

    let mut saw_a_shorter_readout = false;
    let mut saw_no_readout = false;
    for width in [120u16, 100, 90, 84, 80, 76, 72, 68] {
        let m = crate::chrome::title_metrics(&app, width);
        let without = crate::chrome::title_metrics(&bare, width);
        assert_eq!(
            m.path, without.path,
            "幅 {width}: 読み出しが path の取り分を削った"
        );
        if m.readout.width() < wide.readout.width() {
            saw_a_shorter_readout = true;
        }
        if m.readout.is_empty() {
            saw_no_readout = true;
        }
    }
    assert!(saw_a_shorter_readout, "狭くしても読み出しが縮んでいない");
    assert!(saw_no_readout, "最後まで引き下がらない");
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
    app.semantic_doc = Some(
        serde_json::from_str(&std::fs::read_to_string(demo("demo.json")).unwrap()).unwrap(),
    );
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
