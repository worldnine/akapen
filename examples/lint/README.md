# linter の指摘を Review の候補にする（`--lint-cmd`）

akapen の Review（`R`）は、候補を**linter に出させる**ことができます。判定は linter
がして、akapen はその後ろの流れ — 一覧・`x` で捨てる・`a` でコメントにする・`s` で
送る・一覧から `e` で直す・直したあとの引き継ぎ — を受け持ちます。どんな指摘を出す
かは、あなたの linter の設定（`~/.textlintrc` など）で決めます。

設計の話は `docs/design/marks-only-and-review-mode.md` 4 節「判定の出どころとしての
linter」にあります。

## 使い方

```sh
# textlint
akapen --lint-cmd 'python3 /path/to/akapen/examples/lint/textlint-diagnostics.py' doc.md

# natural-japanese（`japanese` スキルの lint.py）
export NATURAL_JAPANESE_LINT_PY=/path/to/japanese/scripts/lint.py
akapen --lint-cmd 'python3 /path/to/akapen/examples/lint/natural-japanese-diagnostics.py' doc.md
```

毎回書くのが面倒なら、環境変数 `AKAPEN_LINT_CMD` に同じ文字列を入れておけば既定に
なります（`--lint-cmd` が勝ちます。空にすれば外れます）。`--semantic-cmd` は要り
ません。

`R` を押すと linter が走り、答えが揃うまで一覧のタイトルは `review · analyzing…`
になります。一覧を開いたことがあれば、開いたままの reload（`r`・`e` から戻った
とき・外で書き換えたとき）でも走り直します。

### akapen がすること・しないこと

- `sh -c` で走らせます。**作業ディレクトリは文書のあるディレクトリ**で、文書の
  **絶対パスを最後の引数**として足します（`'<cmd>' "$@"` の形で渡すので、パスに
  空白があっても引用は要りません）
- linter の設定ファイルは**探しません**。`.textlintrc` などの探索は linter 自身の
  仕事です（textlint は作業ディレクトリから上へ探します）
- **終了コードは見ません。** textlint は指摘があると exit 1 で終わります。標準出力が
  下の形の JSON なら成功、そうでなければフラッシュに
  `lint: output is not diagnostics JSON` と出して、一覧も「0 件」とは言いません
- 見切りは `--semantic-cmd` と同じです（30 秒黙ったら、または 10 分で止めます）
- linter はディスクのファイルを読むので、画面の本文と食い違っているとき（外の
  書き換えを `r` で読み込む前、タイムマシンで過去の版を見ているとき）は走らせず、
  `lint: the file on disk is not the text on screen` と出します

### `--config` を明示した方がよい場合

textlint は作業ディレクトリ（＝文書のあるディレクトリ）から**上へ**設定を探します。
`~/.textlintrc` はホームの下の文書にしか効きません。ホームの外の文書（`/tmp` の
下、外付けディスク、別ユーザーのディレクトリ）を開くときは、設定を明示します。

```sh
akapen --lint-cmd "python3 /path/to/textlint-diagnostics.py --config $HOME/.textlintrc" /tmp/doc.md
```

`textlint-diagnostics.py` は最後の 1 つ（文書）以外の引数を、そのまま textlint へ
渡します。textlint のコマンドは環境変数 `TEXTLINT` で変えられます（既定
`textlint`。例: `TEXTLINT='npx textlint'`）。

## 受け付ける形: LSP の Diagnostic

akapen が読むのはこの 1 つだけです。自分の linter をつなぐときは、この形を
標準出力に出してください。

```json
{"diagnostics": [
  {"range": {"start": {"line": 71, "character": 25}, "end": {"line": 71, "character": 26}},
   "message": "弱い表現: \"かも\" が使われています。",
   "source": "textlint", "code": "ja-no-weak-phrase", "severity": 1}
]}
```

- `line` は 0 始まり、`character` は **UTF-16 のコード単位**（LSP の既定）です
- 範囲が壊れている指摘（行の外・start > end・サロゲートの途中）は、その 1 件だけ
  捨てます
- 幅 0 の範囲（start = end）は、その行の末尾まで広げます
- `severity` は読みますが、いまは表示に使っていません

候補の rule は `<source>/<code>` になり（例 `textlint/ja-no-weak-phrase`）、
**範囲は指摘の範囲そのまま**です（下線もそこに引きます）。

## 候補としての見え方

一覧の 1 行は、本文の先頭ではなく**理由**を見せます。

```text
▸   L72 · ja-no-weak-phrase · 弱い表現: "かも" が使われています。
    L98 · ja-no-redundant-expression · 【dict5】 "対応を行う"は冗長な表現です。…
```

`a` で accept したコメントの本文は

```text
lint: textlint/ja-no-weak-phrase — 弱い表現: "かも" が使われています。
```

で、複数行のメッセージは 1 行に畳みます。`s` / `y` で送る文面には、lint の
コメントがあるときだけ短い段落が先頭に付きます（直し方は指摘に従う・事実を足さ
ない・範囲の外は触らない）。文面は `assets/review-contract-lint.md` で、
`$XDG_CONFIG_HOME/akapen/review-contract-lint.md` を置けば上書きできます。

dismiss・差分越しの引き継ぎ・直したコメントの片付け・一覧から `e` は、Jev の
ルールの候補とまったく同じに動きます（鍵は rule と範囲です）。

## Jev のルールと一緒に使う

Jev のルール（`assets/review-rules.json`）は**既定では 1 本も有効ではありません**。
`$XDG_CONFIG_HOME/akapen/review-rules.json` か `--review-rules` で `enabled: true` に
して `--semantic-cmd` と一緒に使えば、Jev の候補と lint の候補が同じ一覧に文書順で
並びます。どちらも無いまま `R` を押すと
`no review source — set --lint-cmd or enable a rule` と出ます。

## スクリプト

| ファイル | 何をする |
| --- | --- |
| `textlint-diagnostics.py [textlint の引数...] <file>` | textlint を `--format json` で走らせ、上の形に直す。位置は textlint の `range`（UTF-16 の添字）から、無ければ `loc` から。`code` はルール id の最後の区切りで、元の id は `data.ruleId` |
| `natural-japanese-diagnostics.py [--lint-py <lint.py>] <file>` | `lint.py --json` の `findings` を直す。行単位の指摘は**その行全体**を範囲にする。`code` は `category`、`message` は `detail`。`lint.py` の場所は `--lint-py` か `NATURAL_JAPANESE_LINT_PY`、走らせ方は `NATURAL_JAPANESE_RUNNER`（既定 `uv run`、`uv` が無ければ `python3`） |

どちらも Python 標準ライブラリだけで動きます。テストは実物の textlint も lint.py
も呼びません。

```sh
python3 -m pytest examples/lint -q
```

## micro エディタからも使う（`--unix-oneline`）

`textlint-diagnostics.py --unix-oneline` は、JSON の代わりに 1 件 1 行の

```text
<file>:<行>:<桁>: <メッセージ> (<ルール id>)
```

を出します。行・桁は 1 始まり、桁は**文字単位**（micro の位置の数え方）で、複数行の
メッセージ（ja-technical-writing の「理由:」「修正:」など）は 1 行に畳みます。
micro の linter プラグインにそのまま渡せる形です。

`~/.config/micro/init.lua` に次のように書けば、Markdown を保存するたびに走ります
（パスは自分の環境に合わせてください。スクリプトには実行権限が要ります）。

```lua
function init()
    linter.makeLinter(
        "textlint",
        "markdown",
        "/path/to/akapen/examples/lint/textlint-diagnostics.py",
        {"--unix-oneline", "%f"},
        "%f:%l:%c: %m"
    )
end
```

micro は linter を**micro を起動したディレクトリ**で走らせるので、ホームの外で
起動するなら `{"--unix-oneline", "--config", "/path/to/.textlintrc", "%f"}` の
ように設定を明示してください。
