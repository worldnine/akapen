//! Review — **校正候補**（`docs/design/marks-only-and-review-mode.md` 4 節）。
//!
//! marks は「読め」（ラインマーカー）で、Review は「直せ」である。機構は
//! 同じ（問い → Unit ごとに Noul）で、**光ったあとの行き先**だけが違う。
//! 読むなら目で終わる。直すならコメントになり、段階 2 で LLM へ渡る。
//!
//! # 別機能である
//!
//! 共有するのは**判定器の一段の問い**（Noul）・Unit・境界・sha キャッシュ
//! だけで、UI（`R` の一覧）も読み出しも色も別に持つ。marks の定型の環
//! （`m` の popup、`]m` のジャンプ、つまみ、フォーカス）には**入らない**。
//! 判定器 `jev-annotate.py` は 1 行も変えていない — ルールの `text` を
//! 問いとして渡すだけである。
//!
//! # 上位 N % ではなく閾値
//!
//! marks のつまみは「上から何 %」だが、Review はルールごとの
//! [`crate::review_rules::Rule::threshold`] **以上**で切る。文書によって
//! slop の量は違うので、割合で切ると「きれいな文書から 20 % 拾う」に
//! なってしまう。4 節「検知器ではない」は拾いすぎ側に倒せと言っているが、
//! それは**足切りを低くする**ことであって、無い物を拾うことではない。
//!
//! 候補は score 降順ではなく**文書順**に並ぶ。直すのは上から順に読み
//! ながらで、点数の高い順に飛び回る作業ではない。
//!
//! # 捨てた候補は記録する
//!
//! `x` で捨てた候補は `~/.local/share/akapen/review/dismissed.jsonl` に
//! 追記し、同じ文書を開いたら出さない。**本文は書かない** — 範囲と sha と
//! ルールの id だけで、`~/.cache/akapen/semantic/` と同じ機密度で扱う
//! （0600 / 0700、リポジトリの中にも文書の隣にも置かない）。

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::ops::Range;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use semantic_reading::SemanticDocument;
use serde::{Deserialize, Serialize};

use crate::review_rules::{Action, Rule};

/// ルールの識別子。候補が持っている唯一の手がかりで、
/// [`crate::review_rules::Rules::get`] で本体を引く。
pub(crate) type RuleId = String;

/// 候補 1 つの状態。
///
/// **`Accepted` も `Dismissed` も一覧から消えない** — 印が変わるだけで
/// ある（`✓` / `–`）。Pending だけ残す `Tab` 切替は作らない（読み手の
/// 決定、2026-09-23）: 9 本のうち 3 本を見たという事実が、一覧の形その
/// ものであってほしい。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateState {
    /// まだ見ていない。本文に下線が引かれ、ガターに `!` が出る。
    Pending,
    /// `a` — コメントになった。本文の印はコメントのものに変わる。
    Accepted,
    /// `x` — 外れ。本文から消え、`dismissed.jsonl` に載る。
    Dismissed,
}

/// 校正候補 1 つ。
///
/// **diagnostics の形に寄せてある**（範囲・行・出所・重さ）ので、後で
/// LSP の `Diagnostic` へそのまま運べる。`--review-json` が出すのは
/// この列である。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    /// Unit が占めるバイト範囲（先頭 Atom の始まり〜末尾 Atom の終わり）。
    /// **同一性はここで決まる** — `dismissed.jsonl` の照合もこの範囲である。
    pub(crate) range: Range<usize>,
    /// 1 始まりの行（[`crate::comment::Comment`] と同じ数え方）。
    pub(crate) lines: (u32, u32),
    /// どのルールが拾ったか。
    pub(crate) rule: RuleId,
    /// 直し方（ルールから写す。段階 2 の送り先が読む）。
    pub(crate) action: Action,
    /// この Unit の score。
    pub(crate) score: f32,
    pub(crate) state: CandidateState,
    /// この Unit の Atom の範囲（文書順）。
    ///
    /// **下線はここへ引く。** [`Self::range`] に 1 本引くと、Atom の
    /// 間の空白や改行にも線が伸びる（Unit は隣り合わない Atom を持ちうる）。
    pub(crate) atoms: Vec<Range<usize>>,
}

impl Candidate {
    /// まだ見ていない候補か — 本文に下線が要るか。
    pub(crate) fn is_pending(&self) -> bool {
        self.state == CandidateState::Pending
    }

    /// 一覧の行頭に出る印。
    pub(crate) fn mark(&self) -> &'static str {
        match self.state {
            CandidateState::Pending => " ",
            CandidateState::Accepted => "✓",
            CandidateState::Dismissed => "–",
        }
    }
}

/// **コメントの本文の形はここ 1 か所である。**
///
/// 段階 2 の送り先（LLM への指示を組む側）がこの形を読むので、書式を
/// 散らすと送り先とここの 2 か所を直すことになる。`l` の一覧・`y copy`・
/// `s send` は既存の [`crate::comment::Comment`] の経路にそのまま乗る。
///
/// ```text
/// review: filler (0.87)
/// ```
pub(crate) fn comment_text(rule: &str, score: f32) -> String {
    format!("review: {rule} ({score:.2})")
}

/// [`comment_text`] の逆。形に合わなければ `None`（人の書いた赤入れ）。
///
/// 送る文面に契約を足すかどうかは、これで決まる
/// （[`crate::review_contract::compose`]）。形はここと `comment_text` の
/// 2 本で 1 組である。
pub(crate) fn parse_comment_text(text: &str) -> Option<(RuleId, f32)> {
    let rest = text.strip_prefix("review: ")?;
    let (rule, score) = rest.strip_suffix(')')?.split_once(" (")?;
    if rule.is_empty() || rule.contains(char::is_whitespace) {
        return None;
    }
    Some((rule.to_string(), score.parse().ok()?))
}

/// 注釈 1 つ分の候補（文書順）。
///
/// `score >= rule.threshold` の Unit を拾う。**`has_core()` は見ない** —
/// marks の投影（[`semantic_reading::marks::mark`]）は核を持たない Unit を
/// 落とすが、あれは「どこを読むか」を絞る仕掛けで、「ここを直せ」には
/// 関係が無い。4 節「検知器ではないから拾いすぎ側に倒す」に従う。
///
/// Atom を 1 つも持たない Unit は落とす（引く線が無い）。
pub(crate) fn candidates_for(
    document: &SemanticDocument,
    rule: &Rule,
    source: &str,
) -> Vec<Candidate> {
    let starts = tui_markdown::line_starts(source);
    let mut out: Vec<Candidate> = document
        .units
        .iter()
        .filter_map(|unit| {
            let score = unit.score?;
            if score < rule.threshold {
                return None;
            }
            let mut atoms: Vec<Range<usize>> = unit
                .atoms
                .iter()
                .filter_map(|index| document.atoms.get(index.0))
                .map(|atom| atom.range.clone())
                .filter(|range| range.start < range.end)
                .collect();
            atoms.sort_by_key(|range| range.start);
            let first = atoms.first()?.start;
            let last = atoms.last()?.end;
            // 範囲の終端は排他。1 バイト戻して「最後に触っている行」を
            // 取る（`crate::semantic::marked_lines` と同じ理由で、終端が
            // ちょうど行頭だと触っていない次の行を指す）。
            let start_line = tui_markdown::line_at(&starts, first) as u32 + 1;
            let end_line = tui_markdown::line_at(&starts, last.saturating_sub(1)) as u32 + 1;
            Some(Candidate {
                range: first..last,
                lines: (start_line, end_line.max(start_line)),
                rule: rule.id.clone(),
                action: rule.action,
                score,
                state: CandidateState::Pending,
                atoms,
            })
        })
        .collect();
    // **文書順**（score 降順ではない）。同じ位置に複数のルールが当たったら
    // ルールの id で安定させる。
    out.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(a.range.end.cmp(&b.range.end))
            .then(a.rule.cmp(&b.rule))
    });
    out
}

/// 一覧に出す Unit の先頭 `cols` 桁。
///
/// 改行と連続する空白は 1 つの空白に畳む — 一覧は 1 行なので、Unit が
/// 複数行にまたがっていても 1 行に見えなければならない。
pub(crate) fn head_of(source: &str, candidate: &Candidate, cols: usize) -> String {
    let slice = source
        .get(candidate.range.clone())
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    crate::overlay::clip_if_needed(&slice, cols)
}

// ---- 捨てた候補の記録 ------------------------------------------------

/// `dismissed.jsonl` の 1 行。**本文は入らない。**
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DismissedRecord {
    /// 捨てたときの文書の sha256（[`crate::semantic::source_digest`]）。
    source_sha: String,
    /// Unit のバイト範囲 `[start, end]`。
    range: [usize; 2],
    rule: String,
    /// UNIX 秒。**いつ捨てたか**であって、いつの文書かではない。
    at: u64,
}

/// 捨てた候補の置き場（追記のみの JSONL）。
///
/// `~/.local/share/akapen/review/dismissed.jsonl`
/// （`$XDG_DATA_HOME` があればそちら）。ファイルは 0600、ディレクトリは
/// 0700 で作る — [`crate::semantic_cache`] と同じ機密度である。
#[derive(Clone, Debug)]
pub(crate) struct DismissedStore {
    path: PathBuf,
}

/// 同じ文書で二度と出さないための鍵。
///
/// **sha は鍵に入れない** — 読み込むときに文書の sha で絞ってあるので、
/// ここに要るのは (ルール, 範囲) だけである。
pub(crate) type DismissedKey = (String, usize, usize);

impl DismissedStore {
    /// 置き場を、触らずに決める。`None` は「置き場が決められない」=
    /// セッション内だけ消えて記録は残らない、である。
    pub(crate) fn discover() -> Option<Self> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|dir| !dir.as_os_str().is_empty())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local").join("share"))
            })?;
        Some(Self::at(base.join("akapen").join("review")))
    }

    /// ディレクトリを指して作る（テスト用の口でもある）。
    pub(crate) fn at(dir: PathBuf) -> Self {
        Self {
            path: dir.join("dismissed.jsonl"),
        }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// この文書について捨てられた (ルール, 範囲)。
    ///
    /// **読めない行は無かったことにする。** 壊れた 1 行が、捨てた記録を
    /// 丸ごと失う理由にはならない（`serde_json` に落ちた行だけ飛ばす）。
    /// ファイルが無いのは「まだ 1 つも捨てていない」である。
    pub(crate) fn load(&self, source_sha: &str) -> HashSet<DismissedKey> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return HashSet::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str::<DismissedRecord>(line).ok())
            .filter(|record| record.source_sha.eq_ignore_ascii_case(source_sha))
            .map(|record| (record.rule, record.range[0], record.range[1]))
            .collect()
    }

    /// 1 件追記する。
    ///
    /// 呼び出し側は失敗を**握りつぶしてよい** — 書けなかったことは、
    /// 画面から候補を消さない理由にならない（次に開くと戻ってくるだけ）。
    pub(crate) fn append(&self, source_sha: &str, candidate: &Candidate) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("create {}", dir.display()))?;
        let record = DismissedRecord {
            source_sha: source_sha.to_string(),
            range: [candidate.range.start, candidate.range.end],
            rule: candidate.rule.clone(),
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        // 追記のみ。0600 は**作るときにしか効かない**ので、既にある
        // ファイルの権限はここでは触らない（読み手が緩めたなら読み手の判断）。
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        writeln!(file, "{}", serde_json::to_string(&record)?)
            .with_context(|| format!("append {}", self.path.display()))?;
        Ok(())
    }
}

// ---- `--review-json` -------------------------------------------------

/// `--review-json` が出す 1 件。**本文は含まない。**
#[derive(Serialize)]
struct JsonCandidate {
    lines: [u32; 2],
    rule: String,
    action: &'static str,
    score: f32,
}

/// `--review-json` が出す全体。
#[derive(Serialize)]
struct JsonReport {
    version: u32,
    source_sha: String,
    candidates: Vec<JsonCandidate>,
}

/// 候補の列を `--review-json` の形へ。
///
/// **本文は 1 バイトも入らない**（行番号・ルール・action・score だけ）。
/// 段階 2 と LSP の入口で、パイプの向こうが akapen を立てずに同じ候補を
/// 受け取れる。
pub(crate) fn to_json(source_sha: &str, candidates: &[Candidate]) -> Result<String> {
    let report = JsonReport {
        version: 1,
        source_sha: source_sha.to_string(),
        candidates: candidates
            .iter()
            .map(|c| JsonCandidate {
                lines: [c.lines.0, c.lines.1],
                rule: c.rule.clone(),
                action: c.action.as_str(),
                score: c.score,
            })
            .collect(),
    };
    Ok(serde_json::to_string_pretty(&report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_rules::Rules;
    use semantic_reading::{Atom, AtomIndex, AtomKind, SemanticUnit};

    /// 3 文の文書と、スコアの付いた 3 つの Unit。
    fn document(source: &str, scores: [f32; 3]) -> SemanticDocument {
        let mut starts = Vec::new();
        let mut at = 0usize;
        for line in source.split_inclusive('\n') {
            starts.push(at..at + line.trim_end_matches('\n').len());
            at += line.len();
        }
        let atoms: Vec<Atom> = starts
            .iter()
            .map(|r| Atom::new(r.clone(), AtomKind::Sentence))
            .collect();
        let units: Vec<SemanticUnit> = scores
            .iter()
            .enumerate()
            .map(|(i, score)| {
                let mut unit = SemanticUnit::new(format!("u{i}"), [AtomIndex(i)]);
                unit.score = Some(*score);
                unit
            })
            .collect();
        SemanticDocument::new(atoms, units)
    }

    const SOURCE: &str = "いち。\nに。\nさん。\n";

    fn filler() -> Rule {
        Rules::built_in().unwrap().get("filler").unwrap().clone()
    }

    #[test]
    fn the_threshold_is_inclusive_at_its_own_value() {
        // 境界。`>= threshold` であって `>` ではない — 0.5 ちょうどは拾う。
        let rule = filler();
        assert_eq!(rule.threshold, 0.5);
        let doc = document(SOURCE, [0.49, 0.50, 0.51]);
        let got = candidates_for(&doc, &rule, SOURCE);
        assert_eq!(got.len(), 2, "0.50 と 0.51 の 2 本");
        assert_eq!(got[0].lines, (2, 2));
        assert_eq!(got[1].lines, (3, 3));
    }

    #[test]
    fn a_unit_without_a_score_is_never_a_candidate() {
        let mut doc = document(SOURCE, [0.9, 0.9, 0.9]);
        doc.units[1].score = None;
        let got = candidates_for(&doc, &filler(), SOURCE);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|c| c.lines != (2, 2)));
    }

    #[test]
    fn a_unit_without_a_core_is_still_a_candidate() {
        // marks の投影は核を持たない Unit を落とすが、Review は落とさない
        // （「検知器ではない」— 拾いすぎ側に倒す）。
        let mut doc = document(SOURCE, [0.9, 0.9, 0.9]);
        doc.units[0].set_core([]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn candidates_come_out_in_document_order_not_by_score() {
        let doc = document(SOURCE, [0.6, 0.99, 0.7]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        let lines: Vec<u32> = got.iter().map(|c| c.lines.0).collect();
        assert_eq!(lines, [1, 2, 3], "score 降順なら 2,3,1 になる");
    }

    #[test]
    fn a_candidate_carries_its_rules_action_and_id() {
        let doc = document(SOURCE, [0.9, 0.0, 0.0]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        assert_eq!(got[0].rule, "filler");
        assert_eq!(got[0].action, Action::Delete);
        assert_eq!(got[0].state, CandidateState::Pending);
        assert_eq!(got[0].atoms.len(), 1);
    }

    #[test]
    fn a_multi_atom_unit_underlines_each_atom_but_spans_them_all() {
        // 下線は Atom ごと。範囲（同一性）は Unit 全体である。
        let mut doc = document(SOURCE, [0.9, 0.9, 0.0]);
        doc.units[0].atoms.push(AtomIndex(1));
        doc.units.remove(1);
        let got = candidates_for(&doc, &filler(), SOURCE);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].atoms.len(), 2);
        assert_eq!(got[0].lines, (1, 2));
        assert_eq!(got[0].range.start, 0);
    }

    #[test]
    fn the_comment_body_reads_back_and_a_hand_written_one_does_not() {
        let text = comment_text("filler", 0.87);
        assert_eq!(parse_comment_text(&text), Some(("filler".into(), 0.87)));
        for other in ["review: ここは要らない", "review: filler", "filler (0.87)", "review:  (0.5)"] {
            assert_eq!(parse_comment_text(other), None, "{other}");
        }
    }

    #[test]
    fn the_comment_body_is_rule_and_score_to_two_places() {
        assert_eq!(comment_text("filler", 0.8712), "review: filler (0.87)");
        assert_eq!(comment_text("hedge", 1.0), "review: hedge (1.00)");
    }

    #[test]
    fn the_head_folds_newlines_into_one_line() {
        let source = "ひとつめの文。\nふたつめの文。\n";
        let doc = document(source, [0.9, 0.0, 0.0]);
        let mut got = candidates_for(&doc, &filler(), source);
        got[0].range = 0..source.trim_end().len();
        let head = head_of(source, &got[0], 40);
        assert!(!head.contains('\n'), "{head}");
        assert_eq!(head, "ひとつめの文。 ふたつめの文。");
    }

    #[test]
    fn the_json_report_carries_no_document_text() {
        let doc = document(SOURCE, [0.9, 0.0, 0.0]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        let json = to_json(&"a".repeat(64), &got).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["candidates"][0]["lines"], serde_json::json!([1, 1]));
        assert_eq!(value["candidates"][0]["rule"], "filler");
        assert_eq!(value["candidates"][0]["action"], "delete");
        // 本文の 1 文字も入っていない。
        assert!(!json.contains("いち"), "{json}");
    }

    #[test]
    fn a_dismissed_candidate_comes_back_keyed_by_rule_and_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = DismissedStore::at(dir.path().join("review"));
        let sha = "b".repeat(64);
        assert!(store.load(&sha).is_empty(), "空のうちは何も戻らない");

        let doc = document(SOURCE, [0.9, 0.9, 0.0]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        store.append(&sha, &got[0]).unwrap();

        let back = store.load(&sha);
        assert_eq!(back.len(), 1);
        assert!(back.contains(&(
            "filler".to_string(),
            got[0].range.start,
            got[0].range.end
        )));
        // 別の文書の候補は戻らない。
        assert!(store.load(&"c".repeat(64)).is_empty());
        // 本文は書かない。
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(!raw.contains("いち"), "{raw}");
    }

    #[test]
    fn a_broken_line_in_the_store_does_not_lose_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = DismissedStore::at(dir.path().join("review"));
        let sha = "d".repeat(64);
        let doc = document(SOURCE, [0.9, 0.0, 0.0]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        store.append(&sha, &got[0]).unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap();
        writeln!(file, "{{not json").unwrap();
        drop(file);
        assert_eq!(store.load(&sha).len(), 1);
    }

    #[test]
    fn the_store_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = DismissedStore::at(dir.path().join("review"));
        let doc = document(SOURCE, [0.9, 0.0, 0.0]);
        let got = candidates_for(&doc, &filler(), SOURCE);
        store.append(&"e".repeat(64), &got[0]).unwrap();
        let file = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file, 0o600, "ファイルは 0600");
        assert_eq!(dir_mode, 0o700, "ディレクトリは 0700");
    }

    #[test]
    fn the_store_lands_under_the_data_directory() {
        // `$XDG_DATA_HOME` を見る。無ければ `~/.local/share`。
        let store = DismissedStore::at(PathBuf::from("/tmp/x/akapen/review"));
        assert!(store.path().ends_with("akapen/review/dismissed.jsonl"));
    }
}
