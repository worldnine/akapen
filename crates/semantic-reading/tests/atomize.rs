//! `atomize` が実文書に対して守る不変条件と、手書き fixture との一致。
//!
//! 単体の分割規則は `src/atomize.rs` の単体テストが見る。ここが見るのは
//! 「実文書を食わせても位置が壊れない」という一点。

use semantic_reading::{Atom, AtomKind, SemanticDocument, atomize};

const SAMPLE_MD: &str = include_str!("fixtures/sample.md");
const SAMPLE_JSON: &str = include_str!("fixtures/sample.json");

/// 設計書の**凍結コピー**。`sample.md` より雑で長い実文書として食わせる。
///
/// 正典（`docs/design/semantic-reading-layer.md`）を直接読んでいた頃は、
/// **このテストと Python 側の「1 リクエストに収まる」が設計書の大きさに
/// 上限を掛けていた**（余地は実測 30〜45 バイト）。正典を自由に伸ばせる
/// ように、`8994544` の内容を切り出したコピーへ読む先を移した。
/// **内容の更新は不要**で、要るのは「雑で長い実文書」という性質だけである。
/// 逐語ではない（冒頭に注記があり、2 節を落としてある）ので、正典の
/// 当時の姿が要るなら git を見ること —— 詳しくは `fixtures/README.md`。
const DESIGN_DOC: &str = include_str!("fixtures/design-doc-frozen-2026-09-22.md");

/// 設計書が Atom に求める性質を全部検査する。
fn assert_invariants(source: &str, atoms: &[Atom]) {
    let mut previous_end = 0usize;

    for (i, atom) in atoms.iter().enumerate() {
        // 1. 昇順で、範囲が重ならない。
        assert!(
            atom.range.start >= previous_end,
            "atom {i} ({:?}) が直前の Atom（{previous_end} まで）と重なっています",
            atom.range
        );
        assert!(
            atom.range.start < atom.range.end,
            "atom {i} の範囲が空か逆転しています: {:?}",
            atom.range
        );
        assert!(
            atom.range.end <= source.len(),
            "atom {i} が source の外へ出ています: {:?}",
            atom.range
        );

        // 2. UTF-8 境界を割らない。割っていればこの下のスライスが panic する。
        assert!(
            source.is_char_boundary(atom.range.start),
            "atom {i} の始端が文字の途中です: {}",
            atom.range.start
        );
        assert!(
            source.is_char_boundary(atom.range.end),
            "atom {i} の終端が文字の途中です: {}",
            atom.range.end
        );

        // 4. 空の Atom も空白だけの Atom も作らない。
        let text = &source[atom.range.clone()];
        assert!(
            !text.trim().is_empty(),
            "atom {i} が空白だけです: {:?}",
            atom.range
        );
        // 5. 範囲の両端に空白が残っていない（スライスがそのまま期待の文字列）。
        assert_eq!(text, text.trim(), "atom {i} の両端に空白が残っています");

        previous_end = atom.range.end;
    }
}

/// 3. 決定論的。同じ入力に同じ出力。
fn assert_deterministic(source: &str) {
    let first = atomize(source);
    for _ in 0..3 {
        assert_eq!(atomize(source), first, "同じ入力から違う Atom 列が出ました");
    }
}

#[test]
fn the_handwritten_fixture_is_reproduced_exactly() {
    // 手書きの `sample.json` は「atomize がこうあってほしい」の基準。
    // 範囲も種別も 1 つ残らず一致すること。
    let expected: SemanticDocument = serde_json::from_str(SAMPLE_JSON).unwrap();
    assert_eq!(atomize(SAMPLE_MD), expected.atoms);
}

#[test]
fn the_fixture_holds_every_invariant() {
    let atoms = atomize(SAMPLE_MD);
    assert_invariants(SAMPLE_MD, &atoms);
    assert_deterministic(SAMPLE_MD);
}

#[test]
fn a_real_design_document_holds_every_invariant() {
    let atoms = atomize(DESIGN_DOC);
    assert_invariants(DESIGN_DOC, &atoms);
    assert_deterministic(DESIGN_DOC);

    // 実文書なので個数は固定しない（見たいのは大きさと雑さで、Atom の数
    // そのものではない）。見出し・本文・リスト・コード・引用が一通り出て
    // いることだけ確かめる。
    let seen: Vec<AtomKind> = atoms.iter().map(|atom| atom.kind).collect();
    for kind in [
        AtomKind::Heading,
        AtomKind::Sentence,
        AtomKind::ListItem,
        AtomKind::CodeBlock,
        AtomKind::BlockQuote,
    ] {
        assert!(seen.contains(&kind), "{kind:?} が 1 つも出ていません");
    }
}

#[test]
fn atoms_need_not_tile_the_document() {
    // 敷き詰めないのは意図的。空行や `---` はどの Atom にも属さず、
    // `policy::decorate` の扱いでそのまま NORMAL の地の文として残る。
    let source = "# 見出し\n\n---\n\n本文。\n";
    let atoms = atomize(source);
    let covered: usize = atoms.iter().map(|atom| atom.len()).sum();
    assert!(covered < source.len());
    assert_invariants(source, &atoms);
}
