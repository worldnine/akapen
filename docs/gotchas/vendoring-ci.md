# ベンダリングと CI

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。

---


### 上流のバージョンとフォーク自身のバージョンは別物

`third_party/tui-markdown` は tui-markdown のフォークです。Cargo.toml に
2 つのバージョンがあります:

| フィールド | 意味 | 使われ方 |
|---|---|---|
| `[package.metadata] vendored-from` | **ベンダリング元の上流バージョン**（いま `0.3.9`） | 上流ソースの解決（ダウンロード URL / registry キャッシュ）とマニフェストヘッダの照合 |
| `[package] version` | **フォーク自身のバージョン**（いま `0.4.0`） | 表示のみ |

`scripts/check-vendor-diff.sh` は前者を `VER`、後者を `FORK_VER` として
**別々に**読みます。ここを混同すると CI が 2 通りに壊れます — 存在しない
上流 crate を取りに行って 403、あるいはマニフェストヘッダと不一致で契約1 が
落ちる。**フォークの API を変えて `[package] version` を上げるだけなら、
CI はグリーンのままが正しい挙動です。**

上流を本当に上げ直したときは `vendored-from` を書き換えて
`./scripts/check-vendor-diff.sh --update` でマニフェストを再生成してください。

**確認したこと**: `third_party/tui-markdown/Cargo.toml` に
`version = "0.4.0"` と `[package.metadata] vendored-from = "0.3.9"` が
両方あること。`scripts/check-vendor-diff.sh` の `VER` / `FORK_VER` が
別の sed で読まれていること。
`TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh` が
「上流 0.3.9 / フォーク 0.4.0、28 ファイル、1880 変更行」でグリーンに
なることを実行して確認。

### シェルスクリプトの日本語メッセージ内の変数展開は `${VAR}` で括る

`"… $FORK_VER）です"` と書くと、**直後の全角括弧まで変数名として読まれて**
`set -u` の下で `unbound variable` になります。日本語メッセージの中で
変数を展開するときは必ず `${FORK_VER}` と括ってください。

**確認したこと**: `bash -c 'set -u; FORK_VER=0.4.0; echo "フォーク $FORK_VER）です"'`
が `FORK_VER<全角括弧の先頭バイト>: unbound variable` で exit 127 になる
ことを、macOS の bash 3.2 と bash 5.2.37 の両方で実測。`scripts/check-vendor-diff.sh` の
該当メッセージは `${FORK_VER}` になっています。
