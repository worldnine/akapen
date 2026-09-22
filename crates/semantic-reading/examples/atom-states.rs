//! 応答 JSON を当てたときの、Atom 1 つ 1 つの表示状態を並べる。
//!
//! ```sh
//! cargo run -p semantic-reading --example atom-states -- doc.md response.json 20
//! ```
//!
//! 隣の `marks-report.rs` は集計を出すので、「**この行のここが** MARKED
//! から NORMAL に変わったか」を確かめられない。判定を変えたときに行の
//! 途中の切り替わりが残るかどうかを見るために、[`marks::mark`] の結果を
//! そのまま 1 行ずつ出す。

use std::process::ExitCode;

use semantic_reading::{AnalyzeResponse, atomize, marks};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(doc), Some(answer)) = (args.next(), args.next()) else {
        eprintln!("usage: atom-states <file.md> <response.json> [share]");
        return ExitCode::FAILURE;
    };
    let share: u8 = args
        .next()
        .and_then(|a| a.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(marks::DEFAULT_SHARE);
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
    for (range, state) in marks::mark(&document, share) {
        // 本文は 1 行に潰す（表示状態の並びが読めればよい）。
        let text: String = source[range.clone()].split_whitespace().collect::<Vec<_>>().join(" ");
        println!("{:<6} {:>6}..{:<6} {}", format!("{state:?}"), range.start, range.end, text);
    }
    ExitCode::SUCCESS
}
