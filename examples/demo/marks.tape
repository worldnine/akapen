# marks.tape — marks モードのスクリーンキャスト（VHS、約 22 秒）
#
#   vhs examples/demo/marks.tape        # リポジトリの根から
#
# 題材は架空の研究室の夜間バックアップ tidewater の設計メモ
# （examples/demo/marks/tidewater.md、1 画面に収まる 32 行）。
# Essential で決定・未決・保存期間に琥珀が引かれ、自由入力で文中に無い言葉
# で聞くと、要点とは別の場所 — 別棟への複製と、失敗したときの呼び出し —
# が光る。つまみは既定の 20 % から一度も動かさない。
#
# 本物の判定器（Jev）を呼ぶ。隠した区間で同じ操作を 1 度通して答えを
# キャッシュに入れるので、録る区間は待たずに光る。
# Requires: vhs、target/release/akapen、Jev の鍵。
Output examples/demo/out/marks.gif
Output examples/demo/out/marks.mp4

Set Shell bash
Set Framerate 50
Set FontFamily "Guguru Sans Code"
Set FontSize 18
Set Width 1280
Set Height 800
Set Padding 12
Set TypingSpeed 45ms

Env AKAPEN_OPTS "--ime off"

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
Type@5ms "Are we covered if the building burns down?"
Enter
Wait+Screen@60s /burns down\? · 20% +[0-9]/
Type "/"
Sleep 300ms
Type@5ms "Who gets woken up when it breaks?"
Enter
Wait+Screen@60s /breaks\? · 20% +[0-9]/
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
Type "Are we covered if the building burns down?"
Sleep 600ms
Enter
Wait+Screen@30s /burns down\? · 20% +[0-9]/
Sleep 2.5s

# --- 4. 自由入力 2: 別の問いで、別の場所が光る ------------------------------
Type "/"
Sleep 600ms
Type "Who gets woken up when it breaks?"
Sleep 600ms
Enter
Wait+Screen@30s /breaks\? · 20% +[0-9]/
Sleep 2s

# --- 5. ほかを沈めて答えだけ残し、Esc で 1 枚ずつ戻す ------------------------
Type "f"
Sleep 2.5s
Escape
Sleep 800ms
Escape
Sleep 1.5s
