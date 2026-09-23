//! **コメントと候補の結び目** — 同じ行に同じルールの候補が 2 本あっても、
//! 1 本ずつ accept / 取り消しでき、見届けた reload で結び目が写り、送る文面が
//! 行の中の位置を言う。
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節「コメントと候補の結び目」。
//! 候補は lint の経路で作る（範囲を字の単位で置けるので、同じ行の 2 か所を
//! 指せる）。キーは本番と同じ入口から押す。

use crate::*;

use crate::review::CandidateState;
use crate::review_tests::{lint_only, rewrite_and_reload};

/// 1 段落を 1 行で書いた文書。3 行目に「かも」が 2 回ある。
const HEDGES: &str = "\
# みだし

これは効くかもしれないし、効かないかもしれない。

つぎの段落。
";

const RULE: &str = "textlint/ja-no-weak-phrase";

/// `doc` の `byte` から `chars` 字に当たる Diagnostic（同じ語の 2 回目も指せる）。
fn diagnostic_at(doc: &str, byte: usize, chars: usize) -> serde_json::Value {
    let line = doc[..byte].matches('\n').count();
    let line_start = doc[..byte].rfind('\n').map_or(0, |i| i + 1);
    let character: usize = doc[line_start..byte].chars().map(char::len_utf16).sum();
    let width: usize = doc[byte..].chars().take(chars).map(char::len_utf16).sum();
    serde_json::json!({
        "range": {"start": {"line": line, "character": character},
                  "end": {"line": line, "character": character + width}},
        "message": "弱い表現", "source": "textlint", "code": "ja-no-weak-phrase", "severity": 2,
    })
}

/// `doc` の「かも」2 か所を候補にして渡す（本番の入口）。
fn deliver_hedges(app: &mut App, doc: &str) {
    let diagnostics = vec![
        diagnostic_at(doc, doc.find("かも").unwrap(), 2),
        diagnostic_at(doc, doc.rfind("かも").unwrap(), 2),
    ];
    crate::review_tests::deliver_lint(app, diagnostics);
}

/// `HEDGES` を開いて「かも」2 本を候補にし、一覧を開いた App。
fn hedges(dir: &std::path::Path) -> App {
    let mut app = lint_only(dir, "true");
    rewrite_and_reload(&mut app, HEDGES);
    deliver_hedges(&mut app, HEDGES);
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 0;
    assert_eq!(app.review_candidates.len(), 2, "前提: 候補が 2 本");
    assert_eq!(
        app.review_candidates[0].lines, app.review_candidates[1].lines,
        "前提: 同じ行"
    );
    assert_eq!(app.review_candidates[0].rule, app.review_candidates[1].rule, "前提: 同じルール");
    app
}

fn press_at(app: &mut App, index: usize, key: char) {
    app.overlay_cursor = index;
    crate::overlay::on_review_overlay_key(app, KeyCode::Char(key), KeyModifiers::NONE);
}

fn states(app: &App) -> Vec<CandidateState> {
    app.candidate_states()
}

// ---- 照合 -----------------------------------------------------------

#[test]
fn two_candidates_on_one_line_are_accepted_and_undone_one_at_a_time() {
    use CandidateState::*;
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());

    press_at(&mut app, 0, 'a');
    assert_eq!(states(&app), [Accepted, Pending], "1 本目だけが ✓");
    assert_eq!(app.comments.len(), 1);

    press_at(&mut app, 1, 'a');
    assert_eq!(states(&app), [Accepted, Accepted]);
    assert_eq!(app.comments.len(), 2, "2 本目もコメントになる（1 本目に吸われない）");

    // 1 本目を戻すと、1 本目のコメントだけが消える。
    press_at(&mut app, 0, 'a');
    assert_eq!(states(&app), [Pending, Accepted], "戻るのは押した方だけ");
    assert_eq!(app.comments.len(), 1);
    let anchor = app.comments[0].anchor.as_ref().expect("結び目がある");
    assert_eq!(anchor.range, app.review_candidates[1].range, "残ったのは 2 本目のコメント");

    // x も押した方だけ。
    press_at(&mut app, 1, 'x');
    assert_eq!(states(&app), [Pending, Dismissed]);
    assert!(app.comments.is_empty());
}

#[test]
fn only_accept_fills_the_anchor_and_a_hand_written_comment_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    press_at(&mut app, 1, 'a');
    let anchor = app.comments[0].anchor.clone().expect("a で作ったコメントは結び目を持つ");
    assert_eq!(anchor.rule, RULE);
    assert_eq!(anchor.range, app.review_candidates[1].range);

    // 人の赤入れ（composer の経路）は None。
    app.mode = Mode::Source;
    app.overlay = None;
    app.cursor = 4;
    app.input_start = 4;
    app.input_end = 4;
    app.input = "人の赤入れ".into();
    app.mode = Mode::Input;
    crate::on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let hand = app.comments.iter().find(|c| c.text == "人の赤入れ").expect("足された");
    assert_eq!(hand.anchor, None);
}

#[test]
fn a_comment_without_an_anchor_still_matches_by_rule_and_line() {
    use CandidateState::*;
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    // 結び目の無い review コメント（古い経路）。同じルール・同じ行の候補は
    // 今までどおり 2 本とも ✓ で、`a` で戻せば 2 本とも戻る。
    press_at(&mut app, 0, 'a');
    app.comments[0].anchor = None;
    assert_eq!(states(&app), [Accepted, Accepted]);
    press_at(&mut app, 1, 'a');
    assert_eq!(states(&app), [Pending, Pending]);
    assert!(app.comments.is_empty());
}

#[test]
fn sending_one_of_two_marks_only_that_one_sent() {
    use CandidateState::*;
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    press_at(&mut app, 1, 'a');
    crate::clear_sent_comments(&mut app);
    assert_eq!(states(&app), [Pending, Sent], "送ったのは 2 本目だけ");
    press_at(&mut app, 0, 'a');
    assert_eq!(states(&app), [Accepted, Sent], "1 本目はまだ accept できる");
}

// ---- 見届けた reload -------------------------------------------------

#[test]
fn a_watched_reload_carries_the_anchor_across_the_diff() {
    use CandidateState::*;
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    press_at(&mut app, 1, 'a');

    // 同じ行の頭に字を足す — 範囲の中は変わらず、位置だけが動く。
    let edited = HEDGES.replace("これは", "まえ。これは");
    rewrite_and_reload(&mut app, &edited);

    assert_eq!(app.comments.len(), 1, "中身の変わらないコメントは残る");
    let comment = &app.comments[0];
    let anchor = comment.anchor.as_ref().expect("結び目も写る");
    let at = edited.rfind("かも").unwrap();
    assert_eq!(anchor.range, at..at + "かも".len(), "新しい位置へ");
    assert_eq!(anchor.column, 21, "桁も新しい版で数え直す");
    assert_eq!(anchor.excerpt, "…かもしれないし、効かない【かも】しれない。");
    assert_eq!(comment.start, 3);

    // 新しい版の答え: 結び目で照合するので、2 本目だけが ✓ のまま。
    deliver_hedges(&mut app, &edited);
    assert_eq!(states(&app), [Pending, Accepted]);
}

#[test]
fn editing_one_hedge_drops_only_its_comment_and_keeps_the_other_on_the_same_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    press_at(&mut app, 0, 'a');
    press_at(&mut app, 1, 'a');

    // 1 本目の「かも」だけを直す。
    let edited = HEDGES.replacen("効くかも", "効くはず", 1);
    rewrite_and_reload(&mut app, &edited);

    assert_eq!(app.comments.len(), 1, "直した箇所のコメントだけが外れる");
    let at = edited.rfind("かも").unwrap();
    assert_eq!(
        app.comments[0].anchor.as_ref().map(|a| a.range.clone()),
        Some(at..at + "かも".len())
    );
}

// ---- 送る文面 --------------------------------------------------------

#[test]
fn the_sent_text_says_where_in_the_line_with_a_short_excerpt() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    press_at(&mut app, 0, 'a');
    press_at(&mut app, 1, 'a');
    let mut comments = app.comments.clone();
    comments.sort_by_key(|c| c.anchor.as_ref().map(|a| a.range.start));
    let line = "これは効くかもしれないし、効かないかもしれない。";

    let first = crate::export::format_comment(&comments[0]);
    assert!(
        first.ends_with(&format!(
            ":3\n3: {line}\nlint: {RULE} — 弱い表現\n箇所: 3 行目の 6 文字目から「これは効く【かも】しれないし、効かないかも…」"
        )),
        "{first}"
    );
    let second = crate::export::format_comment(&comments[1]);
    assert!(
        second.ends_with(&format!(
            ":3\n3: {line}\nlint: {RULE} — 弱い表現\n箇所: 3 行目の 18 文字目から「…かもしれないし、効かない【かも】しれない。」"
        )),
        "{second}"
    );

    // `--reply` の形: 行番号は持たず、抜粋だけ。まとめて送ると字下げの中に入る。
    assert_eq!(
        crate::export::format_comment_reply(&comments[1], None),
        format!("> {line}\n\nlint: {RULE} — 弱い表現\n箇所: 「…かもしれないし、効かない【かも】しれない。」")
    );
    assert_eq!(
        crate::export::format_all_reply(&comments),
        format!(
            "1. > {line}\n\n    lint: {RULE} — 弱い表現\n    箇所: 「これは効く【かも】しれないし、効かないかも…」\n\n\
             2. > {line}\n\n    lint: {RULE} — 弱い表現\n    箇所: 「…かもしれないし、効かない【かも】しれない。」"
        )
    );
}

#[test]
fn a_comment_without_an_anchor_is_sent_byte_for_byte_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hedges(dir.path());
    press_at(&mut app, 1, 'a');
    let mut bare = app.comments[0].clone();
    bare.anchor = None;
    let line = "これは効くかもしれないし、効かないかもしれない。";
    let path = std::fs::canonicalize(&bare.file_path).unwrap();
    assert_eq!(
        crate::export::format_comment(&bare),
        format!("{}:3\n3: {line}\nlint: {RULE} — 弱い表現", path.display())
    );
    assert_eq!(
        crate::export::format_comment_reply(&bare, None),
        format!("> {line}\n\nlint: {RULE} — 弱い表現")
    );
}

#[test]
fn the_excerpt_folds_a_range_across_lines_and_elides_a_long_one() {
    let doc = "前の行\nあいうえおかきくけこさしすせそたちつてとなにぬねの\nはひふへほ。\n";
    // 2 行目の「か」から 3 行目の「へ」まで — 行を跨ぎ、24 字より長い。
    let start = doc.find('か').unwrap();
    let end = doc.find('ほ').unwrap();
    let anchor = crate::comment::ReviewAnchor::new("r".into(), start..end, doc);
    assert_eq!(anchor.column, 6, "字で数える（バイトではない）");
    assert_eq!(
        anchor.excerpt, "あいうえお【かきくけこさしすせそ…なにぬねの はひふへ】ほ。",
        "範囲の中の改行は空白 1 つに畳み、長い範囲は頭と尻だけ"
    );
}
