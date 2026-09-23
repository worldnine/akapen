# marks.ja.tape — marks モードのスクリーンキャスト、日本語版（VHS）
#
#   vhs examples/demo/marks.ja.tape        # リポジトリの根から
#
# mark for で「要点」を選ぶと琥珀が走り、自由入力で
# 文中に無い言葉（壊す）で聞くと、読み取り専用の約束と「やらないこと」が
# 光る。つまみは絞らず、光った 3 本を ]m で渡り、f で沈め、Esc で 1 枚ずつ
# 消す。題材は stages/design.v3.ja.md。
#
# 本物の判定器（Jev）を呼ぶ。先に 1 度 examples/demo/marks-run.sh で同じ
# 操作をしておくと、答えがキャッシュに入り、録画では待たずに光る。
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
Env MARKS_DOC "design.v3.ja.md"

# --- setup (hidden) --------------------------------------------------------
Hide
Type "examples/demo/marks-run.sh /tmp/akapen-marks-jp" Enter
Wait+Screen@15s /driftwatch/
Sleep 1s
Show

# --- 1. 文書を開いたところ --------------------------------------------------
Sleep 1.5s

# --- 2. mark for → Essential: 琥珀が走る ------------------------------------
Type "m"
Sleep 1.5s
Type "1"
Wait+Screen@30s /Essential · [0-9]+%/
Sleep 3s

# --- 3. 自由入力: 文中に無い言葉で聞く --------------------------------------
Type "/"
Sleep 800ms
Type "サーバーを壊す心配はない？"
Sleep 800ms
Enter
Wait+Screen@30s /心配はない？ · [0-9]+%/
Sleep 2s

# --- 4. 光った 3 本を渡り歩く（つまみは絞らない）-----------------------------
Type "]m"
Sleep 1.5s
Type "]m"
Sleep 1.5s
Type "]m"
Sleep 1.2s
Type "j"
Sleep 1s

# --- 5. ほかを沈める ----------------------------------------------------------
Type "f"
Sleep 3.5s

# --- 6. Esc で 1 枚ずつ消す ---------------------------------------------------
Escape
Sleep 1.2s
Escape
Sleep 2s
