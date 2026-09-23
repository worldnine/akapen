# akapen screencast demo

スクリーンキャスト（60〜90秒）を**毎回同じ挙動で**撮り直すためのデモ一式。
本物のエージェントは使わず、台本どおりに改稿する偽エージェント
（`fake-agent.sh`）を `--send-cmd` に差す。

## 仕組み

- 題材は架空のツール **driftwatch** の設計書 `design.md`（エージェントが書いた体・英語）
- 改稿後の版は `stages/design.v2.md`, `v3.md` として先に用意してある
- `s` で送るとエクスポートが `fake-agent.sh` の stdin に渡り、
  2.5 秒「考えた」あとに次の版を `design.md` へ上書きして git commit する
- akapen が変更を検知（⚡）→ `r` で新稿ロード → 緑/赤マーク、までが毎回再現される

## 実行

```console
$ cargo build --release          # まだなら
$ examples/demo/run.sh           # /tmp/akapen-demo に展開して起動
```

撮り直しは `run.sh` をもう一度実行するだけ（毎回まっさらに作り直す）。
デモ先ディレクトリは引数で変更可、バイナリは `AKAPEN=/path/to/akapen` で指定可。

## 台本（v1 に仕込んだ「赤を入れる箇所」）

Draft 1 には狙って 3 つの欠陥が入れてある:

| 箇所 | 欠陥 | コメント例（英語で書くと絵になる） |
| --- | --- | --- |
| §3 Change detection | 500ms ポーリングを自慢している | "Polling every 500ms per file won't scale — use FSEvents/inotify and keep polling as a fallback." |
| §4 CLI | `fix --auto` の例が §1 の read-only 宣言と矛盾 | "This contradicts §1 — driftwatch is read-only. Drop the fix example." |
| §5 Why driftwatch will change your life | 宣伝口調で設計書の体をなしていない | "Delete this section. This is a design doc, not a landing page." |

Draft 2（1回目の送信後）:

- §3 がイベント駆動に書き直される（**緑マーク**）
- §4 から `fix --auto` の行が消え、`watch` の行が入る（緑＋赤）
- §5 が丸ごと消える（**純削除 → `n`/`N` の削除フォーカスが映る**）
- ただし §6 Roadmap に「auto-fix mode enabled by default」が残っている ← **2周目のネタ**

Draft 3（2回目の送信後）:

- Roadmap の auto-fix が `watch` モードに置き換わり、§5 として Non-goals が入る

## 撮影キュー（90秒）

| 秒 | 操作 | 映るもの |
| --- | --- | --- |
| 0–10 | 起動したまま `j` で流し読み | 設計書がレンダリングされた画面（表・コードブロック） |
| 10–30 | §3 で `v` → 行選択 → `c` → コメント | インラインカード |
| 30–45 | §4 と §5 にもコメント → `s` | 送信トースト |
| 45–60 | ⚡ 点灯 → `r` | 新稿に緑/赤マーク |
| 60–80 | `n` / `N` で差分を渡り歩く | §5 の削除フォーカスも通る |
| 80–90 | `Left` で前の稿へ | タイムマシン（採点簿）で「直った」ことを確認 |

2周目（Roadmap への指摘 → Draft 3）まで見せるなら +30 秒。
キー操作だけで通るので英語圏向けの字幕は `mark → send → ⚡ → check` の 4 語で足りる。

## ファイル

```
examples/demo/
├── README.md          このファイル
├── run.sh             デモ環境を作って akapen を起動（＝リセット）
├── fake-agent.sh      偽エージェント（--send-cmd で刺さる）
└── stages/
    ├── design.v1.md   初稿（欠陥3つ入り）
    ├── design.v2.md   1回目の改稿
    └── design.v3.md   2回目の改稿
```

受け取ったコメントはデモディレクトリに `comments-round-N.txt` として残る。

## marks モードのデモ（`marks.tape` / `marks.ja.tape`、約 18 秒）

見せるのは 2 つ: **素早くマークされること**と、**文中に無い言葉で聞いても
答えの箇所が光ること**。本物の判定器（Jev）を呼ぶので鍵が要る。

```console
$ vhs examples/demo/marks.tape       # 英語版 → out/marks.{gif,mp4}
$ vhs examples/demo/marks.ja.tape    # 日本語版 → out/marks.ja.{gif,mp4}
```

- 題材は架空の研究室の夜間バックアップ **tidewater** の設計メモ
  （`marks/tidewater.md` / `marks/tidewater.ja.md`）。節ごとに話題が分かれて
  いて、**1 画面（123×36）に収まる 32 行**なので、答えが画面の外に出ない
  （`]m` を使わずに済む）。14 Unit、つまみ 20 % で 3 本
- **つまみは既定の 20 % から一度も動かさない**
- tape の隠した区間で同じ操作を 1 度通して答えをキャッシュに入れる。
  キャッシュの鍵にはコマンド行（checkout の絶対パス）が入るので、撮る
  checkout で温める必要がある

問いは、要点（Essential）で光る行と重ならず、問いの語が文中に出てこない
ものを選んだ（2026-09-23、`jev-annotate.py` を直接 2〜4 ラン）:

| 版 | 場面 | 光る行 | スコア | 要点との重なり |
| --- | --- | --- | --- | ---: |
| 英 | Essential | 決定・未決・保存期間から 3 本 | 0.72〜0.80 | — |
| 英 | `Are we covered if the building burns down?` | Off-site copy の 2 行 | 0.88 / 0.77 | 0 / 2 |
| 英 | `Who gets woken up when it breaks?` | When a run fails の 2 行 | 0.95 / 0.30 | 0 / 2 |
| 日 | Essential | 決定・保存期間・別棟の 2 行目から 3 本 | 0.73〜0.78 | — |
| 日 | `実験の邪魔にならない？` | 1 回の流れの 3 行 | 0.78 / 0.42 / 0.29 | 0 / 3 |
| 日 | `止まったら誰に知らせが行く？` | 失敗したときの 2 行 | 0.93 / 0.85 | 0 / 2 |

「要点との重なり」は、Essential が 4 ランで選んだ行の**和集合**との重なり。

選ばなかった問い:

- 日本語の `建物が燃えたらどうなる？`（英語版の火事の問いに当たるもの） —
  日本語の Essential が別棟の 2 行目を 4 ラン中 3 回拾うので、1/2 重なる
- `I deleted my thesis by accident — can I undo that?` /
  `論文を誤って削除した。助かる？` — 取り戻す節に当たるが、3 本目に保存期間
  （要点側）が入って 1/3 重なる
- `火事で全部燃えたらどうなる？` / `災害に遭っても大丈夫？` — 3 本目に
  取り戻す節が雑音として入る（0.31〜0.33）
- `Is our stuff readable by outsiders?` / `外の人に中身を見られない？` —
  暗号化の行より取り戻す節が上に来る
- `電源を切ったままだと？` — 当たるが 0.44 と弱い
- `深夜に誰が起こされる？` / `夜中に叩き起こされるのは誰？` — 当たるが、
  「夜」がタイトルの「夜間」と重なる
