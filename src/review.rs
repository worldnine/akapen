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
//! # 出どころは 2 つ
//!
//! 候補は Jev のルール（下の閾値）か、`--lint-cmd` の linter（[`crate::lint`]）が
//! 出す。違うのは [`Finding`] の中身（score と直し方 / 理由の文）と一覧の行・
//! コメントの本文だけで、範囲・dismiss・差分越しの引き継ぎ・`e` は同じ道を通る。
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
//! 追記し、同じ文書を開いても一覧に薄く `–` で出す（本文には下線も印も
//! 出さない）。もう一度 `x` で戻すと取り消しの行を追記する — 同じ鍵は
//! 最後の行が勝つ（[`DismissedStore::load`]）。**本文は書かない** — 範囲と sha と
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
///
/// **候補はこの状態を持たない。** 毎回ほかの持ち物から導く
/// （[`crate::app::App::candidate_states`]）— 状態を 2 か所に持つと、片方だけ
/// 変わる経路（本文の `d`、`l` の一覧の `d`、`s` で送る）でずれる:
///
/// | 状態 | 何から決まるか |
/// | --- | --- |
/// | `Dismissed` | 捨てた記録（`dismissed.jsonl` の最後の行）に載っている |
/// | `Accepted` | この候補の review / lint コメントが今ある |
/// | `Sent` | そのコメントを `s` で送った（このセッション、この版のうち） |
/// | `Pending` | どれでもない |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateState {
    /// まだ見ていない。本文に下線が引かれ、ガターに白抜きの重さが出る。
    Pending,
    /// `a` — コメントになった。本文の印はコメントのものに変わる。
    Accepted,
    /// `x` — 外れ。本文から消え、`dismissed.jsonl` に載る。
    Dismissed,
    /// accept したコメントを `s` で送った。画面では `Accepted` と同じ `✓`
    /// （新しい印は作らない — 読み手の決定）。`a` / `x` は効かない
    /// （二重に送らせない）。ファイルが書き換わったら決め直す。
    Sent,
}

impl CandidateState {
    /// まだ見ていない候補か — 本文に下線が要るか。
    pub(crate) fn is_pending(self) -> bool {
        self == Self::Pending
    }

    /// 一覧の行頭に出る印。
    pub(crate) fn mark(self) -> &'static str {
        match self {
            Self::Pending => " ",
            Self::Accepted | Self::Sent => "✓",
            Self::Dismissed => "–",
        }
    }
}

/// 候補を**誰が**拾ったか、とその出どころに固有の中身。
///
/// Jev のルールは score と直し方を持ち、linter は理由の文（`message`）を
/// 持つ。一覧の行・コメントの本文・送る契約はここで分かれ、それ以外
/// （範囲・dismiss・差分越しの引き継ぎ・`e`）は同じ道を通る。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Finding {
    /// `review-rules.json` のルール（Jev）。
    Rule {
        /// 直し方（ルールから写す。段階 2 の送り先が読む）。
        action: Action,
        /// この Unit の score。
        score: f32,
    },
    /// `--lint-cmd` の指摘（[`crate::lint`]）。
    Lint {
        /// linter の理由の文。一覧とコメントに出る。
        message: String,
        /// LSP の severity。下線の色とガターの白抜きの印になる
        /// （[`Candidate::severity`]、[`crate::decoration::ReviewSeverity`]）。
        severity: Option<u8>,
    },
}

/// 校正候補 1 つ。
///
/// **diagnostics の形に寄せてある**（範囲・行・出所・重さ）ので、後で
/// LSP の `Diagnostic` へそのまま運べる。`--review-json` が出すのは
/// この列である。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    /// Unit が占めるバイト範囲（先頭 Atom の始まり〜末尾 Atom の終わり）。
    /// lint の指摘なら指摘の範囲そのもの。
    /// **同一性はここで決まる** — `dismissed.jsonl` の照合もこの範囲である。
    pub(crate) range: Range<usize>,
    /// 1 始まりの行（[`crate::comment::Comment`] と同じ数え方）。
    pub(crate) lines: (u32, u32),
    /// どのルールが拾ったか。lint なら `<source>/<code>`。
    pub(crate) rule: RuleId,
    /// 出どころに固有の中身。
    pub(crate) finding: Finding,
    /// この Unit の Atom の範囲（文書順）。
    ///
    /// **下線はここへ引く。** [`Self::range`] に 1 本引くと、Atom の
    /// 間の空白や改行にも線が伸びる（Unit は隣り合わない Atom を持ちうる）。
    pub(crate) atoms: Vec<Range<usize>>,
}

/// **文書全体を範囲にする指摘**の閾値 — 範囲が文書の行の何割を覆えば
/// 「文書全体」とみなすか（分子, 分母）。
///
/// textlint の `ai-tech-writing-guideline` の総評は L1–192 / 192 行を覆う。
/// これを他の候補と同じに扱うと、一覧の先頭に来て、全行に下線と `!` が
/// 立ち、本文のどの指摘も見分けられなくなる。9 割にしたのは、総評が
/// 前付け（front matter）や末尾の空行を外して返す linter でも拾えるように
/// するためで、段落 1 つ・節 1 つの指摘（`sentence-length` の 3 行など）は
/// 長い文書では 1 割にも届かない。
pub(crate) const WHOLE_DOCUMENT_SHARE: (usize, usize) = (9, 10);

/// 「文書全体」とみなす最小の行数。1〜2 行の文書では、1 文の指摘が
/// そのまま全体を覆ってしまう — それは総評ではなく普通の指摘である。
pub(crate) const WHOLE_DOCUMENT_MIN_LINES: usize = 3;

impl Candidate {
    /// **重さ**（下線の色とガターの白抜きの印）。Jev のルールは `Info`（青緑）。
    pub(crate) fn severity(&self) -> crate::decoration::ReviewSeverity {
        match &self.finding {
            Finding::Rule { .. } => crate::decoration::ReviewSeverity::Info,
            Finding::Lint { severity, .. } => crate::decoration::ReviewSeverity::from_lsp(*severity),
        }
    }

    /// **文書全体を範囲にする指摘か**（`total_lines` は文書の行数）。
    ///
    /// そうなら一覧の**末尾**に回り、本文には下線もガターの白抜きの印も出さない。
    /// 一覧の行は `L1` ではなく「文書全体」と名乗る。閾値は
    /// [`WHOLE_DOCUMENT_SHARE`] と [`WHOLE_DOCUMENT_MIN_LINES`]。
    pub(crate) fn is_whole_document(&self, total_lines: usize) -> bool {
        let covered = (self.lines.1 as usize + 1).saturating_sub(self.lines.0 as usize);
        let (num, den) = WHOLE_DOCUMENT_SHARE;
        covered >= WHOLE_DOCUMENT_MIN_LINES && covered * den >= total_lines * num
    }

    /// accept したときのコメントの本文（[`comment_text`] / [`lint_comment_text`]）。
    pub(crate) fn comment_text(&self) -> String {
        match &self.finding {
            Finding::Rule { score, .. } => comment_text(&self.rule, *score),
            Finding::Lint { message, .. } => lint_comment_text(&self.rule, message),
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

/// lint の指摘を accept したコメントの本文。**形はここ 1 か所である。**
///
/// ```text
/// lint: textlint/ja-no-weak-phrase — 弱い表現: "かも" が使われています。
/// ```
///
/// 複数行の `message`（`理由:` `修正:` を続ける linter がある）は 1 行に
/// 畳む — コメントは行の範囲と 1 対 1 で、送る文面の中で次の指摘と
/// 見分けられなければならない。
pub(crate) fn lint_comment_text(rule: &str, message: &str) -> String {
    format!("lint: {rule} — {}", fold(message))
}

/// [`lint_comment_text`] の逆。形に合わなければ `None`（人の書いた赤入れ）。
pub(crate) fn parse_lint_comment_text(text: &str) -> Option<(RuleId, String)> {
    let rest = text.strip_prefix("lint: ")?;
    let (rule, message) = rest.split_once(" — ")?;
    if rule.is_empty() || rule.contains(char::is_whitespace) {
        return None;
    }
    Some((rule.to_string(), message.to_string()))
}

/// Review が作ったコメント（Jev のルールか lint か）なら、その rule。
///
/// **差分越しの片付け・付け直しはこれで見分ける**
/// （[`crate::app::App::settle_review_comments`]）。人の赤入れは `None`。
pub(crate) fn candidate_comment_rule(text: &str) -> Option<RuleId> {
    parse_comment_text(text)
        .map(|(rule, _)| rule)
        .or_else(|| parse_lint_comment_text(text).map(|(rule, _)| rule))
}

/// 改行と連続する空白を 1 つの空白に畳む。
pub(crate) fn fold(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
            Some(Candidate {
                range: first..last,
                lines: lines_at(&starts, &(first..last)),
                rule: rule.id.clone(),
                finding: Finding::Rule {
                    action: rule.action,
                    score,
                },
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

/// バイト範囲が触っている 1 始まりの行（先頭, 末尾）。
///
/// 範囲の終端は排他。1 バイト戻して「最後に触っている行」を取る
/// （`crate::semantic::marked_lines` と同じ理由で、終端がちょうど行頭だと
/// 触っていない次の行を指す）。
pub(crate) fn lines_at(starts: &[usize], range: &Range<usize>) -> (u32, u32) {
    let start_line = tui_markdown::line_at(starts, range.start) as u32 + 1;
    let end_line = tui_markdown::line_at(starts, range.end.saturating_sub(1)) as u32 + 1;
    (start_line, end_line.max(start_line))
}

/// 一覧に出す Unit の先頭 `cols` 桁。
///
/// 改行と連続する空白は 1 つの空白に畳む — 一覧は 1 行なので、Unit が
/// 複数行にまたがっていても 1 行に見えなければならない。
///
/// **lint の候補は本文ではなく理由（`message`）を見せる。** 指摘の範囲は
/// 「かも」の 2 字のように短く、本文の先頭では何を言われたのか分からない。
pub(crate) fn head_of(source: &str, candidate: &Candidate, cols: usize) -> String {
    let slice = match &candidate.finding {
        Finding::Rule { .. } => fold(source.get(candidate.range.clone()).unwrap_or_default()),
        Finding::Lint { message, .. } => fold(message),
    };
    crate::overlay::clip_if_needed(&slice, cols)
}

// ---- 捨てた候補の記録 ------------------------------------------------

/// `dismissed.jsonl` の 1 行。**本文は入らない。**
///
/// **追記のみで、同じ鍵（sha, ルール, 範囲）は最後の行が勝つ。** `x` で
/// 捨てると `undo` の無い行、捨てたのを `x` で戻すと `"undo": true` の行が
/// 足される。書き換えも削除もしない — 途中で落ちても、読めた行までの
/// 判断がそのまま残る。`undo` の無い古い行はそのまま「捨てた」と読む。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DismissedRecord {
    /// 捨てたときの文書の sha256（[`crate::semantic::source_digest`]）。
    source_sha: String,
    /// Unit のバイト範囲 `[start, end]`。
    range: [usize; 2],
    rule: String,
    /// UNIX 秒。**いつ捨てたか（戻したか）**であって、いつの文書かではない。
    at: u64,
    /// 捨てた判断の取り消し。捨てる行には書かない（古い形と同じ行になる）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    undo: bool,
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

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// 読めた行を、書かれた順に。
    ///
    /// **読めない行は無かったことにする。** 壊れた 1 行が、捨てた記録を
    /// 丸ごと失う理由にはならない（`serde_json` に落ちた行だけ飛ばす）。
    /// ファイルが無いのは「まだ 1 つも捨てていない」である。
    fn records(&self) -> Vec<DismissedRecord> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str::<DismissedRecord>(line).ok())
            .collect()
    }

    /// この文書について、鍵ごとの**最後の行**（`true` = 捨てている）。
    fn latest(&self, source_sha: &str) -> std::collections::HashMap<DismissedKey, bool> {
        let mut out = std::collections::HashMap::new();
        for record in self
            .records()
            .into_iter()
            .filter(|record| record.source_sha.eq_ignore_ascii_case(source_sha))
        {
            out.insert((record.rule, record.range[0], record.range[1]), !record.undo);
        }
        out
    }

    /// この文書について**いま**捨てられている (ルール, 範囲)。
    ///
    /// 同じ鍵は最後の行が勝つ — 捨てて戻した候補は入らない。
    pub(crate) fn load(&self, source_sha: &str) -> HashSet<DismissedKey> {
        self.latest(source_sha)
            .into_iter()
            .filter_map(|(key, dismissed)| dismissed.then_some(key))
            .collect()
    }

    /// 捨てた記録を 1 件追記する。
    ///
    /// 呼び出し側は失敗を**握りつぶしてよい** — 書けなかったことは、
    /// 画面から候補を消さない理由にならない（次に開くと戻ってくるだけ）。
    pub(crate) fn append(&self, source_sha: &str, candidate: &Candidate) -> Result<()> {
        self.append_key(source_sha, &candidate.rule, &candidate.range, false)
    }

    /// **捨てた判断の取り消し**を 1 件追記する（`x` で戻す）。
    ///
    /// 前の行は消さない。同じ鍵の最後の行として読まれ、捨てた記録を
    /// 打ち消す（[`Self::load`]）。失敗の扱いは [`Self::append`] と同じ。
    pub(crate) fn append_undo(&self, source_sha: &str, candidate: &Candidate) -> Result<()> {
        self.append_key(source_sha, &candidate.rule, &candidate.range, true)
    }

    /// (ルール, 範囲) を 1 件追記する。[`Self::append`]・[`Self::append_undo`]
    /// と、差分越しの引き継ぎ（[`Self::carry`]）の共通の口。
    fn append_key(
        &self,
        source_sha: &str,
        rule: &str,
        range: &Range<usize>,
        undo: bool,
    ) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("create {}", dir.display()))?;
        let record = DismissedRecord {
            source_sha: source_sha.to_string(),
            range: [range.start, range.end],
            rule: rule.to_string(),
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            undo,
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

    /// **捨てた判断を、見届けた書き換えの向こうへ写す。**
    ///
    /// `old_sha` の版で**いま**捨てている (ルール, 範囲) のうち、`map` で
    /// **中身が 1 バイトも変わっていない**ものだけを新しい位置へ写し、
    /// `new_sha` の記録として追記する。範囲の中が少しでも変わっていれば
    /// 写さない — 直された文は判定し直す。**捨てて戻した記録は写さない**
    /// （最後の行が取り消しなら、その鍵は捨てていない）。
    ///
    /// 戻した判断の方も向こうへ効かせる: 新しい版で同じ範囲が捨てられて
    /// いれば（元に戻した版に、昔の記録が残っている）、取り消しの行を
    /// 足す。足さないと、戻した候補が元の版へ戻ったとたんに消える。
    ///
    /// 呼ぶのは akapen が前後の版を両方見た reload だけである
    /// （[`crate::reload::reload_source`]）。閉じている間に変わった
    /// ファイルには差分の元が無い。
    ///
    /// 新しい版に既にある記録（元に戻した版など）は二度書かない。書けな
    /// かった 1 件は数に入れず先へ進む（[`Self::append`] と同じ理由）。
    pub(crate) fn carry(
        &self,
        old_sha: &str,
        new_sha: &str,
        map: &crate::edit_map::EditMap,
    ) -> Carried {
        let mut out = Carried::default();
        if old_sha.eq_ignore_ascii_case(new_sha) {
            return out;
        }
        let already = self.latest(new_sha);
        let mut records: Vec<(DismissedKey, bool)> = self.latest(old_sha).into_iter().collect();
        // 書く順を決める（HashMap の順は run ごとに変わる）。
        records.sort();
        for ((rule, start, end), dismissed) in records {
            let Some(range) = map.map_range(&(start..end)) else {
                if dismissed {
                    out.changed += 1;
                }
                continue;
            };
            let there = already
                .get(&(rule.clone(), range.start, range.end))
                .copied()
                .unwrap_or(false);
            if !dismissed {
                // 戻した判断。向こうで捨てていなければ、書くことは無い。
                if there && self.append_key(new_sha, &rule, &range, true).is_ok() {
                    out.restored += 1;
                }
                continue;
            }
            if there {
                out.carried += 1;
                continue;
            }
            if self.append_key(new_sha, &rule, &range, false).is_ok() {
                out.carried += 1;
            }
        }
        out
    }

    /// **捨てた記録を全部消す**（`--review-dismissed-clear`）。戻り値は、
    /// 消す前に**捨てていた**候補の数（文書ごと・鍵ごとに最後の行で数える
    /// — 捨てて戻したものは入らない）。
    ///
    /// ファイルが無いのは「0 件消した」。
    pub(crate) fn clear(&self) -> Result<usize> {
        let mut latest: std::collections::HashMap<(String, DismissedKey), bool> =
            std::collections::HashMap::new();
        for record in self.records() {
            latest.insert(
                (
                    record.source_sha.to_ascii_lowercase(),
                    (record.rule, record.range[0], record.range[1]),
                ),
                !record.undo,
            );
        }
        let removed = latest.values().filter(|dismissed| **dismissed).count();
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(removed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).with_context(|| format!("remove {}", self.path.display())),
        }
    }
}

/// [`DismissedStore::carry`] の数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Carried {
    /// 新しい位置へ写した本数。
    pub(crate) carried: usize,
    /// 範囲の中が変わっていて、写さなかった本数。
    pub(crate) changed: usize,
    /// 戻した判断を向こうの版へ効かせた本数（取り消しの行を足した）。
    pub(crate) restored: usize,
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
        // lint の候補は `--review-json` に来ない（判定器の経路だけが呼ぶ）。
        candidates: candidates
            .iter()
            .filter_map(|c| match c.finding {
                Finding::Rule { action, score } => Some(JsonCandidate {
                    lines: [c.lines.0, c.lines.1],
                    rule: c.rule.clone(),
                    action: action.as_str(),
                    score,
                }),
                Finding::Lint { .. } => None,
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
        assert_eq!(
            got[0].finding,
            Finding::Rule {
                action: Action::Delete,
                score: 0.9
            }
        );
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
    fn the_lint_comment_body_reads_back_folded_to_one_line() {
        let text = lint_comment_text("textlint/ja-no-mixed-period", "文末が\"。\"で終わっていません。\n理由: 句点");
        assert_eq!(
            text,
            "lint: textlint/ja-no-mixed-period — 文末が\"。\"で終わっていません。 理由: 句点"
        );
        assert_eq!(
            candidate_comment_rule(&text).as_deref(),
            Some("textlint/ja-no-mixed-period")
        );
        assert_eq!(candidate_comment_rule("review: filler (0.87)").as_deref(), Some("filler"));
        for other in ["lint: ここは要らない", "lint:  — x", "lint: a b — x", "ここは lint: x — y"] {
            assert_eq!(candidate_comment_rule(other), None, "{other}");
        }
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
