#!/bin/sh
# yourcall-split.sh — 「Your call に答えたら直った」を上下 2 枠で開く（yourcall.tape が使う）。
#
#   examples/demo/yourcall-split.sh [demo-dir]
#
# 上の枠が akapen（yourcall-run.sh そのまま）、下の枠が偽のエージェントの端末
# （yourcall-agent-pane.sh）。`s` で送ると下の枠に受け取った答え・進み具合・
# 差分・done が流れ、差し替えを上の akapen が拾って ⚡ を出す。
# **下の枠の文字も差分も台本**（yourcall-agent-pane.sh の頭を参照）。
#
# 上下に分けるのは、akapen 側を単独のときと同じ 123 桁 × 36 行に保つため。
# 題材には 111 桁の行があり、左右に割ると折り返して 1 画面に収まらない。
# 上の枠の行数は AKAPEN_ROWS（既定 36）、残りが下の枠。akapen を q で閉じると
# tmux ごと片付く。tmux は専用のソケット（-L akapen-yourcall）と専用の設定で動く。
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
demo_dir=${1:-/tmp/akapen-yourcall}
doc=${YOURCALL_DOC:-plan.md}
rows=${AKAPEN_ROWS:-36}
tmux="tmux -L akapen-yourcall -f $script_dir/yourcall-tmux.conf"

# 前回の残り（tmux と、下の枠が拾ってしまう comments.txt）を消してから始める。
$tmux kill-server 2>/dev/null || true
rm -f "$demo_dir/comments.txt"

$tmux new-session -d -s yourcall -x "$(tput cols)" -y "$(tput lines)" \
    -e YOURCALL_AGENT_PANE=1 \
    "'$script_dir/yourcall-run.sh' '$demo_dir'; $tmux kill-server"
$tmux split-window -v -t yourcall -l "$(($(tput lines) - rows - 1))" \
    "'$script_dir/yourcall-agent-pane.sh' '$demo_dir' '$doc'"
$tmux select-pane -t yourcall.0
exec $tmux attach -t yourcall
