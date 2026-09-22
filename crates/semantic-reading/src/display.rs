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

#[cfg(test)]
mod tests {
    use super::*;

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
