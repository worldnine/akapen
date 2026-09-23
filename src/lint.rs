//! `--lint-cmd` — **linter の指摘を Review の候補にする。**
//!
//! 設計書 `docs/design/marks-only-and-review-mode.md` 4 節「判定の出どころ
//! としての linter」。判定は linter に任せ、akapen はその後ろの流れ
//! （一覧・捨てる・accept してコメント・送る・`e` で直す・引き継ぐ）だけを
//! 受け持つ。ルールは読み手が自分の linter の設定で決める。
//!
//! # akapen は linter の設定を知らない
//!
//! 作業ディレクトリを文書のあるディレクトリにして、文書の絶対パスを最後の
//! 引数に足すだけである。`.textlintrc` などの探索は linter 自身がする
//! （textlint は実行した場所から上へ探す）。
//!
//! # 受け付ける形は 1 つ: LSP の Diagnostic
//!
//! ```json
//! {"diagnostics": [
//!   {"range": {"start": {"line": 71, "character": 30}, "end": {"line": 71, "character": 32}},
//!    "message": "…", "source": "textlint", "code": "ja-no-weak-phrase", "severity": 3}
//! ]}
//! ```
//!
//! `line` は 0 始まり、`character` は **UTF-16 のコード単位**（LSP の既定）。
//! textlint などの出力は `examples/lint/` の変換スクリプトでこの形に直す。
//!
//! # 成否は終了コードではなく出力で決める
//!
//! textlint は指摘があると exit 1 で JSON を出す。stdout が形どおりの JSON なら
//! 成功、そうでなければ [`NOT_DIAGNOSTICS`] — **「0 件」とは見せない**。
//! 抜粋は出さない（本文が混じりうる）。

use std::ops::Range;
use std::path::Path;

use serde_json::Value;

use crate::review::{Candidate, CandidateState, Finding};

/// 出力が Diagnostic の JSON でなかったときの 1 行。**抜粋を足さない。**
pub(crate) const NOT_DIAGNOSTICS: &str = "lint: output is not diagnostics JSON";

/// 応答として受け取る stdout の上限（`--semantic-cmd` と同じ）。
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

/// 指摘 1 件（akapen のバイト位置へ直したあと）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Diagnostic {
    /// 文書のバイト範囲。空にはならない。
    pub(crate) range: Range<usize>,
    /// `<source>/<code>`。候補の rule になる。
    pub(crate) rule: String,
    pub(crate) message: String,
    /// LSP の severity（1 Error / 2 Warning / 3 Information / 4 Hint）。
    /// 下線とガターの `!` の色を決める（[`crate::decoration::ReviewSeverity::from_lsp`]）。
    pub(crate) severity: Option<u8>,
}

/// `--lint-cmd` のコマンド。
#[derive(Clone, Debug)]
pub(crate) struct LintCommand {
    cmd: String,
}

impl LintCommand {
    pub(crate) fn new(cmd: impl Into<String>) -> Self {
        Self { cmd: cmd.into() }
    }

    /// 文書のあるディレクトリで linter を走らせ、指摘を返す。
    ///
    /// **この関数は秒単位でかかりうる。** 呼ぶのはワーカースレッドだけである。
    /// 見切り方は `--semantic-cmd` と同じ（無音 30 秒 ＋ 天井 10 分）。
    ///
    /// `source` は画面に出ている本文。linter はディスクのファイルを読むので、
    /// **両者が食い違えば位置は意味を持たない** — そのときは断る
    /// （外の書き換えを読み込む前・タイムマシンで過去の版を見ている間）。
    pub(crate) fn run(&self, path: &Path, source: &str) -> Result<Vec<Diagnostic>, String> {
        let absolute = std::path::absolute(path).map_err(|e| format!("lint: {e}"))?;
        match std::fs::read_to_string(&absolute) {
            Ok(on_disk) if on_disk == source => {}
            _ => return Err("lint: the file on disk is not the text on screen".into()),
        }
        let dir = absolute.parent().filter(|dir| !dir.as_os_str().is_empty());
        let file = absolute.to_string_lossy();
        let (stdout, _code) = crate::export::run_capturing_any_exit(
            "--lint-cmd",
            &self.cmd,
            &[&file],
            dir,
            crate::export::Deadline::WhileProgressing {
                idle: crate::semantic::COMMAND_IDLE_TIMEOUT,
                backstop: crate::semantic::COMMAND_BACKSTOP,
            },
            RESPONSE_LIMIT,
        )
        .map_err(|e| format!("lint: {e:#}"))?;
        parse(&stdout, source).ok_or_else(|| NOT_DIAGNOSTICS.to_string())
    }
}

/// stdout を Diagnostic の列へ。**形が違えば `None`**（0 件とは違う）。
///
/// 形とは「`diagnostics` という配列を持つオブジェクト」である。配列の
/// 中の 1 件が壊れていれば（範囲が行の外・start > end・`message` が無い）、
/// **その 1 件だけ**捨てる。
pub(crate) fn parse(stdout: &str, source: &str) -> Option<Vec<Diagnostic>> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let items = value.get("diagnostics")?.as_array()?;
    let lines = Lines::new(source);
    Some(items.iter().filter_map(|item| one(item, &lines)).collect())
}

fn one(item: &Value, lines: &Lines) -> Option<Diagnostic> {
    let range = item.get("range")?;
    let position = |key: &str| -> Option<(usize, usize)> {
        let at = range.get(key)?;
        Some((
            usize::try_from(at.get("line")?.as_u64()?).ok()?,
            usize::try_from(at.get("character")?.as_u64()?).ok()?,
        ))
    };
    let (start_line, start_char) = position("start")?;
    let (end_line, end_char) = position("end")?;
    let start = lines.byte_at(start_line, start_char)?;
    let end = lines.byte_at(end_line, end_char)?;
    if start > end {
        return None;
    }
    // 幅 0 はその行の末尾まで。行末に立っていて広げようが無ければ行全体、
    // それでも空（空行）なら引く線が無いので捨てる。
    let range = if start == end {
        let line = lines.line(start_line)?;
        if start < line.end {
            start..line.end
        } else {
            line
        }
    } else {
        start..end
    };
    if range.is_empty() {
        return None;
    }
    let message = item.get("message")?.as_str()?.to_string();
    let source = item
        .get("source")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("lint");
    let code = match item.get("code") {
        Some(Value::String(code)) if !code.trim().is_empty() => Some(code.clone()),
        Some(Value::Number(code)) => Some(code.to_string()),
        _ => None,
    };
    let rule = match code {
        Some(code) => format!("{source}/{code}"),
        None => source.to_string(),
    };
    // rule はコメントの本文の区切りに使う（`lint: <rule> — …`）ので空白を持たない。
    let rule = rule.split_whitespace().collect::<Vec<_>>().join("_");
    let severity = item
        .get("severity")
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok());
    Some(Diagnostic {
        range,
        rule,
        message,
        severity,
    })
}

/// 行の頭のバイト位置と、UTF-16 の桁からバイトへの変換。
struct Lines<'a> {
    source: &'a str,
    /// 各行の内容のバイト範囲（改行を含まない。CRLF の `\r` も外す）。
    lines: Vec<Range<usize>>,
}

impl<'a> Lines<'a> {
    fn new(source: &'a str) -> Self {
        let mut lines = Vec::new();
        let mut at = 0;
        for piece in source.split_inclusive('\n') {
            let body = piece.strip_suffix('\n').unwrap_or(piece);
            let body = body.strip_suffix('\r').unwrap_or(body);
            lines.push(at..at + body.len());
            at += piece.len();
        }
        // 末尾が改行で終わる（または空の）文書には、その後ろに空の最終行がある
        // （LSP はそこを `{line: n, character: 0}` と呼ぶ）。
        if source.is_empty() || source.ends_with('\n') {
            lines.push(at..at);
        }
        Self { source, lines }
    }

    fn line(&self, line: usize) -> Option<Range<usize>> {
        self.lines.get(line).cloned()
    }

    /// (0 始まりの行, UTF-16 の桁) → バイト位置。行の外・サロゲートの途中は `None`。
    fn byte_at(&self, line: usize, character: usize) -> Option<usize> {
        let range = self.lines.get(line)?;
        let text = &self.source[range.clone()];
        let mut units = 0;
        for (offset, ch) in text.char_indices() {
            if units == character {
                return Some(range.start + offset);
            }
            units += ch.len_utf16();
            if units > character {
                return None; // サロゲートペアの間
            }
        }
        (units == character).then_some(range.end)
    }
}

/// 指摘を候補へ。**範囲は指摘の範囲そのまま**（Unit に寄せない）で、
/// 下線もその範囲に引く。
pub(crate) fn candidates(diagnostics: Vec<Diagnostic>, source: &str) -> Vec<Candidate> {
    let starts = tui_markdown::line_starts(source);
    diagnostics
        .into_iter()
        .map(|d| Candidate {
            lines: crate::review::lines_at(&starts, &d.range),
            atoms: vec![d.range.clone()],
            range: d.range,
            rule: d.rule,
            finding: Finding::Lint {
                message: d.message,
                severity: d.severity,
            },
            state: CandidateState::Pending,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(start: (u64, u64), end: (u64, u64)) -> String {
        format!(
            r#"{{"diagnostics":[{{"range":{{"start":{{"line":{},"character":{}}},
                "end":{{"line":{},"character":{}}}}},"message":"弱い表現","source":"textlint",
                "code":"ja-no-weak-phrase","severity":3}}]}}"#,
            start.0, start.1, end.0, end.1
        )
    }

    const SOURCE: &str = "いち\n効くかもしれない。\n😀かも\n";

    #[test]
    fn utf16_columns_become_byte_offsets() {
        // 2 行目（0 始まりで 1）の「かも」は 2〜4 桁。
        let got = parse(&diag((1, 2), (1, 4)), SOURCE).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(&SOURCE[got[0].range.clone()], "かも");
        assert_eq!(got[0].rule, "textlint/ja-no-weak-phrase");
        assert_eq!(got[0].severity, Some(3));
    }

    #[test]
    fn an_astral_character_counts_two_utf16_units() {
        // 😀 は UTF-16 で 2 単位。「かも」は 2〜4 桁である。
        let got = parse(&diag((2, 2), (2, 4)), SOURCE).unwrap();
        assert_eq!(&SOURCE[got[0].range.clone()], "かも");
        // サロゲートの間に落ちる位置は壊れた範囲。
        assert!(parse(&diag((2, 1), (2, 4)), SOURCE).unwrap().is_empty());
    }

    #[test]
    fn a_broken_range_drops_only_that_diagnostic() {
        for (start, end) in [((9, 0), (9, 1)), ((1, 40), (1, 41)), ((1, 4), (1, 2))] {
            let got = parse(&diag(start, end), SOURCE).unwrap();
            assert!(got.is_empty(), "{start:?}..{end:?}");
        }
        let two = r#"{"diagnostics":[
            {"range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}},"message":"x"},
            {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"message":"y"}]}"#;
        let got = parse(two, SOURCE).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message, "y");
        assert_eq!(got[0].rule, "lint", "source も code も無ければ lint");
    }

    #[test]
    fn a_zero_width_range_widens_to_the_end_of_the_line() {
        let got = parse(&diag((1, 2), (1, 2)), SOURCE).unwrap();
        assert_eq!(&SOURCE[got[0].range.clone()], "かもしれない。");
        // 行末に立っていれば行全体。
        let got = parse(&diag((0, 2), (0, 2)), SOURCE).unwrap();
        assert_eq!(&SOURCE[got[0].range.clone()], "いち");
    }

    #[test]
    fn output_that_is_not_the_shape_is_not_zero_diagnostics() {
        for bad in ["", "not json", "[]", r#"{"messages":[]}"#, r#"{"diagnostics":{}}"#] {
            assert_eq!(parse(bad, SOURCE), None, "{bad:?}");
        }
        assert_eq!(parse(r#"{"diagnostics":[]}"#, SOURCE), Some(vec![]));
    }

    #[test]
    fn a_numeric_code_and_whitespace_are_kept_out_of_the_rule() {
        let json = r#"{"diagnostics":[{"range":{"start":{"line":0,"character":0},
            "end":{"line":0,"character":1}},"message":"m","source":"my lint","code":42}]}"#;
        assert_eq!(parse(json, SOURCE).unwrap()[0].rule, "my_lint/42");
    }

    #[test]
    fn a_candidate_spans_the_diagnostic_itself() {
        let got = parse(&diag((1, 2), (1, 4)), SOURCE).unwrap();
        let range = got[0].range.clone();
        let c = candidates(got, SOURCE);
        assert_eq!(c[0].range, range);
        assert_eq!(c[0].atoms, vec![range]);
        assert_eq!(c[0].lines, (2, 2));
        assert_eq!(c[0].rule, "textlint/ja-no-weak-phrase");
    }

    #[test]
    fn the_command_runs_in_the_documents_directory_with_its_path_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc 1.md");
        std::fs::write(&path, SOURCE).unwrap();
        // 作業ディレクトリと最後の引数を message に書き出し、exit 1 で終わる。
        let script = dir.path().join("lint.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '{\"diagnostics\":[{\"range\":{\"start\":{\"line\":0,\"character\":0},\"end\":{\"line\":0,\"character\":1}},\"message\":\"%s|%s\"}]}' \"$PWD\" \"$1\"\nexit 1\n",
        )
        .unwrap();
        let lint = LintCommand::new(format!("sh '{}'", script.display()));
        let got = lint.run(&path, SOURCE).expect("exit 1 でも JSON なら成功");
        let (cwd, last) = got[0].message.split_once('|').unwrap();
        let canon = |p: &str| std::fs::canonicalize(p).unwrap();
        assert_eq!(canon(cwd), canon(dir.path().to_str().unwrap()));
        assert_eq!(canon(last), canon(path.to_str().unwrap()));
    }

    #[test]
    fn a_command_that_prints_no_json_says_so_without_an_excerpt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, SOURCE).unwrap();
        let err = LintCommand::new("echo 効くかもしれない").run(&path, SOURCE).unwrap_err();
        assert_eq!(err, NOT_DIAGNOSTICS);
        let err = LintCommand::new("false").run(&path, SOURCE).unwrap_err();
        assert_eq!(err, NOT_DIAGNOSTICS);
    }

    #[test]
    fn a_file_that_differs_from_the_screen_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "別の中身\n").unwrap();
        let err = LintCommand::new("echo '{\"diagnostics\":[]}'")
            .run(&path, SOURCE)
            .unwrap_err();
        assert!(err.contains("not the text on screen"), "{err}");
    }
}
