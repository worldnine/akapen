//! 表示状態 — この crate がクライアントへ渡す最後の語彙。
//!
//! ここから先（色・background・underline・どの程度暗くするか）は
//! クライアント側の決定であり、この crate は関与しない。

use serde::{Deserialize, Serialize};

/// Atom 1 つの表示状態。
///
/// この 3 値のみ。DIM は「読み手が他を沈めた」であって削除ではない —
/// 文書は変化しない。**[`crate::marks::mark`] はこれを返さない**;
/// 返すのはクライアント側のフォーカス（akapen の `f`）だけである。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    /// 読む価値が高い。クライアントは薄い semantic marker を重ねる。
    Marked,
    /// 通常表示。
    Normal,
    /// 読み手が他を沈めた（akapen の `f`）。文書からは消さない。
    Dim,
}

impl DisplayState {
    /// attention の強さ。大きいほど目を引く（DIM 0 < NORMAL 1 < MARKED 2）。
    ///
    /// 列挙の宣言順（MARKED / NORMAL / DIM）は設計書の並びに合わせてあり
    /// 強さの順ではないので、比較にはこの関数を使う。
    pub fn attention(self) -> u8 {
        match self {
            DisplayState::Dim => 0,
            DisplayState::Normal => 1,
            DisplayState::Marked => 2,
        }
    }

    /// 2 つの状態のうち attention の強い方。
    ///
    /// 1 つの Atom を複数の Unit が指している場合に使う。弱い方に引きずられて
    /// 文が消えるより、強い方を採る方が「読む場所を失わない」という目的に合う。
    pub fn stronger(self, other: Self) -> Self {
        if other.attention() > self.attention() {
            other
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stronger_prefers_the_more_visible_state() {
        assert_eq!(
            DisplayState::Dim.stronger(DisplayState::Normal),
            DisplayState::Normal
        );
        assert_eq!(
            DisplayState::Normal.stronger(DisplayState::Marked),
            DisplayState::Marked
        );
        assert_eq!(
            DisplayState::Marked.stronger(DisplayState::Dim),
            DisplayState::Marked
        );
        assert_eq!(
            DisplayState::Normal.stronger(DisplayState::Normal),
            DisplayState::Normal
        );
    }

    #[test]
    fn display_state_round_trips_as_snake_case() {
        let json = serde_json::to_string(&DisplayState::Marked).unwrap();
        assert_eq!(json, r#""marked""#);
        assert_eq!(
            serde_json::from_str::<DisplayState>(r#""dim""#).unwrap(),
            DisplayState::Dim
        );
    }
}
