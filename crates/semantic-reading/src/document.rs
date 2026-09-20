//! Semantic Document — Atom 列 + Semantic Unit 列。
//!
//! 文書そのもの（source テキスト）はここには入らない。この crate が扱うのは
//! あくまで「source のどの範囲が、どの意味的まとまりに属し、どの Tier か」
//! までで、テキストの所有はクライアント側の責務である。

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::atom::{Atom, AtomIndex};
use crate::error::Error;
use crate::unit::{Relation, SemanticUnit, UnitId};

/// 1 つの文書についての semantic annotation 一式。
///
/// `atoms` が文書内の位置の唯一の台帳で、`units` はそこへの添字だけを持つ。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticDocument {
    /// 文書を機械的に分割した Atom 列。文書順に並んでいることを期待する
    /// （[`crate::policy::decorate`] の出力順はこの並び順に従う）。
    pub atoms: Vec<Atom>,
    /// Jev が知覚した意味的まとまり。
    pub units: Vec<SemanticUnit>,
}

impl SemanticDocument {
    /// Atom 列と Unit 列から文書を作る。検証はしない（[`Self::validate`]）。
    pub fn new(atoms: Vec<Atom>, units: Vec<SemanticUnit>) -> Self {
        Self { atoms, units }
    }

    /// 添字で Atom を引く。範囲外なら `None`。
    pub fn atom(&self, index: AtomIndex) -> Option<&Atom> {
        self.atoms.get(index.0)
    }

    /// 識別子で Unit を引く。
    pub fn unit(&self, id: &UnitId) -> Option<&SemanticUnit> {
        self.units.iter().find(|unit| &unit.id == id)
    }

    /// Atom も Unit も無いか。
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty() && self.units.is_empty()
    }

    /// 文書としての辻褄を検査する。
    ///
    /// [`crate::policy::decorate`] は壊れた入力でも panic しない作りだが、
    /// provider が外から受け取った JSON はここで一度弾いておく。検査するのは
    ///
    /// - Atom の範囲が逆転していないこと
    /// - Unit の Atom 添字が範囲内であること
    /// - Unit の識別子が重複していないこと
    /// - `RedundantWith` の参照先が実在し、自分自身でないこと
    ///
    /// の 4 点。Atom がどの Unit にも属さないことは**エラーにしない**
    /// （未判断の Atom は NORMAL のまま表示されればよい）。
    pub fn validate(&self) -> Result<()> {
        for (i, atom) in self.atoms.iter().enumerate() {
            if atom.range.start > atom.range.end {
                return Err(Error::Invalid(format!(
                    "atom {i} の範囲が逆転しています: {}..{}",
                    atom.range.start, atom.range.end
                )));
            }
        }

        for (i, unit) in self.units.iter().enumerate() {
            if self.units[..i].iter().any(|other| other.id == unit.id) {
                return Err(Error::Invalid(format!(
                    "unit 識別子 `{}` が重複しています",
                    unit.id
                )));
            }
            for index in &unit.atoms {
                if index.0 >= self.atoms.len() {
                    return Err(Error::Invalid(format!(
                        "unit `{}` が範囲外の atom {} を参照しています（atom は {} 個）",
                        unit.id,
                        index.0,
                        self.atoms.len()
                    )));
                }
            }
            for relation in &unit.relations {
                let Relation::RedundantWith(target) = relation;
                if target == &unit.id {
                    return Err(Error::Invalid(format!(
                        "unit `{}` が自分自身との重複になっています",
                        unit.id
                    )));
                }
                if self.unit(target).is_none() {
                    return Err(Error::Invalid(format!(
                        "unit `{}` が未知の unit `{target}` を参照しています",
                        unit.id
                    )));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomKind;
    use crate::unit::ReadingTier;

    fn doc() -> SemanticDocument {
        SemanticDocument::new(
            vec![
                Atom::new(0..10, AtomKind::Heading),
                Atom::new(12..40, AtomKind::Sentence),
            ],
            vec![
                SemanticUnit::new("u1", [AtomIndex(0)], ReadingTier::Essential),
                SemanticUnit::new("u2", [AtomIndex(1)], ReadingTier::Detail),
            ],
        )
    }

    #[test]
    fn a_well_formed_document_validates() {
        assert!(doc().validate().is_ok());
        assert!(SemanticDocument::default().is_empty());
    }

    #[test]
    fn lookups_go_through_index_and_id() {
        let doc = doc();
        assert_eq!(doc.atom(AtomIndex(1)).unwrap().kind, AtomKind::Sentence);
        assert!(doc.atom(AtomIndex(9)).is_none());
        assert_eq!(
            doc.unit(&UnitId::from("u2")).unwrap().reading_tier,
            ReadingTier::Detail
        );
        assert!(doc.unit(&UnitId::from("nope")).is_none());
    }

    #[test]
    fn validate_rejects_reversed_ranges() {
        let mut doc = doc();
        // 逆転した範囲（リテラルで書くと clippy に叱られるので組み立てる）。
        doc.atoms[0].range = std::ops::Range { start: 40, end: 12 };
        assert!(matches!(doc.validate(), Err(Error::Invalid(_))));
    }

    #[test]
    fn validate_rejects_out_of_range_atom_indices() {
        let mut doc = doc();
        doc.units[0].atoms.push(AtomIndex(7));
        let err = doc.validate().unwrap_err().to_string();
        assert!(err.contains("範囲外の atom 7"), "{err}");
    }

    #[test]
    fn validate_rejects_duplicate_unit_ids() {
        let mut doc = doc();
        doc.units[1].id = UnitId::from("u1");
        let err = doc.validate().unwrap_err().to_string();
        assert!(err.contains("重複"), "{err}");
    }

    #[test]
    fn validate_rejects_a_dangling_redundancy_target() {
        let mut broken = doc();
        broken.units[1]
            .relations
            .push(Relation::RedundantWith("u9".into()));
        assert!(broken.validate().unwrap_err().to_string().contains("未知"));
    }

    #[test]
    fn validate_rejects_self_redundancy() {
        let mut broken = doc();
        broken.units[1]
            .relations
            .push(Relation::RedundantWith("u2".into()));
        assert!(
            broken
                .validate()
                .unwrap_err()
                .to_string()
                .contains("自分自身")
        );
    }

    #[test]
    fn atoms_outside_every_unit_are_allowed() {
        // 未判断の Atom があっても文書としては正しい。
        let mut doc = doc();
        doc.atoms.push(Atom::new(41..60, AtomKind::CodeBlock));
        assert!(doc.validate().is_ok());
    }
}
