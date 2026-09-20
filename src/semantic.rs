//! Semantic Reading Layer をこのクライアントへ繋ぐ継ぎ目
//! （`docs/semantic-reading-layer.md`）。
//!
//! `semantic-reading` crate は「どの source range がどの semantic state か」
//! までを決め、色・背景・modifier の決定はクライアント側の責務だと言っている。
//! この module がその境界そのもので、依存は **akapen → crate の一方向だけ**
//! である（crate からは akapen も ratatui も見えない）。
//!
//! ```text
//! Provider::analyze  ->  SemanticDocument      遅い / 非決定的 / 文書が変わったときだけ
//! policy::decorate   ->  Vec<(range, state)>   速い / 決定論的 / Budget を動かすたび
//! decoration_kind    ->  Vec<Decoration>       ここが akapen の語彙への変換
//! ```
//!
//! # App が provider を叩くのは 1 箇所
//!
//! akapen が [`App::semantic_provider`] へ `analyze` を投げるのは
//! [`App::reanalyze_semantics`] だけで、そこは**文書が入れ替わったとき**に
//! しか呼ばれない（起動 / reload / タイムマシン / ファイル切替）。
//!
//! ```text
//! grep -rn 'semantic_provider' src/
//! ```
//!
//! で全部出る — フィールドを `.analyze` で触っているのは
//! `reanalyze_semantics` の 1 行だけで、残りは宣言・初期化・代入と
//! [`App::semantic_enabled`] の `is_some()` である。[`DigestChecked`] の
//! `analyze` も inner へ委譲するが、それは Provider チェーンの**内側**で
//! あって App からは 1 回の呼び出しに見える。
//!
//! Budget を動かす [`App::nudge_reading_budget`] からは
//! [`decorations_for`] にしか到達せず、その中身は `policy::decorate` の
//! 呼び出し 1 本である。
//!
//! > Budget 変更では Jev を呼ばない
//!
//! を、コメントではなく呼び出しグラフで満たしている。
//!
//! [`App::semantic_provider`]: crate::app::App::semantic_provider
//! [`App::semantic_enabled`]: crate::app::App::semantic_enabled
//! [`App::reanalyze_semantics`]: crate::app::App::reanalyze_semantics
//! [`App::nudge_reading_budget`]: crate::app::App::nudge_reading_budget

use std::path::Path;

use anyhow::{Context, Result};
use semantic_reading::{
    DisplayState, Error as SemanticError, FixtureProvider, Provider, SemanticDocument, policy,
};
use sha2::{Digest, Sha256};

use crate::decoration::{Decoration, DecorationKind};

/// Reading Budget の下限・上限・既定値。刻みは 1 % で、設計書どおり
/// 「43 / 42 / 41 で表示が変わらなくても問題ない」粒度である。
pub(crate) const MIN_BUDGET: u8 = policy::MIN_BUDGET;
pub(crate) const MAX_BUDGET: u8 = policy::MAX_BUDGET;
/// 既定は 100 %。全文を表示したまま ESSENTIAL に薄いマーカーを重ねる、
/// 設計書「最初のデモ」の初期状態。
pub(crate) const DEFAULT_BUDGET: u8 = MAX_BUDGET;

/// source テキストの SHA-256（小文字 hex 64 桁）。
pub(crate) fn source_digest(source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// fixture が名乗っている素性と、実際に開いている文書が一致することを
/// 確かめてから通す [`Provider`]。
///
/// [`FixtureProvider`] は**渡された source を見ない** — range は fixture を
/// 作ったときの文書に対するものなので、別の文書に当てると無意味な位置を
/// 装飾する。[`crate::decoration::sanitize`] は panic を防ぐが、内容のズレは
/// 防げない（文字境界に載ってしまう嘘の range は通ってしまう）。
///
/// 検査を Provider の**内側**に置いたのは、拒否が「継ぎ目で起きる」形に
/// なるからである。将来 Jev provider が刺さったとき、それは source を見て
/// 答える以上ダイジェストを名乗る必要がなく、同じ `analyze` の契約のまま
/// 素通りする。
pub(crate) struct DigestChecked<P> {
    inner: P,
}

impl<P> DigestChecked<P> {
    pub(crate) fn new(inner: P) -> Self {
        Self { inner }
    }
}

impl<P: Provider> Provider for DigestChecked<P> {
    fn analyze(&self, source: &str) -> semantic_reading::Result<SemanticDocument> {
        let document = self.inner.analyze(source)?;
        let Some(expected) = document.source_digest() else {
            // 素性を名乗っていない fixture は照合しない（後方互換）。
            return Ok(document);
        };
        let actual = source_digest(source);
        if expected.eq_ignore_ascii_case(&actual) {
            Ok(document)
        } else {
            Err(SemanticError::Invalid(format!(
                "この fixture は別の文書のものです (source_sha256 {} ≠ 開いている文書 {})",
                &expected[..12.min(expected.len())],
                &actual[..12]
            )))
        }
    }
}

/// `--semantic <fixture.json>` を読み込む。
///
/// JSON が壊れている / 文書として辻褄が合わない場合はここで失敗する
/// （TUI に入る前に loud に落とすため、起動経路から呼ぶこと）。
pub(crate) fn load_fixture(path: &Path) -> Result<Box<dyn Provider>> {
    let provider = FixtureProvider::from_path(path)
        .with_context(|| format!("--semantic {}", path.display()))?;
    Ok(Box::new(DigestChecked::new(provider)))
}

/// 引数から provider を 1 つ決める。**provider の選択はここだけ**。
///
/// `App` が持つのは [`Box<dyn Provider>`] であって具体型ではないので、
/// 供給源を増やすのはこの `match` に腕を 1 本足す作業になる。予定されて
/// いるのは `--semantic-cmd '<コマンド>'`（文書を stdin へ渡し、
/// `SemanticDocument` の JSON を stdout から受け取る外部コマンド）で、
/// akapen 本体に HTTP クライアントも非同期ランタイムも入れずに Jev を
/// 繋ぐための口である。その `CommandProvider` もここへ刺さる。
///
/// `Ok(None)` は「この層は存在しない」。そのとき akapen は改修前と
/// **完全に同じ**挙動になる — READ の読み出しも、Budget のキーも、
/// `?` ヘルプの行も、警告の 1 つも出ない（[`App::semantic_enabled`]）。
///
/// [`App::semantic_enabled`]: crate::app::App::semantic_enabled
pub(crate) fn provider_from_config(config: &crate::config::Config) -> Result<Option<Box<dyn Provider>>> {
    match config.semantic.as_deref() {
        Some(path) => Ok(Some(load_fixture(path)?)),
        None => Ok(None),
    }
}

/// `DisplayState` から akapen の装飾語彙への対応。
///
/// NORMAL は「装飾しない」であって「NORMAL という装飾」ではない — 何も
/// 返さないので、その range は syntax highlight のまま 1 バイトも触られない。
pub(crate) fn decoration_kind(state: DisplayState) -> Option<DecorationKind> {
    match state {
        DisplayState::Marked => Some(DecorationKind::SemanticMark),
        DisplayState::Dim => Some(DecorationKind::Dim),
        DisplayState::Normal => None,
    }
}

/// 現在の Budget での decoration 列。
///
/// **この関数から `Provider::analyze` へ到達する経路は無い。** Budget を
/// 1 % 動かすたびに走るのはここだけで、`policy::decorate` は純粋関数である。
pub(crate) fn decorations_for(document: &SemanticDocument, budget: u8) -> Vec<Decoration> {
    policy::decorate(document, budget)
        .into_iter()
        .filter_map(|(range, state)| {
            decoration_kind(state).map(|kind| Decoration { range, kind })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantic_reading::{Atom, AtomKind, AtomIndex, ReadingTier, Relation, SemanticUnit};

    const DEMO_MD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/semantic/demo.md");
    const DEMO_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/semantic/demo.json");

    fn demo() -> (String, SemanticDocument) {
        let source = std::fs::read_to_string(DEMO_MD).unwrap();
        let document = load_fixture(Path::new(DEMO_JSON))
            .expect("demo fixture を読めること")
            .analyze(&source)
            .expect("demo.md に対して通ること");
        (source, document)
    }

    /// state の 3 値のうち、装飾を持つのは 2 つだけ。
    #[test]
    fn normal_is_the_absence_of_decoration() {
        assert_eq!(
            decoration_kind(DisplayState::Marked),
            Some(DecorationKind::SemanticMark)
        );
        assert_eq!(decoration_kind(DisplayState::Dim), Some(DecorationKind::Dim));
        assert_eq!(decoration_kind(DisplayState::Normal), None);
    }

    #[test]
    fn the_digest_is_lowercase_hex_of_the_source() {
        // 空文字列の SHA-256（既知値）。
        assert_eq!(
            source_digest(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(source_digest("あ").len(), 64);
    }

    /// 手順3の落とし穴: fixture は渡された source を見ないので、別の文書に
    /// 当てると無意味な位置を装飾する。`source_sha256` がそれを止める。
    #[test]
    fn a_fixture_is_refused_on_a_different_document() {
        let source = std::fs::read_to_string(DEMO_MD).unwrap();
        let provider = load_fixture(Path::new(DEMO_JSON)).unwrap();
        assert!(provider.analyze(&source).is_ok());

        let err = provider
            .analyze("# 別の文書\n\nこれは demo.md ではない。\n")
            .expect_err("別の文書は拒否されること");
        let message = err.to_string();
        assert!(message.contains("別の文書のものです"), "{message}");
        // 期待値と実際の両方を名乗る（どちらを直せばよいか分かるように）。
        assert!(message.contains("07090d9efc97"), "{message}");
    }

    /// `source_sha256` を持たない fixture は照合しない（後方互換）。
    #[test]
    fn a_fixture_without_a_digest_is_not_checked() {
        let document = SemanticDocument::new(
            vec![Atom::new(0..3, AtomKind::Sentence)],
            vec![SemanticUnit::new("u1", [AtomIndex(0)], ReadingTier::Essential)],
        );
        assert!(document.source_digest().is_none());
        let provider = DigestChecked::new(FixtureProvider::from_document(document).unwrap());
        assert!(provider.analyze("まったく別のテキスト").is_ok());
    }

    /// 手順4の要求そのもの: 各 Atom の range が、期待するテキストに
    /// スライスされること。byte range は生成スクリプトが出しているので、
    /// これは「スクリプトの出力が demo.md と噛み合っている」の検査である。
    #[test]
    fn every_demo_atom_slices_to_the_text_it_claims() {
        let (source, document) = demo();
        assert_eq!(document.atoms.len(), 27);
        assert_eq!(document.units.len(), 13);

        // (添字, 期待するスライス) — 全 27 個。改行 1 文字で終わるものは、
        // 段落内の soft line break か code block の末尾を覆っている
        // （生成スクリプトの「末尾に足すバイト数」を参照）。
        let expected = [
            "通知基盤リニューアル設計メモ",
            "この文書は社内通知基盤の作り直しについての設計メモである。",
            "現状の課題、採用する方式、制約、移行手順の順に記す。",
            "結論",
            "採用する方式は差分配信である。",
            "詳細は付録にまとめた。",
            "現行の一括配信は購読者数に比例して遅くなるため、配信対象を差分だけに絞る。",
            "背景",
            "通知基盤は導入から四年が経ち、購読者数は当初の想定を大きく超えた。\n",
            "初期の設計では全購読者へ毎回全件を配信していた。\n",
            "当時は購読者が数百人規模だったので、全件配信でも問題にならなかった。",
            "制約",
            "既存の購読者向け API は変更しない。",
            "移行期間中も配信の取りこぼしを出さない。",
            "影響範囲",
            "配信遅延の監視ダッシュボードと、管理画面の配信履歴の二箇所が影響を受ける。\n",
            "どちらも読み取り側なので、配信そのものの整合性は今回の変更では変わらない。",
            "補足",
            "つまり、配信対象を差分だけに絞るということである。",
            "念のため繰り返しておく。",
            "全件配信は購読者数が増えるほど配信時間が線形に伸びる。",
            "数値の目安",
            "購読者一万人で約二秒、十万人で約二十秒かかっている。",
            "実測は社内計測環境のものである。",
            "delivery: incremental\n",
            "なお、差分の算出そのものは既存の差分計算モジュールをほぼそのまま使える。\n",
            "細かい差異は実装時に吸収する予定なので、ここでは触れない。",
        ];
        assert_eq!(expected.len(), document.atoms.len());
        for (i, (atom, want)) in document.atoms.iter().zip(expected).enumerate() {
            assert_eq!(&source[atom.range.clone()], want, "atom {i}");
        }

        // Atom は文書順に、重ならずに並んでいる。
        for pair in document.atoms.windows(2) {
            assert!(
                pair[0].range.end <= pair[1].range.start,
                "{:?} と {:?} が重なっています",
                pair[0].range,
                pair[1].range
            );
        }
        assert!(document.validate().is_ok());
    }

    /// demo.json が demo.md の今の中身を指していること。生成スクリプトを
    /// 走らせ忘れたまま demo.md を触ると、ここで落ちる。
    #[test]
    fn the_demo_fixture_names_the_current_demo_md() {
        let source = std::fs::read_to_string(DEMO_MD).unwrap();
        let document: SemanticDocument =
            serde_json::from_str(&std::fs::read_to_string(DEMO_JSON).unwrap()).unwrap();
        assert_eq!(
            document.source_digest(),
            Some(source_digest(&source).as_str()),
            "demo.md を編集したら examples/semantic/build-demo-json.py を走らせ直すこと"
        );
    }

    /// 設計書が求める 4 段階すべてと、REDUNDANT_WITH を使っていること。
    #[test]
    fn the_demo_fixture_uses_all_four_tiers_and_a_redundancy() {
        let (_, document) = demo();
        for tier in [
            ReadingTier::Essential,
            ReadingTier::Supporting,
            ReadingTier::Context,
            ReadingTier::Detail,
        ] {
            assert!(
                document.units.iter().any(|u| u.reading_tier == tier),
                "{tier:?} を使う Unit が無い"
            );
        }
        let redundant: Vec<_> = document.units.iter().filter(|u| u.is_redundant()).collect();
        assert_eq!(redundant.len(), 1, "REDUNDANT な Unit は 1 つ");
        assert_eq!(
            redundant[0].relations,
            vec![Relation::RedundantWith("u3".into())],
            "補足は結論の言い換え"
        );
    }

    /// 設計書「最初のデモ」の初期状態: 全文を表示し、ESSENTIAL を薄く
    /// マーキングする。DIM は 1 つも無い。
    #[test]
    fn budget_100_marks_the_essentials_and_dims_nothing() {
        let (_, document) = demo();
        let states = policy::decorate(&document, 100);

        let essential_atoms: Vec<usize> = document
            .units
            .iter()
            .filter(|u| u.reading_tier == ReadingTier::Essential && !u.is_redundant())
            .flat_map(|u| u.atoms.iter().map(|a| a.0))
            .collect();
        assert!(!essential_atoms.is_empty());

        for (i, (_, state)) in states.iter().enumerate() {
            let want = if essential_atoms.contains(&i) {
                DisplayState::Marked
            } else {
                DisplayState::Normal
            };
            assert_eq!(*state, want, "atom {i}");
        }
        assert_eq!(
            states.iter().filter(|(_, s)| *s == DisplayState::Dim).count(),
            0,
            "budget 100 で DIM は 0 個"
        );
    }

    /// Budget を下げると DETAIL と REDUNDANT が先に落ちる。閾値は
    /// 生成スクリプトが出す累積表から取っている（勘で選ばない）。
    ///
    /// | 順 | unit | 実効 Tier | 残る下限 budget |
    /// | -- | ---- | --------- | --------------- |
    /// |  9 | u9   | CONTEXT※ | 74              |
    /// | 10 | u4   | DETAIL    | 76              |
    /// | 11 | u10  | DETAIL    | 78              |
    /// | 12 | u12  | DETAIL    | 88              |
    /// | 13 | u13  | DETAIL    | 100             |
    #[test]
    fn lowering_the_budget_dims_the_details_and_the_redundancy_first() {
        let (_, document) = demo();
        let dim_units = |budget: u8| -> Vec<String> {
            let states = policy::decorate(&document, budget);
            document
                .units
                .iter()
                .filter(|unit| {
                    unit.atoms
                        .iter()
                        .all(|a| states[a.0].1 == DisplayState::Dim)
                })
                .map(|unit| unit.id.to_string())
                .collect()
        };

        // 75 %: DETAIL 4 つだけが落ちる。REDUNDANT はまだ残る。
        assert_eq!(dim_units(75), ["u4", "u10", "u12", "u13"]);
        // 73 %: REDUNDANT な u9 も落ちる。CONTEXT（u6/u8/u11）はまだ全部残る。
        assert_eq!(dim_units(73), ["u4", "u9", "u10", "u12", "u13"]);
        // 30 %: ESSENTIAL 3 つと、いちばん短い SUPPORTING だけが残る。
        assert_eq!(
            dim_units(30),
            ["u2", "u4", "u6", "u8", "u9", "u10", "u11", "u12", "u13"]
        );
        // どの Budget でも ESSENTIAL は MARKED のまま（Budget に依存しない）。
        let marked = |budget: u8| -> Vec<usize> {
            policy::decorate(&document, budget)
                .into_iter()
                .enumerate()
                .filter(|(_, (_, s))| *s == DisplayState::Marked)
                .map(|(i, _)| i)
                .collect()
        };
        // ESSENTIAL の 3 Unit（タイトル / 結論 / 制約）が残っているあいだ、
        // MARKED は Budget に依存しない。
        for budget in [100, 75, 73, 30, 14] {
            assert_eq!(marked(budget), [0, 3, 4, 11, 12, 13], "budget {budget}");
        }
        // 残らなければ MARKED でもなくなる。budget 1 % では、Budget が
        // どれだけ小さくても必ず残る先頭の 1 Unit（タイトル）だけになる。
        assert_eq!(marked(1), [0]);
    }

    /// 同じ source 行の中で状態が切り替わること — マイルストーンの
    /// 「しかも同じ行の途中でも切り替わる」を range のレベルで固定する。
    /// セルまで通した検証は `state_tests.rs` にある。
    #[test]
    fn two_states_meet_inside_one_source_line() {
        let (source, document) = demo();
        // demo.md の「採用する方式は差分配信である。詳細は付録にまとめた。」
        let line_start = source.find("採用する方式は差分配信である。").unwrap();
        let line_end = line_start + source[line_start..].find('\n').unwrap();

        for (budget, want) in [
            (100, [DisplayState::Marked, DisplayState::Normal]),
            (75, [DisplayState::Marked, DisplayState::Dim]),
            (30, [DisplayState::Marked, DisplayState::Dim]),
        ] {
            let inside: Vec<(std::ops::Range<usize>, DisplayState)> =
                policy::decorate(&document, budget)
                    .into_iter()
                    .filter(|(range, _)| range.start >= line_start && range.end <= line_end)
                    .collect();
            assert_eq!(inside.len(), 2, "budget {budget}");
            assert_eq!(
                [inside[0].1, inside[1].1],
                want,
                "budget {budget} で 1 行が {inside:?}"
            );
            // 2 つは隙間なく隣り合い、行を覆い尽くす。
            assert_eq!(inside[0].0.start, line_start);
            assert_eq!(inside[0].0.end, inside[1].0.start);
            assert_eq!(inside[1].0.end, line_end);
            assert_eq!(&source[inside[0].0.clone()], "採用する方式は差分配信である。");
            assert_eq!(&source[inside[1].0.clone()], "詳細は付録にまとめた。");
        }
    }

    /// decoration へ変換する段で NORMAL が消えること。
    #[test]
    fn decorations_carry_only_the_marked_and_the_dimmed() {
        let (_, document) = demo();
        let at_100 = decorations_for(&document, 100);
        assert_eq!(at_100.len(), 6, "ESSENTIAL の Atom 6 個だけ");
        assert!(at_100.iter().all(|d| d.kind == DecorationKind::SemanticMark));

        let at_30 = decorations_for(&document, 30);
        assert!(at_30.iter().any(|d| d.kind == DecorationKind::Dim));
        assert!(at_30.iter().any(|d| d.kind == DecorationKind::SemanticMark));
        // 範囲外の budget は丸められる（0 も 200 も端に寄る）。
        assert_eq!(decorations_for(&document, 0), decorations_for(&document, 1));
        assert_eq!(
            decorations_for(&document, 200),
            decorations_for(&document, 100)
        );
    }
}
