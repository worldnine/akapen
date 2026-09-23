# yourcall.tape — 「Your call に答えたら直った」のスクリーンキャスト（VHS、約 25 秒）
#
#   vhs examples/demo/yourcall.tape        # リポジトリの根から
#
# 題材はエージェントが書いた体の架空の計画書 brambleway
# （examples/demo/yourcall/plan.md、1 画面に収まる 31 行）。本文のあちこちに
# 「決めてほしい」が 3 つ埋もれていて、m → 4 Your call でその 3 行だけが光る。
# ]m で渡って c で短く答え、s で送ると、偽のエージェントが直した版に
# 差し替える（**書き換えは台本**。yourcall/plan.v2.md を先に用意してある）。
# r で読み込むと、変更の印（! 3 と緑）の上で Your call が問い直され、
# 光る本数が 3 → 0 になる。つまみは既定の 20 % から一度も動かさない。
#
# **Jev の判定は本物**。隠した区間で初版の答えだけをキャッシュに入れ、
# 直した版の問い直しは毎回キャッシュの外で Jev を呼ぶ（撮るたびに
# AKAPEN_CACHE_DIR を空にする）。
# Requires: vhs、target/release/akapen、Jev の鍵。
Output examples/demo/out/yourcall.gif
Output examples/demo/out/yourcall.mp4

Set Shell bash
Set Framerate 50
Set FontFamily "Guguru Sans Code"
Set FontSize 18
Set Width 1280
Set Height 800
Set Padding 12
Set TypingSpeed 45ms

Env AKAPEN_OPTS "--ime off"
Env AKAPEN_CACHE_DIR "/tmp/akapen-yourcall-cache"

# --- 下ごしらえ（録らない）: 初版の Your call だけをキャッシュに入れる -----
Hide
Type "rm -rf /tmp/akapen-yourcall-cache; examples/demo/yourcall-run.sh /tmp/akapen-yourcall" Enter
Wait+Screen@15s /brambleway/
Type "m"
Sleep 800ms
Type "4"
Wait+Screen@60s /Your call · 20% +3/
Type "q"
Sleep 1s
Type "clear; examples/demo/yourcall-run.sh /tmp/akapen-yourcall" Enter
Wait+Screen@15s /brambleway/
Sleep 1s
Show

# --- 1. エージェントが書いた計画書を開いたところ ---------------------------
Sleep 2s

# --- 2. m → 4 Your call: 判断を求めている 3 行だけが光る -------------------
Type "m"
Sleep 1.5s
Type "4"
Wait+Screen@30s /Your call · 20% +3/
Sleep 2.5s

# --- 3. 光った所へ ]m で渡り、c で短く答える --------------------------------
Type "]m"
Sleep 700ms
Type "c"
Sleep 500ms
Type "A — reuse the mail worker."
Sleep 500ms
Enter
Sleep 1s
Type "]m"
Sleep 700ms
Type "c"
Sleep 500ms
Type "Hold it until the freeze is over."
Sleep 500ms
Enter
Sleep 1s
Type "]m"
Sleep 700ms
Type "c"
Sleep 500ms
Type "No WebP for now."
Sleep 500ms
Enter
Sleep 1.5s

# --- 4. s で送る → エージェント（台本）が直す → ⚡ → r ----------------------
Type "s"
Wait+Screen@15s /⚡/
Sleep 1.5s
Type "r"

# --- 5. 直した版で Your call が問い直され、光る本数が 0 になる -------------
Wait+Screen@30s /Your call · 20% +0/
Sleep 3.5s
