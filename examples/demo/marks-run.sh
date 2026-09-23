#!/bin/sh
# marks-run.sh — marks モード（mark for と自由入力）のデモを起動する。
#
#   examples/demo/marks-run.sh [demo-dir]
#
# 架空の研究室の夜間バックアップ tidewater の設計メモを scratch に写して、
# 本物の判定器（examples/semantic/jev-annotate.py）つきで akapen を開く。
# Jev を呼ぶので鍵が要る（TYPESAFE_API_KEY か macOS キーチェーンの typesafe-jev）。
# 題材は MARKS_DOC で替えられる（既定 tidewater.md、日本語版は tidewater.ja.md）。
# どちらも 1 画面（123×36）に収まる長さにしてある — 答えが画面の外に出ない。
# 境界と答えは ~/.cache/akapen/semantic/ に残るので、2 回目からは速い
# （鍵にはコマンド行が入るので、別の checkout から開くと当たらない）。
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
demo_dir=${1:-/tmp/akapen-marks}
doc=${MARKS_DOC:-tidewater.md}

if [ -n "${AKAPEN:-}" ]; then
    akapen=$AKAPEN
elif [ -x "$repo_root/target/release/akapen" ]; then
    akapen=$repo_root/target/release/akapen
else
    akapen=akapen
fi

rm -rf "$demo_dir"
mkdir -p "$demo_dir"
cp "$script_dir/marks/$doc" "$demo_dir/$doc"
cd "$demo_dir"
# shellcheck disable=SC2086 # AKAPEN_OPTS is intentionally word-split
exec "$akapen" "$doc" \
    --semantic-cmd "python3 $repo_root/examples/semantic/jev-annotate.py" \
    ${AKAPEN_OPTS:-}
