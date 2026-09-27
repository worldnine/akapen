# akapen で作業する人（と、エージェント）へ

## このリポジトリは public です

Semantic Reading Layer の判定品質は実文書でしか測れないので、手元の業務文書を
測定対象に使っています。**その本文を、ファイルにもコミットメッセージにも
書かないでください。**

- 書いてよい: 件数・比率・Unit 数・tokens・ラン数・構造、`業務議事録` のような
  総称、「設計上の制約を述べた一文」のような種類
- 書いてはいけない: **本文の引用、取引先名・個人名・製品名、ファイル名やパス**、
  日付と固有名詞の組

本文が判断材料として要るときは `~/.local/share/akapen/evidence/`（リポジトリの
外）に置き、リポジトリ側には件数と「証拠はそこにある」だけを書きます。応答 JSON
は `state` に文書の全文が入るので、リポジトリには絶対に置けません。

詳しくは `docs/gotchas/public-repo.md`「業務文書の本文はこのリポジトリに書かない」。
**push した後では取り消せません**（未 push のうちなら、履歴ごと書き換えて消せます）。

**解析結果のキャッシュも repo の外です。** `--semantic-cmd` の答えは
`~/.cache/akapen/semantic/` に置きます（`$XDG_CACHE_HOME` / `AKAPEN_CACHE_DIR`
で移せます）。リポジトリの中にも文書の隣にも置かない、ファイルは 0600・
ディレクトリは 0700。実測では**本文は 1 バイトも入りません**（中身は Atom の
byte range と kind、Unit の添字・スコア・核だけ）が、節の構造とどこが要点かは
業務文書を語るので、本文と同じ扱いにしてあります。消すのは
`akapen --semantic-cache-clear`。
なお `~/.cache/akapen/snapshots/` の方は**文書の全文**を持っていて、そちらは
0600 ではありません（この層より前からある別のキャッシュです）。

## 先に読むもの

- `docs/README.md` — 何がどこにあるか
- `docs/design/semantic-reading-layer.md` — 設計の正典。この層を触る前に読む
- `docs/design/jev.md` — **Jev は LLM ではない**。System One model
- `docs/gotchas.md` — 地雷。同じ道を戻らないために

## ゲート

```sh
cargo test --locked --workspace
cargo clippy -p akapen -p semantic-reading --all-targets -- -D warnings
python3 -m pytest examples/semantic -q
./scripts/check-vendor-diff.sh
```

`cargo clippy --workspace --all-targets -- -D warnings` は**緑になったことが
ありません**（vendored fork のテストコード）。判定には使わないこと —
`docs/gotchas/semantic-reading.md` に呼び方の表があります。
