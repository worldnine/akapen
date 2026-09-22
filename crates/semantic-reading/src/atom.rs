//! Atom — 安全に位置を指定できる機械的単位。
//!
//! Atom は意味単位ではない。Markdown の段落が意味のまとまりと一致しない
//! 以上、意味の境界は [`crate::SemanticUnit`] 側が持つ。Atom が担うのは
//! 「ここからここまで」を source のバイト範囲で一意に言えることだけ。

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// 文書を機械的に分割した最小単位。
///
/// `range` は source のバイト範囲（`start..end`）。文字数でも端末セルでも
/// なくバイトなのは、source の identity をバイトオフセットで統一する
/// akapen 側の Range Attribution 基盤に合わせるため。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Atom {
    /// source のバイト範囲。Markdown のマーカー（`## ` や `- `）を含む、
    /// その Atom が source 上で占める範囲そのもの。
    pub range: Range<usize>,
    /// この Atom がどの構文要素に由来するか。
    pub kind: AtomKind,
}

impl Atom {
    /// 指定した範囲と種別の Atom を作る。
    pub fn new(range: Range<usize>, kind: AtomKind) -> Self {
        Self { range, kind }
    }

    /// Atom が占める source のバイト数。「文書のどれだけが光っているか」を
    /// 数えるときの単位である（`marks-report` の例）。
    /// 範囲が逆転している場合は 0（validate 前の壊れた入力でも panic しない）。
    pub fn len(&self) -> usize {
        self.range.end.saturating_sub(self.range.start)
    }

    /// 長さ 0 の Atom か。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Atom の構文上の種別。
///
/// 設計書が挙げる sentence / list item / heading / code block / table /
/// blockquote を持つ。表だけは設計書より 1 段細かく、**行**まで割る
/// （[`AtomKind::Table`] と [`AtomKind::TableRow`]）。将来 parser 側が
/// 増えても壊れないよう、分類しきれないものは [`AtomKind::Other`] に落とす。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtomKind {
    /// 本文中の一文。
    Sentence,
    /// リストの 1 項目。
    ListItem,
    /// 見出し行。
    Heading,
    /// フェンスまたはインデントされたコードブロック。
    CodeBlock,
    /// 表の**枕** — ヘッダ行と、その下の区切り行（`| - | - |`）。
    ///
    /// 表そのものは [`AtomKind::TableRow`] の行へ割る。ヘッダをデータ行と
    /// 別の種別にしてあるのは、**ヘッダは「1 行だけ読むならどれか」の答えに
    /// ならない**からで、判定器はこの種別を見て核の候補から落とす（列の名前
    /// を挙げても中身を言ったことにならない。見出しと同じ理由である）。
    ///
    /// 区切り行を枕に含めるのは、含めないとそこがどの Atom にも属さず、
    /// 行が沈んだときに `| - | - |` の 1 行だけが NORMAL で光り残るため。
    Table,
    /// 表のデータ行 1 行。
    ///
    /// 表は 1 つの Unit のままで、**核だけが行に下りる**（表全体が 1 塊で
    /// 光ると、marks の売りである一文の精度が出ない）。
    TableRow,
    /// 引用ブロック。
    BlockQuote,
    /// 上記のいずれでもないもの。
    Other,
}

/// [`crate::SemanticDocument::atoms`] への添字。
///
/// Unit は Atom を所有せず添字で参照する。Atom 列が文書の唯一の位置台帳で
/// あり、同じ Atom を複数の Unit が指しうるため。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AtomIndex(pub usize);

impl From<usize> for AtomIndex {
    fn from(index: usize) -> Self {
        Self(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_is_bytes_and_reversed_ranges_are_zero() {
        assert_eq!(Atom::new(10..24, AtomKind::Sentence).len(), 14);
        assert!(!Atom::new(10..24, AtomKind::Sentence).is_empty());
        assert!(Atom::new(24..24, AtomKind::Sentence).is_empty());
        // validate 前の壊れた入力（範囲の逆転）でも panic しない。
        let reversed = std::ops::Range { start: 24, end: 10 };
        assert_eq!(Atom::new(reversed, AtomKind::Sentence).len(), 0);
    }

    #[test]
    fn atom_kind_round_trips_as_snake_case() {
        let kinds = [
            AtomKind::Sentence,
            AtomKind::ListItem,
            AtomKind::Heading,
            AtomKind::CodeBlock,
            AtomKind::Table,
            AtomKind::TableRow,
            AtomKind::BlockQuote,
            AtomKind::Other,
        ];
        let json = serde_json::to_string(&kinds).unwrap();
        assert_eq!(
            json,
            r#"["sentence","list_item","heading","code_block","table","table_row","block_quote","other"]"#
        );
        let back: Vec<AtomKind> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kinds);
    }

    #[test]
    fn atom_index_round_trips_as_a_bare_number() {
        assert_eq!(serde_json::to_string(&AtomIndex(3)).unwrap(), "3");
        let back: AtomIndex = serde_json::from_str("3").unwrap();
        assert_eq!(back, AtomIndex::from(3usize));
    }
}
