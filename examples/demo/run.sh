#!/bin/sh
# run.sh — set up and launch the akapen screencast demo.
#
#   examples/demo/run.sh [demo-dir]
#
# Copies the demo into a scratch directory (default /tmp/akapen-demo),
# inits a fresh git repo with draft 1 committed, and launches akapen
# with the scripted fake agent wired to `s`. Re-run to reset everything.
#
# Environment:
#   AKAPEN=/path/to/bin  binary override (else target/release, else PATH)
#   AKAPEN_OPTS="..."    extra akapen flags (e.g. "--ime off --dark")
#   REAL_AGENT=1         launch with --send-agent instead of the fake
#                        agent, and leave the pre-baked drafts out of the
#                        scratch dir so a real agent can't peek at them
#                        (see REAL-AGENT.md)
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
demo_dir=${1:-/tmp/akapen-demo}

if [ -n "${AKAPEN:-}" ]; then
    akapen=$AKAPEN
elif [ -x "$repo_root/target/release/akapen" ]; then
    akapen=$repo_root/target/release/akapen
elif command -v akapen > /dev/null 2>&1; then
    akapen=akapen
else
    echo "error: no akapen binary found." >&2
    echo "build one first:  cargo build --release" >&2
    echo "or point at one:  AKAPEN=/path/to/akapen $0" >&2
    exit 1
fi

rm -rf "$demo_dir"
mkdir -p "$demo_dir"
if [ "${REAL_AGENT:-}" != 1 ]; then
    cp -R "$script_dir/stages" "$demo_dir/stages"
    cp "$script_dir/fake-agent.sh" "$demo_dir/"
    chmod +x "$demo_dir/fake-agent.sh"
    echo 2 > "$demo_dir/.stage"
fi

# Seed the git history with two generations (outline → first draft) so
# the time machine has depth to travel from the very first launch.
cd "$demo_dir"
git init -q
cp "$script_dir/stages/design.v0.md" design.md
git add design.md
git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
    commit -q -m "agent: outline of the driftwatch design"
cp "$script_dir/stages/design.v1.md" design.md
git add design.md
git -c user.name="Demo Agent" -c user.email="agent@example.invalid" \
    commit -q -m "agent: first draft of the driftwatch design"

# shellcheck disable=SC2086 # AKAPEN_OPTS is intentionally word-split
if [ "${REAL_AGENT:-}" = 1 ]; then
    exec "$akapen" design.md --send-agent ${AKAPEN_OPTS:-}
else
    exec "$akapen" design.md --send-cmd ./fake-agent.sh ${AKAPEN_OPTS:-}
fi
