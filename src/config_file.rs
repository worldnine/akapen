//! 設定ファイル — `$XDG_CONFIG_HOME/akapen/config.toml`（無ければ `~/.config/…`）。
//!
//! ```toml
//! semantic_cmd = "python3 /path/to/examples/semantic/jev-annotate.py"
//! lint_cmd     = "python3 /path/to/examples/lint/textlint-diagnostics.py"
//! undercurl    = "auto"          # auto | on | off
//!
//! [theme]
//! dark  = "Catppuccin Mocha"     # two-face の名前か .tmTheme のパス
//! light = "Catppuccin Latte"
//! ```
//!
//! どのキーも省略できる。**ファイルが無ければ、今までと完全に同じ動作である。**
//! 優先は**フラグ > 環境変数 > 設定ファイル > 既定**（テーマには環境変数が無い）。
//! 層を重ねるのは [`crate::config::Config`] の仕事で、ここは読むだけ。
//!
//! # 壊れたファイルは起動時のコマンドラインエラー
//!
//! 構文の誤り・型違い・**知らないキー**は、ファイルのパスを添えて止まる
//! （壊れた問いのファイルと同じ方針。`src/main.rs` の `run`）。知らないキーを
//! 止めるのは、タイプミス（`[theme] drak = …`）を黙って無視しないため。
//! 代わりに、**新しいキーを書いた設定を古い akapen が読むと起動しない。**
//!
//! # 値の扱い
//!
//! - 空・空白だけの値は、書いていないのと同じ（環境変数と同じ作法）
//! - `[theme]` の値が `~/` で始まれば展開する（`.tmTheme` のパス用）
//! - `*_cmd` は展開しない。シェルに渡るので、シェルが展開する（環境変数と同じ）
//!
//! # 置き場の解決はここが 1 か所
//!
//! marks の問い・Review のルール・書き換えの契約も同じディレクトリに置く
//! （[`user_dir`]）。
//!
//! `[theme]` の表の型・値の整え方・置き場の解決は termtheme の
//! [`termtheme::config`] にある（ashiato・aav と同じもの）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer};
use termtheme::config::{ThemeSection, config_dir, non_blank};
use termtheme::theme::ThemePair;

use crate::undercurl::UndercurlMode;

/// 設定ディレクトリの名前（`$XDG_CONFIG_HOME` か `~/.config` の下）。
const APP_NAME: &str = "akapen";

/// 設定ファイルの名前（[`user_dir`] の下）。
const FILE_NAME: &str = "config.toml";

/// akapen の設定ディレクトリ: `$XDG_CONFIG_HOME/akapen`、無ければ
/// `~/.config/akapen`（[`termtheme::config::config_dir`]）。
///
/// 空の `XDG_CONFIG_HOME` は無いのと同じ。`HOME` も無ければ `None`
/// （置き場が決まらない = 何も置いていない）。
pub(crate) fn user_dir() -> Option<PathBuf> {
    config_dir(APP_NAME, |name| std::env::var_os(name))
}

/// 読んだ設定ファイル。値は空・空白を落とし、`~/` を展開した後のもの。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigFile {
    /// どこから読んだか。「どこで設定したか」をエラーで言うのに使う。
    pub path: PathBuf,
    /// `semantic_cmd`（`--semantic-cmd` / `AKAPEN_SEMANTIC_CMD` の下の層）。
    pub semantic_cmd: Option<String>,
    /// `lint_cmd`（`--lint-cmd` / `AKAPEN_LINT_CMD` の下の層）。
    pub lint_cmd: Option<String>,
    /// `undercurl`（`--undercurl` / `AKAPEN_UNDERCURL` の下の層）。
    pub undercurl: Option<UndercurlMode>,
    /// `[theme] dark` / `light`（`--theme-dark` / `--theme-light` の下の層）。
    pub theme: ThemePair,
}

/// ファイルの形そのもの。**知らないキーは断る**（冒頭の doc）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    semantic_cmd: Option<String>,
    lint_cmd: Option<String>,
    #[serde(default, deserialize_with = "undercurl_value")]
    undercurl: Option<UndercurlMode>,
    theme: Option<ThemeSection>,
}

/// `undercurl` の値。**知らない値は断る** — フラグと環境変数は知らない値を
/// `auto` に落とすが、ファイルに書いたタイプミスを黙って `auto` にすると、
/// 書いたのに効かないことに気づく道が無い。空・空白は書いていないのと同じ。
fn undercurl_value<'de, D: Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<UndercurlMode>, D::Error> {
    let value = String::deserialize(de)?;
    match value.trim() {
        "" => Ok(None),
        "auto" => Ok(Some(UndercurlMode::Auto)),
        "on" => Ok(Some(UndercurlMode::On)),
        "off" => Ok(Some(UndercurlMode::Off)),
        other => Err(D::Error::invalid_value(Unexpected::Str(other), &"auto, on or off")),
    }
}

impl ConfigFile {
    /// 中身から読む（**ファイルも環境も読まない**）。`path` はエラーと
    /// 出どころの表示に、`home` は `~/` の展開に使う。
    pub(crate) fn parse(path: &Path, text: &str, home: Option<&Path>) -> Result<Self> {
        let raw: Raw = toml::from_str(text).with_context(|| path.display().to_string())?;
        Ok(Self {
            path: path.to_path_buf(),
            semantic_cmd: non_blank(raw.semantic_cmd),
            lint_cmd: non_blank(raw.lint_cmd),
            undercurl: raw.undercurl,
            // 空・空白を落とし、`~/` を展開する。
            theme: raw.theme.unwrap_or_default().into_pair(home),
        })
    }

    /// 置き場（[`user_dir`]）のファイルを読む。**実ファイルを読むのはここだけ**
    /// （[`crate::config::Config::from_env`] から呼ぶ）。
    ///
    /// 無いのはエラーではない（`Ok(None)`）。あるのに読めない・壊れている、は
    /// エラーである。
    pub(crate) fn discover() -> Result<Option<Self>> {
        let Some(path) = user_dir().map(|dir| dir.join(FILE_NAME)) else {
            return Ok(None);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| path.display().to_string()),
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::parse(&path, &text, home.as_deref()).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ConfigFile> {
        ConfigFile::parse(Path::new("/cfg/akapen/config.toml"), text, Some(Path::new("/home/u")))
    }

    /// エラーの全文（anyhow の文脈と原因を全部つなげたもの）。
    fn error(text: &str) -> String {
        match parse(text) {
            Ok(file) => panic!("{text:?} が通ってしまった: {file:?}"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn the_directory_is_xdg_config_home_else_dot_config_under_home() {
        use std::ffi::OsString;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            config_dir(APP_NAME, env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")])),
            Some(PathBuf::from("/xdg/akapen"))
        );
        // 空の XDG_CONFIG_HOME は無いのと同じ。
        assert_eq!(
            config_dir(APP_NAME, env(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.config/akapen"))
        );
        assert_eq!(config_dir(APP_NAME, env(&[])), None, "置き場が決まらない");
    }

    #[test]
    fn an_empty_file_sets_nothing() {
        let file = parse("").unwrap();
        assert_eq!(
            file,
            ConfigFile { path: PathBuf::from("/cfg/akapen/config.toml"), ..Default::default() }
        );
        // `[theme]` だけあって中身が無いのも同じ。
        assert_eq!(parse("[theme]\n").unwrap(), file);
    }

    #[test]
    fn every_key_reads() {
        let file = parse(
            r#"
semantic_cmd = "python3 /path/to/jev-annotate.py"
lint_cmd     = "python3 /path/to/textlint-diagnostics.py --config ~/.textlintrc"
undercurl    = "off"

[theme]
dark  = "Catppuccin Mocha"
light = "Catppuccin Latte"
"#,
        )
        .unwrap();
        assert_eq!(file.semantic_cmd.as_deref(), Some("python3 /path/to/jev-annotate.py"));
        assert_eq!(
            file.lint_cmd.as_deref(),
            Some("python3 /path/to/textlint-diagnostics.py --config ~/.textlintrc"),
            "*_cmd の ~ はシェルが展開する（ここでは触らない）"
        );
        assert_eq!(file.undercurl, Some(UndercurlMode::Off));
        assert_eq!(file.theme.dark.as_deref(), Some("Catppuccin Mocha"));
        assert_eq!(file.theme.light.as_deref(), Some("Catppuccin Latte"));
    }

    #[test]
    fn a_theme_path_under_home_is_expanded_but_a_command_is_not() {
        let file = parse(concat!(
            "semantic_cmd = \"~/bin/annotate\"\n",
            "[theme]\n",
            "dark = \"~/themes/night.tmTheme\"\n",
            "light = \"Solarized (light)\"\n",
        ))
        .unwrap();
        assert_eq!(file.theme.dark.as_deref(), Some("/home/u/themes/night.tmTheme"));
        assert_eq!(file.theme.light.as_deref(), Some("Solarized (light)"));
        assert_eq!(file.semantic_cmd.as_deref(), Some("~/bin/annotate"));
        // `~` 単独や `~user/` は展開しない（`~/` だけ）。HOME が無ければそのまま。
        let file = parse("[theme]\ndark = \"~other/x.tmTheme\"\n").unwrap();
        assert_eq!(file.theme.dark.as_deref(), Some("~other/x.tmTheme"));
        let file = ConfigFile::parse(Path::new("c.toml"), "[theme]\ndark = \"~/x.tmTheme\"", None)
            .unwrap();
        assert_eq!(file.theme.dark.as_deref(), Some("~/x.tmTheme"));
    }

    #[test]
    fn blank_values_are_the_same_as_not_writing_them() {
        // 環境変数と同じ作法（`config.rs` の空・空白のテスト）。
        let file = parse(concat!(
            "semantic_cmd = \"\"\n",
            "lint_cmd = \"   \"\n",
            "undercurl = \" \"\n",
            "[theme]\n",
            "dark = \"\"\n",
            "light = \"  \"\n",
        ))
        .unwrap();
        assert_eq!(file, parse("").unwrap());
    }

    #[test]
    fn an_unknown_key_is_an_error_that_names_the_file_and_the_key() {
        // タイプミスを黙って無視しない。
        let err = error("semantic_command = \"x\"\n");
        assert!(err.contains("/cfg/akapen/config.toml"), "{err}");
        assert!(err.contains("semantic_command"), "{err}");
        let err = error("[theme]\ndrak = \"Dracula\"\n");
        assert!(err.contains("/cfg/akapen/config.toml"), "{err}");
        assert!(err.contains("drak"), "{err}");
        // 知らない表も同じ。
        let err = error("[colors]\nmark = 1\n");
        assert!(err.contains("colors"), "{err}");
    }

    #[test]
    fn a_wrong_type_is_an_error_that_names_the_file_and_the_key() {
        for (text, key) in [
            ("undercurl = true\n", "undercurl"),
            ("semantic_cmd = 3\n", "semantic_cmd"),
            ("theme = \"Dracula\"\n", "theme"),
            ("[theme]\nlight = [\"a\"]\n", "light"),
        ] {
            let err = error(text);
            assert!(err.contains("/cfg/akapen/config.toml"), "{err}");
            // toml のエラーは該当行を引用する（キーの名前が入る）。
            assert!(err.contains(key), "{key}: {err}");
        }
    }

    #[test]
    fn an_unknown_undercurl_value_is_an_error_not_auto() {
        // フラグと環境変数は知らない値を auto に落とすが、ファイルは断る。
        let err = error("undercurl = \"of\"\n");
        assert!(err.contains("undercurl"), "{err}");
        assert!(err.contains("auto, on or off"), "{err}");
        for (value, mode) in [
            ("auto", UndercurlMode::Auto),
            ("on", UndercurlMode::On),
            (" off ", UndercurlMode::Off),
        ] {
            let file = parse(&format!("undercurl = \"{value}\"\n")).unwrap();
            assert_eq!(file.undercurl, Some(mode), "{value:?}");
        }
    }

    #[test]
    fn broken_syntax_is_an_error_that_names_the_file() {
        let err = error("[theme\ndark = \"x\"\n");
        assert!(err.contains("/cfg/akapen/config.toml"), "{err}");
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn the_example_file_reads() {
        // README から指している例が壊れていないこと。
        let file = ConfigFile::parse(
            Path::new("examples/config.toml"),
            include_str!("../examples/config.toml"),
            Some(Path::new("/home/u")),
        )
        .unwrap();
        assert!(file.theme.dark.is_some() && file.theme.light.is_some());
    }
}
