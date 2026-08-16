#!/bin/bash
# akp herdr plugin action — open（下分割）/ side（横分割）/ float（ポップアップ）
# どの配置も「タブ内に同じ配置の akp pane があれば refresh + focus、なければ
# 新規作成」のトグル。配置ごとに label が違う（akp-down / akp-side / akp-float）
# ので、同じ配置の pane が増えることはない。akapen を抜けると pane ごと
# 自動クローズ（akp が --callback で自分を閉じる）。
#
# ターゲット解決: --current に頼らない。パレット（jt.command-palette）経由の
# invoke では focused pane がパレット overlay 自体を指すため。akp はタブ内
# エージェントの会話をレビューするので、「分割先 = タブのエージェント pane」
# を herdr agent list から解決する（レビュー対象の真下/真横に置く）。
set -euo pipefail

PLACEMENT="${1:-open}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
case "$PLACEMENT" in
  side)  DIR=right; LABEL="akp-side" ;;
  float) DIR="";    LABEL="akp-float" ;;
  *)     DIR=down;  LABEL="akp-down" ;;
esac
# 分割 ratio は「元 pane の取り分」（新 pane は 1-ratio）。
# デフォルト 0.7 → akp pane は 30%。AKP_SPLIT_RATIO で調整可。
SPLIT_RATIO="${AKP_SPLIT_RATIO:-0.7}"

DEBUG_LOG="${TMPDIR:-/tmp}/akp-action.log"
{
  echo "==== $(date '+%H:%M:%S') placement=$PLACEMENT"
  echo "env: TAB_ID=${HERDR_TAB_ID:-unset} PANE_ID=${HERDR_PANE_ID:-unset} PLUGIN_ID=${HERDR_PLUGIN_ID:-unset}"
  echo "ctx: ${HERDR_PLUGIN_CONTEXT_JSON:-unset}"
} >> "$DEBUG_LOG"

# TAB 解決: env → プラグインコンテキスト → focused pane（フォールバック）
TAB="${HERDR_TAB_ID:-}"
if [ -z "$TAB" ] && [ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ]; then
  TAB=$(printf '%s' "$HERDR_PLUGIN_CONTEXT_JSON" | jq -r '.tab_id // empty')
fi
if [ -z "$TAB" ]; then
  for _ in 1 2; do
    TAB=$(herdr pane current --current 2>/dev/null | jq -r '.result.pane.tab_id // empty') || TAB=""
    [ -n "$TAB" ] && break
    sleep 0.2
  done
fi
[ -n "$TAB" ] || { echo "akp: cannot resolve tab" >&2; exit 1; }

# 分割ターゲット: タブのエージェント pane（レビュー対象）
AGENT_PANE=$(herdr agent list 2>/dev/null | jq -r --arg tab "$TAB" \
  '.result.agents[] | select(.tab_id == $tab and .agent != null) | .pane_id' | head -1)
SPLIT_TARGET=("--current")
[ -n "$AGENT_PANE" ] && SPLIT_TARGET=("--pane" "$AGENT_PANE")

EXISTING=$(herdr pane list | jq -r --arg tab "$TAB" --arg label "$LABEL" \
  '.result.panes[] | select(.tab_id == $tab and .label == $label) | .pane_id' | head -1)

if [ -n "$EXISTING" ]; then
  # 内容を更新（安定パスを上書き → 起動中の akapen が自動リロード）して focus
  AKP_FOCUSED_PANE="${HERDR_PANE_ID:-}" "$ROOT/scripts/akp" --refresh 2>/dev/null || true
  if [ "$PLACEMENT" = "float" ]; then
    herdr plugin pane focus "$EXISTING" 2>/dev/null || true
  elif [ -n "$AGENT_PANE" ]; then
    herdr pane focus --direction "$DIR" --pane "$AGENT_PANE" 2>/dev/null || true
  else
    herdr pane focus --direction "$DIR" --current 2>/dev/null || true
  fi
  exit 0
fi

if [ "$PLACEMENT" = "float" ]; then
  # popup は pane ではなくシングルトンセッション資源: pane list に載らない。
  # 「popup already open」= トグル（開いてる popup の akapen は refresh で
  # 自動リロードされる）。
  if ! OUT=$(herdr plugin pane open --plugin akp --entrypoint float 2>&1); then
    if printf '%s' "$OUT" | grep -q "popup already open"; then
      AKP_FOCUSED_PANE="${HERDR_PANE_ID:-}" "$ROOT/scripts/akp" --refresh 2>/dev/null || true
      exit 0
    fi
    printf '%s\n' "$OUT" >&2
    exit 1
  fi
else
  RESPONSE=$(herdr pane split "${SPLIT_TARGET[@]}" --direction "$DIR" --focus --ratio "$SPLIT_RATIO")
  {
    echo "split_target=${SPLIT_TARGET[*]:-} dir=$DIR"
    echo "split_response=$RESPONSE"
  } >> "$DEBUG_LOG"
  PANE_ID=$(printf '%s' "$RESPONSE" | jq -r '.result.pane.pane_id')
  herdr pane rename "$PANE_ID" "$LABEL"
  # 新規 pane は action の環境を引き継がないので、フォーカス中 pane（= 起動時の
  # HERDR_PANE_ID）をコマンド文字列で渡す。複数エージェントのタブで、フォーカス
  # 中のエージェントを返信先に選ぶために使う。
  herdr pane run "$PANE_ID" "AKP_FOCUSED_PANE='${HERDR_PANE_ID:-}' \"$ROOT/scripts/akp\""
fi
