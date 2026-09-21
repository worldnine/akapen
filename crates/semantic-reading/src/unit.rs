//! Semantic Unit — Jev が現在の文書から知覚した意味的まとまり。
//!
//! Markdown の段落は意味のまとまりとは限らない（`paragraph != semantic
//! unit`）。そこで Atom 間の境界判断だけを Jev に任せ、その結果できた
//! まとまりを Unit として持つ。Unit が持つのは粗い [`ReadingTier`] と
//! [`Relation`] だけで、0〜100 の importance score は**持たない**
//! （設計書が明示的に排除している）。

use serde::{Deserialize, Serialize};

use crate::atom::AtomIndex;

/// Semantic Unit の識別子。
///
/// 値は provider（将来の Jev）が付ける文字列で、この crate は中身を
/// 解釈しない。[`Relation::RedundantWith`] の参照先にだけ使う。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnitId(pub String);

impl UnitId {
    /// 識別子の文字列表現。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for UnitId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<String> for UnitId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for UnitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 読む優先度の粗い段階。
///
/// 宣言順がそのまま「Budget が下がっても残りやすい順」であり、derive した
/// [`Ord`] もその向きになっている（`Essential < Supporting < Context <
/// Detail`）。段階を 4 つに留めるのは意図的で、Jev に精密な順位スコアを
/// 出させないための型的な歯止めでもある。
///
/// これは Jev の**能力の制約ではなく設計判断**である。Jev は rubric に沿って
/// 採点する Score primitive を持っているので、0〜100 の importance score を
/// 出させること自体はできる（`docs/design/jev.md`）。それをやらず粗い 4 段に
/// 倒し、同一 Tier 内の順序は redundancy / length / document position といった
/// 決定論的なローカル rule（[`crate::policy`]）で決める、というのが設計書の
/// 選択である。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingTier {
    /// 落とすと文書の要点・結論・制約を取り違えうる。最後まで残る。
    Essential,
    /// ESSENTIAL の理解や納得を支える。次に残る。
    Supporting,
    /// 背景や前提。Budget 低下時に DIM になる。
    Context,
    /// 例・細部・追加説明。早めに DIM になる。
    Detail,
}

impl ReadingTier {
    /// 1 段だけ弱い Tier（DETAIL はこれ以上落ちない）。
    ///
    /// redundancy を「Tier を上書きする」のではなく「1 段押し下げる」形で
    /// 効かせるために使う。詳細は [`crate::policy`] を参照。
    pub fn weakened(self) -> Self {
        match self {
            ReadingTier::Essential => ReadingTier::Supporting,
            ReadingTier::Supporting => ReadingTier::Context,
            ReadingTier::Context | ReadingTier::Detail => ReadingTier::Detail,
        }
    }
}

/// Unit 間の関係。
///
/// **Reading Tier とは別軸**である。`reading_tier = SUPPORTING` かつ
/// `redundant_with = u3`、つまり「意味上は重要だが、すでに得た情報なので
/// 追加 information gain は低い」という状態を表現できる。だから redundancy
/// は [`ReadingTier`] の 5 番目の値ではなく、独立した列挙として
/// [`SemanticUnit::relations`] に並ぶ。
///
/// MVP で必要な relation は `REDUNDANT_WITH` のみ。将来 relation が増えても
/// 既存のパターンマッチが壊れないよう `#[non_exhaustive]` にしてある。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Relation {
    /// この Unit は、参照先の Unit と実質同じ内容である。
    RedundantWith(UnitId),
}

/// Jev が知覚した意味的まとまり 1 つ。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticUnit {
    /// provider が付けた識別子。文書内で一意。
    pub id: UnitId,
    /// この Unit を構成する Atom（[`crate::SemanticDocument::atoms`] の添字）。
    /// 判断単位は Unit だが表示単位は Atom なので、意味情報はここを通って
    /// Atom へ投影される。
    pub atoms: Vec<AtomIndex>,
    /// 読む優先度の粗い段階。
    pub reading_tier: ReadingTier,
    /// この Unit の**核** — 「ここだけ読めば要点が取れる」と判断された Atom。
    /// [`Self::atoms`] の部分集合でなければならない（[`crate::SemanticDocument::validate`]）。
    ///
    /// MARKED をこの Atom だけに絞るために使う（[`crate::policy::decorate`]）。
    /// 効くのは MARKED になる Unit（ESSENTIAL かつ非 REDUNDANT）だけである。
    /// NORMAL と DIM は Unit 全体に一律で掛かる。
    ///
    /// **3 値である。** 「無い」と「空」を区別する:
    ///
    /// | 値 | wire 形 | 意味 |
    /// |---|---|---|
    /// | `None` | フィールドが無い | **絞り込みを受けていない** — Unit 全体が MARKED |
    /// | `Some([])` | `"core_atoms":[]` | **核を持たない** — この Unit は MARKED にならない |
    /// | `Some([i])` | `"core_atoms":[i]` | `i` だけが MARKED |
    ///
    /// `None` が既定なので、このフィールドを知らない判定器・fixture は従来
    /// どおり動く。`Some([])` は、判定器が「この Unit に核は要らない」と
    /// **積極的に決めた**場合のためにある — リスト 1 本につき核を 1 つに絞る
    /// とき、選に漏れた Unit がこれになる（[`crate::policy`] の「run キャップ」）。
    /// ここを 2 値にすると、選に漏れた Unit が丸ごと光ってしまい絞れない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_atoms: Option<Vec<AtomIndex>>,
    /// 他 Unit との関係。空でよい。
    #[serde(default)]
    pub relations: Vec<Relation>,
}

impl SemanticUnit {
    /// Tier だけを持つ（relation の無い）Unit を作る。
    pub fn new(
        id: impl Into<UnitId>,
        atoms: impl IntoIterator<Item = AtomIndex>,
        reading_tier: ReadingTier,
    ) -> Self {
        Self {
            id: id.into(),
            atoms: atoms.into_iter().collect(),
            reading_tier,
            core_atoms: None,
            relations: Vec::new(),
        }
    }

    /// この Atom は、MARKED を絞る先として残るか。
    ///
    /// [`Self::core_atoms`] が `None` なら**すべての Atom が核**である
    /// （絞り込みを受けていない Unit は丸ごと MARKED になる、という従来の
    /// 振る舞い）。`Some` なら、そこに挙がっている Atom だけが核になる —
    /// **空の `Some` はどの Atom も核でない**、という意味になる。
    pub fn is_core(&self, atom: AtomIndex) -> bool {
        match &self.core_atoms {
            None => true,
            Some(core) => core.contains(&atom),
        }
    }

    /// 核を明示的に決める。空を渡すと「**核を持たない**」になり、この Unit は
    /// MARKED にならない（[`Self::core_atoms`] の 3 値のうち `Some([])`）。
    pub fn set_core(&mut self, atoms: impl IntoIterator<Item = AtomIndex>) {
        self.core_atoms = Some(atoms.into_iter().collect());
    }

    /// この Unit が重複だと判断された先。複数あれば最初のもの。
    pub fn redundant_with(&self) -> Option<&UnitId> {
        // MVP の relation は `RedundantWith` 1 種類なので先頭を見れば足りる。
        // 種類が増えたらこの match が網羅でなくなってコンパイルが止まる —
        // それがここを書き直す合図になる。
        match self.relations.first() {
            Some(Relation::RedundantWith(target)) => Some(target),
            None => None,
        }
    }

    /// 何かの重複として印が付いているか。
    pub fn is_redundant(&self) -> bool {
        self.redundant_with().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_order_runs_from_most_to_least_durable() {
        assert!(ReadingTier::Essential < ReadingTier::Supporting);
        assert!(ReadingTier::Supporting < ReadingTier::Context);
        assert!(ReadingTier::Context < ReadingTier::Detail);
    }

    #[test]
    fn weakened_drops_one_step_and_stops_at_detail() {
        assert_eq!(ReadingTier::Essential.weakened(), ReadingTier::Supporting);
        assert_eq!(ReadingTier::Supporting.weakened(), ReadingTier::Context);
        assert_eq!(ReadingTier::Context.weakened(), ReadingTier::Detail);
        assert_eq!(ReadingTier::Detail.weakened(), ReadingTier::Detail);
    }

    #[test]
    fn redundancy_lives_beside_the_tier_not_inside_it() {
        // 「意味上は SUPPORTING だが、すでに得た情報」を素直に表現できる。
        let mut unit = SemanticUnit::new("u7", [AtomIndex(0)], ReadingTier::Supporting);
        assert!(!unit.is_redundant());
        unit.relations.push(Relation::RedundantWith("u3".into()));
        assert_eq!(unit.reading_tier, ReadingTier::Supporting);
        assert_eq!(unit.redundant_with(), Some(&UnitId::from("u3")));
    }

    #[test]
    fn an_unrefined_unit_treats_every_atom_as_its_core() {
        // core_atoms が無いのは「核が無い」ではなく「絞り込みを受けていない」。
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        assert_eq!(unit.core_atoms, None);
        assert!(unit.is_core(AtomIndex(0)));
        assert!(unit.is_core(AtomIndex(1)));
        unit.set_core([AtomIndex(1)]);
        assert!(!unit.is_core(AtomIndex(0)));
        assert!(unit.is_core(AtomIndex(1)));
    }

    #[test]
    fn an_empty_core_means_the_unit_has_no_core_at_all() {
        // 「絞り込みを受けていない」(None) と「核を持たない」(Some([])) は別物。
        // 後者はリスト 1 本につき核を 1 つに絞るときに、選に漏れた Unit が
        // 受け取る値である。ここを混ぜると選に漏れた Unit が丸ごと光る。
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        unit.set_core([]);
        assert_eq!(unit.core_atoms, Some(Vec::new()));
        assert!(!unit.is_core(AtomIndex(0)));
        assert!(!unit.is_core(AtomIndex(1)));
    }

    #[test]
    fn an_empty_core_survives_the_wire_round_trip() {
        // `[]` が `skip_serializing_if` で消えると「絞り込み無し」に化ける。
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0)], ReadingTier::Essential);
        unit.set_core([]);
        let json = serde_json::to_string(&unit).unwrap();
        assert_eq!(
            json,
            r#"{"id":"u1","atoms":[0],"reading_tier":"essential","core_atoms":[],"relations":[]}"#
        );
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    #[test]
    fn core_atoms_are_absent_from_the_wire_form_until_they_are_chosen() {
        // 既存の fixture・判定器の出力の形を変えない（空なら書き出さない、
        // 無ければ空として読む）。
        let unit = SemanticUnit::new("u1", [AtomIndex(0)], ReadingTier::Essential);
        let json = serde_json::to_string(&unit).unwrap();
        assert_eq!(
            json,
            r#"{"id":"u1","atoms":[0],"reading_tier":"essential","relations":[]}"#
        );
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    #[test]
    fn relation_round_trips_as_an_externally_tagged_object() {
        let relation = Relation::RedundantWith("u3".into());
        let json = serde_json::to_string(&relation).unwrap();
        assert_eq!(json, r#"{"redundant_with":"u3"}"#);
        assert_eq!(serde_json::from_str::<Relation>(&json).unwrap(), relation);
    }
}
