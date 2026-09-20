//! `--semantic-cmd` が外部コマンドの stdin へ渡す要求 JSON を、その場で作る。
//!
//! ```sh
//! cargo run -p semantic-reading --example dump-request -- doc.md > request.json
//! ```
//!
//! アダプタ（`examples/semantic/jev-annotate.py`）を akapen を起動せずに
//! 単体で走らせて**測る**ための道具である。akapen 本体の
//! [`CommandProvider::analyze`] と同じ `atomize` → [`AnalyzeRequest`] を
//! 通るので、要求の中身は本番経路と同一になる。Atom の割り方を別実装で
//! 真似すると、測っているものが本番とずれる。

use std::io::Write;
use std::process::ExitCode;

use semantic_reading::{AnalyzeRequest, atomize};

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: dump-request <file.md>");
        return ExitCode::FAILURE;
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("dump-request: {}: {e}", path.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let atoms = atomize(&source);
    let json = match AnalyzeRequest::new(&source, &atoms).to_json() {
        Ok(json) => json,
        Err(e) => {
            eprintln!("dump-request: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = writeln!(stdout, "{json}") {
        eprintln!("dump-request: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
