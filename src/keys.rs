//! Semantic Reading Layer のキー割り当て — **1 か所にまとめてある。**
//!
//! 両方のモードの割り当てがこの 1 ファイルにあるのは、**変えやすくして
//! おくため**である。特に [`MARKS_FREE`]（自由入力の `/`）は、設計書 3 節が
//! 「文字列検索（`/`）は残す」と書いているので将来ぶつかりうる。akapen に
//! 文字列検索はまだ無く、いまは空いている — ぶつける日が来たら**この
//! ファイルの 1 行**を変えれば済む。
//!
//! ```text
//! budget モード   -/+ ±1   </> ±10                       （READ %）
//! marks  モード   -/+ ±1   </> ±10   m 選ぶ  M 前へ  / 自由入力（MARK %）
//!                 f 沈める（短押し = トグル / 長押し = 押している間）
//!                 ]m / [m 次・前のマーク行へ
//! ```
//!
//! **`m` は 2026-09-22 に「次へ巡る」から「選ぶ（popup）」へ変わった。**
//! 巡る形は 1 打ごとに Jev を呼ぶので、4 本めを見るのに 4 回呼んでいた
//! （`crate::app::App::ask_marks_preset`）。`M`（前へ巡る）は**残して
//! ある** — popup を開かずに 1 つ前へ戻る速い道で、割り当てを減らす話は
//! 注文に無い。
//!
//! 量のつまみ 4 本は**両モードで同じキー**である。どちらでも「量のつまみ」
//! という同じ意味で、モードは起動時に決まるので取り違えようがない。

/// 量を 1 ポイント下げる。
pub(crate) const AMOUNT_DOWN: char = '-';
/// 量を 1 ポイント上げる（`=` は Shift を押さずに打てる同義キー）。
pub(crate) const AMOUNT_UP: char = '+';
/// [`AMOUNT_UP`] の同義キー。
pub(crate) const AMOUNT_UP_ALT: char = '=';
/// 量を 10 ポイント下げる。
pub(crate) const AMOUNT_DOWN_FAR: char = '<';
/// 量を 10 ポイント上げる。
pub(crate) const AMOUNT_UP_FAR: char = '>';

/// 定型の選択（popup）を開く（marks モードのみ）。
pub(crate) const MARKS_PICK: char = 'm';
/// 定型を前へ巡る（marks モードのみ）。
pub(crate) const MARKS_CYCLE_BACK: char = 'M';
/// 自由入力のプロンプトを開く（marks モードのみ）。
///
/// **将来ぶつかりうる唯一のキー**である（上のモジュールの注）。
pub(crate) const MARKS_FREE: char = '/';

/// **フォーカス**（marks モードのみ）— マーカーの無い Unit を沈める。
///
/// 短押しでトグル、押しっぱなしで押している間だけ沈む。**同じキーで
/// 振る舞いが変わる**のは端末の都合で、`KeyEventKind::Release` の来る
/// 端末（kitty keyboard protocol）でだけ hold になる
/// （`crate::app::Focus`）。
pub(crate) const MARKS_FOCUS: char = 'f';

/// `]` / `[` に続けて打つと次・前のマーク行へ飛ぶ（marks モードのみ）。
///
/// **chord の 2 打目**であって単独のキーではない（`]c` のレビューマーク
/// ジャンプと同じ体系。`crate::App::pending_chord`）。marks モードの
/// ときだけ chord になり、それ以外では `]` の既定（ファイル切替）に
/// 落ちる — 使えない chord を黙って呑み込まないため。
pub(crate) const MARK_JUMP: char = 'm';

/// このキーが Semantic Reading Layer で何を意味するか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticKey {
    /// 量のつまみを動かす（READ % / MARK %）。
    Amount(i16),
    /// 定型を巡る。`+1` が次、`-1` が前。いまキーが割り当たっているのは
    /// `-1`（`M`）だけで、`+1` は popup が置き換えた。
    CycleQuestion(i16),
    /// 定型の選択（popup）を開く。
    PickQuestion,
    /// 自由入力のプロンプトを開く。
    FreeQuestion,
    /// フォーカス（沈める）の押下。
    Focus,
}

/// キーを [`SemanticKey`] に読む。層に関係のないキーは `None`。
///
/// `marks` が `false` のとき、問いのキー（`m` / `M` / `/`）は `None` を
/// 返す — **budget モードでは束縛しない**。層を使っていないセッションで
/// `-` `+` `<` `>` が素通りするのと同じ作法で、使えないキーを黙って
/// 呑み込まない。
pub(crate) fn semantic_key(c: char, marks: bool) -> Option<SemanticKey> {
    match c {
        AMOUNT_DOWN => Some(SemanticKey::Amount(-1)),
        AMOUNT_UP | AMOUNT_UP_ALT => Some(SemanticKey::Amount(1)),
        AMOUNT_DOWN_FAR => Some(SemanticKey::Amount(-10)),
        AMOUNT_UP_FAR => Some(SemanticKey::Amount(10)),
        MARKS_PICK if marks => Some(SemanticKey::PickQuestion),
        MARKS_CYCLE_BACK if marks => Some(SemanticKey::CycleQuestion(-1)),
        MARKS_FREE if marks => Some(SemanticKey::FreeQuestion),
        MARKS_FOCUS if marks => Some(SemanticKey::Focus),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_amount_keys_mean_the_same_in_both_modes() {
        for marks in [false, true] {
            assert_eq!(semantic_key('-', marks), Some(SemanticKey::Amount(-1)));
            assert_eq!(semantic_key('+', marks), Some(SemanticKey::Amount(1)));
            assert_eq!(semantic_key('=', marks), Some(SemanticKey::Amount(1)));
            assert_eq!(semantic_key('<', marks), Some(SemanticKey::Amount(-10)));
            assert_eq!(semantic_key('>', marks), Some(SemanticKey::Amount(10)));
        }
    }

    #[test]
    fn m_opens_the_picker_and_shift_m_still_cycles_back() {
        // 巡る形は 1 打ごとに Jev を呼ぶので `m` は popup になった。
        // `M` は残してある（popup を開かずに 1 つ前へ戻る道）。
        assert_eq!(semantic_key(MARKS_PICK, true), Some(SemanticKey::PickQuestion));
        assert_eq!(
            semantic_key(MARKS_CYCLE_BACK, true),
            Some(SemanticKey::CycleQuestion(-1))
        );
    }

    #[test]
    fn focus_is_bound_only_in_marks_mode() {
        // DIM 版には沈める機構が既にある（Reading Budget）。marks の
        // フォーカスはその上に重ねない。
        assert_eq!(semantic_key(MARKS_FOCUS, false), None);
        assert_eq!(semantic_key(MARKS_FOCUS, true), Some(SemanticKey::Focus));
    }

    #[test]
    fn the_question_keys_are_bound_only_in_marks_mode() {
        for c in [MARKS_PICK, MARKS_CYCLE_BACK, MARKS_FREE, MARKS_FOCUS] {
            assert_eq!(semantic_key(c, false), None, "budget モードでは素通り: {c}");
            assert!(semantic_key(c, true).is_some(), "marks モードでは効く: {c}");
        }
    }

    #[test]
    fn an_unrelated_key_is_never_claimed() {
        for c in ['j', 'k', 'q', 'l', 't', '?', '[', ']'] {
            assert_eq!(semantic_key(c, true), None, "{c} は他の機能のもの");
        }
    }
}
