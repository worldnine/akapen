# 本物のエージェントと撮る（案B）

偽エージェントではなく、実際のコーディングエージェントが右ペインで打ち返してくる
様子を録る手順。揺れ（応答時間・出力の暴れ）は録画側と台本側で吸収する。

## 構成

- herdr で 2 ペイン: 左 = akapen、右 = エージェント（pi / Claude Code / codex）
- akapen は `--send-agent` で起動 → `s` がそのままエージェントのペインに届く
- 録画は asciinema。**`-i 2`（idle 圧縮）でエージェントの思考時間が勝手に縮む**

## 手順

```console
$ asciinema rec -i 2 akapen-real.cast   # ここから全部録れる
$ REAL_AGENT=1 examples/demo/run.sh     # scratch を作って --send-agent で起動
```

`REAL_AGENT=1` のとき、scratch ディレクトリ（/tmp/akapen-demo）には
**改稿済みドラフト（stages/）を置かない**。カンニング防止 — 本物のエージェントが
`stages/design.v2.md` を見つけて丸写しすると台無しになる。

右ペインのエージェントは /tmp/akapen-demo で起動し、撮影**前**に下のブリーフィングを
渡しておく（この部分は録画に含めない。録画開始後は無言で待たせる）。

## エージェントへのブリーフィング（貼り付け用・英語）

```
You are revising design.md in this directory — a design document you
(the agent) drafted. I will review it with a red pen and send you
review comments, each referencing file and line numbers.

When comments arrive:
- Apply the requested changes to design.md directly. Edit the file;
  do not paste the new version into chat.
- No preamble, no summary, no questions. A one-line acknowledgement
  ("Revised §3 and §4.") is the most you may say.
- Keep every change minimal and scoped to what the comment asks.
- After editing, run: git add design.md && git commit -m "revise per review"
- Then wait silently for the next round.
```

コミットまでさせるのは、akapen のタイムマシン軸に COMMIT マーカー（◼）を
立たせるため。

## 撮影キュー

進行は [README.md](README.md) の 90 秒表と同じ。違いは 45–60 秒の待ちが
実時間になることだけで、`-i 2` が勝手に 2 秒へ詰める。

- 3〜4 テイク撮ってベストを採る。失敗したら `Ctrl+D` で rec を終了して最初から
- エージェントが脇道に逸れたテイクは捨てる（直そうとしない方が早い）
- モデルは速いもの推奨。応答が 10 秒でも 60 秒でも映像上は同じになるが、
  ライブ感を残すなら `-i 2` を `-i 4` に緩めてもいい

## 変換

```console
$ agg akapen-real.cast akapen-real.gif        # README 埋め込み用 GIF
$ agg --speed 1.5 akapen-real.cast fast.gif   # 全体をさらに 1.5 倍速
```

長尺（2 周目まで見せる版）は GIF より asciinema.org へアップして
プレイヤーリンクを貼る方が軽い: `asciinema upload akapen-real.cast`
