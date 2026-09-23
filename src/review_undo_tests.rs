//! Review の**取り消しと状態の整合** — `a` / `x` の切り替え、コメントが
//! 状態の元であること、送った候補、捨てた記録の追記と最後勝ち。
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節「取り消しと状態」。
//! 状態の遷移表の組を 1 本ずつ押さえる:
//!
//! | 状態 \ キー | `a` | `x` |
//! | --- | --- | --- |
//! | Pending | Accepted | Dismissed |
//! | Accepted | Pending（コメントを消す） | Dismissed（コメントを消してから） |
//! | Dismissed | Accepted（捨てたのを取り消してから） | Pending（取り消しを追記） |
//! | Sent | 何もしない | 何もしない |
//!
//! キーは本番と同じ入口（[`crate::overlay::on_review_overlay_key`]）から押す。
//! 判定器は呼ばない（[`crate::review_tests`] の足場を使う）。

use crate::*;

use crate::review::CandidateState;
use crate::review_tests::{
    DOC, EDITED, answer, answer_for, built_in, deliver, rewrite_and_reload, underlined,
};

/// `DOC` の 3 段落すべてが候補になった App（一覧を開いてある）。
fn three(dir: &std::path::Path) -> App {
    let mut app = built_in(dir);
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 0;
    app
}

/// 一覧でキーを 1 つ押す（本番の入口）。
fn press(app: &mut App, key: char) {
    crate::overlay::on_review_overlay_key(app, KeyCode::Char(key), KeyModifiers::NONE);
}

/// 置き場の最後の行（JSON）。無ければ `None`。
fn last_record(app: &App) -> Option<serde_json::Value> {
    let store = app.review_dismissed_store.as_ref()?;
    let raw = std::fs::read_to_string(store.path()).ok()?;
    raw.lines().last().map(|line| serde_json::from_str(line).unwrap())
}

fn record_count(app: &App) -> usize {
    app.review_dismissed_store
        .as_ref()
        .and_then(|store| std::fs::read_to_string(store.path()).ok())
        .map_or(0, |raw| raw.lines().count())
}

/// 候補 0 を `state` にする（キーで — 本番と同じ道）。
fn bring_to(app: &mut App, state: CandidateState) {
    match state {
        CandidateState::Pending => {}
        CandidateState::Accepted => press(app, 'a'),
        CandidateState::Dismissed => press(app, 'x'),
        CandidateState::Sent => {
            press(app, 'a');
            crate::clear_sent_comments(app);
        }
    }
    assert_eq!(app.candidate_state(0), Some(state), "前提");
}

fn footer_texts(app: &App) -> Vec<String> {
    crate::chrome::footer_hint_items(app)
        .into_iter()
        .map(|hint| hint.text)
        .collect()
}

// ---- 1. 遷移表 — 3 状態 × `a` / `x` -------------------------------------

#[test]
fn every_state_goes_straight_to_the_state_of_the_key_pressed() {
    use CandidateState::*;
    // (前の状態, キー, 後の状態, コメントの数, 置き場の最後の行が取り消しか)
    let table: [(CandidateState, char, CandidateState, usize, Option<bool>); 6] = [
        (Pending, 'a', Accepted, 1, None),
        (Pending, 'x', Dismissed, 0, Some(false)),
        (Accepted, 'a', Pending, 0, None),
        (Accepted, 'x', Dismissed, 0, Some(false)),
        (Dismissed, 'a', Accepted, 1, Some(true)),
        (Dismissed, 'x', Pending, 0, Some(true)),
    ];
    for (before, key, after, comments, undo) in table {
        let dir = tempfile::tempdir().unwrap();
        let mut app = three(dir.path());
        bring_to(&mut app, before);
        press(&mut app, key);
        let case = format!("{before:?} で {key}");
        assert_eq!(app.candidate_state(0), Some(after), "{case}");
        assert_eq!(app.comments.len(), comments, "{case}: コメントの数");
        match undo {
            None => assert_eq!(record_count(&app), 0, "{case}: 記録に触らない"),
            Some(undo) => {
                let last = last_record(&app).expect("記録がある");
                assert_eq!(last.get("undo").is_some(), undo, "{case}: {last}");
                assert_eq!(last["rule"], "filler");
            }
        }
        // 下線は Pending だけ。
        let underlined_first = underlined(&app).iter().any(|r| r.start == app.review_candidates[0].range.start);
        assert_eq!(underlined_first, after == Pending, "{case}: 下線");
        // ほかの候補には触らない。
        assert_eq!(app.candidate_state(1), Some(Pending), "{case}: 隣");
        assert_eq!(app.overlay_cursor, 0, "{case}: カーソルは動かさない");
    }
}

#[test]
fn a_accepts_and_undoes_only_its_own_comment_and_leaves_a_hand_written_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    // 同じ行に人の赤入れがある。
    app.comments.push(crate::comment::Comment {
        file_path: app.current_file_path().to_path_buf(),
        start: 3,
        end: 3,
        lines: "ひとつめの段落。".into(),
        revision: app.current_revision_context(),
        text: "ここは言い過ぎ".into(),
    });
    press(&mut app, 'a');
    assert_eq!(app.comments.len(), 2);
    press(&mut app, 'a');
    let texts: Vec<&str> = app.comments.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["ここは言い過ぎ"], "消すのは review コメントだけ");
    assert_eq!(app.candidate_state(0), Some(CandidateState::Pending));
    assert_eq!(
        app.status.as_ref().map(|s| s.0.as_str()),
        Some("review comment removed"),
        "消えたコメントは見えないところ（`l`）にあるので言う"
    );
}

#[test]
fn accept_all_still_takes_only_the_pending_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    bring_to(&mut app, CandidateState::Sent);
    app.overlay_cursor = 1;
    press(&mut app, 'x');
    press(&mut app, 'A');
    assert_eq!(
        app.candidate_states(),
        [CandidateState::Sent, CandidateState::Dismissed, CandidateState::Accepted]
    );
    assert_eq!(app.comments.len(), 1, "送った候補も捨てた候補もコメントにしない");
    // まとめての取り消しは無い — 個々の `a` で戻す。
    app.overlay_cursor = 2;
    press(&mut app, 'a');
    assert_eq!(app.candidate_state(2), Some(CandidateState::Pending));
    assert!(app.comments.is_empty());
}

// ---- 2. Accepted はコメントから決まる -----------------------------------

#[test]
fn deleting_the_comment_in_the_body_returns_the_candidate_to_pending() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'a');
    app.overlay = None;
    app.mode = Mode::Source;
    app.cursor = 2; // 3 行目（0 始まり）
    crate::on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
    assert!(app.comments.is_empty());
    assert_eq!(app.candidate_state(0), Some(CandidateState::Pending));
    assert_eq!(underlined(&app).len(), 3, "下線も戻る");
    assert!(app.review_lines()[2].is_some(), "ガターの印も戻る");
}

#[test]
fn deleting_the_comment_in_the_comments_list_returns_the_candidate_to_pending() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'a');
    app.overlay = Some(crate::overlay::Overlay::Comments);
    app.overlay_cursor = 0;
    crate::overlay::on_comments_overlay_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
    assert!(app.comments.is_empty());
    assert_eq!(app.candidate_state(0), Some(CandidateState::Pending));
    assert_eq!(app.review_counts(), (0, 3));
}

#[test]
fn a_comment_edited_away_by_a_watched_reload_returns_its_candidate_to_pending() {
    // 見届けた reload が範囲の変わったコメントを外すと、その候補も戻る。
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    app.overlay_cursor = 1; // 「ふたつめ」— EDITED で直される
    press(&mut app, 'a');
    rewrite_and_reload(&mut app, EDITED);
    assert!(app.comments.is_empty(), "直した箇所のコメントは外れる");
    deliver(
        &mut app,
        "filler",
        answer_for(EDITED, &["ふたつめを直した。"], &[Some(0.9)]),
    );
    assert_eq!(app.candidate_states(), [CandidateState::Pending]);
}

// ---- 3. 送った候補は `✓` のまま ------------------------------------------

#[test]
fn a_sent_candidate_keeps_its_tick_and_refuses_a_and_x() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    bring_to(&mut app, CandidateState::Sent);
    assert!(app.comments.is_empty(), "送るとコメントは消える");
    assert_eq!(app.candidate_state(0).unwrap().mark(), "✓", "新しい印は作らない");
    assert!(!underlined(&app).iter().any(|r| r.start == app.review_candidates[0].range.start));
    assert_eq!(app.review_counts(), (1, 3));

    for key in ['a', 'x'] {
        app.status = None;
        press(&mut app, key);
        assert_eq!(app.candidate_state(0), Some(CandidateState::Sent), "{key}");
        assert!(app.comments.is_empty(), "{key}: 二重に送らせない");
        assert_eq!(
            app.status.as_ref().map(|s| (s.0.as_str(), s.2)),
            Some((crate::overlay::REVIEW_ALREADY_SENT, true)),
            "{key}: 何もしない理由を言う"
        );
    }
    assert_eq!(record_count(&app), 0, "送った候補の x は記録しない");
}

#[test]
fn a_sent_candidate_survives_esc_and_r_but_is_decided_again_after_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    bring_to(&mut app, CandidateState::Sent);

    // Esc で画面から下げて `R` — 同じ版なので `✓` のまま戻る。
    assert!(app.clear_review());
    app.arm_review();
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    assert_eq!(app.candidate_state(0), Some(CandidateState::Sent), "Esc は状態に触らない");

    // 送った相手が書き換えた（見届けた reload）。1 段落目は 1 バイトも
    // 変わっていないが、送った印は写さない — 決め直す。
    rewrite_and_reload(&mut app, EDITED);
    deliver(
        &mut app,
        "filler",
        answer_for(EDITED, &["ひとつめの段落。"], &[Some(0.9)]),
    );
    assert_eq!(app.candidate_states(), [CandidateState::Pending], "直っていなければ Pending で出る");
    press(&mut app, 'a');
    assert_eq!(app.comments.len(), 1, "新しい版ではもう一度 accept できる");
}

#[test]
fn sending_remembers_only_review_comments_and_only_for_their_own_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'a');
    app.comments.push(crate::comment::Comment {
        file_path: app.current_file_path().to_path_buf(),
        start: 5,
        end: 5,
        lines: "ふたつめの段落。".into(),
        revision: None,
        text: "人の赤入れ".into(),
    });
    app.comments.push(crate::comment::Comment {
        file_path: dir.path().join("other.md"),
        start: 7,
        end: 7,
        lines: String::new(),
        revision: None,
        text: crate::review::comment_text("filler", 0.7),
    });
    assert_eq!(crate::clear_sent_comments(&mut app), 3);
    assert_eq!(app.review_sent.len(), 2, "人の赤入れは覚えない");
    assert_eq!(
        app.candidate_states(),
        [CandidateState::Sent, CandidateState::Pending, CandidateState::Pending],
        "ほかのファイルの 7 行目は、この文書の 7 行目ではない"
    );
}

// ---- 4. 捨てた候補も一覧に残る ------------------------------------------

#[test]
fn a_dismissed_candidate_is_listed_dimmed_on_reopen_and_x_restores_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    app.overlay_cursor = 1;
    press(&mut app, 'x');

    let mut reopened = three(dir.path());
    assert_eq!(
        reopened.candidate_states(),
        [CandidateState::Pending, CandidateState::Dismissed, CandidateState::Pending],
        "文書順の位置のまま"
    );
    assert_eq!(underlined(&reopened).len(), 2, "本文には下線を出さない");
    assert!(reopened.review_lines()[4].is_none(), "ガターの印も出さない");
    assert_eq!(reopened.review_counts(), (1, 3));

    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::draw(f, &mut reopened)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let row = (0..buf.area().height)
        .find(|&y| {
            let line: String = (0..buf.area().width).map(|x| buf[(x, y)].symbol()).collect();
            line.contains("– L5")
        })
        .expect("捨てた候補の行が一覧にある");
    let head = (0..buf.area().width)
        .find(|&x| buf[(x, row)].symbol() == "L")
        .unwrap();
    assert_eq!(buf[(head, row)].fg, ratatui::style::Color::DarkGray, "薄く出る");

    // 戻す。次に開いたときも戻っている（取り消しは追記の最後の行）。
    reopened.overlay_cursor = 1;
    press(&mut reopened, 'x');
    assert_eq!(reopened.candidate_state(1), Some(CandidateState::Pending));
    let again = three(dir.path());
    assert_eq!(again.candidate_state(1), Some(CandidateState::Pending));
    assert_eq!(record_count(&again), 2, "消さずに足す");
}

#[test]
fn the_footer_names_what_a_and_x_will_do_on_the_cursor_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    let cases: [(CandidateState, &[&str], &[&str]); 4] = [
        (CandidateState::Pending, &["a accept", "x dismiss"], &["a undo", "x restore"]),
        (CandidateState::Accepted, &["a undo", "x dismiss"], &["a accept", "x restore"]),
        (CandidateState::Dismissed, &["a accept", "x restore"], &["a undo", "x dismiss"]),
        (CandidateState::Sent, &["a/x sent"], &["a accept", "a undo", "x dismiss", "x restore"]),
    ];
    for (index, (state, shown, hidden)) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = three(dir.path());
        bring_to(&mut app, state);
        let texts = footer_texts(&app);
        for want in shown {
            assert!(texts.iter().any(|t| t == want), "{index} {state:?}: {want} が無い {texts:?}");
        }
        for not in hidden {
            assert!(!texts.iter().any(|t| t == not), "{index} {state:?}: {not} がある {texts:?}");
        }
    }
    // j で行を替えれば、その行の状態の案内になる。
    press(&mut app, 'x');
    assert!(footer_texts(&app).iter().any(|t| t == "x restore"));
    press(&mut app, 'j');
    assert!(footer_texts(&app).iter().any(|t| t == "x dismiss"));
}

// ---- 5. 記録 — 追記・最後勝ち・古い形・引き継ぎ ----------------------------

#[test]
fn a_restore_is_not_carried_across_a_watched_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'x'); // 1 段落目（EDITED でも変わらない）
    app.overlay_cursor = 2;
    press(&mut app, 'x'); // 3 段落目（位置だけ動く）
    press(&mut app, 'x'); // …を戻す
    rewrite_and_reload(&mut app, EDITED);
    let store = app.review_dismissed_store.clone().unwrap();
    let carried = store.load(&crate::semantic::source_digest(EDITED));
    let at = EDITED.find("ひとつめの段落。").unwrap();
    assert_eq!(
        carried.into_iter().collect::<Vec<_>>(),
        [("filler".to_string(), at, at + "ひとつめの段落。".len())],
        "戻した記録は写さない"
    );
    deliver(
        &mut app,
        "filler",
        answer_for(EDITED, &["ひとつめの段落。", "みっつめの段落。"], &[Some(0.9), Some(0.9)]),
    );
    assert_eq!(
        app.candidate_states(),
        [CandidateState::Dismissed, CandidateState::Pending]
    );
}

#[test]
fn a_restore_made_after_the_edit_follows_the_text_back_to_the_old_version() {
    // DOC で捨てる → EDITED へ写る → EDITED で戻す → DOC に戻す。DOC には
    // 昔の「捨てた」が残っているが、戻した判断の方が向こうへ効く。
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'x');
    rewrite_and_reload(&mut app, EDITED);
    deliver(&mut app, "filler", answer_for(EDITED, &["ひとつめの段落。"], &[Some(0.9)]));
    crate::overlay::open_review(&mut app);
    app.overlay_cursor = 0;
    assert_eq!(app.candidate_state(0), Some(CandidateState::Dismissed), "写っている");
    press(&mut app, 'x');
    rewrite_and_reload(&mut app, DOC);
    deliver(&mut app, "filler", answer([Some(0.9), None, None]));
    assert_eq!(app.candidate_states(), [CandidateState::Pending]);
}

#[test]
fn the_store_reads_old_lines_and_lets_the_last_line_win() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::review::DismissedStore::at(dir.path().join("review"));
    let sha = "f".repeat(64);
    std::fs::create_dir_all(dir.path().join("review")).unwrap();
    // 古い形（`undo` の無い行）と、今の形の取り消し・捨て直しが混ざる。
    let lines = [
        format!(r#"{{"source_sha":"{sha}","range":[0,4],"rule":"filler","at":1}}"#),
        format!(r#"{{"source_sha":"{sha}","range":[5,9],"rule":"filler","at":2}}"#),
        format!(r#"{{"source_sha":"{sha}","range":[0,4],"rule":"filler","at":3,"undo":true}}"#),
        format!(r#"{{"source_sha":"{sha}","range":[5,9],"rule":"filler","at":4,"undo":true}}"#),
        format!(r#"{{"source_sha":"{sha}","range":[5,9],"rule":"filler","at":5}}"#),
        format!(r#"{{"source_sha":"{sha}","range":[10,14],"rule":"hedge","at":6}}"#),
    ];
    std::fs::write(store.path(), lines.join("\n") + "\n").unwrap();
    let mut got: Vec<_> = store.load(&sha).into_iter().collect();
    got.sort();
    assert_eq!(
        got,
        [("filler".to_string(), 5, 9), ("hedge".to_string(), 10, 14)],
        "捨てて戻した [0,4] は入らず、戻して捨て直した [5,9] は入る"
    );
}

#[test]
fn the_undo_line_carries_no_document_text_and_no_undo_key_on_a_dismissal() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'x');
    press(&mut app, 'x');
    let raw = std::fs::read_to_string(app.review_dismissed_store.as_ref().unwrap().path()).unwrap();
    let rows: Vec<serde_json::Value> = raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].get("undo").is_none(), "捨てる行は古い形のまま: {}", rows[0]);
    assert_eq!(rows[1]["undo"], true);
    assert_eq!(rows[0]["range"], rows[1]["range"]);
    assert!(!raw.contains("ひとつめ"), "本文は書かない:\n{raw}");
}

#[test]
fn clearing_the_store_counts_what_was_dismissed_and_removes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'x');
    app.overlay_cursor = 1;
    press(&mut app, 'x');
    app.overlay_cursor = 2;
    press(&mut app, 'x');
    press(&mut app, 'x'); // 戻した 1 本は数えない
    let store = app.review_dismissed_store.clone().unwrap();
    assert_eq!(store.clear().unwrap(), 2);
    assert!(!store.path().exists());
    assert_eq!(store.clear().unwrap(), 0, "無ければ 0 件");
    let reopened = three(dir.path());
    assert!(reopened.candidate_states().iter().all(|s| *s == CandidateState::Pending));
}

// ---- 6. `Esc` は状態に触らない ------------------------------------------

#[test]
fn esc_takes_the_candidates_off_the_screen_without_touching_any_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = three(dir.path());
    press(&mut app, 'a');
    app.overlay_cursor = 1;
    press(&mut app, 'x');
    let records = record_count(&app);
    assert!(app.clear_review());
    assert_eq!(app.comments.len(), 1, "コメントは残る");
    assert_eq!(record_count(&app), records, "記録は書かない");
    app.arm_review();
    deliver(&mut app, "filler", answer([Some(0.9), Some(0.8), Some(0.7)]));
    assert_eq!(
        app.candidate_states(),
        [CandidateState::Accepted, CandidateState::Dismissed, CandidateState::Pending]
    );
}
