//! Review の**書き換えの契約** — `s` / `y` で送る文面の先頭に足す段落。
//!
//! 設計書 `docs/design/marks-only-and-review-mode.md` 4 節「一番怖いところ」。
//! 「空疎だから具体的に」と頼むと、LLM は数字や固有名詞を**作る**。
//! 契約は「削る・縮める・統合するだけ」「事実を足さない」「足りなければ
//! `[要: 具体例]` を残して人に返す」「範囲の外は触らない」の 4 つである。
//!
//! ```text
//! $XDG_CONFIG_HOME/akapen/review-contract.md   （無ければ ~/.config/…）
//!   ↓ 無ければ
//! assets/review-contract.md                    （バイナリに焼き込んだ既定）
//! ```
//!
//! # 足すのは review コメントがあるときだけ
//!
//! [`compose`] は、送るコメントに `review:` の形
//! （[`crate::review::comment_text`]）が 1 本も無ければ**本文をそのまま
//! 返す**。赤入れだけの送信は段階 1 までと 1 バイトも変わらない。
//!
//! # ルールの定義も渡す
//!
//! コメントの本文は `review: filler (0.87)` で、ルール名しか持たない。
//! 受け手の LLM には「filler が何か」が分からないので、契約の後ろに
//! **使われたルールだけ**の定義を並べる。定義は `review-rules.json` の
//! `text`（Jev への問い）を**逐語で**渡し、直し方（`action`）を添える。
//! 問いの文面は「下の「対象」は…」で始まるので、その「対象」が指示した
//! 箇所のことだと一言断る。**言い換えた説明を別に持たない** — 持てば
//! Jev に聞いたことと LLM に伝えたことが 2 か所になり、片方だけ直す日が来る。

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::comment::Comment;
use crate::review_rules::{Action, Rules};

/// バイナリに焼き込んだ既定の契約。
const BUILT_IN: &str = include_str!("../assets/review-contract.md");

/// 読み手の契約を置く場所（設定ディレクトリからの相対）。
const USER_FILE: &str = "akapen/review-contract.md";

/// 契約の文面を読む。読み手のファイルがあればそれ、無ければ既定。
///
/// 読み手のファイルが**あるのに読めない**ときは既定に落とす。送信を
/// 止めるほどの理由ではなく、契約が 1 本も無いまま送るよりは安全である。
pub(crate) fn contract_text() -> String {
    user_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| BUILT_IN.to_string())
}

/// 焼き込んだ既定の契約。
#[cfg(test)]
pub(crate) fn built_in() -> &'static str {
    BUILT_IN
}

fn user_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join(USER_FILE))
}

/// 直し方の日本語。**契約の 3 つの動詞の中に収める** — `rewrite` も
/// 「言い換える」ではなく「縮める・統合する」の側で言う。
fn action_words(action: Action) -> &'static str {
    match action {
        Action::Delete => "削る",
        Action::Rewrite => "縮める・統合する。足りなければ印を残す",
        Action::Verify => "書き換えず、印を残す",
    }
}

/// 送る文面を組み立てる。**組み立てはここ 1 か所である。**
///
/// `body` は [`crate::export::format_all`] / `format_all_reply` の出力。
/// `comments` に review コメントが無ければ `body` をそのまま返す。
/// あれば `contract`・ルールの定義・空行・`body` の順に並べる。
pub(crate) fn compose(
    body: String,
    comments: &[Comment],
    rules: Option<&Rules>,
    contract: &str,
) -> String {
    let used: BTreeSet<String> = comments
        .iter()
        .filter_map(|c| crate::review::parse_comment_text(&c.text))
        .map(|(rule, _)| rule)
        .collect();
    if used.is_empty() {
        return body;
    }
    let mut out = contract.trim_end().to_string();
    let definitions: Vec<String> = used
        .iter()
        .filter_map(|id| rules.and_then(|rules| rules.get(id)))
        .map(|rule| {
            format!(
                "- {}（{}）: {}",
                rule.id,
                action_words(rule.action),
                rule.text.trim()
            )
        })
        .collect();
    if !definitions.is_empty() {
        out.push_str("\n\nルールの定義（文中の「対象」は、指示した箇所のことである）:\n");
        out.push_str(&definitions.join("\n"));
    }
    out.push_str("\n\n");
    out.push_str(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn comment(text: &str) -> Comment {
        Comment {
            file_path: PathBuf::from("/tmp/doc.md"),
            start: 3,
            end: 3,
            lines: "ひとつめの段落。".into(),
            revision: None,
            text: text.into(),
        }
    }

    #[test]
    fn a_send_without_review_comments_is_byte_for_byte_the_body() {
        let comments = vec![comment("ここは言い過ぎ"), comment("review は後で")];
        let body = crate::export::format_all(&comments);
        let rules = Rules::built_in().unwrap();
        assert_eq!(
            compose(body.clone(), &comments, Some(&rules), built_in()),
            body
        );
        let reply = crate::export::format_all_reply(&comments);
        assert_eq!(compose(reply.clone(), &comments, None, built_in()), reply);
    }

    #[test]
    fn a_hand_written_review_prefix_is_not_a_review_comment() {
        // 形（`review: <id> (<score>)`）に合わないものは人の赤入れである。
        let comments = vec![comment("review: ここは要らない")];
        let body = crate::export::format_all(&comments);
        assert_eq!(compose(body.clone(), &comments, None, built_in()), body);
    }

    #[test]
    fn one_review_comment_puts_the_contract_first_and_the_body_intact() {
        let comments = vec![
            comment("ここは言い過ぎ"),
            comment(&crate::review::comment_text("filler", 0.87)),
        ];
        let body = crate::export::format_all(&comments);
        let rules = Rules::built_in().unwrap();
        let out = compose(body.clone(), &comments, Some(&rules), built_in());
        assert!(out.starts_with(built_in().trim_end()), "契約が先頭に無い");
        assert!(out.ends_with(&body), "本文が変わっている");
        for must in ["削る・縮める・統合する", "事実・数字・固有名詞を足さない", "[要: 具体例]", "範囲の外"] {
            assert!(out.contains(must), "契約に {must} が無い");
        }
    }

    #[test]
    fn only_the_rules_in_use_are_defined_verbatim_with_their_action() {
        let comments = vec![comment(&crate::review::comment_text("filler", 0.9))];
        let rules = Rules::built_in().unwrap();
        let out = compose(String::new(), &comments, Some(&rules), built_in());
        let filler = rules.get("filler").unwrap();
        assert!(out.contains(&format!("- filler（削る）: {}", filler.text)));
        assert!(!out.contains("- preamble"), "使っていないルールまで並べている");
        assert!(!out.contains("- hedge"));
    }

    #[test]
    fn an_unknown_rule_still_gets_the_contract_without_a_definition() {
        let comments = vec![comment(&crate::review::comment_text("gone", 0.9))];
        let rules = Rules::built_in().unwrap();
        let out = compose("BODY".into(), &comments, Some(&rules), "契約");
        assert_eq!(out, "契約\n\nBODY");
    }
}
