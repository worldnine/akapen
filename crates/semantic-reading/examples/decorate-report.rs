//! 応答 JSON を当てたときの **表示状態の比率**を、複数の Budget で測る。
//!
//! ```sh
//! cargo run -p semantic-reading --example decorate-report -- doc.md response.json
//! ```
//!
//! 隣の `dump-request.rs` が「アダプタへ何を渡すか」を出す道具なら、こちらは
//! 「返ってきた答えを当てると画面がどうなるか」を出す道具である。akapen を
//! 起動せずに [`policy::decorate`] そのものを通すので、測っているものは
//! TUI が描くものと同じ（akapen 側は [`crate::DisplayState`] を色へ写すだけ）。
//!
//! 比率の**分母は Atom のバイト長の合計**である（source 全体ではない —
//! Markdown の記号や空行は Atom に入らない）。Atom 個数の内訳も併記する。

use std::process::ExitCode;

use semantic_reading::{AnalyzeResponse, DisplayState, atomize, policy};

/// 測る Budget。100 から 1 まで、設計書のデモが触る範囲を粗く覆う。
const BUDGETS: [u8; 6] = [100, 80, 60, 40, 20, 1];

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(doc), Some(answer)) = (args.next(), args.next()) else {
        eprintln!("usage: decorate-report <file.md> <response.json>");
        return ExitCode::FAILURE;
    };
    let source = match std::fs::read_to_string(&doc) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("decorate-report: {}: {e}", doc.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let json = match std::fs::read_to_string(&answer) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("decorate-report: {}: {e}", answer.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let atoms = atomize(&source);
    let document = match AnalyzeResponse::from_json(&json).and_then(|r| r.into_document(atoms)) {
        Ok(document) => document,
        Err(e) => {
            eprintln!("decorate-report: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "atoms {}  units {}  source {} bytes",
        document.atoms.len(),
        document.units.len(),
        source.len()
    );
    println!("budget | MARKED  NORMAL  DIM    | marked/normal/dim atoms");
    for budget in BUDGETS {
        let states = policy::decorate(&document, budget);
        let total: usize = states.iter().map(|(r, _)| r.len()).sum();
        let share = |want: DisplayState| -> f64 {
            let bytes: usize = states
                .iter()
                .filter(|(_, s)| *s == want)
                .map(|(r, _)| r.len())
                .sum();
            if total == 0 { 0.0 } else { bytes as f64 * 100.0 / total as f64 }
        };
        let count = |want: DisplayState| states.iter().filter(|(_, s)| *s == want).count();
        println!(
            "{budget:>5}% | {:>5.1}%  {:>5.1}%  {:>5.1}% | {} / {} / {}",
            share(DisplayState::Marked),
            share(DisplayState::Normal),
            share(DisplayState::Dim),
            count(DisplayState::Marked),
            count(DisplayState::Normal),
            count(DisplayState::Dim),
        );
    }
    ExitCode::SUCCESS
}
