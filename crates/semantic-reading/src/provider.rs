//! Provider — semantic annotation の供給源。
//!
//! 「文書を見て意味のまとまりを知覚し、いまの問いに答えているかを測る」と
//! いう遅くて非決定的な部分は、すべてこの trait の内側に閉じ込める。
//! 外側（[`crate::marks`]）は決定論的な純粋計算だけになる。
//!
//! **この crate 内の**実装は [`FixtureProvider`] のみである。akapen 側には
//! 外部コマンドへ委譲する `CommandProvider`（`--semantic-cmd`）があり、
//! Jev はそのコマンドの先に繋がる。Jev 自体を呼ぶ実装はまだ無い。

use std::path::Path;

use crate::Result;
use crate::document::SemanticDocument;

/// source テキストから [`SemanticDocument`] を得る手段。
///
/// Jev を直に叩く provider を書くならこの trait を実装して差し替える
/// （現状は akapen の `CommandProvider` 越しに外部スクリプトが担う）。
/// そのため
///
/// - 入力は source テキストそのもの（Document Profile も summary tree も
///   前段に置かない。設計書の「現在の文書そのものを見て判断させる」）
/// - 出力は [`SemanticDocument`] だけ（表示に関する語彙を一切含まない）
/// - 失敗しうる（ネットワーク・タイムアウト・不正な応答）
///
/// という形にしてある。`&self` なので `Box<dyn Provider>` として保持でき、
/// キャッシュや debounce、rate limit は実装側が内部に持てばよい。
pub trait Provider {
    /// 現在の文書を解析して semantic annotation を返す。
    fn analyze(&self, source: &str) -> Result<SemanticDocument>;
}

/// あらかじめ用意した JSON を返すだけの provider。
///
/// Jev を繋ぐ前に [`crate::marks`] とクライアント側の描画を開発・テストする
/// ための足場。[`Provider::analyze`] は**渡された source を見ない** —
/// fixture の範囲は fixture 作成時の source に対するものなので、別の
/// テキストを渡しても中身は変わらない。
#[derive(Clone, Debug)]
pub struct FixtureProvider {
    document: SemanticDocument,
}

impl FixtureProvider {
    /// JSON 文字列から読み込む。[`SemanticDocument::validate`] を通す。
    pub fn from_json(json: &str) -> Result<Self> {
        let document: SemanticDocument = serde_json::from_str(json)?;
        document.validate()?;
        Ok(Self { document })
    }

    /// JSON ファイルから読み込む。
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        Self::from_json(&json)
    }

    /// 検証済みの文書から直接作る。
    pub fn from_document(document: SemanticDocument) -> Result<Self> {
        document.validate()?;
        Ok(Self { document })
    }

    /// 保持している文書。
    pub fn document(&self) -> &SemanticDocument {
        &self.document
    }
}

impl Provider for FixtureProvider {
    fn analyze(&self, _source: &str) -> Result<SemanticDocument> {
        Ok(self.document.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomKind;
    use crate::error::Error;

    const JSON: &str = r#"{
      "atoms": [{ "range": { "start": 0, "end": 12 }, "kind": "heading" }],
      "units": [{ "id": "u1", "atoms": [0], "score": 0.94 }]
    }"#;

    #[test]
    fn fixture_provider_reads_json_and_ignores_the_source() {
        let provider = FixtureProvider::from_json(JSON).unwrap();
        let doc = provider.analyze("まったく別のテキスト").unwrap();
        assert_eq!(doc.atoms[0].kind, AtomKind::Heading);
        assert_eq!(doc.units[0].score, Some(0.94));
        assert_eq!(&doc, provider.document());
    }

    #[test]
    fn a_unit_may_carry_no_judgement_at_all() {
        let json = r#"{
          "atoms": [{ "range": { "start": 0, "end": 3 }, "kind": "sentence" }],
          "units": [{ "id": "u1", "atoms": [0] }]
        }"#;
        let doc = FixtureProvider::from_json(json)
            .unwrap()
            .analyze("")
            .unwrap();
        assert_eq!(doc.units[0].score, None);
        assert_eq!(doc.units[0].core_atoms, None);
    }

    #[test]
    fn broken_fixtures_are_rejected_at_load_time() {
        let json = r#"{
          "atoms": [],
          "units": [{ "id": "u1", "atoms": [4] }]
        }"#;
        assert!(matches!(
            FixtureProvider::from_json(json),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            FixtureProvider::from_json("{ not json"),
            Err(Error::Json(_))
        ));
        assert!(matches!(
            FixtureProvider::from_path("/存在しない/fixture.json"),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn a_provider_can_be_held_as_a_trait_object() {
        // 将来の Jev provider を実行時に差し替えられること。
        let provider: Box<dyn Provider> = Box::new(FixtureProvider::from_json(JSON).unwrap());
        assert_eq!(provider.analyze("").unwrap().units.len(), 1);
    }
}
