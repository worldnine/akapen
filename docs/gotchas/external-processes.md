# 外部プロセス

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。

---


### TUI が生きているあいだの子プロセスは `export::run_child` を通す

`Command::spawn` + `wait` を素朴に書くと、子プロセスを噛ませるスクリプトなら
何であれ踏む 4 つの事故が返ってきます:

- stdin を読まない子（パイプバッファが埋まって書き込みが止まる）
- 子の exit 後もパイプを掴んでいる孫プロセス
- パイプバッファを超える出力の洪水（相互にブロックする）
- 返ってこない子（デッドラインで kill する必要がある）

4 つとも `src/export.rs` の `run_child` で解決済みです。イベントループは
単一スレッドなので、ここで詰まると**キーが全部死にます。**
出力を**解析する**呼び出し側は `run_capturing` を使い、`truncated` を
エラーにしてください（切れた JSON は「壊れた応答」に見えて原因を隠します）。

例外は TUI を畳んだ後の起動だけです（`src/main.rs` の `--callback` は
`TerminalGuard` を drop した後に fire-and-forget で spawn しています）。

**確認したこと**: `src/export.rs` の `run_child`（非ブロッキング stdin、
stdout/stderr の逐次 drain、`try_wait` + デッドラインでの `child.kill()`）と
`run_capturing`（`out.truncated` で `bail!`）。テストは同ファイルの
`pipe_and_wait_succeeds_when_the_child_reads_stdin` /
`pipe_and_wait_succeeds_when_a_grandchild_keeps_stdin_open` /
`pipe_and_wait_does_not_deadlock_on_a_flood_of_output` /
`pipe_and_wait_times_out_and_kills_the_child`。
