//! Reading Policy — Tier と relation から、現在の Budget での表示状態を決める。
//!
//! ここは完全にローカルな決定論的計算である。`37% -> 36% -> 35%` という
//! 操作で Jev を呼ばないのがこの層の存在意義で、入力が同じなら出力も常に
//! 同じになる（乱数も時刻もスコアも使わない）。
//!
//! # 決め方
//!
//! 1. **Unit を「残したい順」に一列に並べる**（`keep_order` の並び替え鍵）。
//!    この順序は Budget に依存しない。
//! 2. **その先頭から順に、attention が尽きるまで残す**。attention の量は
//!    Unit が占める source のバイト数で測り、`budget` はその何 % までを
//!    残すかを表す。入らない Unit が現れた時点で打ち切る（後ろの小さい
//!    Unit を拾い直さない）。
//! 3. **残った Unit の意味情報を Atom へ投影する**。判断単位は Unit、
//!    表示単位は Atom。
//!
//! 打ち切りを「入らないものを飛ばして詰める」best-fit にしないのは、
//! Budget を 1 下げた瞬間に別の Unit が復活しうるからである。先頭からの
//! 素直な prefix にしておけば、Budget が下がったとき残る集合は必ず縮み、
//! **一度 DIM になった range が再び DIM でなくなることはない**（単調性）。
//!
//! # 並び替え鍵（同一 Tier 内の決定論的 rule）
//!
//! 鍵は次の 5 つ組で、小さいほど残りやすい。
//!
//! ```text
//! (実効 Tier, redundant か, バイト長, 先頭バイト位置, Unit の並び順)
//! ```
//!
//! - **実効 Tier**: `ESSENTIAL < SUPPORTING < CONTEXT < DETAIL`。
//!   ただし redundant な Unit は 1 段だけ弱い Tier として扱う
//!   （[`crate::ReadingTier::weakened`]）。これが「REDUNDANT は元 Tier に
//!   かかわらず優先的に DIM 候補」の実装。Tier を無視して重複を一律
//!   最下位へ落とす案も取れるが、それでは redundancy が Tier を上書きして
//!   しまい「別軸」でなくなるので採らなかった。重複した ESSENTIAL は
//!   重複した SUPPORTING より長く残る。
//! - **redundant か**: 同じ実効 Tier なら、重複していない方を先に残す。
//!   これで「REDUNDANT は同 Tier の非 REDUNDANT より先に DIM になる」。
//! - **バイト長**: 短い方を先に残す。同じ attention でより多くの意味単位が
//!   残り、文書全体の骨格が見えやすくなる。
//! - **先頭バイト位置 / Unit の並び順**: 文書順。最後の同点崩しであり、
//!   これで順序は必ず全順序になる（安定でない並び替えでも結果が揺れない）。
//!
//! 設計書が挙げる rule のうち context preservation は MVP に含めていない。
//!
//! # 表示状態の割り当て
//!
//! ```text
//! 残った Unit で ESSENTIAL かつ非 REDUNDANT  -> MARKED
//! 残ったそれ以外                              -> NORMAL
//! 残らなかったもの                            -> DIM
//! どの Unit にも属さない Atom                 -> NORMAL
//! ```
//!
//! MARKED は Budget に依存しない。Budget 100% で全文を見せつつ ESSENTIAL に
//! 薄い marker を重ねる、という設計書の最初のデモがそのままこの規則である。

use std::ops::Range;

use crate::atom::AtomIndex;
use crate::display::DisplayState;
use crate::document::SemanticDocument;
use crate::unit::ReadingTier;

/// Reading Budget の下限。0 を渡しても 1 として扱う。
pub const MIN_BUDGET: u8 = 1;
/// Reading Budget の上限。
pub const MAX_BUDGET: u8 = 100;

/// 現在の Budget での Atom ごとの表示状態を返す。
///
/// `budget` は「この文書にどれだけ attention を使えるか」を 1〜100 % で
/// 表す。範囲外の値は [`MIN_BUDGET`]〜[`MAX_BUDGET`] に丸める。
///
/// 戻り値は `doc.atoms` と同じ並び順・同じ個数で、各要素は
/// `(source のバイト範囲, 表示状態)`。クライアントはこれをそのまま range
/// decoration に渡せばよく、色や modifier の決定はクライアント側で行う。
///
/// Budget を変えても Jev は呼ばれない。この関数は純粋関数である。
///
/// # 単調性
///
/// `budget` を下げたとき、DIM になった範囲が再び DIM でなくなることはなく、
/// 上げたとき MARKED / NORMAL だった範囲が DIM になることもない。
pub fn decorate(doc: &SemanticDocument, budget: u8) -> Vec<(Range<usize>, DisplayState)> {
    let budget = budget.clamp(MIN_BUDGET, MAX_BUDGET);

    // Unit ごとの attention コスト（構成 Atom のバイト長の合計）。
    // 範囲外の添字は validate で弾かれるが、ここでも黙って無視して panic しない。
    let cost = |unit: &crate::unit::SemanticUnit| -> usize {
        unit.atoms
            .iter()
            .filter_map(|&index| doc.atom(index))
            .map(|atom| atom.len())
            .sum()
    };

    let order = keep_order(doc);
    let total: usize = doc.units.iter().map(cost).sum();

    // 残したい順に attention を積み、Budget を超えた時点で打ち切る。
    // 先頭の 1 つだけは Budget が 1 % でも必ず残す — 読む場所がゼロの
    // 表示には意味がないため。
    let mut kept = vec![false; doc.units.len()];
    let mut spent: usize = 0;
    for (rank, &unit_index) in order.iter().enumerate() {
        spent += cost(&doc.units[unit_index]);
        let fits = (spent as u128) * 100 <= (budget as u128) * (total as u128);
        if fits || rank == 0 {
            kept[unit_index] = true;
        } else {
            break;
        }
    }

    // Unit の判断を Atom へ投影する。1 つの Atom を複数の Unit が指している
    // 場合は attention の強い方を採る（[`DisplayState::stronger`]）。
    let mut states = vec![None; doc.atoms.len()];
    for (unit_index, unit) in doc.units.iter().enumerate() {
        let state = if !kept[unit_index] {
            DisplayState::Dim
        } else if unit.reading_tier == ReadingTier::Essential && !unit.is_redundant() {
            DisplayState::Marked
        } else {
            DisplayState::Normal
        };
        for &AtomIndex(atom_index) in &unit.atoms {
            if let Some(slot) = states.get_mut(atom_index) {
                *slot = Some(slot.map_or(state, |current: DisplayState| current.stronger(state)));
            }
        }
    }

    doc.atoms
        .iter()
        .zip(states)
        // どの Unit にも属さない Atom は未判断であって低優先度ではない。
        .map(|(atom, state)| (atom.range.clone(), state.unwrap_or(DisplayState::Normal)))
        .collect()
}

/// Unit を「残したい順」に並べた添字列を返す。Budget には依存しない。
///
/// 並び替え鍵はモジュールドキュメントのとおり。`decorate` の単調性は
/// 「この順序が Budget に依存しないこと」と「prefix で打ち切ること」の
/// 2 点だけに支えられている。
fn keep_order(doc: &SemanticDocument) -> Vec<usize> {
    let mut order: Vec<usize> = (0..doc.units.len()).collect();
    order.sort_by_cached_key(|&index| {
        let unit = &doc.units[index];
        let redundant = unit.is_redundant();
        let effective_tier = if redundant {
            unit.reading_tier.weakened()
        } else {
            unit.reading_tier
        };
        let length: usize = unit
            .atoms
            .iter()
            .filter_map(|&atom| doc.atom(atom))
            .map(|atom| atom.len())
            .sum();
        let start = unit
            .atoms
            .iter()
            .filter_map(|&atom| doc.atom(atom))
            .map(|atom| atom.range.start)
            .min()
            .unwrap_or(usize::MAX);
        (effective_tier, redundant, length, start, index)
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{Atom, AtomKind};
    use crate::unit::{Relation, SemanticUnit};

    /// 長さの等しい Atom を 1 つずつ持つ Unit を並べた文書を作る。
    fn doc(tiers: &[(ReadingTier, bool)]) -> SemanticDocument {
        let atoms = (0..tiers.len())
            .map(|i| Atom::new(i * 10..i * 10 + 10, AtomKind::Sentence))
            .collect();
        let units = tiers
            .iter()
            .enumerate()
            .map(|(i, &(tier, redundant))| {
                let mut unit = SemanticUnit::new(format!("u{i}"), [AtomIndex(i)], tier);
                if redundant {
                    // 参照先は 0 番固定（0 番自身が redundant な文書は作らない）。
                    unit.relations.push(Relation::RedundantWith("u0".into()));
                }
                unit
            })
            .collect();
        SemanticDocument::new(atoms, units)
    }

    fn states(doc: &SemanticDocument, budget: u8) -> Vec<DisplayState> {
        decorate(doc, budget).into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn essential_units_are_marked_and_the_rest_are_normal_at_full_budget() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Detail, false),
        ]);
        assert_eq!(
            states(&doc, 100),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Normal
            ]
        );
    }

    #[test]
    fn a_redundant_essential_unit_is_never_marked() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Essential, true),
        ]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Marked, DisplayState::Normal]
        );
    }

    #[test]
    fn weaker_tiers_dim_first() {
        let doc = doc(&[
            (ReadingTier::Detail, false),
            (ReadingTier::Context, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Essential, false),
        ]);
        // 4 Unit × 各 25 %。50 % なら ESSENTIAL と SUPPORTING だけが残る。
        assert_eq!(
            states(&doc, 50),
            [
                DisplayState::Dim,
                DisplayState::Dim,
                DisplayState::Normal,
                DisplayState::Marked
            ]
        );
    }

    #[test]
    fn a_redundant_unit_dims_before_its_non_redundant_sibling() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Supporting, true),
        ]);
        assert_eq!(
            states(&doc, 70),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim
            ]
        );
    }

    #[test]
    fn redundancy_weakens_the_tier_by_one_step_without_erasing_it() {
        // 重複した ESSENTIAL は SUPPORTING として競い、重複した SUPPORTING
        // （実効 CONTEXT）より長く残る。
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Essential, true),
            (ReadingTier::Supporting, true),
        ]);
        assert_eq!(
            states(&doc, 70),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim
            ]
        );
    }

    #[test]
    fn shorter_units_win_inside_the_same_tier_then_document_order() {
        let atoms = vec![
            Atom::new(0..30, AtomKind::Sentence),
            Atom::new(30..40, AtomKind::Sentence),
            Atom::new(40..50, AtomKind::Sentence),
        ];
        let units = vec![
            SemanticUnit::new("long", [AtomIndex(0)], ReadingTier::Context),
            SemanticUnit::new("short_a", [AtomIndex(1)], ReadingTier::Context),
            SemanticUnit::new("short_b", [AtomIndex(2)], ReadingTier::Context),
        ];
        let doc = SemanticDocument::new(atoms, units);
        // 短い 2 つ（10 + 10 = 40 %）が先。長い 30 バイトは 50 % では入らない。
        assert_eq!(
            states(&doc, 50),
            [
                DisplayState::Dim,
                DisplayState::Normal,
                DisplayState::Normal
            ]
        );
        // 同じ長さ同士は文書順。20 % なら先に出てくる short_a だけ。
        assert_eq!(
            states(&doc, 20),
            [DisplayState::Dim, DisplayState::Normal, DisplayState::Dim]
        );
    }

    #[test]
    fn at_least_one_unit_survives_the_smallest_budget() {
        let doc = doc(&[
            (ReadingTier::Detail, false),
            (ReadingTier::Essential, false),
        ]);
        assert_eq!(states(&doc, 1), [DisplayState::Dim, DisplayState::Marked]);
        // 0 は 1 に、101 以上は 100 に丸める。
        assert_eq!(states(&doc, 0), states(&doc, 1));
        assert_eq!(states(&doc, 255), states(&doc, 100));
    }

    #[test]
    fn atoms_outside_every_unit_stay_normal_at_any_budget() {
        let mut doc = doc(&[(ReadingTier::Detail, false)]);
        doc.atoms.push(Atom::new(100..120, AtomKind::CodeBlock));
        let states = states(&doc, 1);
        assert_eq!(states[1], DisplayState::Normal);
    }

    #[test]
    fn an_atom_shared_by_two_units_takes_the_stronger_state() {
        let atoms = vec![Atom::new(0..10, AtomKind::Sentence)];
        let units = vec![
            SemanticUnit::new("u0", [AtomIndex(0)], ReadingTier::Detail),
            SemanticUnit::new("u1", [AtomIndex(0)], ReadingTier::Essential),
        ];
        let doc = SemanticDocument::new(atoms, units);
        assert_eq!(states(&doc, 100), [DisplayState::Marked]);
    }

    #[test]
    fn an_empty_document_decorates_to_nothing() {
        assert!(decorate(&SemanticDocument::default(), 50).is_empty());
    }

    #[test]
    fn broken_atom_indices_do_not_panic() {
        let mut doc = doc(&[(ReadingTier::Essential, false)]);
        doc.units[0].atoms.push(AtomIndex(99));
        assert_eq!(states(&doc, 50), [DisplayState::Marked]);
    }

    #[test]
    fn zero_length_atoms_do_not_divide_by_zero() {
        let atoms = vec![Atom::new(5..5, AtomKind::Sentence)];
        let units = vec![SemanticUnit::new("u0", [AtomIndex(0)], ReadingTier::Detail)];
        let doc = SemanticDocument::new(atoms, units);
        assert_eq!(states(&doc, 1), [DisplayState::Normal]);
    }
}
