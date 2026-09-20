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
    /// 答えを**得る過程**が失敗した — provider を起動できない、異常終了、
    /// タイムアウト。応答の中身の問題ではないので [`Error::Invalid`] とは
    /// 分ける（「文書が不正です」と言われると、文書を直せば通ると読める）。
    Provider(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "fixture を読めません: {e}"),
            Error::Json(e) => write!(f, "JSON を解析できません: {e}"),
            Error::Invalid(msg) => write!(f, "semantic document が不正です: {msg}"),
            Error::Provider(msg) => write!(f, "provider が失敗しました: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            Error::Invalid(_) | Error::Provider(_) => None,
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

    /// 「答えが不正」と「答えを得られなかった」は別の話として出る。
    #[test]
    fn a_provider_failure_does_not_claim_the_document_is_broken() {
        let e = Error::Provider("--semantic-cmd timed out after 60s".to_owned());
        let message = e.to_string();
        assert!(message.contains("timed out"), "{message}");
        assert!(
            !message.contains("document が不正"),
            "起動できなかっただけの話を「文書が不正」と言わない: {message}"
        );
        assert!(std::error::Error::source(&e).is_none());
    }

    #[test]
    fn json_errors_keep_their_source() {
        let e: Error = serde_json::from_str::<u8>("{").unwrap_err().into();
        assert!(matches!(e, Error::Json(_)));
        assert!(std::error::Error::source(&e).is_some());
    }
}
