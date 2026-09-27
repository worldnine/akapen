//! Review モードの**ルール** — 校正候補を拾う問いと、その足切り。
//!
//! 設計書 `docs/design/marks-only-and-review-mode.md` 4 節。marks の問い
//! （[`crate::marks_questions`]）と**同じ作法で読む**が、**別のファイルで
//! ある**:
//!
//! ```text
//! $XDG_CONFIG_HOME/akapen/review-rules.json   （無ければ ~/.config/…）
//!   ↓ 無ければ
//! assets/review-rules.json                    （バイナリに焼き込んだ既定）
//! ```
//!
//! # なぜ marks の問いと別のファイルなのか
//!
//! marks は「読め」（ラインマーカー）、Review は「直せ」（校正候補）で、
//! **別の機能である**（読み手の決定、2026-09-22〜23）。marks の定型の環に
//! 混ぜると `m` の popup に「直す側」の問いが並び、`]m` が校正候補へ飛ぶ。
//! 共有するのは**判定器の一段の問い**（Noul）・Unit・境界・sha キャッシュ
//! だけで、UI も読み出しも色も別に持つ。
//!
//! ルールが問いに足しているのは 3 つ — `action`（直し方）、`threshold`
//! （足切り）、`enabled`（既定で走るか）である。marks の「上から N %」に
//! 対して Review は**閾値**で切る（4 節「検知器ではない」）。つまみが
//! 無いのは、拾いすぎ側に倒して 1 キーで捨てる形だからで、量を動かす
//! 操作をもう 1 本増やす理由が無い。
//!
//! # 文面は 4 節の叩き台の逐語である
//!
//! 焼き込んだ既定の `text` は、設計書 4 節の叩き台を**1 字も変えずに**
//! 1 行へ畳んだもの（`docs/design/marks-only-and-review-mode.md` 4 節、
//! 実測は `examples/semantic/measurements/marks-presets.md` 3 節）。
//! **変えたら測り直すこと** — 文面が道具そのもので、変えれば 3 節の数字は
//! 根拠でなくなる。キャッシュは自動で外れる（鍵に文面の sha が入っている。
//! [`crate::semantic_cache`]）。
//!
//! **既定で `enabled` なルールは無い**（2026-09-23）。`Filler` の拾う
//! 「空疎さ」は、読み手が実際に直したい箇所（語の選び方の違和感・ぼかし）と
//! 合わなかった。判定は `--lint-cmd` の linter に任せるのが既定で、Jev の
//! ルールは `$XDG_CONFIG_HOME/akapen/review-rules.json` か `--review-rules`
//! で `enabled: true` にしたときだけ走る（4 節「判定の出どころとしての
//! linter」）。有効にするなら `Filler` 1 本が元の既定である — `Preamble` は
//! `Filler` と相関 0.98 で同じところを光らせ、`Hedge` は契約・医療では
//! 正しい書き方なのでジャンル依存である（4 節の表）。
//!
//! `label` は英語（UI の言葉は全部英語）、`text` は日本語のまま
//! — marks と同じ分担である。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// バイナリに焼き込んだ既定のルール。
const BUILT_IN: &str = include_str!("../assets/review-rules.json");

/// データファイルの版。形が変わった日に、古いファイルが黙って読まれない
/// ようにする。
const FILE_VERSION: u32 = 1;

/// 読み手のルールを置く場所（設定ディレクトリからの相対）。
const USER_FILE: &str = "akapen/review-rules.json";

/// 候補をどう直すか。**akapen はここでは直さない** — 段階 2 で LLM へ
/// 送るときの指示になる値で、段階 1 ではコメントの本文に現れるだけである。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// 削る。落としても読み手が何も失わない箇所（`Filler` / `Preamble`）。
    Delete,
    /// 落とすと意味が変わるが、言い方が悪い箇所（`Hedge`）。送るときは
    /// 言い換えさせず「削る。足りなければ印を残す」と言う
    /// （`crate::review_contract` の `action_words`）。名前はルールファイルの
    /// 値として残している。
    Rewrite,
    /// 人が確かめる。削るとも書き換えるとも決められない箇所。
    Verify,
}

impl Action {
    /// JSON の値からの対応。**知らない値はエラーである** — 黙って
    /// `Verify` に落とすと、読み手は自分の書いたルールが効いているつもりで
    /// 別のものを測ることになる（[`crate::marks_questions`] の
    /// 「明示されたファイルが読めないのはエラー」と同じ判断）。
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "delete" => Self::Delete,
            "rewrite" => Self::Rewrite,
            "verify" => Self::Verify,
            other => bail!("review rules: unknown action {other:?} (delete | rewrite | verify)"),
        })
    }

    /// コメントの本文と `--review-json` に出る名前。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Rewrite => "rewrite",
            Self::Verify => "verify",
        }
    }
}

/// ルール 1 本。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Rule {
    /// 識別子。要求に載り、応答が echo し、キャッシュの照合に使う。
    /// コメントの本文（`review: filler (0.87)`）にも出る。
    pub(crate) id: String,
    /// 一覧に出る短い名前。**英語である**。
    pub(crate) label: String,
    /// 直し方。
    pub(crate) action: Action,
    /// 足切り。この値**以上**の score を持つ Unit が候補になる。
    pub(crate) threshold: f32,
    /// `R` を押したときに走るか。
    pub(crate) enabled: bool,
    /// Jev へ渡す文面。
    pub(crate) text: String,
}

impl Rule {
    /// 判定器へ渡す問い。**marks とまったく同じ型を渡す**ので、キャッシュ
    /// （鍵は (コマンド行, 文書, 文面)）も経路も共有される。
    pub(crate) fn question(&self) -> crate::marks_questions::Question {
        crate::marks_questions::Question {
            id: self.id.clone(),
            label: self.label.clone(),
            hint: String::new(),
            text: self.text.clone(),
        }
    }
}

/// ルール一式。
#[derive(Clone, Debug)]
pub(crate) struct Rules {
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
struct File {
    version: u32,
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    label: String,
    action: String,
    threshold: f32,
    enabled: bool,
    text: String,
}

impl Rules {
    /// 既定（焼き込み）を読む。**ここが失敗するのはビルドの誤りである**
    /// ので、panic ではなく `Result` で返して呼び出し側に言わせる。
    pub(crate) fn built_in() -> Result<Self> {
        Self::parse(BUILT_IN).context("built-in review rules")
    }

    /// 読み手のファイルがあればそれを、無ければ既定を読む。
    ///
    /// `explicit` は `--review-rules <path>` で明示されたファイル。
    /// 明示されたものが読めないのは**エラーである**（黙って既定に落ちると、
    /// 読み手は自分のルールで測っているつもりで既定を測ることになる）。
    /// 一方、置き場に何も無いのはエラーではない。
    pub(crate) fn discover(explicit: Option<&Path>) -> Result<Self> {
        if let Some(path) = explicit {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("--review-rules {}", path.display()))?;
            return Self::parse(&text).with_context(|| format!("{}", path.display()));
        }
        if let Some(path) = user_path()
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            // 置いてあるのに壊れている、は黙って無視しない。
            return Self::parse(&text).with_context(|| format!("{}", path.display()));
        }
        Self::built_in()
    }

    fn parse(json: &str) -> Result<Self> {
        let file: File = serde_json::from_str(json)?;
        if file.version != FILE_VERSION {
            bail!(
                "review rules version {} (this build reads {FILE_VERSION})",
                file.version
            );
        }
        if file.rules.is_empty() {
            bail!("review rules: no rules");
        }
        let mut rules = Vec::with_capacity(file.rules.len());
        for raw in file.rules {
            if !(0.0..=1.0).contains(&raw.threshold) {
                bail!(
                    "review rules: rule `{}` has a threshold of {} (0.0..=1.0)",
                    raw.id,
                    raw.threshold
                );
            }
            if rules.iter().any(|r: &Rule| r.id == raw.id) {
                bail!("review rules: duplicate rule id `{}`", raw.id);
            }
            rules.push(Rule {
                action: Action::parse(&raw.action)?,
                id: raw.id,
                label: raw.label,
                threshold: raw.threshold,
                enabled: raw.enabled,
                text: raw.text,
            });
        }
        Ok(Self { rules })
    }

    /// すべてのルール（ファイルの順）。**いまは読み込みのテストだけが
    /// 使う** — 画面に出るのは有効なルールが拾った候補だけで、無効な
    /// ルールの一覧を見せる口はまだ無い（つまみを 1 本増やす話になる）。
    #[cfg(test)]
    pub(crate) fn all(&self) -> &[Rule] {
        &self.rules
    }

    /// 既定のルールのうち `ids` だけを有効にした写し（テスト用）。
    /// `--review-rules` で有効にした読み手と同じ状態を作る。
    #[cfg(test)]
    pub(crate) fn built_in_enabling(ids: &[&str]) -> Self {
        let mut rules = Self::built_in().expect("焼き込んだ既定");
        for rule in &mut rules.rules {
            rule.enabled = ids.contains(&rule.id.as_str());
        }
        rules
    }

    /// `R` で走るルールだけ。**候補の順はここが決めない** — 候補は
    /// 文書順に並ぶ（[`crate::review`]）。
    pub(crate) fn enabled(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(|rule| rule.enabled)
    }

    /// id からルールを引く（候補が持っているのは id だけである）。
    pub(crate) fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }
}

/// `$XDG_CONFIG_HOME/akapen/review-rules.json`、無ければ
/// `~/.config/akapen/review-rules.json`。
///
/// [`crate::marks_questions`] と同じ作法である。
fn user_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join(USER_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_set_is_filler_preamble_and_hedge() {
        let rules = Rules::built_in().expect("焼き込んだ既定が読めること");
        let ids: Vec<&str> = rules.all().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["filler", "preamble", "hedge"]);
    }

    #[test]
    fn no_rule_is_enabled_by_default() {
        // 4 節「判定の出どころとしての linter」。判定は `--lint-cmd` に任せ、
        // Jev のルールは読み手が有効にしたときだけ走る。
        let rules = Rules::built_in().unwrap();
        assert_eq!(rules.enabled().count(), 0);
    }

    #[test]
    fn a_users_file_can_still_turn_filler_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.json");
        let on = BUILT_IN.replacen(r#""enabled": false"#, r#""enabled": true"#, 1);
        std::fs::write(&path, on).unwrap();
        let rules = Rules::discover(Some(&path)).unwrap();
        let on: Vec<&str> = rules.enabled().map(|r| r.id.as_str()).collect();
        assert_eq!(on, ["filler"]);
    }

    #[test]
    fn the_thresholds_and_actions_are_the_designed_ones() {
        let rules = Rules::built_in().unwrap();
        let filler = rules.get("filler").unwrap();
        assert_eq!(filler.action, Action::Delete);
        assert_eq!(filler.threshold, 0.5);
        assert_eq!(rules.get("preamble").unwrap().action, Action::Delete);
        assert_eq!(rules.get("hedge").unwrap().action, Action::Rewrite);
        assert_eq!(rules.get("hedge").unwrap().threshold, 0.6);
    }

    #[test]
    fn every_built_in_label_is_ascii_and_every_text_is_japanese() {
        // marks と同じ分担。UI の言葉は全部英語で、日本語は Jev へ送る
        // `text` だけである。
        let rules = Rules::built_in().unwrap();
        for rule in rules.all() {
            assert!(rule.label.is_ascii(), "label が英語でない: {}", rule.label);
            assert!(!rule.text.is_ascii(), "text は日本語のまま: {}", rule.id);
        }
    }

    #[test]
    fn the_filler_rule_carries_the_design_documents_wording_verbatim() {
        // 設計書 4 節の叩き台の逐語。ここが動いたら 3 節の
        // 「混ぜた 12 文に AUC 0.98」は根拠でなくなる。
        let rules = Rules::built_in().unwrap();
        let filler = rules.get("filler").unwrap();
        assert_eq!(
            filler.text,
            "下の「対象」は、具体的なことを何も述べていない箇所である。\
             ここを落としても、読み手が失う事実・数字・判断・理由は無い。\
             一般論、決まり文句、それらしい前置きや締め、直前の言い換えが\
             当てはまる。固有の事実・数字・判断・その理由を含む箇所は\
             当てはまらない。"
        );
    }

    #[test]
    fn a_rule_becomes_a_question_with_the_same_id_and_text() {
        // キャッシュの鍵は (コマンド行, 文書, 文面) なので、marks が同じ
        // 文面を聞いていれば当たる。
        let rules = Rules::built_in().unwrap();
        let question = rules.get("filler").unwrap().question();
        assert_eq!(question.id, "filler");
        assert_eq!(question.label, "Filler");
        assert_eq!(question.text, rules.get("filler").unwrap().text);
    }

    #[test]
    fn a_file_of_another_version_is_refused() {
        let json = r#"{"version":99,"rules":[{"id":"a","label":"A","action":"delete",
                       "threshold":0.5,"enabled":true,"text":"…"}]}"#;
        assert!(Rules::parse(json).is_err());
    }

    #[test]
    fn an_unknown_action_is_refused() {
        // 黙って `verify` に落とさない。
        let json = r#"{"version":1,"rules":[{"id":"a","label":"A","action":"summarize",
                       "threshold":0.5,"enabled":true,"text":"…"}]}"#;
        let err = Rules::parse(json).unwrap_err().to_string();
        assert!(err.contains("unknown action"), "{err}");
    }

    #[test]
    fn a_threshold_outside_the_score_range_is_refused() {
        for bad in ["1.5", "-0.1"] {
            let json = format!(
                r#"{{"version":1,"rules":[{{"id":"a","label":"A","action":"delete",
                   "threshold":{bad},"enabled":true,"text":"…"}}]}}"#
            );
            assert!(Rules::parse(&json).is_err(), "{bad} が通ってしまった");
        }
    }

    #[test]
    fn a_duplicate_rule_id_is_refused() {
        // id は候補が持つ唯一の手がかりなので、重なると引けなくなる。
        let json = r#"{"version":1,"rules":[
            {"id":"a","label":"A","action":"delete","threshold":0.5,"enabled":true,"text":"…"},
            {"id":"a","label":"B","action":"verify","threshold":0.5,"enabled":true,"text":"…"}]}"#;
        let err = Rules::parse(json).unwrap_err().to_string();
        assert!(err.contains("duplicate rule id"), "{err}");
    }

    #[test]
    fn broken_json_is_refused() {
        assert!(Rules::parse("{not json").is_err());
        assert!(Rules::parse(r#"{"version":1}"#).is_err());
        assert!(Rules::parse(r#"{"version":1,"rules":[]}"#).is_err());
    }

    #[test]
    fn an_explicit_file_that_cannot_be_read_is_an_error() {
        assert!(Rules::discover(Some(Path::new("/nonexistent/review-rules.json"))).is_err());
    }

    #[test]
    fn an_explicit_file_wins_over_the_built_in_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.json");
        std::fs::write(
            &path,
            r#"{"version":1,"rules":[{"id":"mine","label":"Mine","action":"verify",
                "threshold":0.9,"enabled":true,"text":"…である。"}]}"#,
        )
        .unwrap();
        let rules = Rules::discover(Some(&path)).unwrap();
        assert_eq!(rules.all().len(), 1);
        assert_eq!(rules.all()[0].id, "mine");
        assert_eq!(rules.all()[0].action, Action::Verify);
    }
}
