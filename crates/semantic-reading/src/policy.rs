//! Reading Policy — Tier と relation から、現在の Budget での表示状態を決める。
//!
//! ここは完全にローカルな決定論的計算である。`37% -> 36% -> 35%` という
//! 操作で Jev を呼ばないのがこの層の存在意義で、入力が同じなら出力も常に
//! 同じになる（乱数も時刻もスコアも使わない）。
//!
//! # この層は attention の配分を決めている
//!
//! 設計書（`docs/design/semantic-reading-layer.md`）の「研究的背景」が、
//! この層が何をする層なのかを決めている。
//!
//! > Duggan & Payne の satisficing モデルでは、読み進めることで得られる
//! > information gain が低下すると、読者は次の場所へ移動する。
//!
//! > **限られた attention を、意味的な収穫が高い部分へ配分する**
//!
//! つまりここで決めているのは **attention を文書のどこへ配るか**であって、
//! Unit の重要度ランキングではない。`keep_order` は確かに Unit を一列に
//! 並べるが、それは配分を決めるための手順である。この module から外へ出るのは
//! 「その Budget で残った集合」だけで、Unit ごとの順位もスコアも出さない
//! （設計書「0〜100 の importance score は使用しない」）。
//!
//! 下の並び替え鍵は、この枠組みではこう読める。
//!
//! - **Tier** は、そこを読んで得られる収穫の粗い見積り。粗い 4 段に留めるのは
//!   設計書の選択で、理由は [`crate::ReadingTier`] にある。
//! - **redundancy** は「その収穫をもう得てしまっているか」。satisficing は
//!   読み**進める**過程のモデルなので、重複は文書が単体で持つ性質ではなく
//!   **既読との相対**で決まる。設計書も「**既読内容との** redundancy」と
//!   書いている。`REDUNDANT_WITH` が対称な「重複している」ではなく方向を持つ
//!   関係なのはそのためで、判定器側でもこの方向を落とすと、結論のように文書中で
//!   何度も触れられる箇所を重複と見なしてしまう
//!   （実測は `examples/semantic/README.md`「redundancy は方向を指定する」）。
//! - **バイト長** は、その Unit が食う attention の量そのもの。同じ Tier なら
//!   短い方が attention あたりの収穫が大きい、というのが下の「短い方を先に
//!   残す」の言い換えである。
//!
//! 打ち切りを prefix にして best-fit にしないことも、ここから説明できる。
//! 設計書は Budget について「43% 42% 41% で表示が変わらなくても問題ない」と
//! 書いていて、**attention を余さず詰めること自体は目的ではない**。
//! 詰め方を捨てて代わりに得ているものは、下の単調性である。
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
//! # context preservation — 欠けているのは「1 項目」ではなく逐次性
//!
//! 設計書は同一 Tier 内の rule として
//! `redundancy / length / document position / context preservation` の 4 つを
//! 挙げており、上の鍵は最初の 3 つしか使っていない。ただしこれは
//! **4 つのうち 1 つを省いた**という話ではない。
//!
//! 最初の 3 つはどれも **Unit 単体の属性**から決まる静的な値である
//! （重複しているか・何バイトか・文書のどこにあるか）。だから鍵は Unit を
//! 1 つ見れば計算できる。4 つ目だけが違い、
//! **残った Unit を順に読んだとき文脈が繋がるか**という、選択の結果に
//! 依存する性質を指している。
//!
//! そして satisficing は「読み進めて information gain が落ちたら次へ移る」
//! という**逐次的**なモデルである。4 つ目は、この層で逐次性を担う唯一の
//! 項目だった。いま `decorate` がしているのは「集合を選ぶ」ことまでで、
//! **選んだ集合が読む経路として成立しているかは誰も見ていない**。
//!
//! 鍵のどこにそれが現れるかは具体的に言える。`redundant` は
//! [`crate::SemanticUnit::is_redundant`] であって、`REDUNDANT_WITH` の参照先が
//! その Budget で残っているかは見ていない。u0 を参照して弱められた Unit は、
//! u0 自身が DIM になる Budget でも弱められたままである。ここでの「既読」は
//! 「文書の中で前にある」であって、「読者が実際に辿る経路の上で前にある」では
//! ない。
//!
//! **ここで実装しないこと。** 設計書は context preservation の中身を定義して
//! いない（語が出てくるのは上の rule の列挙 1 箇所だけである）。何をもって
//! 「文脈が繋がる」とするかをこの module で決めると、設計書に無い設計を足す
//! ことになる。同じ話は `docs/gotchas/open-questions.md` にも置いてある。
//!
//! # 表示状態の割り当て
//!
//! ```text
//! 残った Unit で ESSENTIAL かつ非 REDUNDANT
//!     その Unit の核（core_atoms）        -> MARKED
//!     同じ Unit の残り                    -> NORMAL
//! 残ったそれ以外                          -> NORMAL
//! 残らなかったもの                        -> DIM（Unit 全体に一律）
//! どの Unit にも属さない Atom             -> NORMAL
//! ```
//!
//! [`crate::SemanticUnit::core_atoms`] は 3 値で、`None`（絞り込み無し）なら
//! Unit 全体が MARKED、`Some([])`（**核を持たない**）ならその Unit は MARKED に
//! ならない。後者は ESSENTIAL のまま NORMAL になる — **DIM ではない**。沈めるか
//! どうかは Tier と Budget が決めることで、核の選に漏れたことは「読まなくて
//! よい」を意味しないからである。
//!
//! ## run キャップ — リスト 1 本につき核は 1 つ
//!
//! `Some([])` が要るのは判定器の側の事情である。箇条書きを項目ごとに割ると
//! （`examples/semantic/jev-annotate.py` の境界規則 4）、1 本のリストの中で
//! ESSENTIAL な Unit がいくつも立ち、**そのすべてが光る**。実測では業務
//! `CLAUDE.md` の MARKED が 3.3〜3.7 % から 10.8〜11.2 % へ増えた。
//!
//! そこで判定器は、規則 4 でつながった Unit の並び（= 1 本のリスト）ごとに
//! 核を 1 つだけ選び、選に漏れた Unit へ `Some([])` を返す。**この層は
//! 変わらない** — run を知らないし、知る必要もない。ここが読むのは
//! [`crate::SemanticUnit::is_core`] だけである。
//!
//! MARKED は Budget に依存しない。Budget 100% で全文を見せつつ ESSENTIAL に
//! 薄い marker を重ねる、という設計書の最初のデモがそのままこの規則である。
//!
//! ## MARKED だけ、投影を選択的にする
//!
//! 設計書は判断単位と表示単位について
//!
//! > Semantic Unit に付与した意味情報を、その Unit を構成する Atom へ投影する
//!
//! とだけ書いていて、**投影が一律コピーだとは書いていない**。当初の実装が
//! Unit の Tier を構成 Atom 全部へそのまま配ったのはこちらの解釈であり、
//! ここで狭めているのはその解釈である（設計書の変更ではない）。設計書が
//! MARKED / DIM を **Atom** に対して定義していることとは、むしろこの形の方が
//! 整合する。
//!
//! 動機は実測である。45.6 KB の実業務文書で、Unit 一律の投影だと MARKED が
//! Atom バイトの 46.7 %（Budget 100 / 80 / 60 %）を占めた。境界の構造ルール
//! 「どちらも list_item → SAME」が箇条書き 1 つを丸ごと 1 Unit にするため、
//! ESSENTIAL な項目は段落ごと光る。**半分が光っていれば、光っていない方が
//! 目立つ。** 核だけを MARKED にすると、同じ文書・同じ Budget で 5.0 % に
//! 落ちた（実測表は `examples/semantic/measurements/core-selection.md`）。
//!
//! **DIM は一律のままにする。** Unit が落ちたなら、その Unit は丸ごと沈むのが
//! 正しい。ここを選択的にすると「なぜこの行の一部だけが沈んでいるのか」が
//! 読者に説明できなくなる。核の選択は「残った中のどこを読むか」であって、
//! 「何を落とすか」ではない。
//!
//! 核を選ぶのは判定器（Jev）で、この層は [`crate::SemanticUnit::core_atoms`]
//! を読むだけである。**核の選択は Budget に依存しない**ので、上の単調性は
//! そのまま成り立つ。

use std::ops::Range;

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
        // MARKED になりうる Unit か。なる場合だけ、Unit の中で核と残りを
        // 分ける（NORMAL と DIM は Unit 全体に一律で掛かる）。
        let marks = kept[unit_index]
            && unit.reading_tier == ReadingTier::Essential
            && !unit.is_redundant();
        for &atom in &unit.atoms {
            let state = if !kept[unit_index] {
                DisplayState::Dim
            } else if marks && unit.is_core(atom) {
                DisplayState::Marked
            } else {
                DisplayState::Normal
            };
            if let Some(slot) = states.get_mut(atom.0) {
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
    use crate::atom::{Atom, AtomIndex, AtomKind};
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

    /// 核を持つ ESSENTIAL は、核だけ MARKED で残りは NORMAL。
    #[test]
    fn only_the_core_of_an_essential_unit_is_marked() {
        let atoms = (0..3)
            .map(|i| Atom::new(i * 10..i * 10 + 10, AtomKind::Sentence))
            .collect();
        let mut unit = SemanticUnit::new(
            "u0",
            [AtomIndex(0), AtomIndex(1), AtomIndex(2)],
            ReadingTier::Essential,
        );
        unit.set_core([AtomIndex(1)]);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Normal
            ]
        );
    }

    /// 核を持つ Unit が Budget から落ちたら、核も含めて丸ごと DIM。
    /// **DIM は選択的にしない** — 行の一部だけ沈む理由を読者に説明できない。
    #[test]
    fn a_dropped_unit_dims_whole_even_when_it_has_a_core() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
            Atom::new(20..60, AtomKind::Sentence),
        ];
        let mut essential = SemanticUnit::new(
            "keep",
            [AtomIndex(0), AtomIndex(1)],
            ReadingTier::Essential,
        );
        essential.set_core([AtomIndex(0)]);
        let mut dropped = SemanticUnit::new("drop", [AtomIndex(2)], ReadingTier::Essential);
        dropped.set_core([AtomIndex(2)]);
        let doc = SemanticDocument::new(atoms, vec![essential, dropped]);
        // 60 バイト中、短い "keep"（20 バイト）だけが 70 % に入る。
        assert_eq!(
            states(&doc, 70),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim
            ]
        );
    }

    /// 核は Budget に依存しない — 絞られた MARKED も、Unit が残る限り一定。
    #[test]
    fn the_core_does_not_move_when_the_budget_does() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
            Atom::new(20..90, AtomKind::Sentence),
        ];
        let mut essential =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        essential.set_core([AtomIndex(1)]);
        let doc = SemanticDocument::new(
            atoms,
            vec![
                essential,
                SemanticUnit::new("u1", [AtomIndex(2)], ReadingTier::Detail),
            ],
        );
        for budget in [100, 60, 30, 1] {
            assert_eq!(
                states(&doc, budget)[..2],
                [DisplayState::Normal, DisplayState::Marked],
                "budget {budget}"
            );
        }
    }

    /// 核が NORMAL / DIM の Unit に付いていても無視される — 効くのは MARKED
    /// になる Unit だけ。
    #[test]
    fn a_core_on_a_non_marked_unit_changes_nothing() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
        ];
        let mut unit =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Supporting);
        unit.set_core([AtomIndex(0)]);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Normal]
        );
    }

    /// **空の核は「核を持たない」** — その Unit は MARKED にならず NORMAL に
    /// なる。リスト 1 本につき核を 1 つに絞るとき、選に漏れた ESSENTIAL な
    /// Unit がこれを受け取る。DIM ではない（沈めるかどうかは Tier と Budget が
    /// 決めることで、核の選に漏れたことは「読まなくてよい」を意味しない）。
    #[test]
    fn an_essential_unit_with_an_empty_core_is_normal_not_marked() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
        ];
        let mut unit =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        unit.set_core([]);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Normal]
        );
    }

    /// 核を返さない判定器（と既存の fixture）は従来どおり Unit 全体が MARKED。
    #[test]
    fn a_unit_without_a_core_still_marks_all_of_its_atoms() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
        ];
        let unit =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Marked, DisplayState::Marked]
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
