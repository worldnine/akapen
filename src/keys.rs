//! Semantic Reading Layer のキー割り当て — **1 か所にまとめてある。**
//!
//! 両方のモードの割り当てがこの 1 ファイルにあるのは、**変えやすくして
//! おくため**である。特に [`MARKS_FREE`]（自由入力の `/`）は、設計書 3 節が
//! 「文字列検索（`/`）は残す」と書いているので将来ぶつかりうる。akapen に
//! 文字列検索はまだ無く、いまは空いている — ぶつける日が来たら**この
//! ファイルの 1 行**を変えれば済む。
//!
//! ```text
//! budget モード   -/+ ±1   </> ±10                 （READ %）
//! marks  モード   -/+ ±1   </> ±10   m/M 問い   / 自由入力   （MARK %）
//! ```
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

/// 定型を次へ巡る（marks モードのみ）。
pub(crate) const MARKS_CYCLE: char = 'm';
/// 定型を前へ巡る（marks モードのみ）。
pub(crate) const MARKS_CYCLE_BACK: char = 'M';
/// 自由入力のプロンプトを開く（marks モードのみ）。
///
/// **将来ぶつかりうる唯一のキー**である（上のモジュールの注）。
pub(crate) const MARKS_FREE: char = '/';

/// このキーが Semantic Reading Layer で何を意味するか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticKey {
    /// 量のつまみを動かす（READ % / MARK %）。
    Amount(i16),
    /// 定型を巡る。`+1` が次、`-1` が前。
    CycleQuestion(i16),
    /// 自由入力のプロンプトを開く。
    FreeQuestion,
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
        MARKS_CYCLE if marks => Some(SemanticKey::CycleQuestion(1)),
        MARKS_CYCLE_BACK if marks => Some(SemanticKey::CycleQuestion(-1)),
        MARKS_FREE if marks => Some(SemanticKey::FreeQuestion),
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
    fn the_question_keys_are_bound_only_in_marks_mode() {
        for c in [MARKS_CYCLE, MARKS_CYCLE_BACK, MARKS_FREE] {
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
