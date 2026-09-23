#!/bin/sh
# marks-run.sh — marks モード（mark for と自由入力）のデモを起動する。
#
#   examples/demo/marks-run.sh [demo-dir]
#
# driftwatch の設計書（最終稿）を scratch に写して、本物の判定器
# （examples/semantic/jev-annotate.py）つきで akapen を開く。Jev を呼ぶので
# 鍵が要る（TYPESAFE_API_KEY か macOS キーチェーンの typesafe-jev）。
# 題材は MARKS_DOC で替えられる（既定 design.v3.md、日本語版は design.v3.ja.md）。
# 境界と答えは ~/.cache/akapen/semantic/ に残るので、2 回目からは速い。
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
demo_dir=${1:-/tmp/akapen-marks}

if [ -n "${AKAPEN:-}" ]; then
    akapen=$AKAPEN
elif [ -x "$repo_root/target/release/akapen" ]; then
    akapen=$repo_root/target/release/akapen
else
    akapen=akapen
fi

rm -rf "$demo_dir"
mkdir -p "$demo_dir"
cp "$script_dir/stages/${MARKS_DOC:-design.v3.md}" "$demo_dir/design.md"
cd "$demo_dir"
# shellcheck disable=SC2086 # AKAPEN_OPTS is intentionally word-split
exec "$akapen" design.md \
    --semantic-cmd "python3 $repo_root/examples/semantic/jev-annotate.py" \
    ${AKAPEN_OPTS:-}
