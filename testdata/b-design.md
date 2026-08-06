# 設計ノート (ファイル B)

このファイルはセッションモードの **2番目** のファイルです。

## 背景

akapen v0.3 では、常にセッションモードで起動します。
1ファイルでも複数ファイルでも、同じキーバインドで操作できます。

## アーキテクチャ

```
App
├── files: Vec<PathBuf>      // ファイルリスト（引数順）
├── current_file_index: usize // 現在のファイルインデックス
├── file_states: Vec<FileState> // ファイルごとの状態
└── comments: Vec<Comment>    // 全ファイルの全コメント
```

### FileState の内容

- `source` — 読み込んだファイル
- `spans` — シンタックスハイライト済みスパン
- `view` — レンダリング済みビュー
- `mode` — view/source モード（ファイルごとに独立）
- `cursor`, `offset`, `selection` — カーソル位置・選択状態

## 切り替えの流れ

1. `]` 押下 → 現在の状態を `FileState` に保存
2. 次のファイルの `FileState` から状態を復元
3. タイトルバーが新しいファイル名に更新
4. フッタに "switched to b-design.md" とトースト

---

ファイルBでした。
