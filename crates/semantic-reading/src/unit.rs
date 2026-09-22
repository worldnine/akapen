//! Semantic Unit — Jev が現在の文書から知覚した意味的まとまり。
//!
//! Markdown の段落は意味のまとまりとは限らない（`paragraph != semantic
//! unit`）。そこで Atom 間の境界判断だけを Jev に任せ、その結果できた
//! まとまりを Unit として持つ。Unit が持つ判定は、いま問われていることへの
//! [`SemanticUnit::score`] と、その Unit の**核**
//! （[`SemanticUnit::core_atoms`]）の 2 つだけである。

use serde::{Deserialize, Serialize};

use crate::atom::AtomIndex;

/// Semantic Unit の識別子。
///
/// 値は provider（Jev を呼ぶ判定器）が付ける文字列で、この crate は中身を
/// 解釈しない。[`SemanticUnit::section_of`] の参照先にだけ使う。
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

/// Jev が知覚した意味的まとまり 1 つ。
// **`Eq` は derive しない。** [`SemanticUnit::score`] が `f32` で、
// 浮動小数は全順序を持たない（NaN）。この型を HashMap の鍵にしている
// 場所は無く、比較に要るのは `PartialEq`（テストの assert）だけである。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticUnit {
    /// provider が付けた識別子。文書内で一意。
    pub id: UnitId,
    /// この Unit を構成する Atom（[`crate::SemanticDocument::atoms`] の添字）。
    /// 判断単位は Unit だが表示単位は Atom なので、意味情報はここを通って
    /// Atom へ投影される。
    pub atoms: Vec<AtomIndex>,
    /// この Unit の**核** — 「ここだけ読めば要点が取れる」と判断された Atom。
    /// [`Self::atoms`] の部分集合でなければならない（[`crate::SemanticDocument::validate`]）。
    ///
    /// MARKED をこの Atom だけに絞るために使う（[`crate::marks::mark`]）。
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
    /// **積極的に決めた**場合のためにある — 足切りを越えなかった Unit や、
    /// 散文の Atom を 1 つも持たない Unit がこれになる。ここを 2 値にすると、
    /// 核を選ばなかった Unit が丸ごと光ってしまい絞れない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_atoms: Option<Vec<AtomIndex>>,
    /// この Unit が属する**節**の見出し Unit。節の外（見出しより前の前書き）
    /// なら `None`。
    ///
    /// 見出し Unit 自身もこのフィールドを持ち、その値は**親の節**の見出し
    /// Unit である（`### 費用` なら `## 決定事項` の Unit）。だから節は
    /// このフィールドだけで入れ子になり、この crate は `#` の数を知らずに
    /// 済む。
    ///
    /// **この crate には読み手が居ない。** [`crate::marks`] は節を見ない
    /// （問いが選ぶので、見出しを構造で戻す必要が無い）。ワイヤを通って
    /// [`crate::SemanticDocument`] まで運ばれ、`validate` が実在と
    /// 自己参照だけを見る。判定器が埋めるのは
    /// `examples/semantic/jev-annotate.py` の `assign_sections` で、
    /// **構文から決まる値**である（設計書「Jev に判断させないもの:
    /// syntax parsing」）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_of: Option<UnitId>,
    /// **いま問われていることに、この Unit がどれだけ答えているか**
    /// （`docs/design/semantic-reading-layer.md`）。
    ///
    /// [`crate::marks::mark`] が、この値の高い順に上から N % を光らせる。
    ///
    /// **2 値でよい。** [`Self::core_atoms`] と違って「無い」と「空」を
    /// 分ける必要が無い:
    ///
    /// | 値 | wire 形 | 意味 |
    /// |---|---|---|
    /// | `None` | フィールドが無い | **スコアを受け取っていない** — この Unit は光らない |
    /// | `Some(v)` | `"score":0.94` | 問いへの答えの強さ。0.0〜1.0 |
    ///
    /// `None` が既定なので、このフィールドを知らない判定器・fixture は
    /// 従来どおり動く（0 本になり、理由は読み出しに出る。
    /// [`crate::marks::has_scores`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

impl SemanticUnit {
    /// Atom だけを持つ（判定を受けていない）Unit を作る。
    pub fn new(id: impl Into<UnitId>, atoms: impl IntoIterator<Item = AtomIndex>) -> Self {
        Self {
            id: id.into(),
            atoms: atoms.into_iter().collect(),
            core_atoms: None,
            section_of: None,
            score: None,
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

    /// 核を 1 つでも持つか — **この Unit は MARKED になりうるか**。
    ///
    /// [`Self::core_atoms`] の 3 値のうち `Some([])` だけが `false` になる。
    /// `None`（絞り込みを受けていない）は Unit 全体が核なので `true`。
    ///
    /// [`crate::marks::mark`] が、足切りと併せてこれを見る。
    pub fn has_core(&self) -> bool {
        !matches!(&self.core_atoms, Some(core) if core.is_empty())
    }

    /// 核を明示的に決める。空を渡すと「**核を持たない**」になり、この Unit は
    /// MARKED にならない（[`Self::core_atoms`] の 3 値のうち `Some([])`）。
    pub fn set_core(&mut self, atoms: impl IntoIterator<Item = AtomIndex>) {
        self.core_atoms = Some(atoms.into_iter().collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrefined_unit_treats_every_atom_as_its_core() {
        // core_atoms が無いのは「核が無い」ではなく「絞り込みを受けていない」。
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0), AtomIndex(1)]);
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
        // 後者は足切りを越えなかった Unit が受け取る値である。ここを混ぜると、
        // つまみを 100 % まで上げたときにその Unit が丸ごと光る。
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0), AtomIndex(1)]);
        unit.set_core([]);
        assert_eq!(unit.core_atoms, Some(Vec::new()));
        assert!(!unit.is_core(AtomIndex(0)));
        assert!(!unit.is_core(AtomIndex(1)));
    }

    /// `has_core` は 3 値のうち `Some([])` だけを弾く。
    /// [`crate::marks::mark`] が足切りと併せてこれで核を選ぶ。
    #[test]
    fn has_core_is_false_only_for_the_empty_core() {
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0), AtomIndex(1)]);
        // None — 絞り込みを受けていない。Unit 全体が核。
        assert!(unit.has_core());
        unit.set_core([AtomIndex(1)]);
        assert!(unit.has_core());
        unit.set_core([]);
        assert!(!unit.has_core());
    }

    #[test]
    fn an_empty_core_survives_the_wire_round_trip() {
        // `[]` が `skip_serializing_if` で消えると「絞り込み無し」に化ける。
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0)]);
        unit.set_core([]);
        let json = serde_json::to_string(&unit).unwrap();
        assert_eq!(json, r#"{"id":"u1","atoms":[0],"core_atoms":[]}"#);
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    #[test]
    fn core_atoms_are_absent_from_the_wire_form_until_they_are_chosen() {
        // 既存の fixture・判定器の出力の形を変えない（空なら書き出さない、
        // 無ければ空として読む）。
        let unit = SemanticUnit::new("u1", [AtomIndex(0)]);
        let json = serde_json::to_string(&unit).unwrap();
        assert_eq!(json, r#"{"id":"u1","atoms":[0]}"#);
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    #[test]
    fn section_of_is_absent_from_the_wire_until_the_annotator_fills_it() {
        // 節を知らない判定器・既存の fixture の形を変えない。
        let unit = SemanticUnit::new("u1", [AtomIndex(0)]);
        assert_eq!(unit.section_of, None);
        let json = serde_json::to_string(&unit).unwrap();
        assert!(!json.contains("section_of"), "{json}");
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    #[test]
    fn section_of_round_trips_as_a_bare_id() {
        let mut unit = SemanticUnit::new("u9", [AtomIndex(0)]);
        unit.section_of = Some("u4".into());
        let json = serde_json::to_string(&unit).unwrap();
        assert_eq!(json, r#"{"id":"u9","atoms":[0],"section_of":"u4"}"#);
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    #[test]
    fn a_score_rides_the_wire_only_when_the_annotator_gave_one() {
        let mut unit = SemanticUnit::new("u1", [AtomIndex(0)]);
        assert_eq!(unit.score, None);
        assert!(!serde_json::to_string(&unit).unwrap().contains("score"));
        unit.score = Some(0.94);
        let json = serde_json::to_string(&unit).unwrap();
        assert_eq!(json, r#"{"id":"u1","atoms":[0],"score":0.94}"#);
        assert_eq!(serde_json::from_str::<SemanticUnit>(&json).unwrap(), unit);
    }

    /// **DIM 版の残骸を読み飛ばす。** `~/.cache/akapen/semantic/` には
    /// `reading_tier` と `relations` を持つ答えが残っている。読めなく
    /// なると、キャッシュに当たっていた文書が黙って払い直しになる。
    #[test]
    fn a_unit_from_the_old_wire_still_loads_with_its_extra_fields_ignored() {
        let json = r#"{"id":"u1","atoms":[0],"reading_tier":"essential",
                       "relations":[{"redundant_with":"u2"}],"score":0.5}"#;
        let unit: SemanticUnit = serde_json::from_str(json).unwrap();
        assert_eq!(unit.id, UnitId::from("u1"));
        assert_eq!(unit.score, Some(0.5));
        assert_eq!(unit.core_atoms, None);
    }
}
