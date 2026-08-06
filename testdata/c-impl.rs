// 実装メモ (ファイル C)
// そういうことなのか
// このファイルは .rs 拡張子なので、デフォルトで source モードになります。
// .md 以外のファイルは view モードが使えず、Tab を押しても source のままです。

use std::path::PathBuf;

/// セッション内のコメントを表現する構造体。
/// file_path フィールドで、どのファイルへのコメントか識別します。
struct Comment {
    file_path: PathBuf,
    start_line: usize,  // 1-based
    end_line: usize,    // 1-based
    body: String,
}

impl Comment {
    /// `path:start-end` 形式のロケーション文字列。
    pub fn location(&self) -> String {
        let path = self.file_path.display();
        if self.start_line == self.end_line {
            format!("{path}:{}", self.start_line)
        } else {
            format!("{path}:{}-{}", self.start_line, self.end_line)
        }
    }
}

/// セッション全体の状態。
struct Session {
    files: Vec<PathBuf>,
    comments: Vec<Comment>,
    current_file_index: usize,
}

fn main() {
    println!("このファイルは .rs なので source モード固定です。");
    println!("Tab で view に切り替わりません。");

    // ↓ ここにコメントをつけてみてください
    let session = Session {
        files: vec![],
        comments: vec![],
        current_file_index: 0,
    };
}
