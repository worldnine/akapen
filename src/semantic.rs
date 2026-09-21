//! Semantic Reading Layer をこのクライアントへ繋ぐ継ぎ目
//! （`docs/design/semantic-reading-layer.md`）。
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
//! akapen が [`App::semantic_source`] へ `analyze` を投げるのは
//! [`App::reanalyze_semantics`] だけで、そこは**文書が入れ替わったとき**に
//! しか呼ばれない（起動 / reload / タイムマシン / ファイル切替）。
//!
//! ```text
//! grep -rn 'semantic_source' src/
//! ```
//!
//! で全部出る — フィールドを `.analyze` で触っているのは
//! `reanalyze_semantics` の 2 本の腕（同期・非同期）だけで、残りは宣言・
//! 初期化・代入と [`App::semantic_enabled`] の `is_some()` である。
//! [`DigestChecked`] の `analyze` も inner へ委譲するが、それは Provider
//! チェーンの**内側**であって App からは 1 回の呼び出しに見える。
//!
//! Budget を動かす [`App::nudge_reading_budget`] からは
//! [`decorations_for`] にしか到達せず、その中身は `policy::decorate` の
//! 呼び出し 1 本である。
//!
//! > Budget 変更では Jev を呼ばない
//!
//! を、コメントではなく呼び出しグラフで満たしている。
//!
//! # 供給源は 2 つ、違いは「いつ答えるか」だけ
//!
//! | フラグ | 供給源 | 呼び方 | range の出どころ |
//! | ------ | ------ | ------ | ---------------- |
//! | `--semantic <fixture.json>` | [`FixtureProvider`] | 同期 | fixture（だから [`DigestChecked`] で照合する） |
//! | `--semantic-cmd <コマンド>` | [`CommandProvider`] | 別スレッド | akapen の [`atomize`]（照合不要） |
//!
//! 外部コマンドはプロセス起動とネットワーク往復を挟むので、同期に呼ぶと
//! `event::poll` で回っているイベントループが止まる。だから
//! [`SemanticSource`] が型として 2 つを分けている — 「遅いかもしれない」は
//! [`Provider`] の中には隠せない（`analyze` の戻り値が「あとで」を
//! 表現できない）。
//!
//! [`App::semantic_source`]: crate::app::App::semantic_source
//! [`App::semantic_enabled`]: crate::app::App::semantic_enabled
//! [`App::reanalyze_semantics`]: crate::app::App::reanalyze_semantics
//! [`App::nudge_reading_budget`]: crate::app::App::nudge_reading_budget

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use semantic_reading::{
    AnalyzeRequest, AnalyzeResponse, DisplayState, Error as SemanticError, FixtureProvider,
    Provider, SemanticDocument, atomize, policy,
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

// ---------------------------------------------------------------------------
// `--semantic-cmd` — 意味判断を外部コマンドへ委譲する
// ---------------------------------------------------------------------------

/// 外部コマンドが答えを返すまで待つ上限。**設定値はここ 1 箇所**。
///
/// 寛容に取ってある。この先に繋がるのは Jev のアダプタスクリプトである。
/// Jev 自体は多数の question を 1 リクエストで並列評価する判定器で、
/// TypeSafe 自身の Python SDK は `DEFAULT_TIMEOUT = 10.0`（秒）を既定に
/// している（`docs/design/jev.md`）。つまり Jev の応答そのものは 10 秒
/// スケールで、ここの 60 秒はその想定ではない。
///
/// # 合計を実測した（2026-09-21）
///
/// かつてここには「**実測していないのは合計の方**」と書いてあった。測った。
/// プロセス起動 + ネットワーク往復 + 2 ラウンドを含む**全体**が:
///
/// ```text
/// 文書サイズ      解析の全体（アダプタのプロセス寿命）
///  1,664 B         1.50〜1.55 秒
///  9,857 B         2.33〜2.45 秒
/// 24,280 B         2.88〜3.27 秒
/// 45,650 B         2.42〜2.63 秒（7 回）
/// ```
///
/// **60 秒には 18 倍の余裕がある。** 文書サイズに対して時間はほとんど伸びない —
/// Jev は全 question を 1 リクエストで並列評価するので、伸びるのは question 数
/// ではなく input token 数の方で、それも時間への効きは浅い（実測の線形あてはめで
/// 約 0.55 秒 + 1.3 マイクロ秒/token）。
///
/// # 大文書で先に当たるのはここではない
///
/// **タイムアウトは大文書の制約ではない。** 先に当たるのは Jev の context
/// window（実測で input 約 65,536 tokens）で、超えると 2 秒台で HTTP 400
/// （`max_tokens_exceeded`）が返る。45,650 バイトの実文書はその 92〜96 % を
/// 使っており、1.06 倍にすると失敗した。**この値を上げても何も救われない**
/// （詳細は `docs/gotchas.md` と `examples/semantic/jev-annotate.py` の
/// `http_error_message`）。
///
/// それでも短くしないのは、ネットワークが遅い日に「動いているのに切られる」が
/// ユーザーには「壊れている」と区別がつかないためで、寛容な既定の害は小さい。
/// UI が固まらないのはタイムアウトではなく別スレッドで走らせていること
/// （[`crate::app::App::reanalyze_semantics`]）が担保している。
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// 応答として受け取る stdout の上限。超えたら応答を捨てる
/// （途中で切れた JSON を「壊れた応答」として報告するより、
/// 「大きすぎる」と言う方が直せる）。
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

/// `--semantic-cmd <コマンド>`: 意味判断だけを外部プロセスへ委譲する
/// [`Provider`]。
///
/// akapen に HTTP クライアントも async ランタイムも入れないための口である。
/// reqwest / tokio を足すと、この機能を使わない全ユーザーにコンパイル時間・
/// バイナリサイズ・依存監査のコストが乗る。代わりに `--send-cmd` と同じ
/// 作法で外部コマンドへ渡す — API キー管理が akapen の責務から外れ、
/// Jev を呼ぶアダプタスクリプト（`atoms` を question 群へ変換し、
/// typed answer を `units` へ戻すもの。`docs/design/jev.md`）を akapen の外に
/// 置ける。
///
/// ```text
/// stdin   {"version":1,"source":"…","atoms":[{"index":0,…}]}
/// stdout  {"version":1,"units":[{"id":"u1","atoms":[0,1],"reading_tier":"essential",
///                                  "core_atoms":[1]}]}
/// ```
///
/// `core_atoms` は任意で、「この Unit の中で、ここだけ読めば要点が取れる」
/// と判定器が選んだ Atom である。MARKED をそこだけに絞るために
/// [`policy::decorate`] が読む。**3 値である** — 省けば従来どおり Unit 全体が
/// MARKED、`[]` なら「核を持たない」でその Unit は MARKED にならない、
/// `[i]` なら `i` だけが MARKED（`protocol.rs` の「`core_atoms` は 3 値」）。
///
/// # コマンドは range を返さない
///
/// **Atom 生成は akapen 側**で行う（設計書「Jev に判断させないもの:
/// Atom生成 / source position管理」）。コマンドが返すのは Atom の
/// **添字だけ**で、[`SemanticDocument`] は akapen 自身の [`atomize`] の
/// 出力と、返ってきた Unit の構造から組み立てられる。
///
/// したがって**不正な range が原理的に生まれない**。外部コマンドが壊れた
/// 位置を返して文書の違う場所を装飾する、という事故が起きえない。
/// `--semantic`（fixture）経路で必要だった [`DigestChecked`] の
/// `source_sha256` 照合も、**この経路では不要**である — range が akapen
/// 自身のものである以上、「別の文書のもの」ということがありえない。
///
/// ここで `source_sha256` を**書き込んで**いるのは照合のためではなく
/// 素性の記録のためで、「この注釈はどの文書に対するものか」を後から
/// 言えるようにしてある（[`crate::app::App::reanalyze_semantics`] が、
/// 文書が変わったときに古い注釈を落とす判断に使う）。
///
/// # 検証
///
/// 返ってきた JSON は全項目を検証し、1 つでも失敗したら**レスポンス全体を
/// 捨てる**（[`AnalyzeResponse::into_document`]）。部分適用はしない。
#[derive(Clone, Debug)]
pub(crate) struct CommandProvider {
    cmd: String,
    timeout: Duration,
}

impl CommandProvider {
    /// 既定のタイムアウト（[`COMMAND_TIMEOUT`]）でコマンドを包む。
    pub(crate) fn new(cmd: impl Into<String>) -> Self {
        Self {
            cmd: cmd.into(),
            timeout: COMMAND_TIMEOUT,
        }
    }

    /// タイムアウトを指定して作る（テスト用。本番経路は
    /// [`CommandProvider::new`] だけを通る）。
    #[cfg(test)]
    pub(crate) fn with_timeout(cmd: impl Into<String>, timeout: Duration) -> Self {
        Self {
            cmd: cmd.into(),
            timeout,
        }
    }

    /// 委譲先のコマンド行（テスト用）。
    #[cfg(test)]
    pub(crate) fn command(&self) -> &str {
        &self.cmd
    }
}

impl Provider for CommandProvider {
    /// 文書を Atom へ割り、それを渡して Unit を受け取る 1 往復。
    ///
    /// **この関数は数十秒かかりうる。** 呼ぶのはワーカースレッドだけで、
    /// イベントループから直接呼んではならない
    /// （[`crate::app::App::reanalyze_semantics`]）。
    fn analyze(&self, source: &str) -> semantic_reading::Result<SemanticDocument> {
        let atoms = atomize(source);
        let request = AnalyzeRequest::new(source, &atoms).to_json()?;
        let stdout = crate::export::run_capturing(
            "--semantic-cmd",
            &self.cmd,
            &request,
            self.timeout,
            RESPONSE_LIMIT,
        )
        .map_err(|e| SemanticError::Provider(format!("{e:#}")))?;
        let mut document = AnalyzeResponse::from_json(&stdout)?.into_document(atoms)?;
        // 素性の記録（照合のためではない — 上のドキュメント参照）。
        document.source_sha256 = Some(source_digest(source));
        Ok(document)
    }
}

/// 意味判断の供給源。**違いは「いつ答えるか」だけ**で、どちらも
/// [`Provider`] である。
///
/// この区別が型に出ているのは、App がそれに応じて**呼び方**を変えねば
/// ならないからである。fixture はその場で答えるので同期でよい。外部
/// コマンドはプロセス起動とネットワーク往復を挟むので、同じ扱いをすると
/// UI が固まる。
/// 「遅いかもしれない」を [`Provider`] の中に隠すことはできない —
/// `analyze` の戻り値は `Result<SemanticDocument>` であって「あとで」を
/// 表現できないからである。
pub(crate) enum SemanticSource {
    /// `--semantic <fixture.json>`: 即座に答えが出る。その場で呼ぶ。
    Inline(Box<dyn Provider>),
    /// `--semantic-cmd <コマンド>`: 外部プロセス。別スレッドで走らせ、
    /// 結果は mpsc で受ける。
    Command(CommandProvider),
}

/// 引数から供給源を 1 つ決める。**供給源の選択はここだけ**。
///
/// `App` が持つのは [`SemanticSource`] であって具体型ではないので、
/// 供給源を増やすのはこの `match` に腕を 1 本足す作業になる。
///
/// `--semantic` と `--semantic-cmd` は排他で、両立は
/// [`crate::config::Config::parse`] が弾いている（ここへは来ない）。
///
/// `Ok(None)` は「この層は存在しない」。そのとき akapen は改修前と
/// **完全に同じ**挙動になる — READ の読み出しも、Budget のキーも、
/// `?` ヘルプの行も、警告の 1 つも出ない（[`App::semantic_enabled`]）。
///
/// [`App::semantic_enabled`]: crate::app::App::semantic_enabled
pub(crate) fn source_from_config(
    config: &crate::config::Config,
) -> Result<Option<SemanticSource>> {
    match (config.semantic.as_deref(), config.semantic_cmd.as_deref()) {
        (Some(path), _) => Ok(Some(SemanticSource::Inline(load_fixture(path)?))),
        // コマンドは起動時には走らせない（文書が乗ってから、別スレッドで）。
        (None, Some(cmd)) => Ok(Some(SemanticSource::Command(CommandProvider::new(cmd)))),
        (None, None) => Ok(None),
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
    /// 参照実装。Jev を呼ばない決定論的なスクリプトで、API キー無しで
    /// パイプライン全体を端から端まで動かせることがその存在理由である。
    const ANNOTATE_PY: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/semantic/annotate-doc.py");

    /// 参照実装を起動するコマンド行。exec bit に依存せず `python3` を
    /// 明示する（配布物のパーミッションでテストが落ちないように）。
    fn reference_command() -> String {
        format!("python3 '{ANNOTATE_PY}'")
    }

    /// `sh -c` で走る、決まった JSON を返すだけのコマンド。
    fn echoing(json: &str) -> CommandProvider {
        // シングルクォートを含まない JSON だけを渡す前提（テスト内で管理）。
        CommandProvider::new(format!("cat >/dev/null; printf %s '{json}'"))
    }

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

    // -----------------------------------------------------------------
    // `--semantic-cmd` — 外部コマンド委譲
    // -----------------------------------------------------------------

    /// プロトコルのラウンドトリップ: 参照実装を**実際に起動して**、
    /// 文書 → Atom → コマンド → Unit → SemanticDocument を一周させる。
    #[test]
    fn the_reference_script_speaks_the_protocol_end_to_end() {
        let source = std::fs::read_to_string(DEMO_MD).unwrap();
        let document = CommandProvider::new(reference_command())
            .analyze(&source)
            .expect("参照実装が応答すること");

        // **range は akapen 自身のもの。** コマンドは index しか返して
        // いないので、Atom 列は atomize の出力と 1 バイトも違わない。
        assert_eq!(document.atoms, atomize(&source));
        assert!(!document.atoms.is_empty());
        // 参照実装は Atom 1 つにつき Unit 1 つを返す。
        assert_eq!(document.units.len(), document.atoms.len());
        assert!(document.validate().is_ok());

        // 見出しは ESSENTIAL、その直後は SUPPORTING という素朴な規則が
        // 実際に効いている（no-op ではない）。
        let tier_of = |atom: usize| {
            document
                .units
                .iter()
                .find(|unit| unit.atoms.contains(&AtomIndex(atom)))
                .unwrap()
                .reading_tier
        };
        assert_eq!(document.atoms[0].kind, AtomKind::Heading);
        assert_eq!(tier_of(0), ReadingTier::Essential);
        assert_eq!(tier_of(1), ReadingTier::Supporting);
        assert!(
            document.units.iter().any(|u| u.reading_tier == ReadingTier::Detail),
            "DETAIL も出る"
        );
        // Budget を動かすと実際に表示状態が変わる（層として生きている）。
        let at_100 = decorations_for(&document, 100);
        let at_30 = decorations_for(&document, 30);
        assert_ne!(at_100, at_30);
        assert!(at_30.iter().any(|d| d.kind == DecorationKind::Dim));
    }

    /// 組み立てた文書は、解析した source の素性を名乗る。
    ///
    /// 照合のためではない（range は akapen 自身のものなので「別の文書の
    /// もの」がありえない）。「この注釈はどの文書に対するものか」を
    /// App が後から言えるようにするための記録である。
    #[test]
    fn the_command_result_is_stamped_with_the_source_it_read() {
        let source = "# 見出し\n\n本文です。\n";
        let document = echoing(r#"{"version":1,"units":[]}"#)
            .analyze(source)
            .unwrap();
        assert_eq!(document.source_digest(), Some(source_digest(source).as_str()));
    }

    /// 不正な range が**原理的に**生まれないこと。
    ///
    /// コマンドが range を返してきても（プロトコルに無いフィールドとして
    /// 無視される）、文書の位置は akapen の atomize が出したものだけになる。
    #[test]
    fn a_command_cannot_move_a_range_even_if_it_tries() {
        let source = "# 見出し\n\n本文です。\n";
        let document = echoing(
            r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"essential","range":{"start":9999,"end":99999}}]}"#,
        )
        .analyze(source)
        .unwrap();
        assert_eq!(document.atoms, atomize(source));
        // 文書の外を指す range は 1 つも無い。
        for atom in &document.atoms {
            assert!(source.get(atom.range.clone()).is_some(), "{:?}", atom.range);
        }
    }

    /// 検証に 1 つでも失敗したらレスポンス全体を捨てる — 外部コマンド
    /// 経路でも同じこと。詳細な場合分けは crate 側
    /// （`semantic_reading::protocol`）で網羅している。
    #[test]
    fn a_broken_response_is_refused_whole() {
        let source = "# 見出し\n\n本文です。\n";
        let cases = [
            // 範囲外の atom index
            (r#"{"version":1,"units":[{"id":"u1","atoms":[99],"reading_tier":"essential"}]}"#, "範囲外"),
            // id の重複
            (r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"essential"},{"id":"u1","atoms":[1],"reading_tier":"detail"}]}"#, "重複"),
            // 未知の tier
            (r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"urgent"}]}"#, "JSON"),
            // 存在しない relation 先
            (r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"detail","relations":[{"redundant_with":"u9"}]}]}"#, "未知"),
            // version 不一致
            (r#"{"version":7,"units":[{"id":"u1","atoms":[0],"reading_tier":"essential"}]}"#, "version 7"),
            // JSON ですらない
            ("not json", "JSON"),
        ];
        for (response, expected) in cases {
            let err = echoing(response)
                .analyze(source)
                .expect_err("{response} は拒否されること");
            assert!(
                err.to_string().contains(expected),
                "{response} => {err}"
            );
        }
    }

    /// 異常終了: stderr の内容を持って帰る（toast が理由を言えるように）。
    /// クラッシュはしない。
    #[test]
    fn a_command_that_fails_reports_its_stderr_without_crashing() {
        let err = CommandProvider::new(
            "cat >/dev/null; echo 'starting' >&2; echo 'ANTHROPIC_API_KEY is not set' >&2; exit 2",
        )
        .analyze("# a\n")
        .expect_err("異常終了は Err");
        let message = err.to_string();
        assert!(message.contains("--semantic-cmd"), "{message}");
        assert!(message.contains("ANTHROPIC_API_KEY is not set"), "{message}");
        // 「文書が不正」ではない — 答えを得られなかっただけ。
        assert!(!message.contains("document が不正"), "{message}");
    }

    /// 起動できないコマンドでも落ちない。
    #[test]
    fn a_command_that_does_not_exist_is_an_error_not_a_panic() {
        let err = CommandProvider::new("akapen-no-such-command-exists-here")
            .analyze("# a\n")
            .expect_err("起動できなければ Err");
        assert!(err.to_string().contains("--semantic-cmd"), "{err}");
    }

    /// タイムアウト: 返ってこないコマンドは殺して Err にする。
    ///
    /// 既定は [`COMMAND_TIMEOUT`]（60 秒）だが、テストは注入した短い値で
    /// 同じ経路を通す（export.rs の子プロセステストと同じ流儀）。
    #[test]
    fn a_command_that_never_answers_is_killed_and_reported() {
        let start = std::time::Instant::now();
        let err = CommandProvider::with_timeout("sleep 30", Duration::from_millis(200))
            .analyze("# a\n")
            .expect_err("返ってこなければ Err");
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "待たずに殺すこと（{:?} かかった）",
            start.elapsed()
        );
        // 既定値は寛容 — 実測した全体は最大 3.3 秒で、18 倍の余裕がある
        // （`COMMAND_TIMEOUT` のコメントの表）。大文書で先に当たるのは
        // ここではなく Jev の context window である。
        assert_eq!(COMMAND_TIMEOUT, Duration::from_secs(60));
    }

    /// 空の文書でも一周する（Atom 0 個・Unit 0 個）。
    #[test]
    fn an_empty_document_round_trips_as_an_empty_annotation() {
        let document = CommandProvider::new(reference_command()).analyze("").unwrap();
        assert!(document.atoms.is_empty());
        assert!(document.units.is_empty());
        assert!(decorations_for(&document, 50).is_empty());
    }

    /// `--semantic-cmd` から Command の供給源が立ち、`--semantic` からは
    /// Inline が立つ。**供給源の選択はこの関数だけ**。
    #[test]
    fn the_config_chooses_exactly_one_source() {
        let base = |semantic: Option<&str>, cmd: Option<&str>| {
            let mut args: Vec<String> = vec!["x.md".into()];
            if let Some(path) = semantic {
                args.push("--semantic".into());
                args.push(path.into());
            }
            if let Some(cmd) = cmd {
                args.push("--semantic-cmd".into());
                args.push(cmd.into());
            }
            match crate::config::Config::parse(args).unwrap() {
                crate::config::Action::Run(config) => config,
                _ => unreachable!(),
            }
        };

        assert!(source_from_config(&base(None, None)).unwrap().is_none());

        let command = source_from_config(&base(None, Some("annotate-doc")))
            .unwrap()
            .expect("--semantic-cmd で層が立つ");
        match command {
            SemanticSource::Command(provider) => assert_eq!(provider.command(), "annotate-doc"),
            SemanticSource::Inline(_) => panic!("Command のはず"),
        }

        let fixture = source_from_config(&base(Some(DEMO_JSON), None))
            .unwrap()
            .expect("--semantic で層が立つ");
        assert!(matches!(fixture, SemanticSource::Inline(_)));
    }

    /// 起動時にコマンドは走らない。存在しないコマンドを渡しても
    /// `source_from_config` は成功する — 走るのは文書が乗ってからで、
    /// そこで初めて失敗を報告する。
    #[test]
    fn the_command_is_not_run_while_resolving_the_source() {
        let config = match crate::config::Config::parse(
            ["x.md", "--semantic-cmd", "exit 1"]
                .iter()
                .map(|s| s.to_string()),
        )
        .unwrap()
        {
            crate::config::Action::Run(config) => config,
            _ => unreachable!(),
        };
        assert!(source_from_config(&config).unwrap().is_some());
    }
}
