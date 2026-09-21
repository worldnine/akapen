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
    /// `Display` はこの variant だけ接頭辞を付けない（下の実装を見ること）。
    Provider(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "cannot read fixture: {e}"),
            Error::Json(e) => write!(f, "cannot parse JSON: {e}"),
            Error::Invalid(msg) => write!(f, "invalid semantic document: {msg}"),
            // **接頭辞を付けない。** ここに入る文字列は必ず
            // `--semantic-cmd ...` で始まっていて、それ自体が何が失敗したかを
            // 言っている。`provider failed: --semantic-cmd exited non-zero: ...`
            // は同じことを 2 回言っているだけで、ステータス行の幅を食って
            // **肝心の一文を枠の外へ押し出す**。
            Error::Provider(msg) => write!(f, "{msg}"),
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
        let e = Error::Invalid("atom 3 is out of range".to_owned());
        assert!(e.to_string().contains("atom 3 is out of range"));
        assert!(std::error::Error::source(&e).is_none());
    }

    /// 「答えが不正」と「答えを得られなかった」は別の話として出る。
    #[test]
    fn a_provider_failure_does_not_claim_the_document_is_broken() {
        let e = Error::Provider("--semantic-cmd timed out after 60s".to_owned());
        let message = e.to_string();
        // 接頭辞を足さない — 中身がすでに何が失敗したかを言っている。
        assert_eq!(message, "--semantic-cmd timed out after 60s");
        assert!(
            !message.contains("invalid semantic document"),
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
