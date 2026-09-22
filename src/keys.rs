//! Semantic Reading Layer のキー割り当て — **1 か所にまとめてある。**
//!
//! 割り当てがこの 1 ファイルにあるのは、**変えやすくしておくため**である。
//! 特に [`MARKS_FREE`]（自由入力の `/`）は、設計書 3 節が「文字列検索
//! （`/`）は残す」と書いているので将来ぶつかりうる。akapen に文字列検索は
//! まだ無く、いまは空いている — ぶつける日が来たら**このファイルの
//! 1 行**を変えれば済む。
//!
//! ```text
//! -/+ ±1   </> ±10   m 選ぶ  M 前へ  / 自由入力（つまみは %）
//! f 沈める（トグル）
//! ]m / [m 次・前のマーク行へ
//! ```
//!
//! **`m` は 2026-09-22 に「次へ巡る」から「選ぶ（popup）」へ変わった。**
//! 巡る形は 1 打ごとに Jev を呼ぶので、4 本めを見るのに 4 回呼んでいた
//! （`crate::app::App::ask_marks_preset`）。`M`（前へ巡る）は**残して
//! ある** — popup を開かずに 1 つ前へ戻る速い道で、割り当てを減らす話は
//! 注文に無い。

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

/// 定型の選択（popup）を開く。
pub(crate) const MARKS_PICK: char = 'm';
/// 定型を前へ巡る。
pub(crate) const MARKS_CYCLE_BACK: char = 'M';
/// 自由入力のプロンプトを開く。
///
/// **将来ぶつかりうる唯一のキー**である（上のモジュールの注）。
pub(crate) const MARKS_FREE: char = '/';

/// **フォーカス** — マーカーの無い Unit を沈める。
///
/// **トグルである**（押すたびに沈む / 戻る）。押している間だけ沈む形は
/// 2026-09-22 に試して捨てた（`crate::focus`）。
pub(crate) const MARKS_FOCUS: char = 'f';

/// **問いを消す**（Esc）。マーカーも読み出しも消える。
///
/// `KeyCode::Esc` は char ではないので [`semantic_key`] の表には載らない
/// — ここに置いてあるのは**順番**である。Esc には先客が何人もいて、
/// 終了確認 → 選択解除 → フォーカスを解く → **問いを消す** → 終了、の
/// 順に受ける（popup と composer はそもそも別のハンドラが先に取る）。
/// いちばん最後に近いのは、消すと解析をやり直す操作だからで、反射で
/// 押した Esc がそこまで落ちてくることは無い。
///
/// 文字列そのものが `?` ヘルプの行になる（`crate::overlay::help_rows`）。
pub(crate) const MARKS_CLEAR_HINT: &str = "Esc clear the question (marks and readout go)";

/// `]` / `[` に続けて打つと次・前のマーク行へ飛ぶ。
///
/// **chord の 2 打目**であって単独のキーではない（`]c` のレビューマーク
/// ジャンプと同じ体系。`crate::App::pending_chord`）。
pub(crate) const MARK_JUMP: char = 'm';

/// このキーが Semantic Reading Layer で何を意味するか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticKey {
    /// 量のつまみを動かす（上から何 % を光らせるか）。
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
/// 層を使っていないセッションでは呼ばれない（`App::semantic_enabled`）—
/// 使えないキーを黙って呑み込まないため。
pub(crate) fn semantic_key(c: char) -> Option<SemanticKey> {
    match c {
        AMOUNT_DOWN => Some(SemanticKey::Amount(-1)),
        AMOUNT_UP | AMOUNT_UP_ALT => Some(SemanticKey::Amount(1)),
        AMOUNT_DOWN_FAR => Some(SemanticKey::Amount(-10)),
        AMOUNT_UP_FAR => Some(SemanticKey::Amount(10)),
        MARKS_PICK => Some(SemanticKey::PickQuestion),
        MARKS_CYCLE_BACK => Some(SemanticKey::CycleQuestion(-1)),
        MARKS_FREE => Some(SemanticKey::FreeQuestion),
        MARKS_FOCUS => Some(SemanticKey::Focus),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_amount_keys_move_the_knob() {
        assert_eq!(semantic_key('-'), Some(SemanticKey::Amount(-1)));
        assert_eq!(semantic_key('+'), Some(SemanticKey::Amount(1)));
        assert_eq!(semantic_key('='), Some(SemanticKey::Amount(1)));
        assert_eq!(semantic_key('<'), Some(SemanticKey::Amount(-10)));
        assert_eq!(semantic_key('>'), Some(SemanticKey::Amount(10)));
    }

    #[test]
    fn m_opens_the_picker_and_shift_m_still_cycles_back() {
        // 巡る形は 1 打ごとに Jev を呼ぶので `m` は popup になった。
        // `M` は残してある（popup を開かずに 1 つ前へ戻る道）。
        assert_eq!(semantic_key(MARKS_PICK), Some(SemanticKey::PickQuestion));
        assert_eq!(
            semantic_key(MARKS_CYCLE_BACK),
            Some(SemanticKey::CycleQuestion(-1))
        );
    }

    #[test]
    fn the_question_and_focus_keys_are_bound() {
        assert_eq!(semantic_key(MARKS_FREE), Some(SemanticKey::FreeQuestion));
        assert_eq!(semantic_key(MARKS_FOCUS), Some(SemanticKey::Focus));
    }

    #[test]
    fn an_unrelated_key_is_never_claimed() {
        for c in ['j', 'k', 'q', 'l', 't', '?', '[', ']'] {
            assert_eq!(semantic_key(c), None, "{c} は他の機能のもの");
        }
    }
}
