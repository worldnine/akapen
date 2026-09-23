#!/bin/sh
# yourcall-run.sh — 「Your call に答えたら直った」デモを起動する。
#
#   examples/demo/yourcall-run.sh [demo-dir]
#
# エージェントが書いた体の架空の計画書 brambleway（yourcall/plan.md）を
# scratch に写して git に 1 世代積み、本物の判定器（jev-annotate.py）と
# 台本どおりに直す偽のエージェント（yourcall-agent.sh）つきで akapen を開く。
# Jev の判定は本物、書き換えは台本（先に用意した yourcall/plan.v2.md に
# 差し替えるだけ）である。Jev を呼ぶので鍵が要る。
# 題材は YOURCALL_DOC で替えられる（既定 plan.md、日本語版は plan.ja.md）。
# どちらも 1 画面（123×36）に収まる長さにしてある。
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
demo_dir=${1:-/tmp/akapen-yourcall}
doc=${YOURCALL_DOC:-plan.md}
stem=${doc%.md}

if [ -n "${AKAPEN:-}" ]; then
    akapen=$AKAPEN
elif [ -x "$repo_root/target/release/akapen" ]; then
    akapen=$repo_root/target/release/akapen
else
    akapen=akapen
fi

rm -rf "$demo_dir"
mkdir -p "$demo_dir/stages"
cp "$script_dir/yourcall/$doc" "$demo_dir/$doc"
# 直した版は plan.md → plan.v2.md、plan.ja.md → plan.ja.v2.md
cp "$script_dir/yourcall/$stem.v2.md" "$demo_dir/stages/$doc"
cp "$script_dir/yourcall-agent.sh" "$demo_dir/agent.sh"
chmod +x "$demo_dir/agent.sh"

cd "$demo_dir"
git init -q
git add "$doc"
git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
    commit -q -m "agent: draft the brambleway upload plan"

# shellcheck disable=SC2086 # AKAPEN_OPTS is intentionally word-split
exec "$akapen" "$doc" \
    --semantic-cmd "python3 $repo_root/examples/semantic/jev-annotate.py" \
    --send-cmd "./agent.sh $doc" \
    ${AKAPEN_OPTS:-}
