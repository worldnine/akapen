# fixture の作り

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
