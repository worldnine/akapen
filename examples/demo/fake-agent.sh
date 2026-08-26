#!/bin/sh
# fake-agent.sh — a scripted "coding agent" for the akapen screencast demo.
#
# akapen pipes the `s` export to this script's stdin (--send-cmd) and
# waits for it to exit, so we return immediately and do the actual
# "revision" in a detached background process: pause as if thinking,
# then swap in the next pre-baked draft and commit it. akapen notices
# the change (⚡) exactly like it would with a real agent.
set -eu
cd "$(dirname "$0")"

# Only operate inside a scratch dir prepared by run.sh (it has its own
# .git). Running from examples/demo directly would let git walk up and
# commit into the akapen repository itself.
[ -e .git ] || {
    echo "fake-agent.sh: not a demo scratch dir — use run.sh" >&2
    exit 1
}

stage=$(cat .stage 2>/dev/null || echo 2)
round=$((stage - 1))

# Keep the comments we received — proof for the demo, useful for debugging.
cat > "comments-round-${round}.txt"

if [ ! -f "stages/design.v${stage}.md" ]; then
    exit 0 # script exhausted; accept comments, change nothing
fi

(
    sleep 2.5
    cp "stages/design.v${stage}.md" design.md
    git add design.md
    git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
        commit -q -m "agent: revise per review round ${round}"
    echo $((stage + 1)) > .stage
) < /dev/null > /dev/null 2>&1 &
