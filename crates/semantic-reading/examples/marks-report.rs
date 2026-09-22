//! marks の応答 JSON を当てたときの **光る本数**を、複数のつまみで測る。
//!
//! ```sh
//! cargo run -p semantic-reading --example marks-report -- doc.md answer.json
//! ```
//!
//! akapen を起動せずに
//! [`marks::mark`] そのものを通すので、**測っているものは TUI が描くものと
//! 同じ**（akapen 側は [`DisplayState`] を色へ写すだけ）。
//!
//! 出すのは 3 つ:
//!
//! - つまみごとの **光る Unit の数**と、光る Atom のバイト比
//! - スコアの分布（段 1 の `marks-presets.md` 2 節と同じ帯）
//! - 上位 10 本の Unit（id とスコア）。**本文は出さない** —
//!   `docs/gotchas/public-repo.md`「業務文書の本文はこのリポジトリに書かない」
//!   に当たる出力を既定で作らない

use std::process::ExitCode;

use semantic_reading::{AnalyzeResponse, DisplayState, atomize, marks};

/// 測るつまみ。1 から 100 まで、読み手が触る範囲を粗く覆う。
const SHARES: [u8; 6] = [1, 10, 20, 30, 50, 100];

/// スコアの帯（`marks-presets.md` 2 節と同じ切り方）。
const BANDS: [f32; 5] = [0.9, 0.7, 0.5, 0.3, 0.1];

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(doc), Some(answer)) = (args.next(), args.next()) else {
        eprintln!("usage: marks-report <file.md> <answer.json>");
        return ExitCode::FAILURE;
    };
    let source = match std::fs::read_to_string(&doc) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("marks-report: {}: {e}", doc.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let json = match std::fs::read_to_string(&answer) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("marks-report: {}: {e}", answer.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let atoms = atomize(&source);
    let total: usize = atoms.iter().map(|atom| atom.range.len()).sum();
    let document = match AnalyzeResponse::from_json(&json).and_then(|r| r.into_document(atoms)) {
        Ok(document) => document,
        Err(e) => {
            eprintln!("marks-report: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "question {}  units {}  scores {}",
        document.question.as_deref().unwrap_or("(なし)"),
        document.units.len(),
        if marks::has_scores(&document) { "あり" } else { "**なし**" },
    );

    println!("\nつまみ   光る Unit   光るバイト");
    for share in SHARES {
        let lit = marks::lit(&document, share);
        let bytes: usize = marks::mark(&document, share)
            .into_iter()
            .filter(|(_, state)| *state == DisplayState::Marked)
            .map(|(range, _)| range.len())
            .sum();
        let percent = if total == 0 { 0.0 } else { 100.0 * bytes as f32 / total as f32 };
        println!("MARK {share:3}%  {lit:6} 本   {percent:5.1} %");
    }

    let mut scores: Vec<f32> = document.units.iter().filter_map(|unit| unit.score).collect();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    if !scores.is_empty() {
        print!("\nスコア n={}", scores.len());
        for band in BANDS {
            let count = scores.iter().filter(|&&s| s >= band).count();
            print!("  ≥{band:.1}: {count}");
        }
        let median = scores[scores.len() / 2];
        println!("  中央 {median:.2}  足切り超え {}",
            scores.iter().filter(|&&s| s >= marks::SCORE_FLOOR).count());
    }

    println!("\n上位 10 Unit（id とスコア。本文は出さない）");
    let mut ranked: Vec<_> = document
        .units
        .iter()
        .filter_map(|unit| unit.score.map(|score| (unit.id.clone(), score, unit.atoms.len())))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (id, score, atoms) in ranked.into_iter().take(10) {
        println!("  {id:>6}  {score:.2}  ({atoms} atoms)");
    }
    ExitCode::SUCCESS
}
