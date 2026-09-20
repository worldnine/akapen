//! この crate のエラー型。
//!
//! 依存を serde / serde_json に留めるため（thiserror などは使わない）、
//! `Display` と `Error` は手書きする。

use std::fmt;

/// [`crate::Provider`] と文書検証が返すエラー。
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// fixture ファイルの読み込みに失敗した。
    Io(std::io::Error),
    /// JSON の解析または生成に失敗した。
    Json(serde_json::Error),
    /// 構造としては読めたが、文書として辻褄が合わない
    /// （範囲の逆転、範囲外の Atom 添字、未知の Unit への参照など）。
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "fixture を読めません: {e}"),
            Error::Json(e) => write!(f, "JSON を解析できません: {e}"),
            Error::Invalid(msg) => write!(f, "semantic document が不正です: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            Error::Invalid(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_carries_its_reason_in_the_message() {
        let e = Error::Invalid("atom 3 は範囲外".to_owned());
        assert!(e.to_string().contains("atom 3 は範囲外"));
        assert!(std::error::Error::source(&e).is_none());
    }

    #[test]
    fn json_errors_keep_their_source() {
        let e: Error = serde_json::from_str::<u8>("{").unwrap_err().into();
        assert!(matches!(e, Error::Json(_)));
        assert!(std::error::Error::source(&e).is_some());
    }
}
