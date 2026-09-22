//! marks モードの**問い** — 定型 4 本と、自由入力の型。
//!
//! 設計書 `docs/design/marks-only-and-review-mode.md` 0 節「定型プロンプトは
//! 綺麗なセットが欲しい」が言うとおり、**問いが外に出ると定型の質がそのまま
//! 道具の質になる**。だから文面はコードの中の文字列ではなく**データ**で、
//! 読み手が自分の問いを保存できる:
//!
//! ```text
//! $XDG_CONFIG_HOME/akapen/marks-questions.json   （無ければ ~/.config/…）
//!   ↓ 無ければ
//! assets/marks-questions.json                    （バイナリに焼き込んだ既定）
//! ```
//!
//! # 文面の正本はこのデータファイルである
//!
//! 焼き込んだ既定は、段 1 の実測（`examples/semantic/measurements/marks-presets.md`）
//! で使った文面の**逐語**である。証拠側の `tools/presets.py` から機械的に
//! 起こした（枠 `――― 対象 ―――` は判定器が本文と一緒に付けるので、
//! ここには入っていない）。
//!
//! **設計書 0 節の表はこのファイルの写しである。** 逆ではない。正典を機械的に
//! 読むテストは置かない — 設計書がテストの fixture になると設計書が伸ばせなく
//! なる（`docs/gotchas/semantic-reading.md`「凍結コピーは天井の近くで凍っている」、
//! 2026-09-22 の `design-doc-unfrozen`）。一致は人が確かめて、測定の記録に残す。
//!
//! **文面を変えたら測り直すこと。** 文面が道具そのもので、変えれば段 1 の
//! 数字は根拠でなくなる。キャッシュの方は自動で外れる（鍵に文面の sha が
//! 入っている。[`crate::semantic_cache`]）。
//!
//! # 「判断が要る」が入っていない理由
//!
//! 段 1 で**独立した定型にならなかった**（議事録で 0.5 を超えた 19 Unit が
//! すべて「決まっていないこと」でも 0.5 超、相関 0.88）。順序は付くが集合が
//! 分かれない。測定対象に「AI が書いた下書き」が無かったので、設計書 2 節が
//! Jev を使う理由とした「書き手の迷い」を測れていない。**下書きの実物で
//! 測ってから**入れる。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// バイナリに焼き込んだ既定の問い。
const BUILT_IN: &str = include_str!("../assets/marks-questions.json");

/// データファイルの版。形が変わった日に、古いファイルが黙って読まれない
/// ようにする。
const FILE_VERSION: u32 = 1;

/// 読み手の問いを置く場所（設定ディレクトリからの相対）。
const USER_FILE: &str = "akapen/marks-questions.json";

/// 問い 1 つ。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Question {
    /// 識別子。要求に載り、応答が echo し、キャッシュの照合に使う。
    pub(crate) id: String,
    /// ステータス行に出る短い名前。
    pub(crate) label: String,
    /// Jev へ渡す文面（自由入力は `{q}` を埋めたあと）。
    pub(crate) text: String,
}

/// 定型と自由入力の型をまとめたもの。
#[derive(Clone, Debug)]
pub(crate) struct Questions {
    presets: Vec<Question>,
    free_template: String,
}

#[derive(Deserialize)]
struct File {
    version: u32,
    presets: Vec<Preset>,
    free: Free,
}

#[derive(Deserialize)]
struct Preset {
    id: String,
    label: String,
    text: String,
}

/// 自由入力の型。
///
/// データファイルの `free.label`（「自由入力」）は**人が読むためだけ**に
/// あるので、ここでは読まない — ステータス行に出る名前は読み手が打った
/// 入力そのものである。
#[derive(Deserialize)]
struct Free {
    template: String,
}

impl Questions {
    /// 既定（焼き込み）を読む。**ここが失敗するのはビルドの誤りである**
    /// ので、panic ではなく `Result` で返して呼び出し側に言わせる。
    pub(crate) fn built_in() -> Result<Self> {
        Self::parse(BUILT_IN).context("built-in marks questions")
    }

    /// 読み手のファイルがあればそれを、無ければ既定を読む。
    ///
    /// `explicit` は `--marks-questions <path>` で明示されたファイル。
    /// 明示されたものが読めないのは**エラーである**（黙って既定に落ちると、
    /// 読み手は自分の問いで測っているつもりで既定を測ることになる）。
    /// 一方、置き場に何も無いのはエラーではない。
    pub(crate) fn discover(explicit: Option<&Path>) -> Result<Self> {
        if let Some(path) = explicit {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("--marks-questions {}", path.display()))?;
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
                "marks questions version {} (this build reads {FILE_VERSION})",
                file.version
            );
        }
        if file.presets.is_empty() {
            bail!("marks questions: no presets");
        }
        if !file.free.template.contains("{q}") {
            bail!("marks questions: the free-input template has no {{q}}");
        }
        Ok(Self {
            presets: file
                .presets
                .into_iter()
                .map(|p| Question {
                    id: p.id,
                    label: p.label,
                    text: p.text,
                })
                .collect(),
            free_template: file.free.template,
        })
    }

    /// 定型（巡る順）。
    pub(crate) fn presets(&self) -> &[Question] {
        &self.presets
    }

    /// 自由入力の型に `input` を埋めた問い。
    ///
    /// **そのまま埋めない理由**が型にある — 「日程」は主張ではないので
    /// Noul が評価できない（段 1 の 2 節）。`{q}` を「〜について述べている
    /// 箇所である」の形に差し込む。
    ///
    /// id は `free` 固定である。文面が変わったことはキャッシュの鍵
    /// （文面の sha）が検出するので、id に入力を混ぜる必要は無い。
    pub(crate) fn free(&self, input: &str) -> Question {
        Question {
            id: "free".to_string(),
            label: input.to_string(),
            text: self.free_template.replace("{q}", input),
        }
    }
}

/// `$XDG_CONFIG_HOME/akapen/marks-questions.json`、無ければ
/// `~/.config/akapen/marks-questions.json`。
///
/// [`crate::semantic_cache::SemanticCache::discover`] と同じ作法である。
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
    fn the_built_in_set_is_the_four_presets_and_a_free_template() {
        let questions = Questions::built_in().expect("焼き込んだ既定が読めること");
        let ids: Vec<&str> = questions
            .presets()
            .iter()
            .map(|q| q.id.as_str())
            .collect();
        assert_eq!(ids, ["essential", "settled", "unsettled", "numbers"]);
        // 「判断が要る」は段 1 で独立しなかったので入れていない。
        assert!(!ids.contains(&"decision"));
    }

    #[test]
    fn the_essential_preset_carries_the_tier_criteria_verbatim() {
        let questions = Questions::built_in().unwrap();
        let essential = &questions.presets()[0];
        // `TIER_CRITERIA["essential"]` の逐語。ここが動いたら段 1 の
        // 「要点 と既存 ESSENTIAL が AUC 0.81〜0.99 で一致」は根拠でなくなる。
        assert!(
            essential.text.contains(
                "落とすと文書の要点、結論、制約、未決の論点や宿題などを取り違える可能性が高い。"
            ),
            "{}",
            essential.text
        );
    }

    #[test]
    fn the_free_template_substitutes_every_occurrence() {
        let questions = Questions::built_in().unwrap();
        let asked = questions.free("費用の話");
        assert_eq!(asked.id, "free");
        assert_eq!(asked.label, "費用の話");
        assert!(!asked.text.contains("{q}"), "{}", asked.text);
        // 型は入力を 5 回使う（述べている / 取りこぼす / 語が出てこなくても /
        // 内容がそれであれば / そのものを述べていない）。
        assert_eq!(asked.text.matches("費用の話").count(), 5);
    }

    #[test]
    fn a_file_of_another_version_is_refused() {
        let json = r#"{"version":99,"presets":[{"id":"a","label":"a","text":"a"}],
                       "free":{"label":"f","template":"{q}"}}"#;
        assert!(Questions::parse(json).is_err());
    }

    #[test]
    fn a_free_template_without_the_placeholder_is_refused() {
        let json = r#"{"version":1,"presets":[{"id":"a","label":"a","text":"a"}],
                       "free":{"label":"f","template":"no placeholder"}}"#;
        assert!(Questions::parse(json).is_err());
    }

    #[test]
    fn an_explicit_file_that_cannot_be_read_is_an_error() {
        // 黙って既定に落ちない — 自分の問いを測っているつもりで既定を
        // 測る、がいちばん気づきにくい。
        assert!(Questions::discover(Some(Path::new("/nonexistent/questions.json"))).is_err());
    }

    #[test]
    fn an_explicit_file_wins_over_the_built_in_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.json");
        std::fs::write(
            &path,
            r#"{"version":1,"presets":[{"id":"mine","label":"私の問い","text":"…である。"}],
                "free":{"label":"自由","template":"「{q}」について述べている。"}}"#,
        )
        .unwrap();
        let questions = Questions::discover(Some(&path)).unwrap();
        assert_eq!(questions.presets().len(), 1);
        assert_eq!(questions.presets()[0].id, "mine");
    }
}
