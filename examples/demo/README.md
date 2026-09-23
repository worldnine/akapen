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

## Your call に答えたら直った（`yourcall.tape` / `yourcall.ja.tape`、約 25 秒）

見せるのは、**読む層（marks、Jev）と赤入れの往復が 1 本でつながるところ**。
エージェントが書いた計画書に埋もれた「決めてほしい」を Your call で光らせ、
そこに短く答えて送り、直った版で問い直すと光る所が無くなる。

**書き換えは台本、Jev の判定は本物。** `s` の先は偽のエージェント
（`yourcall-agent.sh`、`fake-agent.sh` と同じ作り）で、2.5 秒待ってから先に用意した
直した版（`yourcall/plan.v2.md` / `plan.ja.v2.md`）に差し替えるだけで、答えを読んで
書き直してはいない。直した版は、デモで送る答えを素直に反映したものを人が書いた。
一方、光る箇所はどちらの版も本物の Jev が決めていて、**直した版の問い直しは撮るたびに
キャッシュの外で Jev を呼ぶ**（tape が `AKAPEN_CACHE_DIR` を空にしてから、隠した区間で
初版の答えだけを入れる）。

```console
$ vhs examples/demo/yourcall.tape      # 英語版 → out/yourcall.{gif,mp4}
$ vhs examples/demo/yourcall.ja.tape   # 日本語版 → out/yourcall.ja.{gif,mp4}
$ examples/demo/yourcall-run.sh        # 手で触るなら（/tmp/akapen-yourcall に展開）
```

- 題材は架空のアプリ **brambleway** の「写真のアップロードを要求の外へ出す」計画書
  （`yourcall/plan.md` / `plan.ja.md`、31 行、1 画面に収まる）。判断を求める所は
  「どこで動かすか（A か B か）」「フラグをいつ入れるか」「WebP もやるか」の 3 つで、
  3 つの節に 1 つずつ散らしてある
- **つまみは既定の 20 % から一度も動かさない**。13 Unit の 20 % は 3 本なので、
  初版の 3 本はつまみの上限と同じ数だが、4 位は 0.69 / 0.48 と離れていて、
  直した版の 0 本はつまみではなく足切り（0.20）で決まっている
- `r` で読み込むと、選んでいた問い（Your call）がそのまま直した版に当て直される。
  もう一度 `m 4` を押す必要は無い

| 秒 | 操作 | 映るもの |
| --- | --- | --- |
| 0–2 | 開いたまま | 計画書（判断を求める所は見た目では分からない） |
| 2–6 | `m` → `4` | Your call · 20% **3**。判断を求める 3 行の、問いの文に琥珀 |
| 6–17 | `]m` `c` 答え `Enter` × 3 | 3 行の下にコメントのカード |
| 17–20 | `s` | 送信トースト → ⚡ `file changed` |
| 20–24 | `r` | `! 3` と緑の印、`analyzing…` → Your call · 20% **0** |

送る答え（英 / 日）:

| 箇所 | 英 | 日 |
| --- | --- | --- |
| どこで動かすか | `A — reuse the mail worker.` | `A で。メール用ワーカーを使う。` |
| フラグをいつ入れるか | `Hold it until the freeze is over.` | `凍結が明けるまで待つ。` |
| WebP | `No WebP for now.` | `WebP は今回は要らない。` |

Your call の本数（2026-09-23、どれも空のキャッシュから本物の Jev）:

| 版 | ラン | 光った本数 | 上位のスコア |
| --- | ---: | --- | --- |
| 英・初版 | 3 | 3 / 3 / 3 | 判断を求める 3 行が 0.93〜0.95、4 位 0.66〜0.69 |
| 英・直した版 | 7 | 0 × 7 | 最高 0.13〜0.15 |
| 日・初版 | 3 | 3 / 3 / 3 | 判断を求める 3 行が 0.91〜0.92、4 位 0.46〜0.48 |
| 日・直した版 | 4 | 0 × 4 | 最高 0.15〜0.17 |

ランには撮影中の 1 回（英・日とも）を含む。

Essential（要点）との重なり: 初版の Essential が選ぶ 3 本は「なぜ」「試験」「範囲」
などが主で、3 本目がほぼ同点（英 0.65〜0.68、日 0.51〜0.54）で入れ替わる。
3 ラン中 2 回はそこに判断を求める 1 行が入って **1/3 重なり**、1 回は **0/3**。

選ばなかった直した版の言い方（英、フラグの行）:

- `The flag goes on after next week's release freeze is over.` — 4 ランとも
  その行だけが 0.21〜0.24 で残って **1 本**（足切りのすぐ上）
- `The flag stays off until next week's release freeze is over, then goes on.` —
  5 ランで 1 / 0 / 1 / 0 / 0 と揺れた（0.16〜0.20）
- `We switch the flag on once next week's release freeze is over.` — 1 本（0.21）
- `Decided: the flag goes on after next week's release freeze.` — 3 ランとも 0 本
  だが、答えの反映として「Decided:」を頭に付けるのは不自然なので採らなかった
- 採ったのは `The flag goes on once the release freeze ends next week; not this Friday.`
  — 退けた方の選択肢（今週金曜）まで書くと、もう決まったことに読める
