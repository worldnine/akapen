//! fixture（`tests/fixtures/sample.md` + `sample.json`）に対する Reading
//! Policy の振る舞い。
//!
//! 期待値は decorate の出力を貼り戻したものではなく、fixture の各 Unit の
//! バイト長から独立に算出した値を書いている（keep 順と累積 % は
//! `tests/fixtures/README.md` に控えてある）。

use std::collections::BTreeMap;
use std::ops::Range;

use semantic_reading::{
    DisplayState, FixtureProvider, Provider, ReadingTier, SemanticDocument, SemanticUnit, UnitId,
    policy,
};

const FIXTURE_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.json");

fn fixture() -> SemanticDocument {
    FixtureProvider::from_path(FIXTURE_JSON)
        .expect("fixture を読めること")
        .analyze("")
        .expect("解析できること")
}

/// Unit ごとの表示状態。Unit 内の Atom がばらけていないことも同時に確かめる
/// （判断単位は Unit なので、同じ Unit の Atom は同じ状態になるはず）。
fn unit_states(doc: &SemanticDocument, budget: u8) -> BTreeMap<String, DisplayState> {
    let decorations = policy::decorate(doc, budget);
    let by_range: BTreeMap<(usize, usize), DisplayState> = decorations
        .iter()
        .map(|(range, state)| ((range.start, range.end), *state))
        .collect();

    doc.units
        .iter()
        .map(|unit| {
            let states: Vec<DisplayState> = unit
                .atoms
                .iter()
                .map(|&index| {
                    let range = &doc.atom(index).expect("添字が有効であること").range;
                    by_range[&(range.start, range.end)]
                })
                .collect();
            assert!(
                states.windows(2).all(|w| w[0] == w[1]),
                "unit `{}` の Atom で状態が割れている: {states:?}",
                unit.id
            );
            (unit.id.to_string(), states[0])
        })
        .collect()
}

fn dimmed(doc: &SemanticDocument, budget: u8) -> Vec<String> {
    unit_states(doc, budget)
        .into_iter()
        .filter(|(_, state)| *state == DisplayState::Dim)
        .map(|(id, _)| id)
        .collect()
}

/// 指定した Unit が DIM でなくなる最小の Budget。
fn kept_from(doc: &SemanticDocument, id: &str) -> u8 {
    (1..=100)
        .find(|&budget| unit_states(doc, budget)[id] != DisplayState::Dim)
        .unwrap_or_else(|| panic!("unit `{id}` は 100 % でも DIM のまま"))
}

#[test]
fn budget_100_dims_nothing_and_marks_only_the_essential_units() {
    let doc = fixture();
    let states = unit_states(&doc, 100);
    let marked: Vec<&str> = states
        .iter()
        .filter(|(_, s)| **s == DisplayState::Marked)
        .map(|(id, _)| id.as_str())
        .collect();
    // 見出し・結論・制約が ESSENTIAL。補足（u7）は ESSENTIAL ではなく
    // SUPPORTING かつ REDUNDANT なので MARKED にならない。
    assert_eq!(marked, ["u1", "u3", "u5"]);
    assert!(dimmed(&doc, 100).is_empty(), "100 % で DIM は出ない");
}

#[test]
fn budget_70_drops_the_long_context_the_redundant_unit_and_the_details() {
    let doc = fixture();
    // keep 順の累積: u1 2.5 / u3 9.3 / u5 18.3 / u2 26.3 / u8 33.3 / u6 51.8
    //              / u4 71.1 -> ここで 70 % を超えるので打ち切り。
    assert_eq!(dimmed(&doc, 70), ["u10", "u4", "u7", "u9"]);
}

#[test]
fn budget_30_keeps_only_the_essentials_and_the_problem_statement() {
    let doc = fixture();
    // u2（26.3 %）まで入り、次の u8（33.3 %）で打ち切り。
    assert_eq!(dimmed(&doc, 30), ["u10", "u4", "u6", "u7", "u8", "u9"]);
}

#[test]
fn budget_10_keeps_every_essential_and_nothing_else() {
    let doc = fixture();
    // 一段目は ESSENTIAL 3 つ（u1 / u3 / u5）で 18.3 %。**Budget 10 % を
    // 超えるが、一段目は Budget を見ない。** 二段目の先頭 u2 は
    // 26.3 % なので入らない。
    let states = unit_states(&doc, 10);
    assert_eq!(states["u1"], DisplayState::Marked);
    assert_eq!(states["u3"], DisplayState::Marked);
    assert_eq!(states["u5"], DisplayState::Marked);
    assert_eq!(
        dimmed(&doc, 10),
        ["u10", "u2", "u4", "u6", "u7", "u8", "u9"]
    );
}

#[test]
fn budget_1_keeps_the_cores_and_100_keeps_them_all() {
    let doc = fixture();
    let at_one = unit_states(&doc, 1);
    let survivors: Vec<&str> = at_one
        .iter()
        .filter(|(_, s)| **s != DisplayState::Dim)
        .map(|(id, _)| id.as_str())
        .collect();
    // **READ 1 % は核だけが出る。** 「これさえ見れば」なら、いちばん
    // 少ない予算で出るのは MARKED の全部である。台帳が一段だった頃は
    // ここが `["u1"]` で、READ を下げると MARKED が 3 つから 1 つへ
    // 減っていた。
    assert_eq!(survivors, ["u1", "u3", "u5"]);
    for id in ["u1", "u3", "u5"] {
        assert_eq!(at_one[id], DisplayState::Marked, "{id}");
    }
    // MARKED は 100 % と同じ集合である（Budget に依存しない）。
    let full = unit_states(&doc, 100);
    let at_full: Vec<&str> = full
        .iter()
        .filter(|(_, s)| **s == DisplayState::Marked)
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(survivors, at_full);

    assert_eq!(unit_states(&doc, 100).len(), doc.units.len());
    assert!(dimmed(&doc, 100).is_empty());
}

#[test]
fn budgets_outside_the_range_clamp_to_the_boundaries() {
    let doc = fixture();
    assert_eq!(policy::decorate(&doc, 0), policy::decorate(&doc, 1));
    assert_eq!(policy::decorate(&doc, 255), policy::decorate(&doc, 100));
}

#[test]
fn a_redundant_unit_dims_before_the_non_redundant_unit_of_the_same_tier() {
    let doc = fixture();
    // u2 と u7 はどちらも SUPPORTING。u7 だけが REDUNDANT_WITH(u3) を持つ。
    let u2 = doc.unit(&UnitId::from("u2")).unwrap();
    let u7 = doc.unit(&UnitId::from("u7")).unwrap();
    assert_eq!(u2.reading_tier, ReadingTier::Supporting);
    assert_eq!(u7.reading_tier, ReadingTier::Supporting);
    assert!(!u2.is_redundant());
    assert_eq!(u7.redundant_with(), Some(&UnitId::from("u3")));

    // 「残るのに必要な Budget」が高い方が先に DIM になる。
    let (from_u2, from_u7) = (kept_from(&doc, "u2"), kept_from(&doc, "u7"));
    assert!(
        from_u7 > from_u2,
        "REDUNDANT な u7 ({from_u7} %) が非 REDUNDANT な u2 ({from_u2} %) より先に DIM にならない"
    );

    // 実効 Tier が同じ（CONTEXT）相手にも先に落ちる。u7 は u8 より短いので、
    // 長さ規則だけなら u7 が先に残るはずのところを redundancy が逆転させる。
    assert!(kept_from(&doc, "u7") > kept_from(&doc, "u8"));
}

#[test]
fn dimming_is_monotonic_across_every_budget_on_the_fixture() {
    assert_monotonic(&fixture());
}

#[test]
fn dimming_is_monotonic_on_a_larger_generated_document() {
    // fixture は 10 Unit と小さいので、打ち切り位置が細かく動く文書でも
    // 単調性が崩れないことを確かめる。
    assert_monotonic(&generated_doc());
}

/// Budget を 100 から 1 へ下げる間、DIM の集合が縮まないこと。
/// 逆向き（上げたとき DIM が増えないこと）も同じ包含関係で示せる。
fn assert_monotonic(doc: &SemanticDocument) {
    let mut previous: Vec<(Range<usize>, DisplayState)> = policy::decorate(doc, 100);
    assert_eq!(previous.len(), doc.atoms.len());

    for budget in (1..=99).rev() {
        let current = policy::decorate(doc, budget);
        assert_eq!(current.len(), previous.len());
        for ((range, now), (prev_range, before)) in current.iter().zip(previous.iter()) {
            assert_eq!(range, prev_range, "budget {budget} で range 列が変わった");
            if *before == DisplayState::Dim {
                assert_eq!(
                    *now,
                    DisplayState::Dim,
                    "budget {} -> {budget} で {range:?} が DIM から戻った",
                    budget + 1
                );
            }
        }
        previous = current;
    }
}

/// Tier・長さ・redundancy を機械的に散らした 30 Unit の文書。
fn generated_doc() -> SemanticDocument {
    use semantic_reading::{Atom, AtomIndex, AtomKind, Relation};

    let tiers = [
        ReadingTier::Essential,
        ReadingTier::Supporting,
        ReadingTier::Context,
        ReadingTier::Detail,
    ];
    let mut atoms = Vec::new();
    let mut units = Vec::new();
    let mut cursor = 0usize;
    for i in 0..30usize {
        // 長さは 7 バイト刻みで巡回させ、同長の同点も混ぜる。
        let len = 5 + (i * 7) % 43;
        let first = AtomIndex(atoms.len());
        atoms.push(Atom::new(cursor..cursor + len, AtomKind::Sentence));
        cursor += len + 1;
        // 3 Unit に 1 つは Atom を 2 つ持たせる。
        let mut indices = vec![first];
        if i % 3 == 0 {
            indices.push(AtomIndex(atoms.len()));
            atoms.push(Atom::new(cursor..cursor + 11, AtomKind::ListItem));
            cursor += 12;
        }
        let mut unit = SemanticUnit::new(format!("g{i}"), indices, tiers[i % tiers.len()]);
        if i % 5 == 0 && i > 0 {
            unit.relations.push(Relation::RedundantWith("g0".into()));
        }
        units.push(unit);
    }
    let doc = SemanticDocument::new(atoms, units);
    doc.validate().expect("生成した文書が整合していること");
    doc
}

#[test]
fn every_atom_appears_exactly_once_in_document_order() {
    let doc = fixture();
    let decorations = policy::decorate(&doc, 55);
    assert_eq!(decorations.len(), doc.atoms.len());
    for (decoration, atom) in decorations.iter().zip(doc.atoms.iter()) {
        assert_eq!(decoration.0, atom.range);
    }
    // fixture の Atom は文書順に並んでおり、重なりもない。
    for pair in decorations.windows(2) {
        assert!(pair[0].0.end <= pair[1].0.start);
    }
}
