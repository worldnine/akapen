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


### `--semantic-cmd` の子は壁時計で殺さない — 進捗行が止まると猶予も止まる

`run_child` の見切り方は**呼び出し側が選びます**（`Deadline`）。

| 経路 | 政策 | 上限 |
| --- | --- | --- |
| クリップボード / `--send-cmd` | `Deadline::Absolute` | `CHILD_TIMEOUT` = 10 秒 |
| `--semantic-cmd` | `Deadline::WhileProgressing` | 無音 `COMMAND_IDLE_TIMEOUT` = 30 秒 ／ 天井 `COMMAND_BACKSTOP` = 600 秒 |

**ここが刺すのはアダプタを書き替えるときです。** 進捗の判定材料は
「stdout か stderr にバイトが来たか」だけなので、**黙って働くアダプタは
固まったアダプタと区別がつきません。** 進捗行を出さなくすると、無音の
上限 30 秒がそのままプロセス全体の予算になり、実測 47 秒の文書が落ちます
（`examples/semantic/measurements/speed-and-limits.md`「第 2 版」）。

`jev-annotate.py` は `progress()` で 1 リクエストごとに stderr へ 1 行
出します。置き場所は `ask_jev` で、`send_in_chunks` ではありません
（probe は分割を通らないので、分割側に置くと 1 本漏れる）。

**もう 1 つの順序の約束**: アダプタ自身の 1 リクエストのタイムアウト
（`DEFAULT_TIMEOUT` = 20 秒）は、akapen の無音の上限（30 秒）より
**短くなければなりません**。固まったリクエストを子が先に諦めれば理由が
stderr に出てステータス行に載りますが、逆だと akapen の kill が先に来て
**理由が消えます**。どちらかを動かすなら両方を見ること —— **この約束は
両側のテストで留めてあります**（Rust 側
`the_default_deadline_watches_silence_not_the_wall_clock` が
`COMMAND_IDLE_TIMEOUT > 20 秒`、Python 側
`test_the_request_timeout_stays_under_akapens_idle_limit` が
`DEFAULT_TIMEOUT < 30 秒`。片側だけだと、もう片方を動かしたときに
両方緑のまま約束が壊れます）。

**確認したこと**: `src/export.rs` の `Deadline` の 2 つの腕と、
`run_child` のループが `out.drain() + err.drain() > 0` で `last_output` を
進めること（`Capture::drain` は読んだバイト数を返す）。テストは同ファイルの
`run_capturing_does_not_kill_a_child_that_keeps_talking` /
`run_capturing_distinguishes_a_silent_child_from_a_chatty_one` /
`pipe_and_wait_still_uses_the_wall_clock_even_for_a_chatty_child`、
`src/semantic.rs` の
`a_command_that_keeps_reporting_progress_outlives_the_idle_limit` /
`a_chatty_command_is_still_stopped_by_the_backstop` /
`a_silent_command_that_never_answers_is_killed_and_reported` /
`the_default_deadline_watches_silence_not_the_wall_clock`。
実機では stub の Jev（10 秒/リクエスト）に本物のアダプタを繋いで
**壁時計 70.2 秒**のランが殺されないことを確かめました（旧 60 秒なら死ぬ）。
