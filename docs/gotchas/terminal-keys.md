# 端末とキー入力

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。

---

### 入力は termtheme の読み手で読む — crossterm の `event::poll` / `event::read` / `cursor::position()` を足すと、配色の知らせで固まる

akapen は `--light` / `--dark` が無ければ、端末に配色の知らせ（モード 2031、
`CSI ? 2031 h`）を頼んでいます。知らせ（`CSI ? 997 ; 1 n` / `; 2 n`）を読むのは
**termtheme の読み手（`termtheme::input::poll` / `read`）だけ**です。

crossterm 0.29 の読み手は `CSI ?` で始まる列を `u` か `c` でしか閉じないので、
知らせを受けると**後ろの入力を全部飲み込み、`event::poll(timeout)` も戻らず**、
イベントループごと止まります。herdr は `CSI ? 996 n`（今の配色の問い合わせ）に
即答するので、crossterm で読むものを 1 か所でも足すと、エディタから戻った
直後（問い合わせを送る）に固まります。読み手が 2 つになると端末を取り合う
ので、キーが消える形でも出ます。

- crossterm の `event::poll` / `event::read` を使わない
- `crossterm::cursor::position()` も crossterm の読み手を動かす。ratatui の
  `Terminal::clear()` がこれを呼ぶ（全画面で消し直したいなら
  `terminal.resize(area)`）。`NoBlinkBackend::get_cursor_position` は置いた
  位置を返し、端末に聞かない
- **端末を手放す前に購読を外す**。張り外しは termtheme の
  `scheme::Subscription`（`App::scheme`）が受け持つ: 終わるときは `run()` の
  `stop`（早い戻りと panic の unwind では `App` の Drop — `App` は
  `TerminalGuard` より後に作るので先に落ちる）、エディタに渡すときは
  `suspend`、戻ったら `resume`（張り直し、今の配色も聞く）。**`--callback`
  を起こす前に `stop` する** — `app` はまだ生きているので、Drop を待つと
  callback の時点で購読が残る。SIGTERM・SIGINT で殺されたときは
  `unsubscribe_and_die` が `unsubscribe_in_signal_handler` で外す（エディタの
  あいだは何もしない）。外し忘れると、次に端末を使うもの（シェル、
  `--callback` で開く akapen）に知らせが届く

**確認したこと**: `git grep -n 'event::poll\|event::read\|cursor::position\|terminal.clear'`
が `src/` でコメントにしか当たらないこと。2026-09-28、herdr 0.9.1 のペインで
`script(1)` の下に akapen を開き、エディタ（`e`）を往復して `q` で終えた記録に、
起動の `2031 h`（`996 n` は無い）→ エディタの前の `2031 l` → 戻った後の
`2031 h` と `996 n` → 終わるときの `2031 l` がこの順で出ていること。`--dark`
では `2031 h` が 1 度も出ないこと。2026-09-30、termtheme 0.2.0 の
`Subscription` に置き換えた版で同じ記録を取り、この並びが置き換える前の版と
同じであること（終わるときの `2031 l` は代替画面を出る `1049 l` より前）。
`--dark` では `2031` が `h` も `l` も 1 度も出ないこと（置き換える前は、張って
いない購読を外す `2031 l` をエディタの前と終わるときに書いていた）。
`kill -TERM` / `kill -INT` で最後に `2031 l` が出て、シグナルの番号で死ぬこと
（置き換える前は `2031 l` が出ずに購読が残った）。エディタのあいだの
`kill -TERM` では何も足さないこと。知らせの作り直しと張り外しの列のテストは
`src/color_scheme_tests.rs`。

### `f` はトグル。hold は 2026-09-22 に試して捨てた — kitty protocol を有効にすると auto-repeat が `Repeat` kind になり、既存のキー経路が崩れた

**akapen は kitty keyboard protocol を有効にしていません**（`f` はトグルで、
離したことを見ていないので押す理由がありません）。押す日が来たら、
下の 3 つが対で要ります: **`Repeat` を `Press` と同じに捌くこと**（いまの
イベントループはそうなっています）、**`Release` の腕を足すこと**、
**`PopKeyboardEnhancementFlags` で戻すこと**。

旗を押すと端末の auto-repeat が押下の連打ではなく
`KeyEventKind::Repeat` として届きます。`src/main.rs` のイベントループは
`Press` だけを `on_key` へ渡していたので、**矢印を押しっぱなしにしても
1 回しか動かなくなりました**（選択肢の移動も本文のスクロールも）。

同じ端末で `j` / `k` は続いたので「矢印だけが壊れた」ように見えます。
kitty protocol は `REPORT_EVENT_TYPES` だけのとき、**文字を生むキーの
押下と repeat を素のバイトのまま送り**、矢印のような機能キーだけを CSI の
符号（`CSI 1;1:2 B` のように押下 / repeat / 離すの欄を持つ形）で送るため
です。素のバイトには欄が無いので `j` は今までどおり `Press` として届きます
（離したときにその 2 つがどう届くかは確かめていません。旗を外したので
確かめる手段もありません）。
**壊れ方がキーによって違う**ので、旗を押したことが原因だと気づくのに
時間がかかります。

2026-09-22 に `f`（フォーカス）の「押している間だけ沈む」を作るために
押して、同じ日に**旗ごと外しました**。沈める / 戻すの 1 往復のために
全部のキーの押しっぱなしを賭けるのは釣り合いません。`f` はトグルです
（`src/focus.rs`。押しっぱなしの点滅は 120 ms の門で塞いでいます —— **門は
「見た押下」から測ります**。通した押下だけを憶えると、門の幅を跨いだ
ところで連打が 1 発通り、遅い点滅になります）。

pop を対にする理由: `ratatui::restore()` はこの旗を知らないので、戻さないと
**終了後のシェルに kitty 符号のキーが流れ続けます**（離したことまで送って
くる端末に、それを読めないプログラムが座る形）。

**確認したこと**: `src/main.rs` に `KeyboardEnhancement` を含む識別子が
1 つも無いこと（`grep`）。生 pty で起動して、問い合わせ（`CSI ? u`）も
push（`CSI > … u`）も 1 バイトも出ていないこと。イベントループの
`Event::Key` の腕が
`KeyEventKind::Press | KeyEventKind::Repeat` を**同じに**扱っていること
（旗を押す端末や、将来また押したときの保険）。`Release` の腕が無いこと。
`src/focus.rs` の `Focus` が `on` と `last_press` の 2 つしか持たないこと。
