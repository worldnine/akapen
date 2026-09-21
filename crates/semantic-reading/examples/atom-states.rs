//! **計測用**。応答 JSON を当てたときの、Atom 1 つ 1 つの表示状態を並べる。
//!
//! ```sh
//! cargo run -p semantic-reading --example atom-states -- doc.md response.json 60
//! ```
//!
//! 隣の `decorate-report.rs` は比率だけを出すので、「**この行のここが**
//! MARKED から NORMAL に変わったか」を確かめられない。境界の決め方を変えた
//! ときに行の途中の切り替わりが残るかどうかを測るために、[`policy::decorate`]
//! の結果をそのまま 1 行ずつ出す。
//!
//! 最終的な方式を決める道具ではなく、方式を**比べる**ための道具である。

use std::process::ExitCode;

use semantic_reading::{AnalyzeResponse, atomize, policy};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(doc), Some(answer)) = (args.next(), args.next()) else {
        eprintln!("usage: atom-states <file.md> <response.json> [budget]");
        return ExitCode::FAILURE;
    };
    let budget: u8 = args
        .next()
        .and_then(|a| a.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(100);
    let source = match std::fs::read_to_string(&doc) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("atom-states: {}: {e}", doc.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let json = match std::fs::read_to_string(&answer) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("atom-states: {}: {e}", answer.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let atoms = atomize(&source);
    let document = match AnalyzeResponse::from_json(&json).and_then(|r| r.into_document(atoms)) {
        Ok(document) => document,
        Err(e) => {
            eprintln!("atom-states: {e}");
            return ExitCode::FAILURE;
        }
    };
    for (range, state) in policy::decorate(&document, budget) {
        // 本文は 1 行に潰す（表示状態の並びが読めればよい）。
        let text: String = source[range.clone()].split_whitespace().collect::<Vec<_>>().join(" ");
        println!("{:<6} {:>6}..{:<6} {}", format!("{state:?}"), range.start, range.end, text);
    }
    ExitCode::SUCCESS
}
