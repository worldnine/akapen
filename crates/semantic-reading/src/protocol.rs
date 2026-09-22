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
//! 内部で Jev を 2 回呼ぶのは自由で、こちらのプロトコルが判定器側の段取りを
//! 規定すべきではない。継ぎ目は「Atom を渡して Unit を受け取る」だけ。
//! 将来キャッシュのために段階を分ける必要が出たら `stage` を足せる
//! （未知のフィールドは拒否していない。[`AnalyzeResponse`] 参照）。
//!
//! # `core_atoms` を足しても版は上げない
//!
//! [`crate::SemanticUnit::core_atoms`]（MARKED を絞る核）は後から足した
//! フィールドだが、[`VERSION`] は 1 のままにしてある。版を上げないのは、
//! **上げると古い組み合わせが動かなくなるのに、何も救われない**からである。
//!
//! | 組み合わせ | 起きること |
//! | ---------- | ---------- |
//! | 新しい akapen + 古い判定器 | フィールドが無い = 絞り込み無し。Unit 全体が MARKED（従来の表示） |
//! | 古い akapen + 新しい判定器 | 未知のフィールドとして無視される。従来の表示 |
//!
//! どちらも「意味を取り違える」側へは倒れない。版が守っているのは
//! 「同じフィールドを双方が違う意味で読み書きする」事故であって、
//! 追加されたフィールドを片方が知らないことではない。
//!
//! ## `core_atoms` は 3 値 — 「無い」と「空」は別の意味
//!
//! | wire 形 | 内部 | 意味 |
//! | --- | --- | --- |
//! | フィールドが無い | `None` | **絞り込みを受けていない** — Unit 全体が MARKED |
//! | `"core_atoms":[]` | `Some([])` | **核を持たない** — この Unit は MARKED にならない |
//! | `"core_atoms":[i]` | `Some([i])` | `i` だけが MARKED |
//!
//! **`[]` を「無い」に丸めてはならない。** 丸めると、核の選に漏れた Unit が
//! 丸ごと光る — いちばん避けたい状態へ落ちる。`skip_serializing_if` が
//! `Vec::is_empty` ではなく `Option::is_none` なのはこのためで、
//! `an_empty_core_survives_the_wire_round_trip` が固定している。
//!
//! **これも版を上げない。** `[]` を知らない古い akapen は、それを空の Vec と
//! 読んで「絞り込み無し」に倒す — 表示は従来どおり Unit 全体が MARKED に
//! なるだけで、位置を取り違えることはない。
//!
//! # `score` と `question` を足しても版は上げない — marks モードの分
//!
//! [`crate::SemanticUnit::score`]（問いへの答えの強さ）と、要求側の
//! [`RequestQuestion`]・応答側の [`AnalyzeResponse::question`]
//! （2026-09-22。`docs/design/marks-only-and-review-mode.md`）も
//! [`VERSION`] を 1 のままにしてある。どちらも**追加されたフィールド**なので、
//! 上の `core_atoms` とまったく同じ表になる。
//!
//! | 組み合わせ | 起きること |
//! | ---------- | ---------- |
//! | 新しい akapen（marks モード）+ 古い判定器 | 要求の `question` は未知フィールドとして無視され、DIM 版の答えが返る。`score` が 1 つも無いので **0 本**になり、akapen は「この判定器はスコアを返さない」とステータス行に出す（[`crate::marks::has_scores`]）。黙った空白にはならない |
//! | 古い akapen + 新しい判定器 | `score` も `question` も未知のフィールドとして読み飛ばされる。marks の答えは `reading_tier` が全部 `detail` なので、DIM 版の投影では**何も光らない** |
//!
//! どちらも「意味を取り違える」側へは倒れない。1 行目は理由を言い、
//! 2 行目は安全側（光らせすぎない）へ倒れる。
//!
//! **`reading_tier` は必須のままにしてある。** marks の答えでは使われない
//! ので `#[serde(default)]` を足したくなるが、足すと「Tier を書き忘れた
//! DIM 版の判定器が黙って `detail` になる」— いまエラーとして見えている
//! 誤りが見えなくなる。marks の判定器は `detail` を明示して書く。
//!
//! # `section_of` を足しても版は上げない — `core_atoms` と同じ話である
//!
//! [`crate::SemanticUnit::section_of`]（節の見出し Unit。2026-09-22）も
//! [`VERSION`] を 1 のままにした。**こちらは追加された枝ではなく追加された
//! フィールド**なので、上の `core_atoms` とまったく同じ表になる。
//!
//! | 組み合わせ | 起きること |
//! | ---------- | ---------- |
//! | 新しい akapen + 古い判定器 | フィールドが無い = 節を知らない。見出しは復帰せず、**この変更を入れる前の表示**になる |
//! | 古い akapen + 新しい判定器 | 未知のフィールドとして無視される。同じく従来の表示 |
//!
//! どちらも「意味を取り違える」側へは倒れない。片方が損をするのは
//! 「中身が残っているのに見出しが沈む」という**もともとの状態**であって、
//! 新しく壊れるものは無い。
//!
//! **なぜ [`crate::Relation`] の枝にしなかったか**も、この表がそのまま理由に
//! なっている。枝にすれば古い akapen は `unknown variant` で応答を丸ごと
//! 捨て、注釈が 1 つも付かなくなる（下の `PRESUPPOSES` の表の 2 行目）。
//! フィールドなら読み飛ばされるだけで済む。出自を混ぜない理由のほうは
//! [`crate::SemanticUnit::section_of`] にある。
//!
//! # `redundant_with` の意味が半歩ずれても版は上げない
//!
//! 2026-09-22 に [`crate::Relation::RedundantWith`] の読み方が変わった。
//!
//! | | 読み方 | 弱まるのは |
//! | --- | --- | --- |
//! | 〜2026-09-22 | 「**私は**冗長だ」 | 必ず持ち主 |
//! | いま | 「**この 2 つは**冗長な対だ」 | Tier が低い方、同 Tier なら長い方 |
//!
//! **ワイヤの形は 1 ビットも変わらない。** フィールド名も、値（相手の id）も、
//! 向き（持ち主から前の Unit へ）もそのままである。判定器が書くものは同じで、
//! 変わったのは akapen 側の [`crate::policy`] がそれをどう使うかだけである。
//!
//! だから版を上げない。**版が守っているのは「同じフィールドを双方が違う
//! 意味で読み書きする」事故**だが、ここで意味を持っているのは片側
//! （akapen）だけで、判定器は「この 2 つは同じ内容だ」としか言っていない。
//! 上げると、正しく動いている古い判定器が拒まれるだけになる。
//!
//! **`core_atoms` はこれに引きずられる。** 冗長な Unit も ESSENTIAL なら
//! MARKED になるようになった（[`crate::policy`]「核は奪わない」）ので、
//! 判定器は冗長な Unit にも核を選ぶべきである。選ばずにフィールドを省くと
//! `None` = 絞り込み無しになり、**その Unit が丸ごと光る**。これは版の話
//! ではなく判定器側の宿題で、上の 3 値の表の 1 行目がそのまま起きるだけ
//! である（意味を取り違える側へは倒れない）。
//!
//! # `PRESUPPOSES` を足しても版は上げない — ただし理由の形が違う
//!
//! [`crate::Relation::Presupposes`]（context preservation の前提。2026-09-21）
//! も [`VERSION`] を 1 のままにした。**ただし `core_atoms` と同じ話ではない。**
//! あちらは追加された**フィールド**で、知らない側は読み飛ばせた。こちらは
//! 追加された**列挙の枝**なので、知らない側は読み飛ばせない。
//!
//! | 組み合わせ | 起きること |
//! | ---------- | ---------- |
//! | 新しい akapen + 古い判定器 | `presupposes` が 1 つも来ない。閉包が空なので `decorate` の請求額は従来どおり Unit 1 つ分になり、**表示は 1 ビットも変わらない** |
//! | 古い akapen + 新しい判定器 | serde が `unknown variant `presupposes`` で失敗し、**応答が丸ごと捨てられる**。注釈は付かず、`App::flash_err` がステータス行にその一行を出す |
//!
//! **2 行目は退化ではなく失敗である。** `core_atoms` の表が「どちらも従来の
//! 表示」だったのと違って、こちらは片側が注釈を失う。それでも版を上げない
//! 理由は 2 つある。
//!
//! 1. **上げても直らない。** 版を 2 にしたところで古い akapen は
//!    「cannot read protocol version 2」で同じく応答を捨てる。変わるのは
//!    エラーの文面だけで、注釈は戻らない
//! 2. **上げると、いま動いている組み合わせが死ぬ。** 新しい akapen + 古い
//!    判定器（1 行目）は完全に正しく動いているのに、版を 2 にすると
//!    判定器が名乗る 1 を拒んで壊れる。**直らない側のために、壊れていない
//!    側を壊すことになる**
//!
//! そして版が守っているものは、ここでも守られている。**どちらの向きにも
//! 「それらしく見えるが間違っている注釈」は出ない** — 1 行目は前提を知らない
//! だけで正しく、2 行目は何も出さずに理由を言う。版は「同じフィールドを双方が
//! 違う意味で読み書きする」事故のためにあり、ここにその事故は無い。
//!
//! **失敗は静かではない。** `AnalyzeResponse::from_json` の `Err` は
//! `CommandProvider::analyze` から `App::accept_analysis` へ上がり、
//! `flash_err` でステータス行に出る（`src/app.rs`）。古い akapen に新しい
//! 判定器を繋いだ人は、黙って注釈が消えるのではなく理由を読む。
//!
//! # [`AtomKind`] に値を足しても版は上げない — **向きが片道だからである**
//!
//! 2026-09-22 に [`crate::AtomKind::TableRow`] が増えた（`atomize` が表を行へ
//! 割るようになった分）。これも [`VERSION`] は 1 のままである。
//!
//! 上の `PRESUPPOSES` と同じ「追加された**枝**」だが、**同じ話にならない**。
//! `kind` が載るのは [`RequestAtom`]、つまり **akapen → 判定器の片道**だけで、
//! 判定器から返ってくる応答に `kind` は 1 つも無い。枝を知らない側が
//! 「読んで拒む」場面が、この向きには存在しない。
//!
//! | 組み合わせ | 起きること |
//! | ---------- | ---------- |
//! | 新しい akapen + 古い判定器 | `table_row` は判定器にとってただの未知の文字列。境界の既定（`rule:default`）で表の行が 1 行ずつ別の Unit になり、核の候補（`PROSE_KINDS`）にも入らないので、**表はこの変更を入れる前と同じく光らない**。位置を取り違える余地は無い |
//! | 古い akapen + 新しい判定器 | `kind` は応答に載らないので**1 ビットも変わらない** |
//!
//! **逆流はキャッシュの経路にだけある。** 新しい akapen が書いた
//! `~/.cache/akapen/semantic/…json`（`src/semantic_cache.rs`）には
//! `"kind":"table_row"` が入るので、古い akapen の serde はそれを読めない。
//! そこは `serde_json::from_str(…).ok()?` で**外れになる**だけで、
//! 解析し直して上書きされる。プロトコルの話ではない。
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
    /// **いま聞きたいこと**（marks モード）。無ければ DIM 版の解析。
    ///
    /// 「marks モードである」を表すフラグは**これ 1 つ**である。モードの
    /// フラグと問いを別々に置くと食い違いうるので、置かない。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<RequestQuestion<'a>>,
}

/// 要求に載る問い 1 つ（marks モード）。
///
/// **文面は akapen が持って送る。** 判定器は文面を知らない汎用の器で
/// あってよく、定型の正本は akapen 側のデータファイル 1 か所になる
/// （2 か所に置くとずれる）。自由入力も、akapen が型へ埋めてから送る。
#[derive(Clone, Debug, Serialize)]
pub struct RequestQuestion<'a> {
    /// 問いの識別子（`essential` / `settled` / `free` など）。応答が
    /// そのまま echo し、キャッシュの読み戻しの照合に使われる。
    pub id: &'a str,
    /// Jev へ渡す問いの文面。`docs/design/marks-only-and-review-mode.md`
    /// 0 節の表の逐語（自由入力は `{q}` を埋めたもの）。
    pub text: &'a str,
    /// **核をどの Unit に聞くか**の足切り。判定器はこの値を超えた Unit に
    /// だけ「核はどの一文か」を聞けばよい（越えない Unit は光らないので
    /// 核が要らない）。狭い問いでは核のラウンドごと消える。
    ///
    /// 値は akapen 側の [`crate::marks::SCORE_FLOOR`] である。
    pub core_floor: f32,
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
            question: None,
        }
    }

    /// marks モードの問いを載せる。
    ///
    /// これが載っていない要求は DIM 版の解析になる（判定器の既定）。
    pub fn asking(mut self, question: RequestQuestion<'a>) -> Self {
        self.question = Some(question);
        self
    }

    /// 要求を JSON 1 行にする。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// 外部コマンドの stdout から受け取る応答。
///
/// `units` の要素は [`SemanticUnit`] そのままである — `id` / `atoms` /
/// `reading_tier` / `core_atoms` / `section_of` / `relations` の 6 つで、
/// wire 形と内部表現が 1 対 1 に対応する。別の DTO を挟まないのは、挟めば両者がずれうるからで、
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
    /// **この応答が答えている問いの id**（marks モード）。判定器が要求の
    /// [`RequestQuestion::id`] をそのまま echo する。
    ///
    /// [`SemanticDocument::question`] へそのまま移り、キャッシュの
    /// 読み戻しで「別の問いの答え」を撥ねるために使われる。DIM 版の
    /// 応答は持たない。
    #[serde(default)]
    pub question: Option<String>,
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
    /// | `core_atoms` がその unit の `atoms` の部分集合 | [`SemanticDocument::validate`] |
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
                "cannot read protocol version {} (this build speaks {VERSION})",
                self.version
            )));
        }
        let mut units = self.units;
        let mut claimed = vec![false; atoms.len()];
        for unit in &mut units {
            let mut taken_away = Vec::new();
            unit.atoms.retain(|&index| {
                let AtomIndex(position) = index;
                match claimed.get_mut(position) {
                    // 先勝ち: すでに誰かが取っている Atom は落とす。
                    Some(taken) if *taken => {
                        taken_away.push(index);
                        false
                    }
                    Some(taken) => {
                        *taken = true;
                        true
                    }
                    // 範囲外。落とさずに残し、validate に弾かせる — ここで
                    // 黙って捨てると「不正な添字を送っても通る」ことになる。
                    None => true,
                }
            });
            // 核は先勝ちの結果にだけ追従させる。取られた Atom を核に選んで
            // いたら、その核も一緒に落ちる。同じ Unit が同じ Atom を 2 回
            // 並べただけなら添字はまだ残っているので、核も残す。
            //
            // **全部落ちても「絞り込み無し」には戻さない。** `Some([])` の
            // まま、つまり「核を持たない」になる。戻すと、核に選んだ Atom を
            // 他の Unit に取られた Unit が**丸ごと光る**ことになり、いちばん
            // 避けたい状態（`docs/gotchas/semantic-reading.md`「半分だけ DIM
            // のリスト」の裏返し）に落ちる。核が指していた Atom はもう隣の
            // Unit のものなので、この Unit に光らせるべき中身は残っていない。
            //
            // **ここで落とすのは取られた核だけである。** 最初から自分の
            // Atom でない核は落とさず、validate に弾かせる — そちらは
            // 判定器の誤りであって、黙って直すと気づけない。
            let lost: Vec<_> = taken_away
                .into_iter()
                .filter(|index| !unit.atoms.contains(index))
                .collect();
            if !lost.is_empty()
                && let Some(core) = unit.core_atoms.as_mut()
            {
                core.retain(|index| !lost.contains(index));
            }
        }
        let mut document = SemanticDocument::new(atoms, units);
        document.question = self.question;
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

    /// 前提が wire を通って relation になる。**`redundant_with` と取り違え
    /// ない** — 効き方が逆で、`presupposes` は**指した先を引き上げる**
    /// （持ち主が残るなら参照先も一緒に残す）。`redundant_with` のほうは
    /// 対の負けた側を 1 段弱めるだけで、どちらの核も奪わない。
    #[test]
    fn a_prerequisite_crosses_the_wire_as_its_own_relation() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"context"},
                {"id":"u2","atoms":[1,2],"reading_tier":"essential","relations":[{"presupposes":"u1"}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            document.units[1].relations,
            [Relation::Presupposes("u1".into())]
        );
        assert!(!document.units[1].is_redundant());
        assert_eq!(document.units[1].redundant_with(), None);
        assert_eq!(
            document.units[1].presupposes().collect::<Vec<_>>(),
            [&UnitId::from("u1")]
        );
    }

    /// 参照先が居ない前提は、redundancy と同じで**応答ごと捨てる**。
    #[test]
    fn a_prerequisite_pointing_at_nobody_throws_the_whole_response_away() {
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0,1,2],"reading_tier":"essential","relations":[{"presupposes":"u9"}]}
            ]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown unit"), "{err}");
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
        assert!(err.to_string().contains("atom 9, which is out of range"), "{err}");

        // unit id の重複。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential"},
                {"id":"u1","atoms":[1],"reading_tier":"detail"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate unit id"), "{err}");

        // 存在しない unit への relation。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","relations":[{"redundant_with":"u9"}]}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown unit"), "{err}");

        // 自分自身との重複。
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","relations":[{"redundant_with":"u1"}]}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("redundant with itself"), "{err}");

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
        assert!(err.to_string().contains("atom 42, which is out of range"), "{err}");
    }

    /// 核はそのまま文書まで届き、MARKED を絞る。
    #[test]
    fn a_core_atom_rides_through_to_the_document() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0,1,2],"reading_tier":"essential","core_atoms":[1]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(document.units[0].core_atoms, Some(vec![AtomIndex(1)]));
        assert!(!document.units[0].is_core(AtomIndex(0)));
        let states = crate::policy::decorate(&document, 100);
        assert_eq!(
            states.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
            [
                crate::DisplayState::Normal,
                crate::DisplayState::Marked,
                crate::DisplayState::Normal
            ]
        );
    }

    /// 核を名乗らない応答（従来の判定器）は、従来どおり Unit 全体が MARKED。
    #[test]
    fn a_response_without_core_atoms_keeps_marking_the_whole_unit() {
        let document = response(
            r#"{"version":1,"units":[{"id":"u1","atoms":[0,1],"reading_tier":"essential"}]}"#,
        )
        .unwrap();
        assert_eq!(document.units[0].core_atoms, None);
        let states = crate::policy::decorate(&document, 100);
        assert!(
            states[..2]
                .iter()
                .all(|(_, s)| *s == crate::DisplayState::Marked)
        );
    }

    /// 自分の Atom でない核は応答ごと捨てる。
    #[test]
    fn a_core_atom_outside_its_own_unit_throws_the_response_away() {
        let err = response(
            r#"{"version":1,"units":[
                {"id":"u1","atoms":[0],"reading_tier":"essential","core_atoms":[1]},
                {"id":"u2","atoms":[1,2],"reading_tier":"detail"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("core atom 1"), "{err}");

        // 範囲外の核も同じ経路で落ちる（黙って捨てない）。
        let err = response(
            r#"{"version":1,"units":[{"id":"u1","atoms":[0],"reading_tier":"essential","core_atoms":[42]}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("core atom 42"), "{err}");
    }

    /// 先勝ちで Atom を取られたら、その核も一緒に落ちる — 残りの核が
    /// あればそれが効き、全部落ちれば絞り込み無しに戻る。
    #[test]
    fn the_first_come_rule_takes_the_core_with_the_atom() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"first","atoms":[0,1],"reading_tier":"detail"},
                {"id":"second","atoms":[1,2],"reading_tier":"essential","core_atoms":[1,2]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(document.units[1].atoms, [AtomIndex(2)]);
        assert_eq!(document.units[1].core_atoms, Some(vec![AtomIndex(2)]), "1 は取られた");

    }

    /// **核を全部取られた Unit は `Some([])` に倒す。`None` へは戻さない。**
    ///
    /// ここは 3 値のうちどちらへ倒すかの選択で、どちらも筋は通る:
    ///
    /// - `None`（絞り込み無し）へ戻す … 変更前の挙動。Unit 全体が MARKED
    /// - `Some([])`（核を持たない）のまま … **こちらを選んだ**
    ///
    /// 選んだ理由は、核に選ばれた Atom はもう**隣の Unit のもの**だからである。
    /// この Unit に光らせるべき中身は残っていないのに `None` へ戻すと、残った
    /// Atom が**丸ごと光る** — 絞ったつもりが元より広く光る、という向きに倒れる。
    ///
    /// 再解析やキャッシュで Unit の割り方が変わると通る経路なので、
    /// **黙って挙動が変わらないようにここで固定する。**
    #[test]
    fn a_unit_that_loses_every_core_atom_keeps_an_empty_core_not_an_absent_one() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"first","atoms":[1],"reading_tier":"detail"},
                {"id":"second","atoms":[0,1,2],"reading_tier":"essential","core_atoms":[1]}
            ]}"#,
        )
        .unwrap();
        // 先勝ちで atom 1 は first のものになり、second の核は空になる。
        assert_eq!(document.units[1].atoms, [AtomIndex(0), AtomIndex(2)]);
        assert_eq!(
            document.units[1].core_atoms,
            Some(Vec::new()),
            "None へ戻すと、残った atom 0 と 2 が丸ごと MARKED になる"
        );
        assert!(!document.units[1].is_core(AtomIndex(0)));
        assert!(!document.units[1].is_core(AtomIndex(2)));
    }

    /// 核が**一部だけ**取られたら、残った核はそのまま効く。
    #[test]
    fn a_unit_that_loses_some_core_atoms_keeps_the_rest() {
        let document = response(
            r#"{"version":1,"units":[
                {"id":"first","atoms":[1],"reading_tier":"detail"},
                {"id":"second","atoms":[1,2],"reading_tier":"essential","core_atoms":[1,2]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(document.units[1].core_atoms, Some(vec![AtomIndex(2)]));
        assert!(document.units[1].is_core(AtomIndex(2)));
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
