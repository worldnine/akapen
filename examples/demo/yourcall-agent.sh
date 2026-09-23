#!/bin/sh
# yourcall-agent.sh — 台本どおりに直す偽のエージェント（yourcall-run.sh が写す）。
#
# fake-agent.sh と同じ作り: akapen が `s` の文面を stdin に流すので、それを
# comments.txt に残してすぐ返す。答えを読んで書き直しているのではない —
# 直した版（stages/<doc>）は、デモで送るコメントを素直に反映したものを
# 人が書いてある。
#
# 差し替えの担い手は 2 通り:
#   - YOURCALL_AGENT_PANE=1（yourcall-split.sh の下の枠）: ここは受け取るだけ。
#     下の枠の yourcall-agent-pane.sh が comments.txt を拾い、進み具合と差分を
#     流してから差し替える
#   - それ以外（yourcall-run.sh を単独で使うとき）: 裏で 2.5 秒「考えた」あとに
#     黙って差し替えて commit する
set -eu
cd "$(dirname "$0")"
doc=$1

# yourcall-run.sh が作った scratch（自前の .git がある）でしか動かない。
[ -e .git ] || {
    echo "yourcall-agent.sh: not a demo scratch dir — use yourcall-run.sh" >&2
    exit 1
}

# 下の枠が書きかけを読まないよう、書き終えてから名前を付ける。
cat > comments.txt.part
mv comments.txt.part comments.txt
[ -f "stages/$doc" ] || exit 0 # 2 回目以降は受け取るだけ
[ "${YOURCALL_AGENT_PANE:-}" = 1 ] && exit 0

(
    sleep 2.5
    mv "stages/$doc" "$doc"
    git add "$doc"
    git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
        commit -q -m "agent: apply your answers"
) < /dev/null > /dev/null 2>&1 &
