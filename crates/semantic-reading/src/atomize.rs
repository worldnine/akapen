//! Atom 生成 — 文書を機械的な位置単位へ割る。
//!
//! 設計書の「Jev に判断させないもの」に `syntax parsing` と `Atom 生成` が
//! 並んでいるとおり、ここは意味を一切見ない。Jev も呼ばず、ネットワークも
//! 使わず、同じ入力からは必ず同じ Atom 列が出る。API キーを持たない
//! ユーザーにも、この層までは値が届く。
//!
//! 生成するのは [`AtomKind`] の各種別だけで、[`crate::ReadingTier`] も
//! [`crate::SemanticUnit`] も付けない。意味の境界は Jev の仕事である。
//!
//! # 範囲の取り方
//!
//! Markdown の解析には `pulldown-cmark` の `into_offset_iter()` を使う。
//! event ごとの byte range が直接手に入るので、位置を自分で数えることは
//! しない。akapen 本体のレンダラと同じ version・同じ parse option を使う
//! ため、Atom の境界は画面に出ているものとずれない。
//!
//! 得られた range は、
//!
//! - 前後の空白・改行を落とす（`# 見出し\n` は `# 見出し` まで）
//! - 行頭のマーカー（`- ` / `1. ` / `> `）を含める
//!
//! という 2 点だけ調整する。マーカーを含めるのは、含めないと DIM にした
//! ときにマーカーだけ明るく残るため。
//!
//! # 文まで割る
//!
//! 散文・リスト項目・引用は、どれも同じ規則で**文**へ割る。Atom は
//! 「安全に位置を指定できる機械的単位」であって段落ではないので、
//! 複数の文を 1 つにまとめた塊は Atom ではない。表示単位は Atom なので、
//! 項目を割らないと「項目まるごと光る / 項目まるごと沈む」しか選べない。
//!
//! 項目が複数の文へ割れるとき、行頭のマーカー（`- ` / `1. `）は
//! **最初の文の Atom に入る**。マーカーは項目の先頭にあり、range は
//! そこから始まるので、文へ割れば自然にそうなる。2 文目以降の
//! [`AtomKind`] も `ListItem` のままにする — 種別は「どの構文要素に
//! 由来するか」であって、文の通し番号ではない。
//!
//! # 敷き詰めない
//!
//! Atom 列は文書を隙間なく覆わない。空行、`---`、リストの入れ子の
//! インデントなど、どの Atom にも属さない領域があってよい。
//! [`crate::policy::decorate`] は「どの Unit にも属さない Atom は NORMAL」と
//! 扱うので、隙間はそのまま NORMAL の地の文として残る。

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::atom::{Atom, AtomKind};

/// source を Atom 列へ分解する。
///
/// 返る Atom 列は
///
/// 1. source 位置の昇順に並び、範囲が互いに重ならない
/// 2. 両端が UTF-8 の文字境界にある
/// 3. 同じ入力に対して常に同じ（乱数も反復順依存も無い）
/// 4. 空・空白のみの Atom を含まない
///
/// ことを保証する。文書全体を覆うことは保証しない。
///
/// ```
/// use semantic_reading::{AtomKind, atomize};
///
/// let source = "# 見出し\n\n本文です。二文目です。\n";
/// let atoms = atomize(source);
///
/// assert_eq!(atoms.len(), 3);
/// assert_eq!(atoms[0].kind, AtomKind::Heading);
/// assert_eq!(&source[atoms[0].range.clone()], "# 見出し");
/// assert_eq!(&source[atoms[1].range.clone()], "本文です。");
/// assert_eq!(&source[atoms[2].range.clone()], "二文目です。");
/// ```
pub fn atomize(source: &str) -> Vec<Atom> {
    let events: Vec<(Event<'_>, Range<usize>)> = Parser::new_ext(source, parse_options())
        .into_offset_iter()
        .collect();

    let mut atoms = Atoms::new(source);
    let mut quote_depth = 0usize;
    let mut i = 0;

    while i < events.len() {
        let (event, range) = &events[i];
        match event {
            // 見出し・コードブロック・表は中身を割らず、ブロックごと 1 Atom。
            Event::Start(Tag::Heading { .. }) => {
                atoms.push(range.clone(), AtomKind::Heading);
                i = block_end(&events, i);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                atoms.push(range.clone(), AtomKind::CodeBlock);
                i = block_end(&events, i);
            }
            Event::Start(Tag::Table(_)) => {
                atoms.push(range.clone(), AtomKind::Table);
                i = block_end(&events, i);
            }
            // YAML front matter は本文ではないので Atom にしない。
            Event::Start(Tag::MetadataBlock(_)) => {
                i = block_end(&events, i);
            }
            // 段落は文へ割る。引用の中なら種別を BlockQuote にする。
            Event::Start(Tag::Paragraph)
            | Event::Start(Tag::DefinitionListTitle)
            | Event::Start(Tag::DefinitionListDefinition) => {
                let end = block_end(&events, i);
                let kind = if quote_depth > 0 {
                    AtomKind::BlockQuote
                } else {
                    AtomKind::Sentence
                };
                let guards = inline_guards(&events[i..end]);
                for sentence in split_sentences(source, range.clone(), &guards) {
                    atoms.push(sentence, kind);
                }
                i = end;
            }
            // 項目の中身も段落と同じ規則で文へ割る。行頭のマーカーは range の
            // 先頭にあるので、割れば最初の文の Atom に入る。入れ子のリストが
            // あれば、そこで親を打ち切り、子は独立した Atom として続けて歩く
            // （範囲を重ねないため）。
            Event::Start(Tag::Item) => {
                let end = block_end(&events, i);
                let (body, next) = match nested_list(&events[i..end]) {
                    Some(offset) => (range.start..events[i + offset].1.start, i + offset),
                    None => (range.clone(), end),
                };
                if !is_marker_only(source[body.clone()].trim()) {
                    let mut guards = inline_guards(&events[i..end]);
                    // `1.` の `.` は文末ではない。マーカーを guard に入れて
                    // 終止記号の探索から外す（`1. 一つ目` が割れないように）。
                    let marker = marker_len(source, body.start);
                    if marker > 0 {
                        guards.push(body.start..body.start + marker);
                    }
                    for sentence in split_sentences(source, body, &guards) {
                        atoms.push(sentence, AtomKind::ListItem);
                    }
                }
                i = next;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                quote_depth += 1;
                i += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote_depth = quote_depth.saturating_sub(1);
                i += 1;
            }
            // 生 HTML ブロックは分類しきれないので Other に落とす。
            Event::Html(_) => {
                atoms.push(range.clone(), AtomKind::Other);
                i += 1;
            }
            // List / Item の End、区切り線、脚注定義の口など。Atom にしない。
            _ => i += 1,
        }
    }

    atoms.finish()
}

/// akapen 本体のレンダラ（`third_party/tui-markdown`）と同じ parse option。
///
/// 揃えないと、同じ version の parser でも table や tasklist の解釈が変わり、
/// Atom の境界が画面に出ているものとずれる。
fn parse_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options.insert(Options::ENABLE_SUPERSCRIPT);
    options.insert(Options::ENABLE_SUBSCRIPT);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_DEFINITION_LIST);
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_TABLES);
    options
}

/// `events[start]` の `Start` に対応する `End` の 1 つ後ろの添字。
fn block_end(events: &[(Event<'_>, Range<usize>)], start: usize) -> usize {
    let mut depth = 0usize;
    for (i, (event, _)) in events.iter().enumerate().skip(start) {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    events.len()
}

/// 項目の中に最初に現れる入れ子リストの、項目先頭からの相対添字。
///
/// 先に見つかるリストは必ず直下の子である（さらに深いリストは別の項目の
/// 内側にあり、その項目より後ろに来る）。
fn nested_list(item: &[(Event<'_>, Range<usize>)]) -> Option<usize> {
    item.iter()
        .position(|(event, _)| matches!(event, Event::Start(Tag::List(_))))
}

/// 文の切れ目を探してはいけない領域。
///
/// インラインコード・リンク・画像・数式の中の `.` や `。` で切ると、URL や
/// コード片が途中で割れる。ブロックの event を見れば範囲がそのまま手に入る。
///
/// コードブロック・表・生 HTML も入れてある。段落の中には現れないので
/// そこでは何もしないが、**リスト項目の中には現れうる**（項目にぶら下がった
/// フェンスや表）。入れておかないと、コードの中の `.` で項目が割れる。
fn inline_guards(block: &[(Event<'_>, Range<usize>)]) -> Vec<Range<usize>> {
    block
        .iter()
        .filter(|(event, _)| {
            matches!(
                event,
                Event::Code(_)
                    | Event::InlineMath(_)
                    | Event::DisplayMath(_)
                    | Event::InlineHtml(_)
                    | Event::Html(_)
                    | Event::Start(Tag::Link { .. })
                    | Event::Start(Tag::Image { .. })
                    | Event::Start(Tag::CodeBlock(_))
                    | Event::Start(Tag::Table(_))
            )
        })
        .map(|(_, range)| range.clone())
        .collect()
}

/// 収集した Atom 列。不変条件はここで一括して守る。
struct Atoms<'a> {
    source: &'a str,
    atoms: Vec<Atom>,
}

impl<'a> Atoms<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            atoms: Vec::new(),
        }
    }

    /// 範囲を整えてから積む。空になるもの、直前の Atom と重なるものは捨てる。
    fn push(&mut self, range: Range<usize>, kind: AtomKind) {
        let Some(range) = tighten(self.source, range) else {
            return;
        };
        if let Some(last) = self.atoms.last()
            && range.start < last.range.end
        {
            return;
        }
        self.atoms.push(Atom::new(range, kind));
    }

    fn finish(self) -> Vec<Atom> {
        self.atoms
    }
}

/// 範囲の前後の空白を落とし、行頭の引用マーカーを取り込む。
///
/// 空白だけの範囲なら `None`。`str::trim` で境界を出しているので、両端は
/// 必ず UTF-8 の文字境界に落ちる。
fn tighten(source: &str, range: Range<usize>) -> Option<Range<usize>> {
    if range.start >= range.end || range.end > source.len() {
        return None;
    }
    let slice = &source[range.clone()];
    let leading = slice.len() - slice.trim_start().len();
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = range.start + leading;
    Some(with_quote_marker(source, start)..start + trimmed.len())
}

/// 行頭から `start` までが `>` と空白だけなら、行頭まで引き戻す。
///
/// 引用の `> ` を Atom に含めるための処理。`>` が 1 つも無い場合は動かない
/// （入れ子リストのインデントまで飲み込まないため）。`- ` や `1. ` は
/// parser がくれる range に最初から入っているので、ここでは扱わない。
fn with_quote_marker(source: &str, start: usize) -> usize {
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let gap = &source[line_start..start];
    if gap.contains('>') && gap.chars().all(|c| c == '>' || c.is_whitespace()) {
        line_start
    } else {
        start
    }
}

/// 項目の行頭マーカー（`-` / `*` / `+` / `1.` / `3)`）が占めるバイト数。
/// マーカーが見当たらなければ 0。
///
/// レンダラ（`third_party/tui-markdown` の `start_item`）が source から
/// マーカーを読むのと同じ規則。Item の event range はマーカーそのものから
/// 始まるので、先頭だけ見れば決まる。
fn marker_len(source: &str, start: usize) -> usize {
    let rest = &source[start..];
    match rest.chars().next() {
        Some('-' | '*' | '+') => 1,
        Some(c) if c.is_ascii_digit() => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if matches!(rest.as_bytes().get(digits), Some(b'.' | b')')) {
                digits + 1
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// 中身が無くマーカーだけの項目か。入れ子のリストだけを持つ親項目で起こる。
///
/// `-` や `1.` だけの Atom は DIM にしても意味が無いので作らない。
fn is_marker_only(text: &str) -> bool {
    text.trim_end_matches(['.', ')'])
        .chars()
        .all(|c| matches!(c, '-' | '*' | '+') || c.is_ascii_digit())
}

/// 文を終える記号。
fn is_terminator(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '．' | '.' | '!' | '?')
}

/// 終止記号の直後にあれば同じ文に含める、閉じ括弧・閉じ引用符。
fn is_trailing_closer(c: char) -> bool {
    matches!(c, '」' | '』' | '）' | ')' | '"' | '\'')
}

/// 入れ子を数える開き括弧。`"` と `'` は開きか閉じか決まらないので入れない。
fn is_bracket_open(c: char) -> bool {
    matches!(c, '(' | '（' | '「' | '『')
}

/// 入れ子を数える閉じ括弧。
fn is_bracket_close(c: char) -> bool {
    matches!(c, ')' | '）' | '」' | '』')
}

/// 直前に来ても文の終わりとみなさない略語。`.` を落とした形で持つ。
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "etc", "vs", "cf", "al", "approx", "Mr", "Mrs", "Ms", "Dr", "Prof", "St", "Inc",
    "Ltd", "Co", "No", "Fig", "Vol", "Jr", "Sr",
];

/// ブロック（段落・リスト項目・引用）の範囲を文へ割る。切れ目が 1 つも
/// 無ければブロック全体で 1 つ。
///
/// raw な source をそのまま見るので、`**強調**` の `*` も soft break も
/// 行頭のリストマーカーも引用の継続マーカーも、文の range の中に自然に残る。
fn split_sentences(
    source: &str,
    range: Range<usize>,
    guards: &[Range<usize>],
) -> Vec<Range<usize>> {
    let text = &source[range.clone()];
    let base = range.start;
    let chars: Vec<(usize, char)> = text.char_indices().collect();

    let mut sentences = Vec::new();
    let mut segment_start = 0usize;
    let mut depth = 0usize;
    let mut k = 0usize;

    while k < chars.len() {
        let (index, c) = chars[k];
        k += 1;

        // コード片やリンクの中では切らない。括弧の数も数えない。
        if guards.iter().any(|guard| guard.contains(&(base + index))) {
            continue;
        }
        if is_bracket_open(c) {
            depth += 1;
            continue;
        }
        if is_bracket_close(c) {
            depth = depth.saturating_sub(1);
            continue;
        }
        // 括弧・鉤括弧の内側の終止記号では切らない。
        if depth > 0 || !is_terminator(c) {
            continue;
        }

        // 続く終止記号（`？！`）と閉じ括弧・閉じ引用符を同じ文に含める。
        let mut m = k;
        while m < chars.len() && (is_terminator(chars[m].1) || is_trailing_closer(chars[m].1)) {
            m += 1;
        }
        let end = chars.get(m).map_or(text.len(), |(i, _)| *i);

        // 英語の `.` `!` `?` は誤爆しやすいので、追加の条件を通ったときだけ切る。
        if c.is_ascii() && !ascii_ends_a_sentence(text, index, end) {
            continue;
        }

        sentences.push(base + segment_start..base + end);
        segment_start = end;
        k = m;
    }

    if segment_start < text.len() {
        sentences.push(base + segment_start..base + text.len());
    }
    sentences
}

/// `text[terminator]` の ASCII 終止記号が本当に文末か。
///
/// 迷ったら「切らない」側に倒す。切りすぎより切らなさすぎの方が安全で、
/// 切らなければ段落全体が 1 Atom になるだけで位置は壊れない。
fn ascii_ends_a_sentence(text: &str, terminator: usize, end: usize) -> bool {
    let rest = &text[end..];

    // 直後が空白でも行末でもない = 小数・バージョン番号・URL の途中。
    if rest.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return false;
    }

    // 直前の語が略語、または 1 文字（`J. R. R.` のようなイニシャル）。
    let token = text[..terminator]
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("")
        .trim_start_matches(|c: char| !c.is_alphanumeric());
    if ABBREVIATIONS
        .iter()
        .any(|abbrev| abbrev.eq_ignore_ascii_case(token))
    {
        return false;
    }
    let mut token_chars = token.chars();
    if matches!((token_chars.next(), token_chars.next()), (Some(c), None) if c.is_ascii_alphabetic())
    {
        return false;
    }

    // 次の語が小文字始まり = まだ文が続いている（未知の略語の後ろなど）。
    !rest
        .chars()
        .find(|c| !c.is_whitespace())
        .is_some_and(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Atom を `(スライスした文字列, 種別)` の列にする。range とテキストが
    /// 食い違えばここで露見する。
    fn split(source: &str) -> Vec<(&str, AtomKind)> {
        atomize(source)
            .into_iter()
            .map(|atom| (&source[atom.range], atom.kind))
            .collect()
    }

    fn sentences(source: &str) -> Vec<&str> {
        split(source).into_iter().map(|(text, _)| text).collect()
    }

    #[test]
    fn a_japanese_paragraph_splits_at_every_terminator() {
        assert_eq!(
            sentences("一文目です。二文目です！三文目ですか？"),
            ["一文目です。", "二文目です！", "三文目ですか？"]
        );
    }

    #[test]
    fn terminators_and_closers_stay_with_their_sentence() {
        // 連続する終止記号はまとめて 1 文の末尾に付く。
        assert_eq!(
            sentences("本当に？！ と思った。"),
            ["本当に？！", "と思った。"]
        );
        // 閉じ引用符も同じ文に含める。
        assert_eq!(
            sentences(r#"He said "Go." Then he left."#),
            [r#"He said "Go.""#, "Then he left."]
        );
    }

    #[test]
    fn terminators_inside_brackets_do_not_split() {
        assert_eq!(
            sentences("「終わった。」と彼は言った。次の文。"),
            ["「終わった。」と彼は言った。", "次の文。"]
        );
        assert_eq!(
            sentences("（注：これは例です。）以上。"),
            ["（注：これは例です。）以上。"]
        );
    }

    #[test]
    fn english_periods_do_not_fire_on_decimals_urls_or_abbreviations() {
        assert_eq!(
            sentences(
                "See e.g. the docs. Version 0.3.9 is out. Visit https://example.com/a.b now. Mr. Smith agrees."
            ),
            [
                "See e.g. the docs.",
                "Version 0.3.9 is out.",
                "Visit https://example.com/a.b now.",
                "Mr. Smith agrees.",
            ]
        );
        // 未知の略語でも、次の語が小文字始まりなら文末とみなさない。
        assert_eq!(
            sentences("Approx. ten items fit."),
            ["Approx. ten items fit."]
        );
        // イニシャルの 1 文字も同じ。
        assert_eq!(
            sentences("J. R. R. Tolkien wrote it."),
            ["J. R. R. Tolkien wrote it."]
        );
    }

    #[test]
    fn japanese_and_english_mix_in_one_paragraph() {
        assert_eq!(
            sentences("これは Rust 1.85 の話。See the docs. 終わり。"),
            ["これは Rust 1.85 の話。", "See the docs.", "終わり。"]
        );
    }

    #[test]
    fn a_paragraph_without_a_terminator_is_one_atom() {
        assert_eq!(
            split("終止記号の無い一行"),
            [("終止記号の無い一行", AtomKind::Sentence)]
        );
    }

    #[test]
    fn a_soft_break_does_not_split_but_a_blank_line_does() {
        // soft break は文を切らない（終止記号が無いので 1 Atom）。
        assert_eq!(sentences("前半\n後半"), ["前半\n後半"]);
        // 空行は段落の切れ目なので必ず切れる。
        assert_eq!(sentences("前半\n\n後半"), ["前半", "後半"]);
    }

    #[test]
    fn a_sentence_keeps_the_inline_markup_it_crosses() {
        // range は `**` を含む source 全体を指す。Phase 2 の交差ルールは
        // 装飾 range が span を完全に覆えば装飾するので、これで正しく出る。
        assert_eq!(
            split("これは**重要**です。"),
            [("これは**重要**です。", AtomKind::Sentence)]
        );
        // インラインコードの中の `.` では切らない。
        assert_eq!(
            sentences("`a. b` は識別子です。"),
            ["`a. b` は識別子です。"]
        );
        // リンクの中の `.` でも切らない。
        assert_eq!(
            sentences("[e.g. これ](https://example.com/a.b) を見る。"),
            ["[e.g. これ](https://example.com/a.b) を見る。"]
        );
    }

    #[test]
    fn emoji_and_full_width_characters_keep_their_boundaries() {
        let source = "絵文字🎉が入る文です。全角（ＡＢＣ）も大丈夫。";
        assert_eq!(
            sentences(source),
            ["絵文字🎉が入る文です。", "全角（ＡＢＣ）も大丈夫。"]
        );
        for atom in atomize(source) {
            assert!(source.is_char_boundary(atom.range.start));
            assert!(source.is_char_boundary(atom.range.end));
        }
    }

    #[test]
    fn a_heading_is_one_atom_without_its_trailing_newline() {
        assert_eq!(
            split("# タイトル\n\n## 節\n"),
            [
                ("# タイトル", AtomKind::Heading),
                ("## 節", AtomKind::Heading),
            ]
        );
    }

    #[test]
    fn list_items_start_at_their_marker() {
        assert_eq!(
            split("- 一つ目。\n- 二つ目。\n"),
            [
                ("- 一つ目。", AtomKind::ListItem),
                ("- 二つ目。", AtomKind::ListItem),
            ]
        );
        assert_eq!(
            split("1. 一つ目\n2. 二つ目\n"),
            [
                ("1. 一つ目", AtomKind::ListItem),
                ("2. 二つ目", AtomKind::ListItem),
            ]
        );
        // 項目の中も文へ割る。マーカーは最初の文に付く。
        assert_eq!(
            sentences("- 一文目。二文目。\n"),
            ["- 一文目。", "二文目。"]
        );
        // 2 文目以降も種別は ListItem のまま（由来する構文要素は変わらない）。
        assert_eq!(
            split("1. 一文目。二文目。\n"),
            [
                ("1. 一文目。", AtomKind::ListItem),
                ("二文目。", AtomKind::ListItem),
            ]
        );
    }

    #[test]
    fn a_list_item_splits_with_the_same_guards_as_prose() {
        // 小数・略語・URL は項目の中でも誤爆しない。
        assert_eq!(
            sentences("- Version 0.3.9 is out. See e.g. the docs.\n"),
            ["- Version 0.3.9 is out.", "See e.g. the docs."]
        );
        // 鉤括弧の中の `。` では切らない。
        assert_eq!(
            sentences("- 「終わった。」と彼は言った。次の文。\n"),
            ["- 「終わった。」と彼は言った。", "次の文。"]
        );
        // インラインコードの中の `.` でも切らない。
        assert_eq!(
            sentences("- `a. b` は識別子です。以上。\n"),
            ["- `a. b` は識別子です。", "以上。"]
        );
        // 項目にぶら下がったコードフェンスの中では切らない。
        assert_eq!(
            sentences("- 例です。\n\n  ```\n  let x = 1. ;\n  ```\n"),
            ["- 例です。", "```\n  let x = 1. ;\n  ```"]
        );
        // 行をまたぐ項目でも、2 文目の Atom は先頭の空白を含まない。
        assert_eq!(
            sentences("- 一文目。\n  二文目。\n"),
            ["- 一文目。", "二文目。"]
        );
    }

    #[test]
    fn a_quoted_list_item_keeps_its_quote_marker_on_the_first_sentence() {
        assert_eq!(
            split("> - 一文目。二文目。\n"),
            [
                ("> - 一文目。", AtomKind::ListItem),
                ("二文目。", AtomKind::ListItem),
            ]
        );
    }

    #[test]
    fn nested_list_items_are_independent_atoms() {
        // 親の range に子を含めると範囲が重なる。親は入れ子の手前で打ち切り、
        // 子は独立した Atom にする。まとめ直すのは Unit の仕事。
        assert_eq!(
            split("- 親\n  - 子 1\n  - 子 2\n- 別の親\n"),
            [
                ("- 親", AtomKind::ListItem),
                ("- 子 1", AtomKind::ListItem),
                ("- 子 2", AtomKind::ListItem),
                ("- 別の親", AtomKind::ListItem),
            ]
        );
        // 本文の無い親（`-` の直後に入れ子）は空になるので Atom にしない。
        assert_eq!(split("-\n  - 子\n"), [("- 子", AtomKind::ListItem)]);
    }

    #[test]
    fn a_code_fence_is_one_atom_including_its_fences() {
        assert_eq!(
            split("```rust\nlet x = 1;\n```\n"),
            [("```rust\nlet x = 1;\n```", AtomKind::CodeBlock)]
        );
    }

    #[test]
    fn a_table_is_one_atom() {
        assert_eq!(
            split("| a | b |\n| - | - |\n| 1 | 2 |\n"),
            [("| a | b |\n| - | - |\n| 1 | 2 |", AtomKind::Table)]
        );
    }

    #[test]
    fn blockquote_atoms_carry_their_quote_marker() {
        // 中身は通常どおり文へ割り、各 Atom は行頭の `> ` を含む。
        assert_eq!(
            split("> 一文目。\n> 二文目。\n"),
            [
                ("> 一文目。", AtomKind::BlockQuote),
                ("> 二文目。", AtomKind::BlockQuote),
            ]
        );
        // 行の途中で切れた文には引き込むマーカーが無い。
        assert_eq!(
            sentences("> 一文目。二文目。\n"),
            ["> 一文目。", "二文目。"]
        );
        // 引用の中の見出しやコードは自分の種別のまま。
        assert_eq!(
            split("> ## 節\n>\n> 本文。\n"),
            [
                ("> ## 節", AtomKind::Heading),
                ("> 本文。", AtomKind::BlockQuote)
            ]
        );
    }

    #[test]
    fn an_empty_or_blank_document_yields_no_atoms() {
        assert!(atomize("").is_empty());
        assert!(atomize("   \n\n\t\n  ").is_empty());
        assert!(atomize("\n\n---\n\n").is_empty());
    }

    #[test]
    fn front_matter_and_rules_are_not_atoms_but_raw_html_is() {
        assert_eq!(
            split("---\ntitle: x\n---\n\n本文。\n"),
            [("本文。", AtomKind::Sentence)]
        );
        assert_eq!(
            split("<div class=\"x\">\n\n本文。\n"),
            [
                ("<div class=\"x\">", AtomKind::Other),
                ("本文。", AtomKind::Sentence),
            ]
        );
    }

    #[test]
    fn atomize_is_deterministic() {
        let source = include_str!("../tests/fixtures/sample.md");
        assert_eq!(atomize(source), atomize(source));
    }
}
