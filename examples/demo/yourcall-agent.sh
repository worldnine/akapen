#!/bin/sh
# yourcall-agent.sh — 台本どおりに直す偽のエージェント（yourcall-run.sh が写す）。
#
# fake-agent.sh と同じ作り: akapen が `s` の文面を stdin に流すので、それを
# comments.txt に残してすぐ返し、裏で 2.5 秒「考えた」あとに先に用意した
# 直した版（stages/<doc>）へ差し替えて commit する。答えを読んで書き直して
# いるのではない — 直した版は、デモで送るコメントを素直に反映したものを
# 人が書いてある。
set -eu
cd "$(dirname "$0")"
doc=$1

# yourcall-run.sh が作った scratch（自前の .git がある）でしか動かない。
[ -e .git ] || {
    echo "yourcall-agent.sh: not a demo scratch dir — use yourcall-run.sh" >&2
    exit 1
}

cat > comments.txt
[ -f "stages/$doc" ] || exit 0 # 2 回目以降は受け取るだけ

(
    sleep 2.5
    mv "stages/$doc" "$doc"
    git add "$doc"
    git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
        commit -q -m "agent: apply your answers"
) < /dev/null > /dev/null 2>&1 &
