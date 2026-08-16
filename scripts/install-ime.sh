#!/bin/sh
# Install scripts/ime.swift as a standalone `ime` CLI.
#
#   scripts/install-ime.sh                          # -> ~/.local/bin/ime
#   INSTALL_DIR=/usr/local/bin scripts/install-ime.sh
#
# macOS only (needs swiftc from Xcode Command Line Tools). Re-run after
# pulling an update: the binary is overwritten in place, so other tools
# (focus watchers, plugin wrappers) keep working without reconfiguration.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src="$here/ime.swift"
dir=${INSTALL_DIR:-"$HOME/.local/bin"}
bin="$dir/ime"

command -v swiftc >/dev/null 2>&1 || {
    echo "install-ime: swiftc not found (install Xcode Command Line Tools: xcode-select --install)" >&2
    exit 1
}

mkdir -p "$dir"
tmp="$dir/.ime.tmp.$$"
trap 'rm -f "$tmp"' EXIT

swiftc -O "$src" -o "$tmp"
chmod +x "$tmp"
mv -f "$tmp" "$bin"
echo "installed: $bin"
