#!/usr/bin/env bash
# check-vendor-diff.sh — vendored fork が上流 tui-markdown から「想定内の差分」しか
# 持っていないことを検証する。未来の自分（または CI）が、誤った再ベンダリングや
# 意図しないフォーク変更に気づくための保険。
#
# 「上流バージョン」と「フォーク自身のバージョン」は別物である。
#   上流バージョン   = third_party/tui-markdown/Cargo.toml の
#                      [package.metadata] vendored-from
#                      （= ベンダリング元の crates.io リリース。上流ソースの
#                        ダウンロード URL / registry キャッシュの探索に使う）
#   フォークのバージョン = 同 Cargo.toml の [package] version
#                      （= フォーク自身の API が変わるたびに上がる。上流とは無関係）
# この 2 つは 0.3.9 のあいだ偶然一致していただけで、フォークの API が変わると
# ずれる。version を上流バージョンとして使ってはいけない。
#
# 検証する3つの契約:
#   1. 上流バージョンが、フォークの Cargo.toml（vendored-from）とマニフェスト
#      （scripts/vendor-expected.tsv のヘッダ）で一致している
#   2. vendored src/ のファイル一覧が上流と完全一致（追加・欠落なし）
#   3. ファイルごとの変更行数（diff -d の '<' + '>' 行数）がマニフェストと
#      1 行単位で一致
#
# 使い方:
#   ./scripts/check-vendor-diff.sh             # 検証（不一致で exit 1）
#   ./scripts/check-vendor-diff.sh --update    # 意図的な変更後にマニフェストを再生成
#   ./scripts/check-vendor-diff.sh --diff      # フォーク差分の全体を表示
#   TUI_MARKDOWN_SRC=/path/to/upstream ./scripts/check-vendor-diff.sh  # 上流ソース明示指定
#
# 上流ソースの解決順（<ver> はすべて上流バージョン = vendored-from）:
#   1. TUI_MARKDOWN_SRC 環境変数
#   2. cargo registry の src キャッシュ（$CARGO_HOME/registry/src/*/tui-markdown-<ver>）
#   3. static.crates.io から .crate をダウンロードして展開（CI 等、キャッシュがない環境）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDORED="$ROOT/third_party/tui-markdown"
MANIFEST="$ROOT/scripts/vendor-expected.tsv"

MODE=verify
[ "${1:-}" = "--update" ] && MODE=update
[ "${1:-}" = "--diff" ] && MODE=diff
case "${1:-}" in
  --update|--diff|"") ;;
  *) echo "usage: $0 [--update|--diff]" >&2; exit 2 ;;
esac

# --- フォークの Cargo.toml から 2 つのバージョンを読む ---------------------------
# VER      … ベンダリング元の上流バージョン（vendored-from）。以降の上流解決と
#            マニフェストのヘッダはすべてこちらを使う。
# FORK_VER … フォーク自身のバージョン（[package] version）。表示のみ。
VER="$(sed -n 's/^vendored-from = "\([0-9][^"]*\)"/\1/p' "$VENDORED/Cargo.toml" | head -1)"
FORK_VER="$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' "$VENDORED/Cargo.toml" | head -1)"
[ -n "$VER" ] || {
  echo "error: 上流バージョンを $VENDORED/Cargo.toml から読めない" >&2
  echo "hint: [package.metadata] の vendored-from = \"<上流バージョン>\" を書いてください" >&2
  echo "      （[package] の version はフォーク自身のバージョンで、上流とは別物です）" >&2
  exit 2
}
[ -n "$FORK_VER" ] || { echo "error: version を $VENDORED/Cargo.toml から読めない" >&2; exit 2; }

# --- 上流ソースを解決 -----------------------------------------------------------
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
UPSTREAM=""
if [ -n "${TUI_MARKDOWN_SRC:-}" ]; then
  UPSTREAM="$TUI_MARKDOWN_SRC"
elif [ -d "$CARGO_HOME/registry/src" ]; then
  for d in "$CARGO_HOME"/registry/src/*/tui-markdown-"$VER"; do
    [ -d "$d" ] && { UPSTREAM="$d"; break; }
  done
fi

TMP=""
if [ -z "$UPSTREAM" ]; then
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  URL="https://static.crates.io/crates/tui-markdown/tui-markdown-$VER.crate"
  echo "registry キャッシュに tui-markdown-$VER がないので $URL から取得します" >&2
  curl -fsSL "$URL" -o "$TMP/upstream.crate" || {
    echo "error: ダウンロード失敗。TUI_MARKDOWN_SRC=/path/to/tui-markdown-$VER で上流ソースを指定してください" >&2
    exit 2
  }
  tar -xzf "$TMP/upstream.crate" -C "$TMP"
  UPSTREAM="$TMP/tui-markdown-$VER"
fi
[ -d "$UPSTREAM/src" ] || { echo "error: $UPSTREAM に src/ がない" >&2; exit 2; }

# --- ファイル一覧の一致確認 -----------------------------------------------------
list_files() { (cd "$1" && find src -type f | sort); }
UP_FILES="$(list_files "$UPSTREAM")"
VEND_FILES="$(list_files "$VENDORED")"

added="$(comm -13 <(printf '%s\n' "$UP_FILES") <(printf '%s\n' "$VEND_FILES"))"
deleted="$(comm -23 <(printf '%s\n' "$UP_FILES") <(printf '%s\n' "$VEND_FILES"))"
if [ -n "$added$deleted" ]; then
  echo "error: vendored src/ のファイル一覧が上流と一致しません" >&2
  [ -n "$added" ] && printf '  上流にない追加ファイル:\n%s\n' "$(printf '%s\n' "$added" | sed 's/^/    /')" >&2
  [ -n "$deleted" ] && printf '  上流にあるのに欠落しているファイル:\n%s\n' "$(printf '%s\n' "$deleted" | sed 's/^/    /')" >&2
  echo "hint: フォークにファイルを足す/消すのは設計変更。意図的なら --update ではなく設計を見直してください" >&2
  exit 1
fi

# --- ファイルごとの変更行数を数える ----------------------------------------------
# -d（最小差分）: 最小なら行数は一意。素の diff は近似で、GNU diff だと table.rs が 895（最小は 893）になる
count_changed() { diff -d "$1" "$2" 2>/dev/null | grep -cE '^[<>]' || true; }

declare -A COUNTS
TOTAL=0
while IFS= read -r f; do
  n="$(count_changed "$UPSTREAM/$f" "$VENDORED/$f")"
  COUNTS["$f"]="$n"
  TOTAL=$((TOTAL + n))
done <<< "$UP_FILES"

# --- マニフェストの操作 ----------------------------------------------------------
if [ "$MODE" = update ]; then
  tmp="$MANIFEST.tmp"
  {
    echo "# tui-markdown $VER — vendored fork の想定差分（変更行数 = diff -d の '<' + '>' 行数）"
    # 上流バージョンだけを書く。フォーク自身のバージョンはここに焼き込まない
    # （--update を挟まずに上がると、黙って古い値が残るため）。
    echo "# 上の $VER は【ベンダリング元の上流バージョン】= third_party/tui-markdown/Cargo.toml"
    echo "# の [package.metadata] vendored-from。フォーク自身の [package] version とは別物。"
    echo "# このファイルは scripts/check-vendor-diff.sh --update で再生成する。手編集はしないこと。"
    echo "# フォークに意図的な変更を加えたら --update で更新し、その diff をレビューすること。"
    while IFS= read -r f; do
      printf '%s\t%s\n' "$f" "${COUNTS[$f]}"
    done <<< "$UP_FILES"
  } > "$tmp"
  mv "$tmp" "$MANIFEST"
  echo "updated $MANIFEST — 上流 tui-markdown $VER / フォーク $FORK_VER" \
       "($TOTAL changed lines across $(printf '%s\n' "$UP_FILES" | wc -l | tr -d ' ') files)"
  exit 0
fi

if [ "$MODE" = diff ]; then
  diff -ru "$UPSTREAM/src" "$VENDORED/src" || true
  exit 0
fi

# --- 検証 ------------------------------------------------------------------------
[ -f "$MANIFEST" ] || {
  echo "error: $MANIFEST がない。まず ./scripts/check-vendor-diff.sh --update で生成してください" >&2
  exit 2
}
MANIFEST_VER="$(sed -n 's/^# tui-markdown \([0-9][^ ]*\).*/\1/p' "$MANIFEST" | head -1)"
if [ "$MANIFEST_VER" != "$VER" ]; then
  echo "error: マニフェストの上流バージョン ($MANIFEST_VER) が Cargo.toml の vendored-from ($VER) と不一致" >&2
  echo "hint: 上流を上げてベンダリングし直したなら --update でマニフェストを再生成すること。" >&2
  echo "      フォーク自身のバージョン（[package] version、いまは ${FORK_VER}）は無関係です。" >&2
  exit 1
fi

MISMATCH=0
N_EXPECTED=0
while IFS=$'\t' read -r f expected; do
  [ -n "$f" ] || continue
  case "$f" in \#*) continue ;; esac
  actual="${COUNTS[$f]:-}"
  if [ -z "$actual" ]; then
    echo "error: マニフェストにだけあるファイル: $f" >&2
    MISMATCH=1
    continue
  fi
  N_EXPECTED=$((N_EXPECTED + 1))
  if [ "$actual" != "$expected" ]; then
    echo "error: $f — 変更行数 ${actual}（マニフェスト: ${expected}）" >&2
    MISMATCH=1
  fi
done < "$MANIFEST"

if [ "$MISMATCH" != 0 ]; then
  echo "hint: 意図的なフォーク変更なら ./scripts/check-vendor-diff.sh --update でマニフェストを更新し、diff をレビューすること" >&2
  echo "hint: 上流のバージョンが変わったなら、新しい tui-markdown をベンダリングし直してから --update すること" >&2
  exit 1
fi

echo "OK: 上流 tui-markdown $VER / フォーク akapen-tui-markdown $FORK_VER —" \
     "$N_EXPECTED ファイル、$TOTAL 変更行、$MANIFEST の想定どおり"
