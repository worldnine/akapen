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
            relations: Vec::new(),
        }
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
    fn relation_round_trips_as_an_externally_tagged_object() {
        let relation = Relation::RedundantWith("u3".into());
        let json = serde_json::to_string(&relation).unwrap();
        assert_eq!(json, r#"{"redundant_with":"u3"}"#);
        assert_eq!(serde_json::from_str::<Relation>(&json).unwrap(), relation);
    }
}
