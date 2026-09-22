# fixture の作り

## `sample.md` / `sample.json` — 手書きの基準

`sample.md` は AI が書きがちな設計メモを模した Markdown（日本語・1408 バイト）で、
`sample.json` はそれに対する semantic annotation。Jev の代わりに手で付けてある。

Atom の範囲は `sample.md` の実バイトオフセット。日本語なので 1 文字 3 バイトであり、
バイトオフセットと文字数・端末セルが一致しないケースを意図的に含んでいる。

## Unit

| id   | 内容                     | Tier       | relation            | バイト長 |
| ---- | ------------------------ | ---------- | ------------------- | -------- |
| u1   | タイトル見出し           | ESSENTIAL  |                     | 35       |
| u2   | 問題の提示               | SUPPORTING |                     | 111      |
| u3   | 結論                     | ESSENTIAL  |                     | 93       |
| u4   | 背景（見出し + 3 文）    | CONTEXT    |                     | 265      |
| u5   | 制約（見出し + 2 項目）  | ESSENTIAL  |                     | 123      |
| u6   | 影響範囲（見出し + 2 文）| CONTEXT    |                     | 255      |
| u7   | 補足（結論の言い換え）   | SUPPORTING | REDUNDANT_WITH(u3)  | 87       |
| u8   | 引用                     | CONTEXT    |                     | 95       |
| u9   | 数値例 + コード          | DETAIL     |                     | 116      |
| u10  | 余談 2 文                | DETAIL     |                     | 194      |

Unit の合計は 1374 バイト（空行と改行は Atom に含めないので `sample.md` より短い）。

## keep 順と累積

`policy::decorate` の並び替え鍵 `(実効 Tier, redundant か, 長さ, 先頭位置, 並び順)`
で並べると次のようになる。u7 は SUPPORTING だが REDUNDANT なので実効 CONTEXT へ
1 段落ち、さらに同じ実効 Tier の中では非 REDUNDANT の後ろに回る。

| 順 | unit | 実効 Tier  | 長さ | 累積 | 累積 % |
| -- | ---- | ---------- | ---- | ---- | ------ |
| 1  | u1   | ESSENTIAL  | 35   | 35   | 2.5 %  |
| 2  | u3   | ESSENTIAL  | 93   | 128  | 9.3 %  |
| 3  | u5   | ESSENTIAL  | 123  | 251  | 18.3 % |
| 4  | u2   | SUPPORTING | 111  | 362  | 26.3 % |
| 5  | u8   | CONTEXT    | 95   | 457  | 33.3 % |
| 6  | u6   | CONTEXT    | 255  | 712  | 51.8 % |
| 7  | u4   | CONTEXT    | 265  | 977  | 71.1 % |
| 8  | u7   | CONTEXT※  | 87   | 1064 | 77.4 % |
| 9  | u9   | DETAIL     | 116  | 1180 | 85.9 % |
| 10 | u10  | DETAIL     | 194  | 1374 | 100 %  |

※ 元 Tier は SUPPORTING。

`tests/policy.rs` の budget 100 / 70 / 30 / 10 の期待値はこの表の累積 % から出している。

## 台帳は二段

`policy::decorate` は**核（ESSENTIAL かつ非 REDUNDANT かつ核を持つ）を先に、
Budget を見ずに確保する**。この fixture では u1 / u3 / u5 がそれで、合計
251 バイト = 18.3 % になる。残りの 7 Unit が二段目で残りの予算を奪い合う。

**この fixture では累積の列は上の表のままである** — 核がちょうど keep 順の
先頭 3 つなので、一段目の合計は表の 3 行目の累積と一致する。変わるのは
**18.3 % より小さい Budget** で、そこでは表の打ち切り位置に関係なく
u1 / u3 / u5 の 3 つが出る（`budget_1_keeps_the_cores_and_100_keeps_them_all`）。

前提（`PRESUPPOSES`）はこの fixture には無いので、一段目は閉包を連れてこない。

## `source_sha256`

このファイルには入れていないが、`SemanticDocument` は任意フィールド
`source_sha256`（source テキストの SHA-256、hex 64 桁）を持てる。Atom の
範囲は annotation を作った時点の文書に対するバイト位置なので、別の文書に
当てれば無意味な位置を装飾する。クライアントはこの値で取り違えを検出できる
（akapen の `src/semantic.rs` の `DigestChecked` がそれ）。

crate 側は**形（hex 64 桁）だけ**を `validate` で見る。実際に一致するかは
source を持っているクライアントの仕事で、この crate はハッシュ実装を持たない。
フィールドが無ければ照合しない — この fixture がその後方互換の実例になっている。

## `design-doc-frozen-2026-09-22.md` — 設計書の凍結コピー

`docs/design/semantic-reading-layer.md`（正典）から `8994544`（2026-09-22）の
内容を切り出したコピーです。**これは fixture で、正典ではありません。内容の
更新は不要です** —— 2 つのテストが要求しているのは中身ではなく、**大きさと構造**
（10.7 KB・223 Atom・見出しと本文とリストとコードと引用が一通りある雑な実文書）
だけです。

読んでいるのは 2 つです。

- `tests/atomize.rs::a_real_design_document_holds_every_invariant` ——
  実文書を食わせても Atom の位置が壊れないこと
- `examples/semantic/test_jev_annotate.py::test_small_documents_still_go_in_one_request`
  —— 分割が不要な文書がラウンド 1〜3 で 1 リクエストに収まること

**切り離した理由**: 両方が正典を `include_str!` / パスで直接読んでいたので、
**テストが設計書の大きさに上限を掛けていました**。とくに後者は
「1 リクエストに収まる」を要求するので、実測で**書き足せる余地が 30〜45
バイト**しかなく、2026-09-22 に入った規則（「核は奪わない」）を設計書へ
書けませんでした。正典が自由に伸びるように、読む先をこのコピーへ移しました。

**逐語のコピーではありません。** 冒頭に fixture であることの注記を入れ、
その分の余地を作るために**「Role」節と「最初のデモ」節（計 579 バイト）を
落としてあります**。落とせるのは、どちらも他の節に無い Atom の種類を持って
いないからです。正典の 2026-09-22 の姿が要るなら `git show 8994544` を見て
ください —— このファイルの仕事はアーカイブではありません。

**余地は実測で残り 2,282 tokens です**（ラウンド 2、`whole` 58,161 −
question 55,879）。切り離す前の正典は残り 21 tokens で崖のふちに立っていた
ので、落とした 579 バイトはその崖から離れるための余地でもあります。
**ここに書き足すなら測り直してください** —— ラウンド 2 の question は
ほぼ Unit ごとなので、**Atom が 1 つ増えると約 300 tokens 増えます**
（実測 55,879 ÷ 188）。測り方は `dry_run` の `rounds[1]` を直接読みます。
