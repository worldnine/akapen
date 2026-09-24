//! マーカー — **問いに答えている箇所だけを光らせる**
//! （`docs/design/semantic-reading-layer.md`）。**この層の唯一の機構**で
//! ある。
//!
//! 判決を下さない — 光るか、何も言わないか、だけである。
//!
//! ```text
//! 問い = 定型（5 本）または 自由入力
//!   ↓
//! 散文の Atom ごとに Noul 1 ラウンド → [`crate::SemanticUnit::score`]
//!   （Unit = 散文の Atom 1 つ。境界も核も聞かない — 2026-09-24）
//!   ↓
//! スコアの高い箇所から光らせる。つまみで量を変える。DIM は出さない
//! ```
//!
//! # ここが返さないもの
//!
//! **[`DisplayState::Dim`] を 1 つも返さない。** DIM は verdict（外れると
//! 読み手が信頼を払う）で、MARKED は offer（外れても損をしない）である
//! （`docs/design/marks-only-and-review-mode.md` 1 節「判決」）。この層は
//! offer しか出さない。沈めるのは読み手が自分で押したとき（akapen の `f`）
//! だけで、それはクライアント側にある。
//!
//! # つまみ — 上から N %、ただし [`SCORE_FLOOR`] 未満は出さない
//!
//! 段 1 の実測（`examples/semantic/measurements/marks-presets.md` 2 節）が
//! 2 つのことを言っている:
//!
//! - **「スコアの境目」は文書をまたがない。** 同じ「要点」でも 0.5 を超える
//!   Unit は設計書で 171 中 15、gotchas で 126 中 107
//! - **「上から N %」はまたぐ。** 上位 20 % のスコアは 0.30〜0.82 で、
//!   どの文書でも「上位 20 %」という指し方が成立する
//!
//! だからつまみは N % である。ただしそれだけだと、**狭い問い**（費用の話を
//! しない文書に「費用の話」）で上から N 本を取ると雑音が光る。そこで
//! [`SCORE_FLOOR`] を足切りに置く。**0 本は正しい答え**でありうる。
//!
//! # 単調性
//!
//! `share` を上げたとき、光っていた Unit が消えることはない。選ぶ順は
//! スコアの降順（同点は文書順）で固定なので、集合は**入れ子で広がる**。
//! 足切りは `share` の関数ではないので、この性質を壊さない。

use std::ops::Range;

use crate::display::DisplayState;
use crate::document::SemanticDocument;
use crate::unit::SemanticUnit;

/// つまみの下限（上から 1 %）。
pub const MIN_SHARE: u8 = 1;
/// つまみの上限（上から 100 % = 足切りを超えた Unit すべて）。
pub const MAX_SHARE: u8 = 100;
/// つまみの既定（上から 15 %）。
///
/// **分母は Unit 数 ＝ 散文の Atom の数**である（2026-09-24 から判定器の
/// Unit は散文の Atom 1 つで、見出し・コード・表のヘッダ行は Unit を
/// 持たない）。それまでの 20 % は Jev が束ねた Unit（段落ほど）を分母に
/// した値で、文ごとにすると同じ 20 % で光る本数が 1.2〜1.9 倍になる。
///
/// 15 は、前の形（核の問いを直した 3 ラウンド）の 20 % と**同じ本数**に
/// なる share を、6 文書 × 5 問 × 2 ランの応答から Jev を呼ばずに数えて
/// 選んだ（`examples/semantic/measurements/unit-per-atom.md`）。足切りで
/// 頭打ちにならない 44 セルで中央値 15.3、文書ごとに 11〜23（前の形は
/// 見出しやコードも Unit に数えていたので、それが多い文書ほど高い）。
/// 15 % で光る本数の合計は前の 20 % の 98 %。ラン間の顔ぶれの一致は
/// 10〜20 % のどれでもほぼ同じ（Jaccard の平均 0.86〜0.88）で、決め手に
/// ならなかった。
///
/// 議事録（169 Unit）で 25 本、設計書（153 Unit）で 23 本が光る量である。
/// `<` `>` の 10 刻みは 5・25・35 … と 5 の位に乗り、下は 1、上は 100 で
/// 止まる。
pub const DEFAULT_SHARE: u8 = 15;

/// **これ未満のスコアは、上から N % に入っていても光らせない。**
///
/// 段 1 の生値（5 文書 × 8 問 × 2 ラン = 3,712 Unit-問）を引き直して決めた。
/// 根拠は 2 つの帯の間に空きがあること:
///
/// | | 問い | 両ランの最大 |
/// | --- | --- | ---: |
/// | **0 本が正しい側** | `demo` に「判断が要る」 | 0.11 |
/// | | `demo` に「費用の話」 | **0.15** |
/// | | 設計書に「費用の話」 | 0.13 |
/// | | 設計書に「日程」 | 0.10 |
///
/// | | 問い | 上位 20 % の最小 |
/// | --- | --- | ---: |
/// | **光ってほしい側** | 設計書の「要点」 | **0.30** |
/// | | 他 4 文書の広い問い | 0.53〜0.89 |
///
/// 空きは **(0.15, 0.30)**。この中で光らせる側へ寄せた 0.20 を採る —
/// マーカーは offer なので、迷ったら出す側に倒す。
///
/// **0.15 にしない理由**は上の表の 2 行目 1 つである。`demo` の「費用の話」が
/// ラン 1 で 0.15 ちょうどに触るので、0.15 の足切りだと **ラン 1 で 1 本・
/// ラン 2 で 0 本**になる。3,712 Unit-問の中で唯一、つまみが揺れる実例で、
/// 0.20 は両ランで消す（余裕 0.05 は、その 4 問のラン間の差の最大 0.030 の
/// 1.7 倍）。**0.25 まで上げない理由**は、設計書の「要点」の上位
/// （下端 0.30）が削れ始めるからである。
///
/// **問いごとの閾値は置かない。** 上の 2 つの帯は 5 文書 8 問すべてに共通で、
/// 定数 1 つで足りることが測れている。
pub const SCORE_FLOOR: f32 = 0.20;

/// 光る Unit を、スコアの降順（同点は文書順）で返す。
///
/// 足切りを超えていて、かつ核を持つ Unit だけが候補になる — 核を持たない
/// Unit（`core_atoms` が `Some([])`）は光らせる先が無いので、上位に居ても
/// 数に入れない。いまの判定器（`examples/semantic/jev-annotate.py`）は
/// 散文の Atom 1 つを Unit にして核に `[その Atom]` を明示するので、
/// `Some([])` を返すことは無い。プロトコルの 3 値として残っていて、
/// Unit を束ねる判定器や fixture が使う。
fn ranked(doc: &SemanticDocument) -> Vec<usize> {
    let mut candidates: Vec<usize> = doc
        .units
        .iter()
        .enumerate()
        .filter(|(_, unit)| lights_up(unit))
        .map(|(index, _)| index)
        .collect();
    // 降順。同点は文書順（`sort_by` は安定なので、添字の昇順がそのまま残る）。
    candidates.sort_by(|&a, &b| {
        score_of(&doc.units[b])
            .partial_cmp(&score_of(&doc.units[a]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// この Unit は光りうるか — スコアが足切りを超え、核を持つか。
fn lights_up(unit: &SemanticUnit) -> bool {
    unit.score.is_some_and(|score| score >= SCORE_FLOOR) && unit.has_core()
}

/// スコア。持たない Unit は 0.0（[`lights_up`] が先に落としている）。
fn score_of(unit: &SemanticUnit) -> f32 {
    unit.score.unwrap_or(0.0)
}

/// 上から何本取るか。
///
/// **`share` % は文書の Unit 数に対してである**（足切りを超えた数では
/// ない）。段 1 の測定がその取り方で、「上位 20 %」という指し方が文書を
/// またいで成立することもその単位で測ってある。足切りを超えた数のほうを
/// 分母にすると、狭い問いで「10 本しか該当が無いのに N % で 2 本」に
/// なり、**つまみが該当の一部を隠す**。
///
/// 四捨五入し、**最低 1 本**にする。つまみが「何も無い」と言うことは
/// ない — **0 本になるのは足切りを超える Unit が無いときだけ**で、それは
/// 「この問いに答えている箇所が無い」という意味を持つ。つまみの刻みで
/// 0 本になると、その 2 つが混ざる。
///
/// 戻り値は `candidates` で頭打ちになる。**足切りが上限を作る**のは
/// ここで、つまみを 100 % まで上げても該当の無いものは出てこない。
fn take_count(units: usize, candidates: usize, share: u8) -> usize {
    if candidates == 0 {
        return 0;
    }
    let share = share.clamp(MIN_SHARE, MAX_SHARE) as usize;
    // 四捨五入。整数のまま (n*share + 50) / 100 で計算する。
    ((units * share + 50) / 100).max(1).min(candidates)
}

/// **光る Unit の数**（ステータス行の「n 本」）。
///
/// [`mark`] と同じ選び方をするので、画面に出ている本数と一致する。
pub fn lit(doc: &SemanticDocument, share: u8) -> usize {
    let candidates = ranked(doc);
    take_count(doc.units.len(), candidates.len(), share)
}

/// この annotation はスコアを持っているか。
///
/// `false` は「判定器（または fixture）がスコアを返していない」を意味する。呼び出し側はそれを**黙った 0 本にせず、
/// 理由として言う**（akapen 側の `crate::chrome`）。
pub fn has_scores(doc: &SemanticDocument) -> bool {
    doc.units.iter().any(|unit| unit.score.is_some())
}

/// 現在のつまみでの Atom ごとの表示状態を返す。
///
/// 戻り値は `doc.atoms` と同じ並び順・同じ個数で、各要素は
/// `(source のバイト範囲, 表示状態)`。
///
/// **返るのは [`DisplayState::Marked`] と [`DisplayState::Normal`] だけ
/// である。** DIM は出さない。
///
/// つまみを動かしても Jev は呼ばれない。この関数は純粋関数である。
pub fn mark(doc: &SemanticDocument, share: u8) -> Vec<(Range<usize>, DisplayState)> {
    let candidates = ranked(doc);
    let take = take_count(doc.units.len(), candidates.len(), share);
    let mut selected = vec![false; doc.units.len()];
    for &index in candidates.iter().take(take) {
        selected[index] = true;
    }

    let mut states = vec![DisplayState::Normal; doc.atoms.len()];
    for (index, unit) in doc.units.iter().enumerate() {
        if !selected[index] {
            continue;
        }
        for &atom in &unit.atoms {
            // 核だけを光らせる。`core_atoms` が `None` の Unit は
            // 全体が核なので全体が光る（[`crate::SemanticUnit::is_core`]）。
            if unit.is_core(atom)
                && let Some(slot) = states.get_mut(atom.0)
            {
                *slot = DisplayState::Marked;
            }
        }
    }

    doc.atoms
        .iter()
        .zip(states)
        .map(|(atom, state)| (atom.range.clone(), state))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{Atom, AtomIndex, AtomKind};
    use crate::unit::SemanticUnit;

    /// スコア付きの Unit を 1 Atom ずつ並べた文書。
    fn doc(scores: &[Option<f32>]) -> SemanticDocument {
        let atoms: Vec<Atom> = (0..scores.len())
            .map(|i| Atom {
                range: i * 10..i * 10 + 9,
                kind: AtomKind::Sentence,
            })
            .collect();
        let units: Vec<SemanticUnit> = scores
            .iter()
            .enumerate()
            .map(|(i, score)| {
                let mut unit = SemanticUnit::new(format!("u{}", i + 1), [AtomIndex(i)]);
                unit.score = *score;
                unit
            })
            .collect();
        SemanticDocument::new(atoms, units)
    }

    fn marked(doc: &SemanticDocument, share: u8) -> Vec<usize> {
        mark(doc, share)
            .into_iter()
            .enumerate()
            .filter(|(_, (_, state))| *state == DisplayState::Marked)
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn nothing_is_ever_dimmed() {
        let document = doc(&[Some(0.9), Some(0.1), Some(0.5), None]);
        for share in [MIN_SHARE, 20, 50, MAX_SHARE] {
            assert!(
                mark(&document, share)
                    .iter()
                    .all(|(_, state)| *state != DisplayState::Dim),
                "marks モードは DIM を出さない (share {share})"
            );
        }
    }

    #[test]
    fn the_knob_grows_a_nested_set() {
        let document = doc(&[Some(0.30), Some(0.95), Some(0.60), Some(0.80), Some(0.40)]);
        let mut previous: Vec<usize> = Vec::new();
        for share in MIN_SHARE..=MAX_SHARE {
            let now = marked(&document, share);
            assert!(
                previous.iter().all(|atom| now.contains(atom)),
                "share {share} で光っていたものが消えた: {previous:?} -> {now:?}"
            );
            assert!(now.len() >= previous.len());
            previous = now;
        }
        // 100 % では足切りを超えた 5 本すべて。
        assert_eq!(previous.len(), 5);
    }

    #[test]
    fn the_highest_scores_come_first() {
        let document = doc(&[Some(0.30), Some(0.95), Some(0.60), Some(0.80), Some(0.40)]);
        // 5 Unit の 20 % = 1 本。いちばん高い u2（Atom 1）。
        assert_eq!(marked(&document, 20), vec![1]);
        // 50 % = 2.5 -> 3 本。u2 u4 u3。
        assert_eq!(marked(&document, 50), vec![1, 2, 3]);
    }

    #[test]
    fn the_share_is_taken_over_every_unit_not_over_the_candidates() {
        // 10 Unit のうち足切りを超えるのは 2 本。20 % は 2 本 = 両方で、
        // 「該当の 20 %」（= 0 本）ではない。
        let document = doc(&[
            Some(0.95),
            Some(0.90),
            Some(0.05),
            Some(0.04),
            Some(0.03),
            Some(0.02),
            Some(0.01),
            Some(0.01),
            Some(0.01),
            Some(0.01),
        ]);
        assert_eq!(lit(&document, 20), 2);
        assert_eq!(lit(&document, 100), 2, "足切りが上限を作る");
    }

    #[test]
    fn a_question_nothing_answers_lights_nothing() {
        // 段 1 の `demo` に「判断が要る」を聞いたときの形（最大 0.11）。
        let document = doc(&[Some(0.11), Some(0.07), Some(0.05), Some(0.02)]);
        for share in [MIN_SHARE, 20, 50, MAX_SHARE] {
            assert!(
                marked(&document, share).is_empty(),
                "足切り未満しかない問いは 0 本 (share {share})"
            );
            assert_eq!(lit(&document, share), 0);
        }
    }

    #[test]
    fn the_floor_caps_a_narrow_question() {
        // 足切りを超えるのが 2 本だけなら、つまみを上げても 2 本で止まる。
        let document = doc(&[Some(0.95), Some(0.60), Some(0.05), Some(0.02), Some(0.01)]);
        assert_eq!(lit(&document, MAX_SHARE), 2);
        assert_eq!(marked(&document, MAX_SHARE), vec![0, 1]);
    }

    #[test]
    fn a_unit_without_a_score_never_lights() {
        let document = doc(&[None, None, Some(0.95)]);
        assert_eq!(marked(&document, MAX_SHARE), vec![2]);
    }

    #[test]
    fn a_unit_without_a_core_is_not_counted() {
        let mut document = doc(&[Some(0.95), Some(0.90)]);
        // 核を持たない Unit（`Some([])`）。
        document.units[0].set_core([]);
        assert_eq!(lit(&document, MAX_SHARE), 1);
        assert_eq!(marked(&document, MAX_SHARE), vec![1]);
    }

    #[test]
    fn only_the_core_of_a_unit_lights() {
        let atoms: Vec<Atom> = (0..3)
            .map(|i| Atom {
                range: i * 10..i * 10 + 9,
                kind: AtomKind::Sentence,
            })
            .collect();
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0), AtomIndex(1), AtomIndex(2)]);
        unit.score = Some(0.9);
        unit.set_core([AtomIndex(1)]);
        let document = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(marked(&document, MAX_SHARE), vec![1]);
    }

    #[test]
    fn ties_break_in_document_order() {
        let document = doc(&[Some(0.5), Some(0.5), Some(0.5), Some(0.5)]);
        assert_eq!(marked(&document, 25), vec![0]);
        assert_eq!(marked(&document, 50), vec![0, 1]);
    }

    #[test]
    fn has_scores_tells_an_empty_answer_from_a_scoreless_one() {
        assert!(!has_scores(&doc(&[None, None])));
        assert!(has_scores(&doc(&[None, Some(0.01)])));
    }

    #[test]
    fn a_document_without_units_marks_nothing() {
        let document = SemanticDocument::new(Vec::new(), Vec::new());
        assert!(mark(&document, DEFAULT_SHARE).is_empty());
        assert_eq!(lit(&document, DEFAULT_SHARE), 0);
    }
}
