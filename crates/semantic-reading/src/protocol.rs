//! 外部コマンドとの wire protocol — **Atom を渡して Unit を受け取る**。
//!
//! 意味判断（Atom 間の境界・Reading Tier・redundancy）だけを外へ出し、
//! 位置の管理はこちら側に残すための継ぎ目である。設計書「Jev に判断
//! させないもの」に `Atom生成` と `source position管理` が並んでいるのが
//! そのまま形になっている。
//!
//! ```text
//! akapen  --(version, source, atoms[])-->  外部コマンド
//! akapen  <--(version, units[])----------  外部コマンド
//! ```
//!
//! # コマンドは range を返さない
//!
//! 返ってくるのは [`Atom`] の**添字だけ**である。[`SemanticDocument`] は
//! こちら側の [`crate::atomize`] が出した Atom 列と、返ってきた Unit の
//! 構造から組み立てられる。
//!
//! したがって**不正な range が原理的に生まれない**。外部コマンドが壊れた
//! 位置を返して文書の違う場所を装飾する、という事故が起きえない — 位置を
//! 一度も外へ渡して受け取り直していないからである。
//!
//! この性質は [`crate::FixtureProvider`] には無い。fixture の range は
//! fixture を作ったときの文書に対するもので、別の文書に当てれば無意味な
//! 位置を指す。だから fixture 経路には `source_sha256` の照合が要る
//! （akapen 側の `DigestChecked`）。**この経路には要らない。**
//!
//! # 一括で返させる
//!
//! 設計書は Jev への問いを 2 段階（境界判定 → Tier 付け）に分けているが、
//! ワイヤ上は 1 往復（atoms in, units out）にしてある。外部コマンドが
//! 内部で LLM を 2 回呼ぶのは自由で、こちらのプロトコルが LLM 側の段取りを
//! 規定すべきではない。継ぎ目は「Atom を渡して Unit を受け取る」だけ。
//! 将来キャッシュのために段階を分ける必要が出たら `stage` を足せる
//! （未知のフィールドは拒否していない。[`AnalyzeResponse`] 参照）。
//!
//! # 検証は受け取る側の責務
//!
//! [`AnalyzeResponse::into_document`] は全項目を検査し、**1 つでも失敗
//! したらレスポンス全体を捨てる**。部分適用は何もしないより悪い —
//! 「半分だけ意味が付いた文書」は、意味が付いていない文書より誤読を
//! 誘う。
//!
//! # 一周
//!
//! ```
//! use semantic_reading::{AnalyzeRequest, AnalyzeResponse, atomize};
//!
//! let source = "# 見出し\n\n本文です。\n";
//!
//! // 1. こちらで Atom へ割り、それを渡す。
//! let atoms = atomize(source);
//! let request = AnalyzeRequest::new(source, &atoms).to_json()?;
//! assert!(request.contains(r#""index":0"#));
//!
//! // 2. コマンドは index だけを返す（range は返さない）。
//! let answer = r#"{"version":1,"units":[
//!   {"id":"u1","atoms":[0],"reading_tier":"essential"},
//!   {"id":"u2","atoms":[1],"reading_tier":"detail"}
//! ]}"#;
//!
//! // 3. 位置はこちらの Atom 列のまま文書になる。
//! let document = AnalyzeResponse::from_json(answer)?.into_document(atoms)?;
//! assert_eq!(&source[document.atoms[0].range.clone()], "# 見出し");
//! # Ok::<(), semantic_reading::Error>(())
//! ```

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::atom::{Atom, AtomIndex, AtomKind};
use crate::document::SemanticDocument;
use crate::error::Error;
use crate::unit::SemanticUnit;

/// このプロトコルの版。要求にも応答にも載り、**一致しなければ応答全体を
/// 捨てる**。
///
/// 互換のために緩めない: 版が違うということは、少なくとも片方が相手の
/// 知らない意味でフィールドを読み書きしているということで、そのまま
/// 通せば「それらしく見えるが間違っている注釈」になる。
pub const VERSION: u32 = 1;

/// 外部コマンドの stdin へ渡す要求。
///
/// `source` を丸ごと入れているのは設計書「全文を Context として扱う」に
/// よる — 判断は前段の要約ではなく現在の文書そのものを見て行わせる。
/// `atoms` は同じ文書を機械的に割った位置台帳で、`index` がそのまま
/// [`AnalyzeResponse`] で参照される添字になる。
#[derive(Clone, Debug, Serialize)]
pub struct AnalyzeRequest<'a> {
    /// プロトコルの版（[`VERSION`]）。
    pub version: u32,
    /// 文書全文。
    pub source: &'a str,
    /// 文書を割った Atom 列。`index` の昇順に並ぶ。
    pub atoms: Vec<RequestAtom<'a>>,
}

/// 要求に載る Atom 1 つ。
///
/// `range` と `text` はどちらも**参考情報**である。コマンドが返すのは
/// `index` だけなので、ここを読み違えても位置が壊れることはない。
/// `text` を入れてあるのは、コマンド側が source を自分で切り出さずに
/// 済むようにするためで（切り出しはバイト位置の再実装になる）、`range`
/// はログやデバッグのため。
#[derive(Clone, Debug, Serialize)]
pub struct RequestAtom<'a> {
    /// `atoms` 配列中の位置。応答はこの値で Atom を指す。
    pub index: usize,
    /// 構文上の種別。
    pub kind: AtomKind,
    /// source のバイト範囲（`{"start":…,"end":…}`）。
    pub range: Range<usize>,
    /// `source[range]` そのもの。
    pub text: &'a str,
}

impl<'a> AnalyzeRequest<'a> {
    /// `source` と、それを [`crate::atomize`] で割った `atoms` から要求を
    /// 組み立てる。
    ///
    /// `atoms` は `source` から作られたものでなければならない（range で
    /// `source` を切るため）。範囲が `source` の外に出ている Atom は
    /// `text` が空になるだけで panic しない。
    pub fn new(source: &'a str, atoms: &[Atom]) -> Self {
        Self {
            version: VERSION,
            source,
            atoms: atoms
                .iter()
                .enumerate()
                .map(|(index, atom)| RequestAtom {
                    index,
                    kind: atom.kind,
                    range: atom.range.clone(),
                    text: source.get(atom.range.clone()).unwrap_or(""),
                })
                .collect(),
        }
    }

    /// 要求を JSON 1 行にする。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// 外部コマンドの stdout から受け取る応答。
///
/// `units` の要素は [`SemanticUnit`] そのままである — `id` / `atoms` /
/// `reading_tier` / `relations` の 4 つで、wire 形と内部表現が 1 対 1 に
/// 対応する。別の DTO を挟まないのは、挟めば両者がずれうるからで、
/// ずれない形にしてあれば「wire では通るが内部では表現できない」値が
/// 存在しなくなる。
///
/// 未知のフィールドは**拒否しない**。将来 `stage` のような追加が
/// 入ったときに古い akapen が動かなくなるのは割に合わないし、未知の
/// フィールドは読まなければ影響が無い（意味を取り違える版ずれの方は
/// [`VERSION`] が受け持つ）。
#[derive(Clone, Debug, Deserialize)]
pub struct AnalyzeResponse {
    /// プロトコルの版。[`VERSION`] と一致しなければ応答全体を捨てる。
    pub version: u32,
    /// 外部コマンドが知覚した意味的まとまり。
    #[serde(default)]
    pub units: Vec<SemanticUnit>,
}

impl AnalyzeResponse {
    /// コマンドの stdout を解析する。JSON として読めなければここで失敗する。
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// 応答と**こちら側の** Atom 列から [`SemanticDocument`] を組み立てる。
    ///
    /// `atoms` は要求に載せたものと同じ列でなければならない（応答の添字が
    /// それを指しているため）。
    ///
    /// # 検査するもの
    ///
    /// | 条件 | 見る場所 |
    /// | ---- | -------- |
    /// | `version` が一致する | ここ |
    /// | `reading_tier` が 4 種のいずれか | serde（[`Self::from_json`]） |
    /// | `relations` が既知の関係である | serde（[`Self::from_json`]） |
    /// | atom 添字が `atoms.len()` 未満 | [`SemanticDocument::validate`] |
    /// | unit の id が重複しない | [`SemanticDocument::validate`] |
    /// | relation の参照先が実在し、自分自身でない | [`SemanticDocument::validate`] |
    ///
    /// **1 つでも失敗したら `Err` を返し、応答は丸ごと捨てられる。**
    /// 通った Unit だけ適用する、はしない。
    ///
    /// # 同じ Atom を複数の Unit が主張したとき — 先勝ち
    ///
    /// エラーにはせず、**先に現れた Unit が取り**、後の Unit からはその
    /// 添字を落とす。Atom は「安全に位置を指定できる機械的単位」であって
    /// 意味単位ではないので、同じ位置に 2 つの意味判断が乗るのは
    /// コマンド側の取りこぼしであり、文書が壊れているわけではない。
    /// 先勝ちにするのは、
    ///
    /// - [`crate::policy`] の attention コストが二重計上にならない
    /// - 文書順に読んだときの最初の判断が残る（後から上書きされない）
    ///
    /// の 2 点による。**添字を全部落とされて空になった Unit は残す** —
    /// [`crate::policy::decorate`] はコスト 0・装飾 0 として素通りさせる
    /// だけだし、他の Unit の `redundant_with` の参照先として生きている
    /// 可能性があるからである（消すと参照が宙に浮き、応答全体が捨てられる
    /// ことになる）。
    pub fn into_document(self, atoms: Vec<Atom>) -> Result<SemanticDocument> {
        if self.version != VERSION {
            return Err(Error::Invalid(format!(
                "protocol version {} は解釈できません（このビルドは {VERSION}）",
                self.version
            )));
        }
        let mut units = self.units;
        let mut claimed = vec![false; atoms.len()];
        for unit in &mut units {
            unit.atoms.retain(|&AtomIndex(index)| match claimed.get_mut(index) {
                // 先勝ち: すでに誰かが取っている Atom は落とす。
                Some(taken) if *taken => false,
                Some(taken) => {
                    *taken = true;
                    true
                }
                // 範囲外。落とさずに残し、validate に弾かせる — ここで
                // 黙って捨てると「不正な添字を送っても通る」ことになる。
                None => true,
            });
        }
        let document = SemanticDocument::new(atoms, units);
        document.validate()?;
        Ok(document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomize::atomize;
    use crate::unit::{ReadingTier, Relation, UnitId};

    const SOURCE: &str = "## 見出し\n\n本文です。二文目です。\n";

    fn response(json: &str) -> Result<SemanticDocument> {
        AnalyzeResponse::from_json(json)?.into_document(atomize(SOURCE))
    }

    #[test]
    fn a_request_carries_the_source_the_atoms_and_their_text() {
        let atoms = atomize(SOURCE);
        let request = AnalyzeRequest::new(SOURCE, &atoms);
        assert_eq!(request.version, VERSION);
        assert_eq!(request.source, SOURCE);
        assert_eq!(request.atoms.len(), 3);
        assert_eq!(request.atoms[0].index, 0);
        assert_eq!(request.atoms[0].kind, AtomKind::Heading);
        assert_eq!(request.atoms[0].text, "## 見出し");
        assert_eq!(request.atoms[2].text, "二文目です。");
        // text は range で source を切ったものと必ず一致する。
        for atom in &request.atoms {
            assert_eq!(&SOURCE[atom.range.clone()], atom.text);
        }
    }

    #[test]
    fn the_request_is_the_json_shape_the_protocol_documents() {
        let atoms = atomize("# A\n");
        let json = AnalyzeRequest::new("# A\n", &atoms).to_json().unwrap();
        assert_eq!(
            json,
            r##"{"version":1,"source":"# A\n","atoms":[{"index":0,"kind":"heading","range":{"start":0,"end":3},"text":"# A"}]}"##
        );
    }

    #[test]
    fn a_well_formed_response_becomes_a_document_over_our_own_atoms() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","relations":[]},
                {"id":"u2","atoms":[1,2],"reading_tier":"detail","relations":[{"redundant_with":"u1"}]}
            ]}"#,
        )
        .unwrap();
        // range は atomize の出力そのもの — コマンドは位置を返していない。
        assert_eq!(document.atoms, atomize(SOURCE));
        assert_eq!(document.units.len(), 2);
        assert_eq!(document.units[1].atoms, [AtomIndex(1), AtomIndex(2)]);
        assert_eq!(
            document.units[1].redundant_with(),
            Some(&UnitId::from("u1"))
        );
        assert_eq!(document.units[0].reading_tier, ReadingTier::Essential);
    }

    #[test]
    fn relations_may_be_omitted() {
        let document =
            response(r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"context"}]}"#)
                .unwrap();
        assert!(document.units[0].relations.is_empty());
    }

    #[test]
    fn an_empty_unit_list_is_a_document_with_no_judgement() {
        // 「どの Atom にも意味を付けなかった」は正当な答え — 全部 NORMAL に
        // なるだけで、エラーではない。
        let document = response(r#"{"version":1,"units":[]}"#).unwrap();
        assert_eq!(document.atoms.len(), 3);
        assert!(document.units.is_empty());
    }

    /// 応答は全部通るか全部捨てるかのどちらかで、部分適用は無い。
    #[test]
    fn one_bad_field_throws_the_whole_response_away() {
        // 範囲外の atom 添字（2 つ目の unit が壊れていても 1 つ目は残らない）。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential"},
                {"id":"u2","atoms":[9],"reading_tier":"detail"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("範囲外の atom 9"), "{err}");

        // unit id の重複。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential"},
                {"id":"u1","atoms":[1],"reading_tier":"detail"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("重複"), "{err}");

        // 存在しない unit への relation。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","relations":[{"redundant_with":"u9"}]}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("未知"), "{err}");

        // 自分自身との重複。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","relations":[{"redundant_with":"u1"}]}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("自分自身"), "{err}");

        // 未知の tier — ここは serde が落とす。
        let err = response(
            r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"critical"}]}"#,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Json(_)), "{err}");

        // 未知の relation。
        let err = response(
            r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"detail","relations":[{"contradicts":"u2"}]}]}"#,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Json(_)), "{err}");

        // JSON ですらない。
        assert!(matches!(response("not json at all"), Err(Error::Json(_))));
    }

    #[test]
    fn a_version_mismatch_is_refused_before_anything_else_is_read() {
        let err = response(
            r#"{"version":2,"units":[{"id":"u1","atoms":[0],"reading_tier":"essential"}]}"#,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("version 2"), "{message}");
        assert!(message.contains('1'), "このビルドの版も名乗る: {message}");
        // 版が違えば、中身が完璧でも通らない。
        assert!(matches!(
            response(r#"{"version":0,"units":[]}"#),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn version_is_required_not_assumed() {
        // 版を名乗らない応答は「版 1 のつもり」ではなく不正。
        assert!(matches!(
            response(r#"{"units":[]}"#),
            Err(Error::Json(_))
        ));
    }

    /// 同じ Atom を 2 つの Unit が主張したら先勝ち。
    #[test]
    fn the_first_unit_to_claim_an_atom_keeps_it() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"first","atoms":[0,1],"reading_tier":"essential"},
                {"id":"second","atoms":[1,2],"reading_tier":"detail"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(document.units[0].atoms, [AtomIndex(0), AtomIndex(1)]);
        assert_eq!(
            document.units[1].atoms,
            [AtomIndex(2)],
            "1 は先に取られている"
        );
    }

    /// 全部取られて空になった Unit は消さない — relation の参照先として
    /// 生きている。
    #[test]
    fn a_unit_emptied_by_the_first_come_rule_survives_as_a_relation_target() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential"},
                {"id":"u2","atoms":[0],"reading_tier":"detail"},
                {"id":"u3","atoms":[1],"reading_tier":"detail","relations":[{"redundant_with":"u2"}]}
            ]}"#,
        )
        .unwrap();
        assert!(document.units[1].atoms.is_empty());
        assert_eq!(document.units.len(), 3, "空でも消さない");
        // 空の Unit があっても policy は素通りする。
        let states = crate::policy::decorate(&document, 100);
        assert_eq!(states.len(), document.atoms.len());
    }

    /// 同じ Unit が同じ Atom を 2 回並べても 1 回に潰れる。
    #[test]
    fn a_unit_repeating_an_atom_keeps_it_once() {
        let document = response(
            r#"{"version":1,"units":[{"id":"u1","atoms":[0,0,0],"reading_tier":"essential"}]}"#,
        )
        .unwrap();
        assert_eq!(document.units[0].atoms, [AtomIndex(0)]);
    }

    /// 先勝ちの dedup が、範囲外の添字を隠してしまわないこと。
    #[test]
    fn dedup_does_not_swallow_an_out_of_range_index() {
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential"},
                {"id":"u2","atoms":[0,42],"reading_tier":"detail"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("範囲外の atom 42"), "{err}");
    }

    /// 未知のフィールドは無視する（将来の `stage` などのため）。
    #[test]
    fn unknown_fields_ride_along_without_breaking_anything() {
        let document = response(
            r#"{"version":1,"stage":"boundaries","units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","note":"見出し"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(document.units.len(), 1);
    }

    /// 組み立てた文書に `source_sha256` は載らない — range はこちらの
    /// atomize が出したものなので、照合すべき「別の文書」が存在しない。
    #[test]
    fn the_assembled_document_names_no_digest_of_its_own() {
        let document = response(r#"{"version":1,"units":[]}"#).unwrap();
        assert!(document.source_digest().is_none());
    }

    /// relation の丸ごとの往復（要求 → 応答 → 文書）。
    #[test]
    fn a_relation_survives_the_round_trip_as_the_same_value() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential"},
                {"id":"u2","atoms":[1],"reading_tier":"supporting","relations":[{"redundant_with":"u1"}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            document.units[1].relations,
            vec![Relation::RedundantWith(UnitId::from("u1"))]
        );
        assert!(document.units[1].is_redundant());
        // Tier と redundancy は別軸のまま（設計書）。
        assert_eq!(document.units[1].reading_tier, ReadingTier::Supporting);
    }
}
