# yourcall.tape — 「Your call に答えたら直った」のスクリーンキャスト（VHS、約 33 秒）
#
#   vhs examples/demo/yourcall.tape        # リポジトリの根から
#
# 画面は tmux で上下 2 枠（yourcall-split.sh）: 上が akapen、下が偽の
# エージェントの端末。題材はエージェントが書いた体の架空の計画書 brambleway
# （examples/demo/yourcall/plan.md、1 画面に収まる 31 行）。本文のあちこちに
# 「決めてほしい」が 3 つ埋もれていて、m のダイアログで ▸ を 4 Your call まで
# 動かして選ぶと、その 3 行だけが光る。]m で渡って c で短く答え、s で送ると、
# 下の枠に受け取った答え・進み具合・差分・done が流れ、直した版への差し替えを
# 上の akapen が拾って ⚡ を出す（**書き換えは台本**。下の枠の文字も差分も
# 台本で、yourcall/plan.v2.md を先に用意してある）。r で読み込むと、変更の印
# （! 3 と緑）の上で Your call が問い直され、光る本数が 3 → 0 になる。
# つまみは既定の 20 % から一度も動かさない。
#
# **Jev の判定は本物**。隠した区間で初版の答えだけをキャッシュに入れ、
# 直した版の問い直しは毎回キャッシュの外で Jev を呼ぶ（撮るたびに
# AKAPEN_CACHE_DIR を空にする）。
# Requires: vhs、tmux、target/release/akapen、Jev の鍵。
Output examples/demo/out/yourcall.gif
Output examples/demo/out/yourcall.mp4

Set Shell bash
Set Framerate 50
Set FontFamily "Guguru Sans Code"
Set FontSize 18
# 上の枠（akapen）を単独のときと同じ 123 × 36 に保ち、下に agent の 14 行を足す
Set Width 1280
Set Height 1120
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
Type "clear; examples/demo/yourcall-split.sh /tmp/akapen-yourcall" Enter
Wait+Screen@15s /agent/
Wait+Screen@15s /brambleway/
Sleep 1s
Show

# --- 1. エージェントが書いた計画書を開いたところ（下の agent は待っている） -
Sleep 2s

# --- 2. m のダイアログで ▸ を 4 Your call まで動かして選ぶ ------------------
Type "m"
Sleep 1.2s
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Type "j"
Sleep 900ms
Enter
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

# --- 4. s で送る → 下の agent（台本）が受け取って直す → 上に ⚡ → r ---------
Type "s"
Wait+Screen@20s /⚡/
Sleep 2.5s
Type "r"

# --- 5. 直した版で Your call が問い直され、光る本数が 0 になる -------------
Wait+Screen@30s /Your call · 20% +0/
Sleep 3.5s

# --- 後片付け（録らない）: akapen を閉じると tmux ごと消える ---------------
Hide
Type "q"
Sleep 1s
