#!/bin/sh
# yourcall-agent-pane.sh — 偽のエージェントの端末（yourcall-split.sh の下の枠）。
#
#   yourcall-agent-pane.sh <demo-dir> <doc>
#
# akapen が `s` で送った文面（yourcall-agent.sh が残す comments.txt）を待ち、
# 届いたら「受け取った → 読んでいる → 直す → 差分 → done」を流して、
# 先に用意した直した版（stages/<doc>）に差し替えて commit する。
# その差し替えを akapen が拾って ⚡ file changed を出す。
#
# **流す文字も差分も台本である。** 受け取った答えの一覧だけは comments.txt から
# 抜き出しているが、読んで書き直してはいない — 差分は先に用意した 2 つの版の
# 差をそのまま出しているだけ。見た目はどの製品にも似せない、素の `agent`。
set -eu
demo_dir=$1
doc=$2

case $doc in
*.ja.md)
    msg_wait="akapen からのコメントを待っています…"
    msg_recv="件のコメントを受け取りました"
    msg_read="$doc を読んでいます"
    msg_edit="$doc を直しています"
    msg_done="done — 3 行を直して commit しました"
    ;;
*)
    msg_wait="waiting for review comments from akapen…"
    msg_recv="comment(s) received"
    msg_read="reading $doc"
    msg_edit="editing $doc"
    msg_done="done — 3 lines changed, committed"
    ;;
esac
dim=$(printf '\033[2m')
bold=$(printf '\033[1m')
off=$(printf '\033[0m')

# 素っ気ない行頭。製品名もロゴも無い。
say() { printf '%s %s\n' "${bold}agent${off}" "$*"; }

# 進み具合: 点を 1 つずつ足して、最後に ok
progress() {
    printf '%s %s ' "${bold}agent${off}" "$1"
    i=0
    while [ "$i" -lt "$2" ]; do
        sleep 0.35
        printf '.'
        i=$((i + 1))
    done
    printf ' %sok%s\n' "$dim" "$off"
}

clear
say "${dim}${msg_wait}${off}"

# yourcall-run.sh が scratch を作り直し、akapen が送るまで待つ。
until [ -e "$demo_dir/.git" ] && [ -f "$demo_dir/comments.txt" ]; do
    sleep 0.2
done
cd "$demo_dir"
clear

# 受け取った文面の要点: 「plan.md:12」の行と、その下の答え 1 行。
# comments.txt は「path:N / N: 本文の行 / 答え / 空行」の繰り返し。
count=$(grep -c "^/.*:[0-9][0-9]*$" comments.txt || true)
say "${count} ${msg_recv}"
awk -v dim="$dim" -v off="$off" '
    /^\/.*:[0-9]+$/ { n = split($0, p, "/"); where = p[n]; state = 1; next }
    state == 1 { state = 2; next }                 # 本文の行（引用）は省く
    state == 2 && NF { printf "  %s%-11s%s %s\n", dim, where, off, $0; state = 0 }
' comments.txt
sleep 1.2

progress "$msg_read" 4
progress "$msg_edit" 3
sleep 0.3

# 差分: 先に用意した直した版との差。見出し（diff --git / index / --- / +++）は
# 落として、- を赤・+ を緑で 1 か所（@@）ずつ間を置いて出す。
red=$(printf '\033[31m')
green=$(printf '\033[32m')
git --no-pager diff --no-index --no-color -U0 -- "$doc" "stages/$doc" |
    while IFS= read -r line; do
        case $line in
        "diff --git"* | "index "* | "--- "* | "+++ "*) ;;
        @@*) sleep 0.5 ;;
        -*) printf '  %s%s%s\n' "$red" "$line" "$off" ;;
        +*) printf '  %s%s%s\n' "$green" "$line" "$off" ;;
        esac
    done
sleep 0.3

mv "stages/$doc" "$doc"
git add "$doc"
git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
    commit -q -m "agent: apply your answers"
say "$msg_done"

# 上の枠（akapen）が閉じるまで居残る。yourcall-split.sh が片付ける。
while :; do sleep 60; done
