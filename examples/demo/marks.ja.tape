# marks.ja.tape — marks モードのスクリーンキャスト、日本語版（VHS、約 22 秒）
#
#   vhs examples/demo/marks.ja.tape        # リポジトリの根から
#
# 題材は examples/demo/marks/tidewater.ja.md（英語版と同じ内容、1 画面）。
# Essential で決定・保存期間などに琥珀が引かれ、自由入力で文中に無い言葉
# で聞くと、要点とは別の場所 — 1 回の流れと、失敗したときの知らせ — が
# 光る。つまみは既定の 20 % から一度も動かさない。
#
# 英語版の「火事」の問いは日本語では使わない。日本語版の Essential は
# 別棟の 2 行目を 4 ラン中 3 回拾うので、火事の答えと重なる。
#
# 本物の判定器（Jev）を呼ぶ。隠した区間で同じ操作を 1 度通して答えを
# キャッシュに入れるので、録る区間は待たずに光る。
# Requires: vhs、target/release/akapen、Jev の鍵。
Output examples/demo/out/marks.ja.gif
Output examples/demo/out/marks.ja.mp4

Set Shell bash
Set Framerate 50
Set FontFamily "Guguru Sans Code"
Set FontSize 18
Set Width 1280
Set Height 800
Set Padding 12
Set TypingSpeed 45ms

Env AKAPEN_OPTS "--ime off"
Env MARKS_DOC "tidewater.ja.md"

# --- 下ごしらえ（録らない）: 同じ操作を 1 度通して答えをキャッシュに入れる --
#
# キャッシュの鍵にはコマンド行（この checkout の絶対パス）が入るので、別の
# checkout で温めたものは当たらない。ここで温めれば、録る区間は待たずに光る。
Hide
Type "examples/demo/marks-run.sh /tmp/akapen-marks" Enter
Wait+Screen@15s /tidewater/
Type "m"
Sleep 800ms
Type "1"
Wait+Screen@60s /Essential · 20% +[0-9]/
Type "/"
Sleep 300ms
Type@5ms "実験の邪魔にならない？"
Enter
Wait+Screen@60s /邪魔にならない？ · 20% +[0-9]/
Type "/"
Sleep 300ms
Type@5ms "止まったら誰に知らせが行く？"
Enter
Wait+Screen@60s /知らせが行く？ · 20% +[0-9]/
Type "q"
Sleep 1s
Type "clear; examples/demo/marks-run.sh /tmp/akapen-marks" Enter
Wait+Screen@15s /tidewater/
Sleep 1s
Show

# --- 1. 文書を開いたところ（1 画面に収まっている）--------------------------
Sleep 1.5s

# --- 2. mark for → Essential: 決定・未決・保存期間に琥珀が引かれる ----------
Type "m"
Sleep 1.2s
Type "1"
Wait+Screen@30s /Essential · 20% +[0-9]/
Sleep 2.5s

# --- 3. 自由入力 1: 文中に無い言葉で聞く（つまみは 20 % のまま）-------------
Type "/"
Sleep 600ms
Type "実験の邪魔にならない？"
Sleep 600ms
Enter
Wait+Screen@30s /邪魔にならない？ · 20% +[0-9]/
Sleep 2.5s

# --- 4. 自由入力 2: 別の問いで、別の場所が光る ------------------------------
Type "/"
Sleep 600ms
Type "止まったら誰に知らせが行く？"
Sleep 600ms
Enter
Wait+Screen@30s /知らせが行く？ · 20% +[0-9]/
Sleep 2s

# --- 5. ほかを沈めて答えだけ残し、Esc で 1 枚ずつ戻す ------------------------
Type "f"
Sleep 2.5s
Escape
Sleep 800ms
Escape
Sleep 1.5s
