//! fixture 自体の健全性 — serde のラウンドトリップと、Atom 範囲が
//! `sample.md` の実バイト位置と一致していること。

use semantic_reading::{AtomKind, FixtureProvider, Provider, SemanticDocument};

const FIXTURE_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.json");
const FIXTURE_MD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.md");

fn fixture() -> SemanticDocument {
    FixtureProvider::from_path(FIXTURE_JSON)
        .expect("fixture を読めること")
        .analyze("")
        .expect("解析できること")
}

#[test]
fn the_fixture_round_trips_through_json() {
    let source = std::fs::read_to_string(FIXTURE_JSON).unwrap();
    let doc: SemanticDocument = serde_json::from_str(&source).unwrap();

    let reserialized = serde_json::to_string(&doc).unwrap();
    let back: SemanticDocument = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(doc, back);

    // pretty 形式でも同じ（fixture はインデント付きで保存してある）。
    let pretty = serde_json::to_string_pretty(&doc).unwrap();
    assert_eq!(
        serde_json::from_str::<SemanticDocument>(&pretty).unwrap(),
        doc
    );
}

#[test]
fn the_fixture_shape_matches_the_design_vocabulary() {
    let doc = fixture();
    assert_eq!(doc.atoms.len(), 21);
    assert_eq!(doc.units.len(), 10);

    // 設計書が挙げる Atom 種別のうち、fixture は 5 種類を使う。
    let kinds: Vec<AtomKind> = doc.atoms.iter().map(|atom| atom.kind).collect();
    for kind in [
        AtomKind::Heading,
        AtomKind::Sentence,
        AtomKind::ListItem,
        AtomKind::BlockQuote,
        AtomKind::CodeBlock,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} が fixture に無い");
    }

    // スコアと核が付いていること — marks の投影が効く形かどうか。
    assert!(doc.units.iter().all(|unit| unit.score.is_some()));
    assert!(doc.units.iter().all(|unit| unit.core_atoms.is_some()));
}

#[test]
fn every_atom_range_is_a_valid_slice_of_the_sample_document() {
    let source = std::fs::read_to_string(FIXTURE_MD).unwrap();
    let doc = fixture();

    let mut previous_end = 0;
    for (i, atom) in doc.atoms.iter().enumerate() {
        assert!(
            atom.range.end <= source.len(),
            "atom {i} が sample.md の末尾を越えている"
        );
        // 日本語を含むので、UTF-8 境界を割っていたらここで None になる。
        let text = source
            .get(atom.range.clone())
            .unwrap_or_else(|| panic!("atom {i} の範囲が UTF-8 境界を割っている"));
        assert!(!text.trim().is_empty(), "atom {i} が空白だけ");
        assert!(!text.contains("\n\n"), "atom {i} が空行をまたいでいる");
        assert!(
            atom.range.start >= previous_end,
            "atom {i} が前の Atom と重なる"
        );
        previous_end = atom.range.end;
    }

    // 位置がずれていないことを、実際の本文で 2 箇所だけ固定しておく。
    assert_eq!(
        &source[doc.atoms[0].range.clone()],
        "# キャッシュ層の刷新メモ"
    );
    assert_eq!(
        &source[doc.atoms[2].range.clone()],
        "結論として、キーごとの差分更新へ移行する。"
    );
}
