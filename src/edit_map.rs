//! 版と版のあいだの**変わっていないバイト**の対応表。
//!
//! `docs/design/marks-only-and-review-mode.md` 4 節「直接編集」。akapen が
//! 前後の版を見届けた reload のときだけ使う — 捨てた候補
//! （`dismissed.jsonl`）と accept 済みの review コメントを、**1 バイトも
//! 変わっていない範囲に限って**新しい位置へ写すためである。
//!
//! 差分は既存の `similar` で取る。行で比べてから、変わった行の塊の中だけを
//! 文字で比べ直す（2 段）。文字で全文を比べると、LLM が全体を書き直した
//! ときに O(N·D) で重くなるので、行の段で絞ってから細かく見る。

use std::ops::Range;
use std::time::Duration;

use similar::{DiffTag, TextDiff};

/// 1 回の比較に使ってよい時間。越えたら similar が粗い答えで打ち切る —
/// 粗くなるのは「変わった」と見なす側なので、写しすぎることは無い。
const DIFF_TIMEOUT: Duration = Duration::from_millis(200);

/// 変わっていないバイトの区間（旧 → 新）の列。旧の位置で昇順。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditMap {
    /// `(旧の区間, 新の始まり)`。長さは旧と新で同じである。
    segments: Vec<(Range<usize>, usize)>,
}

impl EditMap {
    /// `old` から `new` への対応を作る。
    pub(crate) fn between(old: &str, new: &str) -> Self {
        let mut raw = Vec::new();
        let lines = TextDiff::configure()
            .timeout(DIFF_TIMEOUT)
            .diff_lines(old, new);
        let old_starts = offsets(lines.old_slices());
        let new_starts = offsets(lines.new_slices());
        // 直前の Equal の終わり（旧・新）。そこから次の Equal までが
        // 「変わった塊」で、その中だけを文字で比べ直す。
        let (mut old_at, mut new_at) = (0usize, 0usize);
        for op in lines.ops() {
            if op.tag() != DiffTag::Equal {
                continue;
            }
            let old_range = old_starts[op.old_range().start]..old_starts[op.old_range().end];
            let new_start = new_starts[op.new_range().start];
            chars_between(old, new, old_at..old_range.start, new_at..new_start, &mut raw);
            raw.push((old_range.clone(), new_start));
            old_at = old_range.end;
            new_at = new_start + old_range.len();
        }
        chars_between(old, new, old_at..old.len(), new_at..new.len(), &mut raw);
        Self { segments: merge(raw) }
    }

    /// `range` の中が 1 バイトも変わっていなければ、新しい位置を返す。
    ///
    /// 変わっていないとは、**旧の 1 区間にまるごと収まる**ことである。
    /// 区間の継ぎ目（挿入・削除があった所）を跨ぐ範囲は、中身が変わって
    /// いる。空の範囲は写さない（引く線も付けるコメントも無い）。
    pub(crate) fn map_range(&self, range: &Range<usize>) -> Option<Range<usize>> {
        if range.start >= range.end {
            return None;
        }
        let (old, new_start) = self.segment_at(range.start)?;
        (range.end <= old.end).then(|| {
            let start = new_start + (range.start - old.start);
            start..start + range.len()
        })
    }

    /// 旧の位置 `at` を新の位置へ。変わった所に落ちたら、**そこを
    /// 置き換えた新しい中身の始まり**を返す（削られていれば、削られた
    /// 跡の位置）。一覧のカーソルを「直した箇所の次」に置くために使う。
    pub(crate) fn map_pos(&self, at: usize) -> usize {
        let mut before = 0usize;
        for (old, new_start) in &self.segments {
            if at < old.start {
                return before;
            }
            if at < old.end {
                return new_start + (at - old.start);
            }
            before = new_start + old.len();
        }
        before
    }

    fn segment_at(&self, at: usize) -> Option<(&Range<usize>, usize)> {
        let index = self.segments.partition_point(|(old, _)| old.end <= at);
        let (old, new_start) = self.segments.get(index)?;
        (old.start <= at).then_some((old, *new_start))
    }
}

/// トークンの列からバイト位置の表（先頭 0、末尾は全長）。
fn offsets(slices: &[&str]) -> Vec<usize> {
    let mut out = Vec::with_capacity(slices.len() + 1);
    let mut at = 0usize;
    out.push(0);
    for slice in slices {
        at += slice.len();
        out.push(at);
    }
    out
}

/// 変わった塊の中を文字で比べ、同じだった区間を `out` へ足す。
fn chars_between(
    old: &str,
    new: &str,
    old_span: Range<usize>,
    new_span: Range<usize>,
    out: &mut Vec<(Range<usize>, usize)>,
) {
    if old_span.is_empty() || new_span.is_empty() {
        return;
    }
    let (Some(old_text), Some(new_text)) = (old.get(old_span.clone()), new.get(new_span.clone()))
    else {
        return;
    };
    let chars = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_chars(old_text, new_text);
    let old_starts = offsets(chars.old_slices());
    let new_starts = offsets(chars.new_slices());
    for op in chars.ops() {
        if op.tag() != DiffTag::Equal {
            continue;
        }
        let start = old_span.start + old_starts[op.old_range().start];
        let end = old_span.start + old_starts[op.old_range().end];
        out.push((start..end, new_span.start + new_starts[op.new_range().start]));
    }
}

/// 旧でも新でも隣り合っている区間を 1 つに繋ぐ。行の段の Equal と、
/// その隣の文字の段の Equal は、繋がっていれば 1 つの変わっていない
/// 区間である（範囲がその継ぎ目を跨いでも、中身は変わっていない）。
fn merge(mut raw: Vec<(Range<usize>, usize)>) -> Vec<(Range<usize>, usize)> {
    raw.retain(|(old, _)| !old.is_empty());
    raw.sort_by_key(|(old, _)| old.start);
    let mut out: Vec<(Range<usize>, usize)> = Vec::with_capacity(raw.len());
    for (old, new_start) in raw {
        if let Some((last, last_new)) = out.last_mut()
            && last.end == old.start
            && *last_new + last.len() == new_start
        {
            last.end = old.end;
            continue;
        }
        out.push((old, new_start));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice<'a>(text: &'a str, range: &Range<usize>) -> &'a str {
        &text[range.clone()]
    }

    #[test]
    fn an_untouched_sentence_moves_with_the_text_before_it() {
        let old = "いち。\nに。\nさん。\n";
        let new = "いち。\nにを直した。\nさん。\n";
        let map = EditMap::between(old, new);
        let san = old.find("さん。").unwrap();
        let range = san..san + "さん。".len();
        let got = map.map_range(&range).expect("変わっていない");
        assert_eq!(slice(new, &got), "さん。");
    }

    #[test]
    fn a_range_with_one_changed_byte_is_not_mapped() {
        let old = "いち。\nに。\nさん。\n";
        let new = "いち。\nにい。\nさん。\n";
        let map = EditMap::between(old, new);
        let ni = old.find("に。").unwrap();
        assert_eq!(map.map_range(&(ni..ni + "に。".len())), None);
    }

    #[test]
    fn an_insertion_inside_a_range_counts_as_a_change() {
        // 1 字も消えていなくても、中に足されたら中身は変わっている。
        let old = "あいうえお\n";
        let new = "あいXうえお\n";
        let map = EditMap::between(old, new);
        assert_eq!(map.map_range(&(0.."あいうえお".len())), None);
        // 足された所の外側は、それぞれ写る。
        assert_eq!(map.map_range(&(0.."あい".len())), Some(0.."あい".len()));
    }

    #[test]
    fn an_unchanged_sentence_on_an_edited_line_is_still_mapped() {
        // 行の段では「変わった行」でも、文字の段で同じなら写す。
        let old = "前の文。後の文。\n";
        let new = "前の文を直した。後の文。\n";
        let map = EditMap::between(old, new);
        let after = old.find("後の文。").unwrap();
        let got = map.map_range(&(after..after + "後の文。".len())).unwrap();
        assert_eq!(slice(new, &got), "後の文。");
    }

    #[test]
    fn a_range_across_an_unchanged_line_and_an_unchanged_prefix_is_mapped() {
        // 行の Equal と、次の行の変わっていない頭は繋がっている。
        let old = "いち。\nに。さん。\n";
        let new = "いち。\nに。よん。\n";
        let map = EditMap::between(old, new);
        let end = old.find("さん").unwrap();
        let got = map.map_range(&(0..end)).unwrap();
        assert_eq!(slice(new, &got), "いち。\nに。");
    }

    #[test]
    fn a_deleted_spot_maps_to_where_it_was() {
        let old = "いち。\nに。\nさん。\n";
        let new = "いち。\nさん。\n";
        let map = EditMap::between(old, new);
        let ni = old.find("に。").unwrap();
        assert_eq!(map.map_pos(ni), new.find("さん。").unwrap());
        // 変わっていない所はそのまま写る。
        assert_eq!(map.map_pos(old.find("さん").unwrap()), new.find("さん").unwrap());
        assert_eq!(map.map_pos(0), 0);
    }

    #[test]
    fn identical_texts_map_everything_to_itself() {
        let text = "いち。\nに。\n";
        let map = EditMap::between(text, text);
        assert_eq!(map.map_range(&(0..text.len())), Some(0..text.len()));
    }

    #[test]
    fn an_empty_range_is_never_mapped() {
        let map = EditMap::between("abc\n", "abc\n");
        assert_eq!(map.map_range(&(1..1)), None);
    }
}
