//! akapen — a markdown line-comment TUI.
//!
//! One pane, two modes: `view` (natively rendered markdown) and `source`
//! (raw source with line numbers and syntect highlighting, where comments
//! are anchored). This module is the entry point, the mode state machine,
//! and the event loop.
//!
//! Supports multiple files: `]`/`[` switch between files; per-file cursor,
//! selection, and mode are preserved. `Ctrl+p` shows the file picker and
//! `l` the all-comments list. Non-Markdown files are source-only.

mod app;
mod chrome;
mod comment;
mod config;
mod config_file;
mod decoration;
mod draw;
mod edit_map;
mod effects;
mod esc;
mod export;
mod focus;
mod highlight;
mod history;
mod ime;
mod keys;
mod marks_questions;
mod overlay;
mod reload;
mod render;
mod lint;
mod review;
mod review_contract;
mod review_dock;
mod review_rules;
mod semantic;
mod semantic_cache;
mod snapshot;
mod source;
mod timeline;
mod undercurl;
mod view;
mod yank;



use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ratatui::Frame;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use tachyonfx::EffectRenderer;
use termtheme::input::{self, Input};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::*;
use crate::chrome::*;
use crate::comment::{Comment, Selection};
use crate::config::{Action, Config};
use crate::draw::*;
use crate::highlight::{Highlighter, Span as HiSpan, syntax_for, wrap_spans, wrap_spans_tagged};
use crate::history::DocumentHistory;
use crate::overlay::*;
use crate::reload::*;
use crate::snapshot::SnapshotCache;
use crate::source::Source;
use similar::{DiffTag, TextDiffConfig};
use crate::view::{
    is_table_delimiter_line, lerp_color, scroll_offset_at, scroll_offset_drag, scroll_thumb,
    GutterCell, ViewState, CURSOR_GLYPH,
};







fn main() -> Result<()> {
    match Config::from_env()? {
        Action::Help => {
            println!(
                "akapen — read markdown rendered, comment on source lines\n\
                 \n\
                 usage: akapen <file...> [--send-cmd <cmd> | --send-agent] [--reply]\n\
                 \x20                         [--theme <name>] [--theme-dark <name>] [--theme-light <name>]\n\
                 \x20                         [--ime <off|ascii|jp>] [--light|--dark]\n\
                 \x20                         [--callback <cmd>]\n\
                 \n\
                 \x20 --send-cmd <cmd>  pipe `s` export to a shell command via stdin\n\
                 \x20 --send-agent      send `s` export to the sole herdr agent in this tab\n\
                 \x20                   (else the sole workspace agent; needs herdr on PATH)\n\
                 \x20 --reply           quote the snippet without file/line\n\
                 \x20                   references; external changes auto-reload\n\
                 \x20                   (instant reply to an agent message; see scripts/akp)\n\
                 \x20 --theme <name>    syntect theme name or path to a .tmTheme file,\n\
                 \x20                   used on both light and dark backgrounds\n\
                 \x20                   (beats --theme-dark/--theme-light)\n\
                 \x20 --theme-dark <name>  the theme on a dark background\n\
                 \x20                   (default Catppuccin Mocha)\n\
                 \x20 --theme-light <name> the theme on a light background\n\
                 \x20                   (default Solarized (light)). Which side\n\
                 \x20                   applies follows --light/--dark or OSC 11,\n\
                 \x20                   and the terminal's light/dark switches\n\
                 \x20 --ime <off|ascii|jp> input-source control around the composer\n\
                 \x20                   (default ascii; needs swiftc on macOS)\n\
                 \x20 --light           force light mode (default: auto-detect\n\
                 \x20                   the terminal background via OSC 11, and\n\
                 \x20                   follow its switches while open — mode 2031)\n\
                 \x20 --dark            force dark mode\n\
                 \x20 --no-fx           disable the animated time-machine frame\n\
                 \x20                   (the rotating gradient border while browsing the past)\n\
                 \x20 --no-cursor-anchor stop publishing the hidden cursor position at\n\
                 \x20                   the composer's caret (calms cursor-following\n\
                 \x20                   terminal shaders; the IME composition window\n\
                 \x20                   then loses its anchor)\n\
                 \x20 --semantic <file> paint the Semantic Reading Layer from a\n\
                 \x20                   semantic-reading annotation (JSON); -/+ by 1\n\
                 \x20                   and </> by 10 move how many passages light\n\
                 \x20                   up, in both views\n\
                 \x20 --semantic-cmd <cmd> get that annotation from a command instead:\n\
                 \x20                   akapen writes version/source/atoms JSON to\n\
                 \x20                   its stdin and reads version/units back (atom\n\
                 \x20                   INDICES, never ranges). Runs off the UI thread;\n\
                 \x20                   see examples/semantic/annotate-doc.py.\n\
                 \x20                   Exclusive with --semantic. Picking a question\n\
                 \x20                   (m/M, or / to type one) starts the analysis\n\
                 \x20                   (opening a file does not), and answers are\n\
                 \x20                   cached per document and question under\n\
                 \x20                   $XDG_CACHE_HOME/akapen/semantic.\n\
                 \x20                   $AKAPEN_SEMANTIC_CMD, then semantic_cmd in\n\
                 \x20                   the config file, is the default when this\n\
                 \x20                   flag is absent (the flag wins)\n\
                 \x20 --marks-questions <file>  read the marks questions from this\n\
                 \x20                   JSON instead of the built-in five (also\n\
                 \x20                   $XDG_CONFIG_HOME/akapen/marks-questions.json)\n\
                 \x20 --review-rules <file>  read the Review rules (R) from this JSON\n\
                 \x20                   instead of the built-in three, all off by default (also\n\
                 \x20                   $XDG_CONFIG_HOME/akapen/review-rules.json)\n\
                 \x20                   Accepted Review comments are sent with a\n\
                 \x20                   rewrite contract first (built in; override\n\
                 \x20                   with $XDG_CONFIG_HOME/akapen/review-contract.md)\n\
                 \x20 --lint-cmd <cmd>  let a linter find the Review candidates (R).\n\
                 \x20                   Runs via sh in the document's directory with\n\
                 \x20                   the document's absolute path appended; must\n\
                 \x20                   print LSP diagnostics JSON on stdout (any exit\n\
                 \x20                   code). See examples/lint/. Needs no\n\
                 \x20                   --semantic-cmd. $AKAPEN_LINT_CMD, then\n\
                 \x20                   lint_cmd in the config file, is the\n\
                 \x20                   default when this flag is absent\n\
                 \x20 --review-json     print the Review candidates as JSON and exit,\n\
                 \x20                   without starting the TUI (lines, rule, action\n\
                 \x20                   and score only — never the document text).\n\
                 \x20                   Needs --semantic-cmd\n\
                 \x20 --semantic-cache-clear  wipe that cache and exit (needed after\n\
                 \x20                   changing an analyser's prompts without\n\
                 \x20                   changing its command line)\n\
                 \x20 --review-dismissed-clear  forget every Review candidate\n\
                 \x20                   dismissed with x or sent with s, in every\n\
                 \x20                   document, and exit (one dismissal at a\n\
                 \x20                   time: x again in the list)\n\
                 \x20 --undercurl <auto|on|off> draw the Review underlines as curly\n\
                 \x20                   lines (default auto: on where the terminal\n\
                 \x20                   is known to draw them). $AKAPEN_UNDERCURL,\n\
                 \x20                   then undercurl in the config file, is the\n\
                 \x20                   default when this flag is absent\n\
                 \x20 --mark-blend <f>  how far the MARKED background is lifted off\n\
                 \x20                   the page, 0.0..1.0 (default 0.27)\n\
                 \x20 --dim-blend <f>   how far a DIM foreground is moved toward the\n\
                 \x20                   page, 0.0..1.0 (default 0.60; 1.0 is the page\n\
                 \x20                   itself, i.e. invisible)\n\
                 \x20 --callback <cmd>  shell command to spawn on exit\n\
                 \x20                   (e.g. return to a file-picker after quit)\n\
                 \x20 --esc-quit <auto|always|never> whether Esc may quit\n\
                 \x20                   (default auto: only with --callback;\n\
                 \x20                   always = unconditionally, never = Esc\n\
                 \x20                   stays a pure cancel)\n\
                 \n\
                 config file: $XDG_CONFIG_HOME/akapen/config.toml (else\n\
                 \x20 ~/.config/akapen/config.toml), every key optional:\n\
                 \x20 semantic_cmd, lint_cmd, undercurl, and [theme] dark / light.\n\
                 \x20 flags > environment > config file > defaults. Unknown keys,\n\
                 \x20 wrong types and broken syntax stop at startup. See\n\
                 \x20 examples/config.toml\n\
                 \n\
                 keys:\n\
                 \x20 view/source:  j/k scroll, Left/Right time travel, g/G top/bottom, PgUp/PgDn, Ctrl+u/Ctrl+d,\n\
                 \x20                v select, c comment, s send, y copy, d delete, e edit,\n\
                 \x20                r reload, a seen/set baseline, n/F7/]c next difference, ]/[ files,\n\
                 \x20                l comments, Ctrl+o files, Tab source mode\n\
                 \x20 source mode:  j/k move, v select, c comment, s send,\n\
                 \x20                y copy, d delete, e edit, r reload, ^n/^p next comment,\n\
                 \x20                a seen/set baseline, n/F7/]c next difference, ]/[ files,\n\
                 \x20                l comments, Ctrl+o files, Tab view mode (Markdown only)\n\
                 \x20 overlays:     j/k move, Enter jump/switch, d delete (comments),\n\
                 \x20                ? help, Esc/q close, click outside close\n\
                 \x20 input:        Enter confirm, Ctrl+j newline, Esc cancel"
            );
            Ok(())
        }
        Action::Version => {
            println!("akapen {VERSION}");
            Ok(())
        }
        Action::ClearSemanticCache => {
            // 置き場が決められない環境（HOME も XDG も無い）は「無い」。
            let Some(cache) = semantic_cache::SemanticCache::discover() else {
                println!("no semantic cache to clear (no HOME/XDG_CACHE_HOME)");
                return Ok(());
            };
            let root = cache.root().display().to_string();
            let removed = cache.clear()?;
            println!("cleared {removed} cached analyses ({root})");
            Ok(())
        }
        Action::ClearReviewDismissed => {
            let Some(store) = review::DismissedStore::discover() else {
                println!("no dismissed review candidates to clear (no HOME/XDG_DATA_HOME)");
                return Ok(());
            };
            let path = store.path().display().to_string();
            let removed = store.clear()?;
            println!("cleared {removed} dismissed review candidates ({path})");
            // 送った記録も同じ置き場にあるので、一緒に消す。
            let sent = store.sent();
            let sent_path = sent.path().display().to_string();
            let removed = sent.clear()?;
            println!("cleared {removed} sent review candidates ({sent_path})");
            Ok(())
        }
        Action::Run(config) => run(*config),
    }
}





/// When stdin is not a terminal (xargs gives children /dev/null; scripts
/// redirect it), rebind fd 0 to a real tty so the input reader
/// ([`termtheme::input`]) and the startup OSC 11 query
/// ([`termtheme::background`], which reads the answer from stdin) both
/// see the terminal. On macOS this must be the actual pty slave: /dev/tty
/// is a synthetic node that kqueue (mio) rejects with EINVAL — crossterm's
/// own /dev/tty fallback failed with "Failed to initialize input reader"
/// when it was still the reader, and the watchdog's `stdin_hung_up` polls
/// fd 0.
fn ensure_terminal_stdin() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return;
    }
    #[cfg(target_os = "macos")]
    if rebind_to_controlling_pty() {
        return;
    }
    // Other platforms (and macOS without a matching pty): the controlling
    // terminal node itself — epoll etc. accept it.
    if let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        use std::os::fd::AsRawFd;
        // SAFETY: both fds are valid; dup2 replaces fd 0 with the tty.
        unsafe {
            libc::dup2(tty.as_raw_fd(), libc::STDIN_FILENO);
        }
    }
}

/// macOS: find the real pty slave whose foreground process group is ours
/// (the controlling terminal's `tcgetpgrp` == our `getpgrp`) and rebind
/// stdin to it. Returns whether a match was found and dup2'ed.
#[cfg(target_os = "macos")]
fn rebind_to_controlling_pty() -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let me = unsafe { libc::getpgrp() };
    let Ok(rd) = std::fs::read_dir("/dev") else { return false };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("ttys") {
            continue;
        }
        let path = format!("/dev/{name}");
        let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&path)
        else {
            continue;
        };
        if unsafe { libc::tcgetpgrp(f.as_raw_fd()) } == me {
            // SAFETY: valid fds; rebind stdin to the real tty.
            unsafe {
                libc::dup2(f.as_raw_fd(), libc::STDIN_FILENO);
            }
            return true;
        }
    }
    false
}

/// Restores the terminal on drop: cursor, mouse capture, raw mode, and
/// alternate screen. `run()` creates it immediately after the no-blink
/// terminal init, so every early error return (a failed `terminal.size()`,
/// an I/O error in `event_loop`) and a panic (the unwind drops it) still
/// leave the shell usable. ratatui 0.30's `Terminal` has no Drop-based
/// restore and the panic hook only fires on panics, so a plain `?` would
/// otherwise exit the process with the terminal stuck in raw mode and echo
/// off.
///
/// 配色の知らせの購読（[`App::scheme`]）はここでは外さない — 外すのは
/// `run()` の `stop`（早い戻りと panic の unwind では、この guard より後に
/// 作る `App` が先に落ちて外す）。どの道でも**購読が先、端末の戻しが後**。
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), Show);
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

/// 端末に配色の知らせ（モード 2031、[`termtheme::scheme`]）を頼む。知らせは
/// イベントループの読み手が [`Input::ColorScheme`] として受け取り、
/// [`App::follow_color_scheme`] が `light` から導いたものを作り直す。`out` は
/// 書き先（`run()` は `Subscription::stdout()`、テストは書いた列を見る書き先）。
///
/// **`--light` / `--dark` で固定しているときは頼まない**（固定は固定。知らせを
/// 受けても使わない）— [`App::scheme`] は `Subscription::fixed()` のままで、
/// エディタの前後も終わるときも何も書かない。起動時は今の配色を聞かない
/// （起動時の判定は OSC 11 のまま）。2031 を知らない端末は黙って捨てる。
///
/// **読み手は crossterm であってはならない。** crossterm 0.29 は知らせの後ろの
/// 入力を飲み込み、`event::poll` ごと止まる（herdr は `CSI ? 996 n` に即答する
/// ので、crossterm で読んだまま頼むと起動直後に固まる）。
pub(crate) fn start_following_color_scheme(app: &mut App, out: termtheme::scheme::Subscription) {
    if !app.follows_color_scheme() {
        return;
    }
    app.scheme = out;
    let _ = app.scheme.start();
}

/// SIGTERM・SIGINT で殺されたとき: 配色の知らせの購読を外してから、既定の
/// 扱いで投げ直す（シェルが見る終わり方 128 + signum のまま死ぬ）。
///
/// ハンドラの中では `Drop` が走らないので、[`App::scheme`] は外れない —
/// 外さずに死ぬと、次に端末を使うもの（シェル、そこで開く akapen や
/// ashiato）に知らせが届き、crossterm で読むものはそこで止まる。
/// **async-signal-safe なことだけをする**: 外すのは
/// [`termtheme::scheme::unsubscribe_in_signal_handler`]（張っているときだけ
/// `write(2)` 1 回。エディタに端末を渡しているあいだは何もしない）。
/// 端末のほかの戻し（代替画面・マウス・raw モード）はしない — 入れる前と
/// 同じく、殺されたときは戻らない。raw モードでは Ctrl+C はキーとして届く
/// ので、ここへ来るのは外から送られたときだけ。
extern "C" fn unsubscribe_and_die(sig: libc::c_int) {
    termtheme::scheme::unsubscribe_in_signal_handler();
    // SAFETY: 既定の扱いに戻して投げ直す。どちらも async-signal-safe。
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// [`unsubscribe_and_die`] を SIGTERM・SIGINT に入れる。**無視されている
/// シグナルは無視のまま**にする（`sh -c 'akapen … &'` の SIGINT のように、
/// 親が無視させたものを、死ぬシグナルに変えない）。子（エディタ）は exec で
/// 既定の扱いに戻るので、ハンドラを受け継がない。
fn install_signal_handlers() {
    let handler: extern "C" fn(libc::c_int) = unsubscribe_and_die;
    for sig in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `unsubscribe_and_die` はハンドラとして正しい形で、
        // async-signal-safe なことだけをする。
        unsafe {
            if libc::signal(sig, handler as libc::sighandler_t) == libc::SIG_IGN {
                libc::signal(sig, libc::SIG_IGN);
            }
        }
    }
}

/// Move the first file's per-file state into the live App fields —
/// run()'s startup activation. Every field switch_to_file saves/restores
/// must ride along. Kept as a function so the wiring is exercised by
/// tests instead of only by the TUI path.
fn activate_first_file(app: &mut App) {
    let fs = &mut app.file_states[0];
    app.source = std::mem::take(&mut fs.source);
    app.spans = std::mem::take(&mut fs.spans);
    app.view = std::mem::take(&mut fs.view);
    app.file_stamp = fs.file_stamp;
    app.last_loaded_stamp = fs.last_loaded_stamp;
    app.baseline_changed = std::mem::take(&mut fs.baseline_changed);
    app.baseline_deleted_before = std::mem::take(&mut fs.baseline_deleted_before);
    app.comparison_changed = std::mem::take(&mut fs.comparison_changed);
    app.comparison_deleted_before = std::mem::take(&mut fs.comparison_deleted_before);
    app.comparison_deleted_blocks = std::mem::take(&mut fs.comparison_deleted_blocks);
}

/// A `CrosstermBackend` whose `show_cursor` is a no-op.
///
/// ratatui sends `show_cursor` on EVERY frame that publishes a cursor
/// position — which akapen does whenever the composer is open (the macOS
/// IME anchors its composition window there) — and akapen re-hides it
/// right after the draw. That visibility toggling makes the terminal's
/// hardware cursor flash at the redraw rate: the fast, annoying blinking
/// around the caret. akapen draws its own caret, so the hardware cursor
/// must simply never appear; the published POSITION still flows for the
/// IME. Visibility and position are separate, so dropping only the Show
/// keeps the anchor intact.
#[derive(Debug)]
pub(crate) struct NoBlinkBackend<W: std::io::Write + Send> {
    inner: CrosstermBackend<W>,
    /// **セルを書く先**（[`Backend::draw`] だけが使う）。
    ///
    /// `inner` の writer は ratatui-crossterm の unstable な API（
    /// `writer_mut`）でしか触れず、その feature は Cargo.lock に使わない
    /// バックエンドを引き込む。そこで**同じ stdout の別の handle** を持つ。
    /// `Stdout` の handle はすべて 1 つの大域のバッファを共有するので、
    /// `inner` が書くカーソル移動・消去と、ここが書くセルの順序は崩れない
    /// （`flush` は `inner` の 1 回で両方が出る）。
    out: W,
    /// **Review の下線を波線にするか**（`--undercurl`、[`crate::undercurl::resolve`]）。
    /// 描画の出口（[`Backend::draw`]）だけが読む。
    pub(crate) undercurl: bool,
    /// 最後に置いたカーソルの位置。[`Backend::get_cursor_position`] はこれを返し、
    /// **端末に聞かない** — crossterm の `cursor::position()` は crossterm の入力の
    /// 読み手を動かし、その間に届いたキーを crossterm の側に残して消す（配色の
    /// 知らせが届けばそこで止まる）。入力を読むのは [`termtheme::input`] だけ。
    cursor: ratatui::layout::Position,
}

impl NoBlinkBackend<std::io::Stdout> {
    /// Enter raw mode + the alternate screen and return a terminal whose
    /// cursor never becomes visible (the analogue of `ratatui::init()`
    /// plus the no-blink promise). The caller's own guard restores the
    /// terminal on exit.
    pub(crate) fn init(undercurl: bool) -> Result<AppTerminal> {
        ratatui::crossterm::terminal::enable_raw_mode()?;
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::cursor::Hide,
            ratatui::crossterm::terminal::EnterAlternateScreen
        );
        let backend = NoBlinkBackend {
            inner: CrosstermBackend::new(std::io::stdout()),
            out: std::io::stdout(),
            undercurl,
            cursor: ratatui::layout::Position::ORIGIN,
        };
        Ok(ratatui::Terminal::new(backend)?)
    }
}

impl<W: std::io::Write + Send> Backend for NoBlinkBackend<W> {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        // `CrosstermBackend::draw` の代わりに、波線の印を読み替える出口を
        // 通す（[`crate::undercurl::draw`]）。
        crate::undercurl::draw(&mut self.out, content, self.undercurl)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        // The whole point: never let the hardware cursor blink. The IME
        // anchor (set_cursor_position) still updates.
        Ok(())
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        // 端末に聞かない（`cursor` の doc）。全画面なので、置いた位置が今の位置。
        Ok(self.cursor)
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.cursor = position;
        Ok(())
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.inner.size()
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// The session terminal: ratatui over [`NoBlinkBackend`], so the hardware
/// cursor never blinks while the IME anchor keeps flowing.
pub(crate) type AppTerminal = ratatui::Terminal<NoBlinkBackend<std::io::Stdout>>;

fn run(config: Config) -> Result<()> {
    // `--review-json` は TUI を立てない。端末にも IME にも触る前に抜ける
    // — stdout へ JSON を出すだけの経路が raw mode を通ると、パイプの
    // 向こうが制御シーケンスを受け取る。
    if config.review_json {
        return run_review_json(&config);
    }
    // A piped/redirected stdin must not kill the TUI (see above).
    ensure_terminal_stdin();
    // Compile the macOS IME helper in the background so the first composer
    // close never blocks on swiftc (see [`crate::ime::start_background_build`]).
    ime::start_background_build();
    let files = config.files.clone();

    // Load every file BEFORE entering raw mode: a missing or non-UTF-8
    // file fails here with the terminal untouched, so the anyhow error
    // stays readable and no restore is needed.
    let mut sources: Vec<Source> = Vec::with_capacity(files.len());
    for f in &files {
        sources.push(Source::load(f.clone())?);
    }

    // The Semantic Reading Layer's source (`--semantic` / `--semantic-cmd`).
    // Resolved BEFORE the terminal enters raw mode, so a broken fixture is
    // an ordinary command-line error instead of a TUI that paints nothing.
    // `--semantic-cmd` is NOT run here — it runs once a document is on
    // screen, from its own thread.
    let semantic_source = crate::semantic::source_from_config(&config)?;
    // marks モードの問い。**ここで読む** — 壊れた問いのファイルは、
    // 何も光らない TUI ではなく普通のコマンドラインエラーであるべきで、
    // fixture を起動前に読むのと同じ理由である。
    //
    // **層が無ければ読まない。** ここを無条件にすると、
    // `$XDG_CONFIG_HOME` の問いのファイルが壊れている人は
    // `akapen foo.md` すら開けなくなる。問いは層が無ければ 1 度も
    // 使われないので、読む理由も無い。
    let marks_questions = if semantic_source.is_some() {
        Some(crate::marks_questions::Questions::discover(
            config.marks_questions.as_deref(),
        )?)
    } else {
        None
    };
    // Review のルールも同じ扱い — 壊れたルールのファイルは、`R` を押す
    // まで黙っている TUI ではなく普通のコマンドラインエラーであるべき。
    // **層が無ければ読まない**（marks の問いと同じ理由）。
    let review_rules = if semantic_source.is_some() {
        Some(crate::review_rules::Rules::discover(
            config.review_rules.as_deref(),
        )?)
    } else {
        None
    };

    let mut terminal =
        NoBlinkBackend::init(crate::undercurl::resolve_from_env(config.undercurl))?;
    // From here on the terminal is in raw mode + alternate screen; the
    // guard's Drop restores it on every return path, early or normal.
    let terminal_guard = TerminalGuard;
    // Light/dark resolution: --light/--dark win, else the terminal's
    // background is queried (OSC 11). Needs raw mode (init enables it)
    // and must run before the event loop consumes input; unanswerable
    // terminals fall back to dark. Keys typed during the wait are handed
    // to the input reader, not dropped; an answer later than the wait
    // arrives there as `Input::Background` and is followed like a
    // color-scheme notification. While open, mode 2031 notifications
    // re-decide it (`start_following_color_scheme` below).
    let light = config
        .light
        .unwrap_or_else(|| termtheme::background::detect_light().unwrap_or(false));
    // The syntax theme: the side matching light/dark (--theme covers both,
    // else --theme-dark / --theme-light, else the config file's [theme]);
    // a missing or unresolvable name falls back to a default matching
    // light/dark (highlight.rs).
    let highlight = Highlighter::new(config.theme.for_background(light), light);
    let size = terminal.size()?;

    // Build FileState entries eagerly so spans/views are ready before the
    // first frame (the sources themselves were loaded above, pre-init).
    let mut file_states: Vec<FileState> = Vec::with_capacity(files.len());
    for (f, source) in files.iter().zip(sources) {
        let spans = highlight.highlight_with(&source.content, syntax_for(f));
        // Non-Markdown files are source-only, so their view is never
        // shown; rendering it here would be discarded work at startup
        // (a session of many large .rs files pays for it).
        let view = if supports_view(f) {
            render_view_with_cards(&source, view_render_width(size.width), &highlight, &[], config.decoration_blend)
        } else {
            ViewState::default()
        };
        let mut fs = FileState {
            source,
            spans,
            view,
            mode: if supports_view(f) { Mode::View } else { Mode::Source },
            ..Default::default()
        };
        // Record the on-disk stamp so the first poll doesn't treat the file
        // as freshly edited.
        if let Ok(meta) = std::fs::metadata(f) {
            let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
            fs.file_stamp = Some(stamp);
            fs.last_loaded_stamp = Some(stamp);
        }
        file_states.push(fs);
    }

    // Move the first file's loaded state into the live App fields (the
    // emptied slot is never read: switch_to_file always saves the live
    // state back into it before leaving the file). Extracted so tests
    // exercise the same wiring (git_tests::startup_activation_...).
    let snapshot_cache = if config.reply {
        None
    } else {
        SnapshotCache::discover()
    };
    let mut app = App::new(config, Source::default(), highlight, ViewState::default(), light);
    let histories: Vec<DocumentHistory> = files
        .iter()
        .zip(file_states.iter())
        .map(|(path, state)| {
            snapshot_cache
                .as_ref()
                .and_then(|cache| {
                    DocumentHistory::open_cached(path, &state.source.content, 64, cache).ok()
                })
                .unwrap_or_else(|| DocumentHistory::load(path, &state.source.content, 64))
        })
        .collect();
    for (state, history) in file_states.iter_mut().zip(histories.iter()) {
        if let Some(baseline) = history.baseline_content.as_deref() {
            let (changed, deleted, blocks) =
                history::comparison_transition(baseline, &state.source.content);
            state.baseline_changed = changed;
            state.baseline_deleted_before = deleted;
            state.comparison_changed = state.baseline_changed.clone();
            state.comparison_deleted_before = state.baseline_deleted_before.clone();
            state.comparison_deleted_blocks = blocks;
        }
    }
    app.histories = histories;
    app.snapshot_cache = snapshot_cache;
    app.file_states = file_states;
    app.marks_questions = marks_questions;
    app.review_rules = review_rules;
    app.lint = app.config.lint_cmd.as_deref().map(crate::lint::LintCommand::new);
    // 捨てた候補の置き場。決められない環境（HOME も XDG も無い）は
    // `None` で、セッション内だけ消えて記録は残らない。
    app.review_dismissed_store = review::DismissedStore::discover();
    app.set_semantic_source(semantic_source);
    activate_first_file(&mut app);
    // The first document is on screen now: ask the provider about it.
    // (`activate_first_file` is what puts `app.source` in place.)
    app.reanalyze_semantics();
    // Command mode always runs in ASCII so j/k etc. are never swallowed by
    // the IME. **最初の画面を描いてから**切り替える（イベントループが
    // 描いた直後に試す）: ヘルパーを 2 回（get と abc）起こすので 0.15 秒
    // ほどかかり、ここで呼ぶとその分だけ文書が出るのが遅れる。初回は
    // ヘルパーのコンパイル中で空振りしうるが、ループが効くまで試し直す。
    if app.source.len() > HUGE_FILE {
        app.flash(format!(
            "warning: {} lines — view mode keeps the whole render in memory",
            app.source.len()
        ));
    }
    // The hardware cursor stays hidden for the whole session (modern-TUI
    // pattern, like Hermes/pi.dev): the composer draws its own block
    // caret, and the IME still anchors its inline composition window to
    // the logical cursor position we keep publishing via
    // set_cursor_position.
    let _ = execute!(std::io::stdout(), Hide);
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // 開いたまま外観（ライト／ダーク）が切り替わっても追いつくように、配色の
    // 知らせを頼む。外すのはエディタの前と、下の `stop`（早い戻り・panic では
    // `app` の Drop）、殺されたときはシグナルハンドラ。ハンドラは張る前に入れる。
    install_signal_handlers();
    start_following_color_scheme(&mut app, termtheme::scheme::Subscription::stdout());
    // **kitty keyboard protocol は押さない。** 押すと端末の auto-repeat が
    // `KeyEventKind::Repeat` に変わり、矢印の押しっぱなしが効かなくなる
    // （`docs/gotchas/terminal-keys.md`）。`f` はトグルなので要らない。
    let res = event_loop(&mut terminal, &mut app);
    // **購読を先に外す。** 購読は端末の状態で、プロセスが終わっても残る —
    // `app` はまだ生きている（callback が `app.config` を読む）ので、外さずに
    // callback を起こすと、そこで開く ashiato や akapen に配色の知らせが届き、
    // crossterm で読むものはそこで止まる（[`termtheme::scheme`]）。
    let _ = app.scheme.stop();
    // The guard's Drop performs the rest of the shutdown (cursor, mouse
    // capture, raw mode, alternate screen). Drop it explicitly BEFORE
    // spawning the callback so the callback inherits a clean terminal; on
    // early error returns the same Drop runs at scope exit instead.
    drop(terminal_guard);
    if let Some(cmd) = &app.config.callback {
        let _ = Command::new("sh").arg("-c").arg(cmd).spawn();
    }
    res
}

/// `--review-json`: 有効なルールの候補を JSON で stdout に出して終わる。
///
/// **TUI は立たない。** 端末にも IME にも触らないので、パイプの向こうは
/// JSON だけを受け取る。段階 2（LLM へ送る側）と LSP の入口である。
///
/// 経路は `R` とまったく同じ — 同じルール、同じ
/// [`crate::semantic::CommandProvider`]、同じキャッシュ、同じ足切り、
/// 同じ `dismissed.jsonl` の除外である。**捨てた候補は出さない**: 捨てる
/// という操作は「これは候補ではない」と読み手が言ったことで、出力先が
/// 画面かパイプかでその判断が変わる理由が無い。
///
/// 解析は**逐次**に走らせる（ルール 1 本ずつ）。TUI と違って待っている
/// 人が居ないので、スレッドを増やして判定器を同時に叩く理由が無い。
fn run_review_json(config: &Config) -> Result<()> {
    use anyhow::{Context, bail};
    let rules = review_rules::Rules::discover(config.review_rules.as_deref())?;
    let Some(semantic::SemanticSource::Command(provider)) =
        semantic::source_from_config(config)?
    else {
        // `Config::parse` が弾いているので、ここへは来ない。
        bail!("--review-json needs --semantic-cmd");
    };
    // 1 文書だけを見る。複数渡されたら先頭 — 出力が 1 つの `source_sha` を
    // 名乗る形なので、2 つ目以降を黙って混ぜることはできない。
    let path = &config.files[0];
    let source = Source::load(path.clone())?;
    let sha = semantic::source_digest(&source.content);
    let dismissed = review::DismissedStore::discover()
        .map(|store| store.load(&sha))
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for rule in rules.enabled() {
        let document = provider
            .analyze(&source.content, &rule.question())
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("review rule `{}`", rule.id))?;
        candidates.extend(
            review::candidates_for(&document, rule, &source.content)
                .into_iter()
                .filter(|c| {
                    !dismissed.contains(&(c.rule.clone(), c.range.start, c.range.end))
                }),
        );
    }
    candidates.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(a.rule.cmp(&b.rule))
    });
    println!("{}", review::to_json(&sha, &candidates)?);
    Ok(())
}

/// Events processed per frame at most, so a pathological input flood can't
/// starve the draw (the rest is handled on the next frame).
const MAX_EVENTS_PER_FRAME: usize = 64;
const HISTORY_RENDER_DEBOUNCE: Duration = Duration::from_millis(300);

/// Whether this process still has a controlling terminal — i.e. the
/// pane/window this TUI runs in is alive. Cheap (one open(2) on
/// `/dev/tty`), so it is safe to poll every tick.
///
/// Why this matters: when the session leader exits (window/pane closed),
/// the kernel releases the controlling terminal. The input reader never
/// notices — the pty master can stay open (herdr keeps it for
/// scrollback), so reads just block and draws keep succeeding, and no
/// signal arrives.
/// Once the terminal is released, `/dev/tty` stops opening (ENXIO), the
/// only reliable "session is dead" signal from inside the process.
fn controlling_terminal_alive() -> bool {
    std::fs::OpenOptions::new().read(true).open("/dev/tty").is_ok()
}

/// Whether stdin's writer is gone (POLLHUP): a pipe-based virtual
/// terminal (e.g. a herdr plugin pane) whose owner closed. (The input
/// reader also reports the EOF as an error once it reads it; this catches
/// the hang-up before a read is attempted.)
fn stdin_hung_up() -> bool {
    let mut pfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: 0,
        revents: 0,
    };
    // SAFETY: poll(2) on fd 0, which is open here (the input reader reads it).
    let n = unsafe { libc::poll(&mut pfd, 1, 0) };
    n > 0 && (pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0
}

/// Draw one frame through ratatui, then repair the terminal's wide-char
/// afterimages (see [`clear_wide_char_residue`]). The main session draws
/// go through here so the repair covers every redraw.
pub(crate) fn draw_frame(
    terminal: &mut AppTerminal,
    app: &mut App,
) -> std::io::Result<()> {
    terminal.draw(|f| draw(f, app))?;
    let current = app.last_frame.take().unwrap_or_default();
    if let Some(prev) = app.prior_frame.take() {
        clear_wide_char_residue(&prev, &current);
    }
    app.prior_frame = Some(current);
    Ok(())
}

/// Re-draw the right halves of wide characters that a previous frame
/// blanked but ratatui's diff never addressed.
///
/// When a wide (CJK) character is removed or replaced, the diff emits the
/// new glyph for its LEFT cell but skips the RIGHT cell: in ratatui's
/// buffer that continuation cell is blank both before and after, so the
/// diff considers it unchanged. A real terminal, however, visibly renders
/// the wide character's right half there — the skipped cell keeps the
/// halved glyph, so backspacing through mixed-width text leaves
/// afterimages (and shifts everything below). This pass queues an
/// explicit space over every such cell after the draw.
pub(crate) fn clear_wide_char_residue(
    prev: &ratatui::buffer::Buffer,
    curr: &ratatui::buffer::Buffer,
) {
    let mut out = std::io::stdout().lock();
    let _ = clear_wide_char_residue_to(prev, curr, &mut out);
}

/// The writer-injectable core of [`clear_wide_char_residue`], so tests
/// can capture the queued cleanup cells.
pub(crate) fn clear_wide_char_residue_to<W: std::io::Write>(
    prev: &ratatui::buffer::Buffer,
    curr: &ratatui::buffer::Buffer,
    out: &mut W,
) -> std::io::Result<()> {
    use ratatui::crossterm::queue;
    use ratatui::crossterm::cursor::MoveTo;
    use ratatui::crossterm::style::{Print, ResetColor, SetBackgroundColor};
    use ratatui::backend::IntoCrossterm;
    use unicode_width::UnicodeWidthStr;

    // A resize replaces ratatui's frame buffer with the terminal's new
    // dimensions. Only compare cells that exist in both frames: columns
    // clipped by a horizontal shrink no longer exist on the terminal and
    // need no afterimage repair.
    let area = prev.area.intersection(curr.area);
    let mut wrote = false;
    for y in area.y..area.bottom() {
        for x in area.x..area.right().saturating_sub(1) {
            // A column that no longer begins a wide character, whose next
            // cell is blank (the diff skipped it), where the previous
            // frame HAD a wide character covering x..x+1.
            let wide_before = UnicodeWidthStr::width(prev[(x, y)].symbol()) == 2;
            if !wide_before {
                continue;
            }
            if UnicodeWidthStr::width(curr[(x, y)].symbol()) == 2 {
                continue; // a new wide character owns both cells now
            }
            if curr[(x + 1, y)].symbol() != " " {
                continue; // the diff emitted the content there
            }
            // The space must carry the cell's own background: a bare
            // Print(" ") wrote with whatever SGR state the terminal was
            // left in — over a highlight band (the cursor row's fill in
            // the other mode) that punched default-background holes into
            // the band (visible as stripes after a view/source toggle).
            let bg = curr[(x + 1, y)]
                .style()
                .bg
                .unwrap_or(ratatui::style::Color::Reset);
            queue!(
                out,
                MoveTo(x + 1, y),
                SetBackgroundColor(bg.into_crossterm()),
                Print(" ")
            )?;
            wrote = true;
        }
    }
    if wrote {
        queue!(out, ResetColor)?;
        out.flush()?;
    }
    Ok(())
}

fn event_loop(terminal: &mut AppTerminal, app: &mut App) -> Result<()> {
    // Paint the initial frame before waiting for input.
    draw_frame(terminal, app)?;
    // Death watchdog: exit cleanly when the session dies under us. With
    // a controlling terminal we watch `/dev/tty`; without one from the
    // start (pipe-based virtual terminal) we watch stdin for HUP.
    let watch_terminal = controlling_terminal_alive();
    let watch_stdin = !watch_terminal;
    loop {
        if (watch_terminal && !controlling_terminal_alive())
            || (watch_stdin && stdin_hung_up())
        {
            return Err(anyhow::anyhow!("terminal gone; exiting"));
        }
        // Wait up to one tick for input, then drain everything that
        // arrived. A fast wheel flick queues dozens of mouse events, and
        // each one used to trigger its own full redraw — on a large
        // document (or through herdr, which can deliver a burst at once)
        // that made scrolling crawl. Batching the burst into a single
        // frame keeps the wheel responsive.
        // While an effect animates, tick fast enough for smooth frames
        // (the idle cadence of 10 fps would cut a 450 ms stream into 4-5
        // jumps); otherwise keep the lazy 100 ms poll.
        // 外部コマンドの答え待ちのあいだは、待ちの印が回る間隔で描く。
        let tick = if app.has_active_fx() {
            FX_TICK_MS
        } else if app.waiting() {
            crate::chrome::SPINNER_FRAME_MS
        } else {
            TICK_MS
        };
        // 入力は termtheme の読み手で読む（crossterm の `event::poll` /
        // `event::read` と混ぜない — 読み手が 2 つになると端末を取り合う）。
        // crossterm と同じ `Event` に加えて、配色の知らせ（モード 2031）と
        // 遅れて届いた OSC 11 の答えが来る。
        if input::poll(Duration::from_millis(tick))? {
            for _ in 0..MAX_EVENTS_PER_FRAME {
                if !input::poll(Duration::ZERO)? {
                    break;
                }
                let event = match input::read()? {
                    Input::Event(event) => event,
                    other => {
                        // 描き直しはこの回の終わりの `draw_frame`。
                        if let Some(light) = other.light() {
                            app.follow_color_scheme(light);
                        }
                        continue;
                    }
                };
                match event {
                    // **`Repeat` は `Press` と同じに扱う。** いまの
                    // akapen は kitty keyboard protocol を押さないので
                    // repeat は押下の連打として届き、この腕には `Press`
                    // しか来ない。旗を押す端末（あるいは将来また押した
                    // とき）に矢印の押しっぱなしが 1 回で止まらないための
                    // 保険である（`docs/gotchas/terminal-keys.md`）。
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        let history_direction = history_key_direction(app, key.code, key.modifiers);
                        if let Some(direction) = history_direction {
                            select_history(app, direction);
                        } else {
                            render_pending_history(app, true);
                            on_key(app, key.code, key.modifiers, Some(terminal));
                        }
                    }
                    Event::Mouse(mouse) => {
                        render_pending_history(app, true);
                        on_mouse(app, mouse);
                    }
                    Event::Resize(..) => {
                        mark_view_dirty(app);
                        // 高さだけの変化では view を描き直さない。据え付けの
                        // 高さが変わると本文の高さも変わるので、ここで送る。
                        crate::review_dock::follow(app);
                    }
                    _ => {}
                }
            }
        }
        // Expire a pending `]`/`[` chord into its default file switch.
        expire_pending_chord(app);
        render_history_when_settled(app);
        expire_history_ghosts(app);
        // Collect any finished `--semantic-cmd` analysis. This is the
        // whole of the asynchrony: the worker thread does the waiting,
        // and the loop picks the answer up on its next turn — so a
        // command that takes 30 seconds costs one `try_recv` per tick
        // and never a frame. Placed before the draw so an answer that
        // landed this tick is painted this tick.
        app.poll_semantic_analysis();
        // Review は別の線で答える（`crate::app::ReviewChannel`）。marks の
        // 答えと混ざらないのは型の段でそうなっている。
        app.poll_review_analysis();
        draw_frame(terminal, app)?;
        // The hardware cursor never becomes visible: the session runs on
        // [`NoBlinkBackend`], whose show_cursor is a no-op, so the
        // per-frame publish of the composer's IME anchor cannot make the
        // terminal cursor flash at the redraw rate. This hide is a
        // belt-and-suspenders reminder for the crossterm Hide at startup;
        // the position still updates for the macOS IME anchor.
        let _ = terminal.backend_mut().hide_cursor();
        begin_landing_pulse_after_draw(app);
        // Resize debounce: re-render the view once the pane settles.
        if app.view_dirty {
            let since = app.view_dirty_since.unwrap_or_else(Instant::now);
            if since.elapsed() >= RESIZE_DEBOUNCE {
                rerender_view(app);
                app.view_dirty = false;
                app.view_dirty_since = None;
            }
        }
        // Keep retrying the session's ASCII force until the helper exists
        // (first run compiles it in the background).
        if !app.ime_forced {
            app.ime_forced = app.ime_session.force_ascii();
        }
        // File-change polling: an external edit (the agent rewriting the
        // doc) is debounced; the reload is manual (`r`) so content is
        // never swapped under the user mid-work — except in reply mode,
        // where the doc only changes via the akp refresh (i.e. the agent's
        // new message is here) and reloads automatically.
        poll_file_change(app);
        if let Some(since) = app.reload_pending
            && since.elapsed() >= RELOAD_DEBOUNCE
            && app.mode != Mode::Input
        {
            app.reload_pending = None;
            if app.config.reply {
                // Auto-reload: comments on the old message are dropped
                // with it (they were either sent or are stale).
                reload_now_auto(app);
            } else {
                notify_file_changed(app);
            }
        }
        // Expire transient footer messages.
        if app
            .status
            .as_ref()
            .is_some_and(|(_, at, _)| at.elapsed() > STATUS_SECS)
        {
            app.status = None;
            app.toast_fx = None;
        }
        // Expire the scrubber tooltip (its dissolve has played out).
        if app
            .timeline_tooltip_until
            .is_some_and(|until| until.elapsed() > Duration::ZERO)
        {
            app.timeline_tooltip_until = None;
        }
        if !app.running {
            return Ok(());
        }
    }
}

fn on_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut AppTerminal>) {
    // `A` の取り消しは Review の一覧の直後の `A` だけ（一覧のキーは
    // `on_review_overlay_key` が自分で外す）。一覧の外のキーでは外す。
    if app.overlay != Some(Overlay::Review) {
        app.review_accept_all_undo = None;
    }
    // Overlay intercepts its own keys first.
    if app.overlay.is_some() {
        // Review の一覧の `e` だけは端末が要る（エディタのあいだ TUI を
        // 畳む）ので、端末を持たない overlay のキーの道の手前で拾う。
        if app.overlay == Some(Overlay::Review)
            && key == KeyCode::Char(crate::keys::REVIEW_EDIT)
            && modifiers.is_empty()
        {
            app.review_accept_all_undo = None;
            if let Some(t) = terminal {
                review_edit_with(app, |app, line| open_editor_at(app, t, line));
            }
            return;
        }
        return on_overlay_key(app, key, modifiers);
    }
    // A pending `]`/`[` chord resolves on the next key: `c` within the
    // window completes it into a review-mark jump (the F7 fallback), `]`/`[`
    // again falls back to the file switch and arms the new bracket, Esc
    // cancels entirely (no file switch), and any other key falls back to
    // the file switch and is then processed normally.
    if let Some((_, bracket)) = app.pending_chord {
        match key {
            KeyCode::Char('c') if modifiers.is_empty() => {
                app.pending_chord = None;
                jump_change_mark(app, if bracket == ']' { 1 } else { -1 });
                return;
            }
            // `]m` / `[m`: 次・前のマーク行へ（`crate::keys::MARK_JUMP`）。
            //
            // 代償は `]` のあとの `m` が「ファイル切替 ＋ 問いの popup」では
            // なくジャンプになること。popup は `]` を
            // 挟まずに `m` を打てば開く。
            KeyCode::Char(crate::keys::MARK_JUMP) if modifiers.is_empty() => {
                app.pending_chord = None;
                jump_mark(app, if bracket == ']' { 1 } else { -1 });
                return;
            }
            KeyCode::Char(']') | KeyCode::Char('[') if modifiers.is_empty() => {
                let next = if key == KeyCode::Char(']') { ']' } else { '[' };
                switch_file(app, bracket);
                app.pending_chord = Some((Instant::now(), next));
                return;
            }
            KeyCode::Esc => {
                // Cancel: the bracket is dropped, Esc proceeds normally.
                app.pending_chord = None;
            }
            _ => {
                app.pending_chord = None;
                switch_file(app, bracket);
                // Fall through: the new key is processed normally.
            }
        }
    }
    match app.mode {
        Mode::Input => on_input_key(app, key, modifiers),
        Mode::View => on_view_key(app, key, modifiers, terminal),
        Mode::Source => on_source_key(app, key, modifiers, terminal),
    }
}


















/// Mouse handling: wheel scrolls like j/k, left-click moves the cursor,
/// left-drag selects a line range (source mode only). Clicking without
/// dragging never starts a selection, so the mouse stays optional.
fn on_mouse(app: &mut App, mouse: MouseEvent) {
    // A mouse action cancels a pending `]`/`[` chord: the user moved on,
    // the bracket's default file switch must not fire later.
    app.pending_chord = None;
    // マウスも「ほかの操作」— `A` の直後ではなくなる。
    app.review_accept_all_undo = None;
    // **Review の一覧は窓ではない**（本文の下に据え付けてある）。一覧の
    // 上のマウスは一覧を動かし、本文の上のマウスは本文に届く — 本文を
    // 押しても一覧は閉じない（外を押して閉じるのは被さる窓の作法である）。
    if crate::review_dock::is_open(app) {
        if let Some(dock) = crate::review_dock::current(app)
            && mouse.row >= dock.area.y
            && mouse.row < dock.area.y + dock.area.height
        {
            on_review_dock_mouse(app, mouse);
            return;
        }
        // 本文の上: 下の本文の扱いへ落ちる。
    } else if app.overlay.is_some() {
        // With an overlay open the mouse drives the overlay only: a click
        // outside the panel closes it (modal dismiss), a click on an entry
        // selects it, and the wheel moves the selection (j/k semantics).
        // Nothing touches the content underneath.
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (w, h) = crate::app::terminal_size();
                // 開いている overlay 自身の枠。`m` の popup は小さい箱
                // なので、70 % パネルで測ると箱の外を押しても行が選ばれる。
                let panel = crate::overlay::active_overlay_panel(
                    app,
                    Rect { x: 0, y: 0, width: w, height: h },
                );
                if mouse.row < panel.y
                    || mouse.row >= panel.y + panel.height
                    || mouse.column < panel.x
                    || mouse.column >= panel.x + panel.width
                {
                    // A click outside dismisses; it can't be part of a
                    // double-click either.
                    app.last_overlay_click = None;
                    app.overlay = None;
                } else if let Some(idx) = overlay_entry_at(app, mouse.row) {
                    // A second click within the window on the SAME entry
                    // is a double-click: it activates the selection (the
                    // Enter-equivalent, e.g. switching the file or
                    // jumping to the comment).
                    let is_double = app.last_overlay_click.is_some_and(
                        |(t, prev)| t.elapsed() < app.double_click_ms && prev == idx,
                    );
                    app.last_overlay_click = Some((Instant::now(), idx));
                    app.overlay_cursor = idx;
                    keep_overlay_cursor_visible(app);
                    // The timeline list is a live scrubber: a click
                    // seeks the history cursor immediately (Enter only
                    // confirms the position).
                    if app.overlay == Some(Overlay::Timeline) {
                        timeline_overlay_seek(app, idx);
                    }
                    if is_double {
                        activate_overlay_selection(app);
                    }
                } else {
                    // Title/header rows: not an entry, not a double-click.
                    app.last_overlay_click = None;
                }
            }
            MouseEventKind::ScrollDown => match app.overlay {
                Some(Overlay::Help) => {
                    let max = help_rows(app.esc_quit_enabled(), app.config.reply, false, app.semantic_enabled(), app.semantic_enabled() || app.lint.is_some())
                        .len()
                        .saturating_sub(overlay_visible_rows());
                    app.overlay_cursor = (app.overlay_cursor + 1).min(max);
                }
                Some(Overlay::Timeline) => timeline_overlay_move(app, 1),
                Some(Overlay::Files) => {
                    if !app.files.is_empty() {
                        app.overlay_cursor = (app.overlay_cursor + 1).min(app.files.len() - 1);
                        keep_overlay_cursor_visible(app);
                    }
                }
                _ => {
                    // Comments overlay: the current tab's entries.
                    let total = overlay_entry_count(app);
                    if total > 0 {
                        app.overlay_cursor = (app.overlay_cursor + 1).min(total - 1);
                        keep_overlay_cursor_visible(app);
                    }
                }
            },
            MouseEventKind::ScrollUp => match app.overlay {
                Some(Overlay::Help) => {
                    let max = help_rows(app.esc_quit_enabled(), app.config.reply, false, app.semantic_enabled(), app.semantic_enabled() || app.lint.is_some())
                        .len()
                        .saturating_sub(overlay_visible_rows());
                    app.overlay_cursor = app.overlay_cursor.saturating_sub(1).min(max);
                }
                Some(Overlay::Timeline) => timeline_overlay_move(app, -1),
                _ => {
                    app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
                    keep_overlay_cursor_visible(app);
                }
            },
            _ => {}
        }
        return;
    }
    // The content area starts right under the title bar (row 0); the view
    // pane's frame adds its top border (source mode draws no frame).
    let content_row = mouse
        .row
        .saturating_sub(if app.view_active() { 2 } else { 1 }) as usize;
    // The scrollbar track is the pane's rightmost column (in view mode it
    // rides the frame's right border, in source mode the pane's own
    // edge). A press on the track jumps the viewport to the clicked row
    // and grabs the thumb; a drag scrubs it — the pointer may leave the
    // column while grabbed (the row clamps). Wheel events fall through to
    // the normal handling below. The track exists only while the content
    // overflows; when everything fits, clicks on the column fall through
    // to the content. Viewport-only scroll, like the wheel: the cursor
    // keeps its absolute position.
    let (w, _) = crate::app::terminal_size();
    let viewport = if app.view_active() {
        app.view_viewport_rows()
    } else {
        app.source_viewport_rows()
    };
    let content_len = if app.view_active() {
        app.view.rows.len()
    } else {
        app.line_rows.iter().sum()
    };
    let current_offset = if app.view_active() { app.view.offset } else { app.offset };
    // The track column: view mode rides the frame's right border (one
    // column inside it, where the right pad is); source mode has no frame,
    // so the track is the pane's own rightmost column.
    let track_col = if app.view_active() { w - 2 } else { w - 1 };
    let on_track = mouse.column == track_col
        && mouse.row as usize > if app.view_active() { 1 } else { 0 }
        && content_row < viewport
        && scroll_thumb(content_len, viewport, current_offset).is_some();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if on_track => {
            let row = content_row.min(viewport - 1);
            if let Some(target) = scroll_offset_at(content_len, viewport, row) {
                if app.view_active() {
                    app.view.offset = target;
                } else {
                    app.offset = target;
                }
                app.scrollbar_drag = Some((row, target));
            }
            return;
        }
        MouseEventKind::Drag(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            let (start_row, start_offset) = app.scrollbar_drag.unwrap();
            let row = content_row.min(viewport - 1);
            if let Some(target) = scroll_offset_drag(content_len, viewport, start_row, start_offset, row) {
                if app.view_active() {
                    app.view.offset = target;
                } else {
                    app.offset = target;
                }
            }
            return;
        }
        MouseEventKind::Up(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            app.scrollbar_drag = None;
            return;
        }
        _ => {}
    }
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            app.drag_anchor = None;
            // The selection survives scrolling: a selected range stays
            // virtually selected even when the cursor line scrolls off
            // screen (source_scroll moves the viewport only).
            match app.mode {
                Mode::View => app.view.wheel_scroll(1, app.view_viewport_rows()),
                Mode::Source | Mode::Input => source_scroll(app, 1),
            }
        }
        MouseEventKind::ScrollUp => {
            app.drag_anchor = None;
            match app.mode {
                Mode::View => app.view.wheel_scroll(-1, app.view_viewport_rows()),
                Mode::Source | Mode::Input => source_scroll(app, -1),
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // While the composer is open, the mouse must not touch the
            // cursor or the selection: the commented range is pinned
            // (input_start/input_end) and a click/drag rewriting the
            // selection mid-typing leaves a confusing state after Enter.
            // Wheel scrolling above stays available.
            if app.mode == Mode::Input {
                return;
            }
            // The title bar (row 0) is clickable: `▌ N` opens the comment
            // list, `1/3 files` the file picker, the path copies the
            // full path to the clipboard, and the `esc …` badge is one Esc
            // (`crate::esc::peel` — the badge says what that peels). The
            // hit regions come from the same layout math draw_title uses.
            if mouse.row == 0 {
                let (w, _) = crate::app::terminal_size();
                match title_hit_at(app, w, mouse.column) {
                    Some(TitleHit::Esc) => esc::peel(app),
                    Some(TitleHit::CommentCount) => open_overlay(app, Overlay::Comments, 0),
                    Some(TitleHit::FileCount) => {
                        open_overlay(app, Overlay::Files, app.current_file_index);
                    }
                    Some(TitleHit::Path) => {
                        let path = app.current_file_path().display().to_string();
                        match export::copy_to_clipboard(&path) {
                            Ok(()) => app.flash("path copied"),
                            Err(e) => app.flash_err(format!("path copy failed: {e:#}")),
                        }
                    }
                    None => {}
                }
                return;
            }
            if let Some(idx) = source_row_at(app, content_row, mouse.column as usize) {
                app.drag_anchor = Some(idx);
                // A click moves the cursor and drops any pending selection
                // (a drag right after re-creates it) — both modes alike.
                app.selection = None;
                if app.mode == Mode::View {
                    app.view.cursor = idx;
                    let viewport = app.view_viewport_rows();
                    app.view.keep_cursor_visible(viewport);
                } else {
                    app.cursor = idx;
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.mode == Mode::Input {
                return;
            }
            if let (Some(anchor), Some(end)) = (
                app.drag_anchor,
                source_row_at(app, content_row, mouse.column as usize),
            ) {
                app.selection = Some(Selection {
                    anchor,
                    cursor: end,
                });
                if app.mode == Mode::View {
                    app.view.cursor = end;
                    let viewport = app.view_viewport_rows();
                    app.view.keep_cursor_visible(viewport);
                } else {
                    app.cursor = end;
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.drag_anchor = None;
        }
        _ => {}
    }
}

/// 据え付けた Review の一覧の上のマウス。行を押せばその候補を選んで
/// 本文を送り（j/k と同じ）、同じ行の 2 度押しは `Enter`。ホイールは
/// 一覧のカーソルを動かす。題・出どころ・理由の行は押しても何もしない。
fn on_review_dock_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(idx) = crate::review_dock::entry_at(app, mouse.row) else {
                app.last_overlay_click = None;
                return;
            };
            let is_double = app
                .last_overlay_click
                .is_some_and(|(t, prev)| t.elapsed() < app.double_click_ms && prev == idx);
            app.last_overlay_click = Some((Instant::now(), idx));
            app.overlay_cursor = idx;
            keep_overlay_cursor_visible(app);
            crate::review_dock::follow(app);
            if is_double {
                activate_overlay_selection(app);
            }
        }
        MouseEventKind::ScrollDown => {
            on_overlay_key(app, KeyCode::Char('j'), KeyModifiers::NONE);
        }
        MouseEventKind::ScrollUp => {
            on_overlay_key(app, KeyCode::Char('k'), KeyModifiers::NONE);
        }
        _ => {}
    }
}

/// Source-mode wheel scroll: the viewport alone moves. The cursor keeps
/// its absolute file position — it may scroll off screen, and scrolling
/// back finds it exactly where it was (herdr-review style). Keyboard j/k,
/// c, and G still pull the viewport to the cursor
/// (`keep_cursor_visible`), and clicking moves the cursor to the clicked
/// line. A selection survives scrolling unchanged.
fn source_scroll(app: &mut App, dir: i32) {
    let viewport = app.source_viewport_rows();
    let max = app.max_offset(viewport as u16);
    let o = (app.offset as i32 + dir).clamp(0, max as i32) as usize;
    app.offset = o;
}

/// Map a content row (0-based, below title bar and frame border) back to
/// the source line under it, in the current mode.
fn source_row_at(app: &App, content_row: usize, col: usize) -> Option<usize> {
    match app.mode {
        Mode::View => {
            let display = app.view.offset + content_row;
            // The text column: the page's left margin, the frame's left
            // border, and the text column's left pad shift the text right
            // of the mouse column (the marker column rides the border
            // itself).
            let text_col = col.saturating_sub(1 + 1 + 1);
            app.view.line_at_position(display, text_col)
        }
        Mode::Source | Mode::Input => {
            let width = source_content_width(app) as usize;
            source_line_at(app, width, content_row)
        }
    }
}

/// Source-mode content width (mirrors `draw_source`'s computation;
/// source mode draws no frame, so the whole terminal width is content
/// except the scrollbar's track on the rightmost column).
pub(crate) fn source_content_width(app: &App) -> u16 {
    let (w, _) = crate::app::terminal_size();
    let gutter_cols = 1 + app.source.gutter_width as u16 + 1;
    w.saturating_sub(gutter_cols + 1)
}

/// View-mode paragraph width: the terminal minus the page's left margin,
/// the frame's borders, and the text column's 1-column pads on each side
/// (the marker column rides the frame's left border, reserving no width
/// of its own). Mirrors `draw_view`'s `content.width`.
pub(crate) fn view_content_width(_app: &App) -> usize {
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    (w.saturating_sub(5)) as usize
}

/// Source mode: which source line contains `display_row` (0-based content
/// row), walking the same wrap/card/composer layout as `build_rows`.
fn source_line_at(app: &App, width: usize, display_row: usize) -> Option<usize> {
    // Comment bars span the whole pane (gutter included), matching
    // `build_rows`'s full_width.
    let full_width = width + app.gutter_cols as usize;
    // Same card set as build_rows: current file, minus the edited one.
    let cards = visible_cards(app);
    let rows: Vec<usize> = (0..app.source.len())
        .map(|idx| {
            // Wrap rows only — `rows_of` folds the attached bars in, which
            // would double-count the cards below.
            let line_rows = app.base_rows.get(idx).copied().unwrap_or(1);
            // Inline deleted rows belong to their anchor line's band, so
            // a click on one selects the anchor line (same attribution as
            // the comment bars below).
            let (deleted_above, deleted_below) = app.deleted_blocks_at(idx);
            let deleted_rows: usize = deleted_above
                .iter()
                .chain(&deleted_below)
                .map(|block| crate::app::deleted_block_rows(&block.content, width))
                .sum();
            let card_rows: usize = cards
                .iter()
                .filter(|c| c.end as usize - 1 == idx)
                .map(|c| card_line_count(c, full_width))
                .sum();
            let composer_rows = if app.mode == Mode::Input && app.input_end == idx {
                composer_line_count(&app.input, app.input_cursor, full_width)
            } else {
                0
            };
            line_rows + deleted_rows + card_rows + composer_rows
        })
        .collect();
    index_at_abs(&rows, app.offset, app.offset + display_row)
}

/// Which index's row span (in absolute rows) contains `abs_row`, walking
/// the same wrap/card/composer layout as `build_rows`. The source line AND
/// its attached bars (comment cards, the composer) all map to this index,
/// so a click on a bar selects the line it belongs to instead of drifting
/// by the bar's height. Comparing in absolute rows keeps the mapping stable
/// regardless of the scroll offset (a viewport-relative `display_row` must
/// be converted by the caller: `abs_row = offset + display_row`).
fn index_at_abs(rows: &[usize], offset: usize, abs_row: usize) -> Option<usize> {
    let mut row = 0usize;
    for (idx, &total) in rows.iter().enumerate() {
        if row + total <= offset {
            row += total;
            continue;
        }
        if row > abs_row {
            break;
        }
        if abs_row < row + total {
            return Some(idx);
        }
        row += total;
    }
    None
}











/// The last display row of the rendered block source line `end` belongs
/// to: every row before the first row a LATER source line starts on. A
/// merged paragraph's member lines share one start, so the block runs to
/// the group's last wrapped row; a 1:1 line (an HTML block line, a code
/// line) keeps exactly its own rows. The mapping is exact, so the card
/// and the composer sit directly under the end line — the same position
/// source mode shows — never inside a shared row. (The old heuristic
/// walked forward to the next BLANK row, so a comment ending inside an
/// HTML block dropped its card below the whole block: the two modes
/// disagreed about where the comment sits.)
fn block_last_row(starts: &[usize], n_rows: usize, end: usize) -> usize {
    let s = starts.get(end).copied().unwrap_or(0);
    (end + 1..starts.len())
        .map(|i| starts[i])
        .find(|&n| n > s)
        .unwrap_or(n_rows)
        .saturating_sub(1)
}

/// The display row the view-mode composer (and the card after Enter) sits
/// below: the rendered block bottom of the selection's END line (`input_end`
/// = the range max, so an upward selection still lands at the bottom).
pub(crate) fn view_composer_anchor(app: &App) -> usize {
    block_last_row(&app.view.source_starts, app.view.rows.len(), app.input_end)
}

/// Render the view with the inline comment cards folded into the layout:
/// each comment's card is inserted right after its end line's rendered
/// block, and the source-line mapping for everything below shifts by the
/// card's height (so scroll, cursor follow, and the view↔comment handoff
/// all stay consistent with what is painted).
pub(crate) fn render_view_with_cards(
    source: &Source,
    columns: u16,
    highlighter: &Highlighter,
    comments: &[Comment],
    blend: crate::decoration::DecorationBlend,
) -> ViewState {
    let mut view = ViewState::render(source, columns, highlighter, blend);
    insert_cards(&mut view, comments, columns as usize);
    view
}

/// Fold comment cards into a rendered view: each comment's card is
/// inserted right after its end line's rendered block, and the
/// source-line mapping for everything below shifts by the card's height
/// (so scroll, cursor follow, and the view↔comment handoff all stay
/// consistent with what is painted).
fn insert_cards(view: &mut ViewState, comments: &[Comment], columns: usize) {
    let mut card_rows = vec![false; view.rows.len()];
    let mut order: Vec<&Comment> = comments.iter().collect();
    order.sort_by_key(|c| c.end);
    for c in order {
        let end = (c.end as usize)
            .saturating_sub(1)
            .min(view.source_starts.len().saturating_sub(1));
        // The card sits below the end line's whole rendered block (a merged
        // paragraph, a heading, a blank line) — never inside it, never
        // above it.
        let last_row = block_last_row(&view.source_starts, view.rows.len(), end);
        let insert_at = last_row + 1;
        let lines = comment_bar_lines(c, columns);
        // Shift the source-line mapping only for lines whose text starts
        // at or below the card's insert row. Lines merged with the
        // comment's END line keep their start row: their text renders
        // above the card (the paragraph shares the end line's row), so
        // shifting them would point the cursor/selection at the card
        // rows — the cursor marker and the selection highlight would
        // vanish (rendered as a plain card) and mouse hits would resolve
        // to the wrong line.
        for s in view.source_starts.iter_mut().skip(end + 1) {
            if *s >= insert_at {
                *s += lines.len();
            }
        }
        for (i, line) in lines.into_iter().enumerate() {
            let spans: Vec<HiSpan> = line
                .spans
                .into_iter()
                .map(|s| HiSpan {
                    text: s.content.to_string(),
                    style: s.style,
                })
                .collect();
            // The card row is synthesized: no source, so one `None`
            // attribution per span. `row_attrs` runs parallel to `rows`
            // down to the span, and the range decoration reads it —
            // skipping this insert shifts every attribution below the
            // card down one row, silently.
            let card_attrs = vec![None; spans.len()];
            view.rows.insert(insert_at + i, spans);
            card_rows.insert(insert_at + i, true);
            view.row_segments.insert(insert_at + i, Vec::new());
            view.row_attrs.insert(insert_at + i, card_attrs);
        }
    }
    view.card_rows = card_rows;
}

/// Render the current complete document with its inline comment cards.
pub(crate) fn render_current_view(app: &App, comments: &[Comment]) -> ViewState {
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let width = view_render_width(w);
    render_view_with_cards(&app.source, width, &app.highlight, comments, app.config.decoration_blend)
}







/// `]`/`[` file switching — the chord's default action, shared by the
/// immediate resolution paths and the chord timeout.
fn switch_file(app: &mut App, bracket: char) {
    let n = app.files.len();
    if n == 0 {
        return;
    }
    let next = if bracket == ']' {
        (app.current_file_index + 1).min(n - 1)
    } else {
        app.current_file_index.saturating_sub(1)
    };
    app.switch_to_file(next);
}



/// Expire a pending `]`/`[` chord into its default file switch. Called
/// every frame by the event loop; exposed so tests drive the same path.
fn expire_pending_chord(app: &mut App) {
    if let Some((at, bracket)) = app.pending_chord
        && at.elapsed() >= CHORD_MS
    {
        app.pending_chord = None;
        switch_file(app, bracket);
    }
}

/// Re-render the view after a comment was added/deleted (or a reload), and
/// keep the cursor line at the same screen row it was on — inserting a card
/// shifts the rows below, so without this the cursor jumps.
pub(crate) fn replace_view_preserving_cursor(app: &mut App) {
    // The view is being rebuilt: the scatter effects' view-relative rows
    // would map to the wrong cells now, so they drop (their own timers
    // would have expired within ~650 ms anyway).
    app.appear_fx.clear();
    app.ghost_fx.clear();
    // A comment op aborts a transition that was still in flight: the
    // screen will not settle into the new generation on its own, so the
    // landing pulse that marks that settlement is dropped too.
    app.landing_pulse_pending = false;
    rebuild_view_preserving_cursor(app);
}

/// The shared half of [`replace_view_preserving_cursor`]: re-render the
/// current document and keep the cursor line on its screen row. The
/// history-ghost collapse uses this directly — it rebuilds the SAME
/// ghost-free layout the parked appear effects were built against, so
/// their rows stay valid and the reveal survives to stream in right
/// after the collapse.
fn rebuild_view_preserving_cursor(app: &mut App) {
    let line = app.view.cursor;
    let screen = app.view.cursor_row() as isize - app.view.offset as isize;
    // Current file's cards, minus the one being re-edited (hidden under
    // the edit composer).
    let file_comments: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
    let mut view = render_current_view(app, &file_comments);
    view.goto_source_line(line);
    let target = view.cursor_row() as isize - screen;
    let viewport = app.view_viewport_rows();
    let max_off = view.rows.len().saturating_sub(viewport) as isize;
    view.offset = target.clamp(0, max_off.max(0)) as usize;
    app.view = view;
}

fn history_key_direction(
    app: &App,
    key: KeyCode,
    modifiers: KeyModifiers,
) -> Option<isize> {
    if app.overlay.is_some()
        || !matches!(app.mode, Mode::View | Mode::Source)
        || !modifiers.is_empty()
    {
        return None;
    }
    match key {
        KeyCode::Left => Some(1),
        KeyCode::Right => Some(-1),
        _ => None,
    }
}

/// Recompute the active file's one, cumulative change mark set. Git and the
/// currently displayed historical generation do not affect this baseline.
pub(crate) fn refresh_baseline_marks(app: &mut App) {
    let index = app.current_file_index;
    let baseline = app
        .histories
        .get(index)
        .and_then(|history| history.baseline_content.clone());
    let Some(baseline) = baseline else {
        app.baseline_changed.clear();
        app.baseline_deleted_before.clear();
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
        app.comparison_deleted_blocks.clear();
        return;
    };
    let (changed, deleted) = history::baseline_transition(
        supports_view(app.current_file_path()),
        &baseline,
        &app.histories[index].revisions[0].content,
    );
    app.baseline_changed = changed;
    app.baseline_deleted_before = deleted;
    refresh_comparison_marks(app);
}

/// Recompute baseline-relative marks for the complete document currently
/// rendered on screen. This is distinct from the durable NOW change set:
/// historical generations can be inspected without changing what remains
/// unseen in the working document.
fn refresh_comparison_marks(app: &mut App) {
    let Some(baseline) = app
        .history()
        .and_then(|history| history.baseline_content.as_deref())
    else {
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
        app.comparison_deleted_blocks.clear();
        app.focused_deletion = None;
        return;
    };
    let (changed, deleted, blocks) =
        history::comparison_transition(baseline, &app.source.content);
    app.comparison_changed = changed;
    app.comparison_deleted_before = deleted;
    app.comparison_deleted_blocks = blocks;
    // The blocks were replaced: a stale focus could light rows that mean
    // something else now.
    app.focused_deletion = None;
}

/// Move only the lightweight history cursor. The rendered Markdown remains
/// untouched until input settles, so holding an arrow can scan dozens of
/// revisions without paying the renderer cost for intermediate choices.
pub(crate) fn select_history(app: &mut App, delta: isize) -> bool {
    if app.config.reply {
        app.flash_err("history unavailable in reply mode");
        return false;
    }
    let index = app.current_file_index;
    let was_browsing = app
        .histories
        .get(index)
        .is_some_and(|history| history.position > 0);
    let moved = app
        .histories
        .get_mut(index)
        .is_some_and(|history| history.move_by(delta));
    if !moved {
        let Some(history) = app.histories.get(index) else {
            return false;
        };
        // The lightweight cursor can reach an edge before its Markdown has
        // rendered. Keep the scrubber tooltip alive while the debounce
        // catches up; reporting a boundary error here would hide the
        // destination throughout a held-arrow scrub.
        if history.rendered_position != history.position {
            refresh_timeline_tooltip(app);
            return false;
        }
        let message = if delta > 0 {
            "oldest document version"
        } else {
            "already at the present"
        };
        app.flash_err(message);
        return false;
    }

    // The timeline bar slides in on the first step into the past and
    // slides out when the cursor returns to NOW (fx only; without fx
    // the bar simply appears and disappears).
    let browsing = app.histories[index].position > 0;
    if browsing && !was_browsing && app.config.fx {
        app.timeline_exit_until = None;
        app.timeline_fx = Some(crate::effects::timeline_slide_in());
    }
    if !browsing && was_browsing && app.config.fx {
        // The linger window is injectable: the default is the slide
        // duration, tests widen it to remove the wall-clock race.
        app.timeline_exit_until = Some(Instant::now() + app.timeline_exit_ms);
        app.timeline_fx = Some(crate::effects::timeline_slide_out());
    }
    // While the bar covers the bottom rows, keep the cursor above them
    // (a cursor parked on the last line would hide under the times row).
    keep_cursor_out_of_timeline(app);

    app.history_render_due = Some(Instant::now() + HISTORY_RENDER_DEBOUNCE);
    // A previous landing pulse must not bleed into the first frame of the
    // next selected document.
    app.landing_pulse_fx = None;
    app.landing_pulse_until = None;
    app.landing_pulse_pending = false;
    refresh_timeline_tooltip(app);
    true
}

/// Arm (or re-arm) the scrubber tooltip: every history step shows the
/// revision readout (provenance · id · age · summary) above the axis
/// for a short hold, then it dissolves. This replaced the old per-step
/// toast — the same information, but next to the axis where the eye
/// already is, and gone once the traveler settles. A step also
/// retires any lingering toast (an old edge error must not squat on the
/// tooltip's row).
fn refresh_timeline_tooltip(app: &mut App) {
    // The deadline covers the hold plus — with fx on — the exit
    // dissolve the drawer plays over the final stretch; without fx the
    // band simply vanishes when the hold ends.
    let total = crate::effects::TOOLTIP_HOLD_MS
        + if app.config.fx {
            crate::effects::TOOLTIP_DISSOLVE_MS
        } else {
            0
        };
    app.timeline_tooltip_until = Some(Instant::now() + Duration::from_millis(total as u64));
    app.status = None;
    app.toast_fx = None;
}

/// While the timeline bar is on screen, nudge the scroll so the cursor
/// does not sit under the covered rows: the times row takes the last
/// content row on wide terminals, and source mode's frameless body
/// loses one more row to the axis. No-op at NOW or on narrow terminals
/// (no bar).
fn keep_cursor_out_of_timeline(app: &mut App) {
    if !app.is_historical() {
        return;
    }
    let width = ratatui::crossterm::terminal::size()
        .map(|s| s.0 as usize)
        .unwrap_or(80);
    // The bar is always two rows: the words row replaces the footer,
    // and the axis replaces the view frame's bottom border — so view
    // mode loses no content row. Source mode has no frame, so the axis
    // covers the last content row.
    let covered = if width >= 60 && app.mode == Mode::Source {
        1
    } else {
        0
    };
    if covered == 0 {
        return;
    }
    let viewport = app.source_viewport_rows().saturating_sub(covered);
    app.keep_cursor_visible(viewport as u16);
}

/// A beat of stillness after the ghost collapse before the add phase
/// streams in: the folded layout needs a moment to register before the
/// new text types over it (delete phase → add phase handoff).
const GHOST_SETTLE_MS: u32 = 60;

/// Render the final revision selected by [`select_history`]. This is the
/// only expensive half of time travel and runs once after the arrow stops.
pub(crate) fn render_pending_history(app: &mut App, animate: bool) -> bool {
    // While an overlay owns the screen, the document behind it must not
    // re-render: the timeline overlay's list IS the timeline, and a
    // render here would stall every keypress inside it (a full render
    // pipeline on a 50 KB document costs ~100 ms in a debug build). The
    // due flag stays set, so the render runs once when the overlay
    // closes.
    if app.overlay.is_some() {
        return false;
    }
    if app.history_render_due.take().is_none() {
        return false;
    }
    let index = app.current_file_index;
    let Some((position, content)) = app
        .histories
        .get(index)
        .and_then(|history| {
            history
                .current()
                .map(|revision| (history.position, revision.content.clone()))
        })
    else {
        return false;
    };
    // Which way this travel went, judged against the last RENDERED
    // position (higher index = older): the warp flies inward going
    // deeper into the past, outward coming back toward NOW.
    let old_rendered = app
        .histories
        .get(index)
        .map_or(position, |history| history.rendered_position);
    if let Some(history) = app.histories.get_mut(index) {
        history.rendered_position = position;
    }
    if app.source.content == content {
        refresh_comparison_marks(app);
        app.landing_pulse_pending = true;
        return true;
    }

    let old_lines = app.source.lines.clone();
    let source_mode = app.mode == Mode::Source;
    let old_cursor = if source_mode { app.cursor } else { app.view.cursor };
    let screen_row = if source_mode {
        let row = if app.line_rows.is_empty() {
            old_cursor
        } else {
            app.row_of(old_cursor)
        };
        row as isize - app.offset as isize
    } else {
        app.view.cursor_row() as isize - app.view.offset as isize
    };
    let path = app.current_file_path().to_path_buf();
    let new_source = Source::from_content(path.clone(), content);
    let anchor = history::anchored_line(&old_lines, &new_source.lines, old_cursor);
    let (changed_lines, removed_lines) =
        line_level_transition(&old_lines, &new_source.lines);
    let deleted_blocks = ghost_blocks(&old_lines, &new_source.lines, &changed_lines, removed_lines);
    app.source = new_source;
    // The view is about to be rebuilt: scatter effects captured against
    // the old view's rows would map to the wrong cells now (a revision
    // without deletions leaves the previous ghost list stale — its
    // backspace pacing was captured against the old rect, and mapping
    // it onto the new view underflows), so they drop here and are
    // rebuilt below when the new view actually has ghosts.
    app.appear_fx.clear();
    app.ghost_fx.clear();
    refresh_comparison_marks(app);
    app.history_ghost_until = None;
    app.spans = app
        .highlight
        .highlight_with(&app.source.content, syntax_for(&path));
    app.selection = None;
    app.cursor = anchor;
    app.base_rows.clear();
    app.line_rows.clear();
    // A different generation of the document is on screen: the previous
    // annotation described other byte positions. Re-ask the provider
    // (already debounced — this runs once input settles, not per arrow
    // press). A fixture pinned to NOW refuses the past and the layer
    // goes quiet, which is the honest answer.
    app.reanalyze_semantics();

    let file_comments: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
    let mut view = render_current_view(app, &file_comments);
    // The delete phase: removed lines ghost out (backspace order) and the
    // layout folds shut. The scatter-in effects for the new text are built
    // BEFORE the ghosts are inserted — against the GHOST-FREE layout, so
    // their rows are the FINAL ones (ghost rows are transient and push
    // content down; masks computed against the ghost-included view would
    // map to displaced rows and jump when the collapse pulls them back
    // up). The reveal is parked behind the delete phase: the new text
    // streams in only after the ghosts have backspaced away and the
    // collapse has settled — delete first, then add, so nothing moves
    // after appearing.
    let delete_phase = animate && !source_mode && !deleted_blocks.is_empty();
    let appear_delay = if delete_phase {
        crate::effects::GHOST_PHASE_MS + GHOST_SETTLE_MS
    } else {
        0
    };
    let appear_fx = if app.config.fx && !source_mode {
        appear_effects(
            &view,
            &changed_lines,
            &old_lines,
            &app.highlight,
            &path,
            appear_delay,
        )
    } else {
        Vec::new()
    };
    if delete_phase {
        let ghost_rows = insert_history_ghosts(
            &mut view,
            &deleted_blocks,
            &app.highlight,
            &path,
            app.ui_history_glow_bg,
        );
        // The scatter-out effects ride the ghost rows (view-relative);
        // they complete exactly when the ghosts expire below, and the
        // collapse hands off to the parked appear effects above.
        app.history_ghost_until = Some(
            Instant::now() + Duration::from_millis(crate::effects::GHOST_PHASE_MS as u64),
        );
        if app.config.fx {
            app.ghost_fx = ghost_rows
                .into_iter()
                .map(|(row, height)| (row, height, crate::effects::ghost_effect()))
                .collect();
        }
    }
    view.goto_source_line(anchor);
    if source_mode {
        // Source and rendered view wrap differently. Rebuild the source row
        // cache first, then preserve the anchored line's physical screen row.
        app.view = view;
        let width = source_content_width(app);
        app.ensure_row_cache(width);
        app.refresh_line_rows();
        let target = app.row_of(anchor) as isize - screen_row;
        let max_offset = app.max_offset(app.source_viewport_rows() as u16) as isize;
        app.offset = target.clamp(0, max_offset.max(0)) as usize;
    } else {
        let target = view.cursor_row() as isize - screen_row;
        let max_offset = view.rows.len().saturating_sub(app.view_viewport_rows()) as isize;
        view.offset = target.clamp(0, max_offset.max(0)) as usize;
        app.view = view;
    }
    // Scatter-in effects for the blocks that appeared in this revision
    // (view mode, `--fx` on): each changed block's mask covers only the
    // characters that are genuinely new (diffed against the old
    // revision's rendered text), built above against the ghost-free
    // layout and parked behind the delete phase; the new characters
    // cascade in reading order once the ghosts have collapsed.
    if app.config.fx && !source_mode {
        app.appear_fx = appear_fx;
        // The generation warp: window outlines fly through the frame in
        // the direction of travel. Only when the position actually
        // moved — a same-position re-render is not a journey.
        if animate && position != old_rendered {
            app.warp_fx = Some(crate::effects::warp_effect(
                position > old_rendered,
                app.ui_light,
            ));
        }
    }
    // The anchor preserved the cursor's place, but the new document's
    // rows may land it under the timeline bar again; nudge the scroll
    // back above the covered rows (no-op at NOW).
    keep_cursor_out_of_timeline(app);
    app.landing_pulse_pending = true;
    // The render above is the expensive half of time travel (~100 ms on
    // a large document in a debug build). The effects created in it
    // (warp, appear, ghosts) are advanced by the wall-clock delta since
    // the LAST draw — without this reset their first frame would be
    // charged for the render itself and skip most of the flight.
    app.last_draw = Some(Instant::now());
    true
}

/// The changed blocks' scatter-in effects: contiguous runs of changed
/// source lines become one block; its mask is the display-column ranges
/// of the characters that are new relative to the old revision's
/// rendered text (see [`appear_mask`]). Each effect is parked behind
/// `base_delay_ms` (the delete phase — ghost backspace + collapse —
/// when one runs; 0 for a transition with nothing to delete) plus a
/// per-block stagger of 60 ms that cascades later blocks.
fn appear_effects(
    view: &ViewState,
    changed: &HashSet<usize>,
    old_lines: &[String],
    highlighter: &Highlighter,
    path: &Path,
    base_delay_ms: u32,
) -> Vec<(usize, usize, tachyonfx::Effect)> {
    let mut lines: Vec<usize> = changed.iter().copied().collect();
    lines.sort_unstable();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let start = lines[i];
        let mut end = start;
        while i + 1 < lines.len() && lines[i + 1] == end + 1 {
            end += 1;
            i += 1;
        }
        i += 1;
        let row0 = view.source_starts.get(start).copied().unwrap_or(0);
        let row1 = view
            .source_starts
            .get(end + 1)
            .copied()
            .unwrap_or(view.rows.len());
        let height = row1.saturating_sub(row0).max(1);
        // The old block's rendered rows: the mask compares rendered text
        // against rendered text, so markup is stripped on both sides and
        // the inserted ranges are display columns directly.
        let old_block: String = old_lines
            .get(start..=end.min(old_lines.len().saturating_sub(1)))
            .map(|lines| lines.join("\n"))
            .unwrap_or_default();
        let old_rows = if old_block.trim().is_empty() {
            Vec::new()
        } else {
            let old_source = Source::from_content(path.to_path_buf(), old_block);
            ViewState::render(&old_source, view.width.max(1) as u16, highlighter, Default::default()).rows
        };
        let mask = appear_mask(view, row0, height, &old_rows);
        out.push((
            row0,
            height,
            crate::effects::appear_effect(mask, base_delay_ms + out.len() as u32 * 60),
        ));
    }
    out
}

/// The display-column mask of a changed block's new characters: the
/// old and new blocks are diffed as WHOLE texts (newlines join the
/// rows), so a wrap that shifted because of the edit never misaligns the
/// comparison — the LCS anchors on the common characters wherever the
/// line breaks fell. Inserted char ranges map back to display rows and
/// columns; only those cells materialize.
fn appear_mask(
    view: &ViewState,
    row0: usize,
    height: usize,
    old_rows: &[Vec<crate::highlight::Span>],
) -> Vec<Vec<(u16, u16)>> {
    let new_rows: Vec<String> = (0..height)
        .map(|i| {
            view.rows[row0 + i]
                .iter()
                .map(|s| s.text.as_str())
                .collect()
        })
        .collect();
    let new_block = new_rows.join("\n");
    let old_block: String = old_rows
        .iter()
        .map(|row| row.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    if old_block.is_empty() {
        // Nothing to compare: every row is new, whole-row masks.
        return new_rows
            .iter()
            .map(|t| vec![(0, unicode_width::UnicodeWidthStr::width(t.as_str()) as u16)])
            .collect();
    }
    // Char offsets where each new row starts (the newline is the row
    // separator; a char position at a boundary belongs to the row ABOVE
    // and is clamped to that row's width).
    let mut row_starts: Vec<usize> = Vec::with_capacity(new_rows.len() + 1);
    let mut off = 0usize;
    for t in &new_rows {
        row_starts.push(off);
        off += t.chars().count() + 1; // +1 for the joining '\n'
    }
    row_starts.push(off);
    let mut mask: Vec<Vec<(u16, u16)>> = vec![Vec::new(); height];
    for (s, e) in inserted_char_ranges(&old_block, &new_block) {
        if s >= e {
            continue;
        }
        // The rows the insertion spans: start at the row containing `s`,
        // end at the row containing the char before `e`.
        let mut row_s = row_starts.partition_point(|&p| p <= s).saturating_sub(1);
        let mut row_e = row_starts
            .partition_point(|&p| p < e)
            .saturating_sub(1);
        row_s = row_s.min(height - 1);
        row_e = row_e.min(height - 1);
        for r in row_s..=row_e {
            let text = &new_rows[r];
            let start = row_starts[r];
            let end = row_starts[r] + text.chars().count(); // the row's own text
            let col_of = |idx: usize| -> u16 {
                text.chars()
                    .take(idx.saturating_sub(start).min(text.chars().count()))
                    .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1) as u16)
                    .sum()
            };
            let cs = if r == row_s {
                col_of(s.min(end))
            } else {
                0
            };
            let ce = if r == row_e {
                col_of(e.min(end))
            } else {
                unicode_width::UnicodeWidthStr::width(text.as_str()) as u16
            };
            if cs < ce {
                mask[r].push((cs, ce));
            }
        }
    }
    // The old-text lookup is by line INDEX; when the block was inserted
    // (not rewritten in place) that index lands on unrelated text, and
    // the diff can "match" stray characters inside the new text (e.g. an
    // old "end" matching the "e d" of a new paragraph). A block whose
    // mask covers most of its text has no trustworthy alignment: mask
    // the whole block, which is the truth anyway (it reads as one
    // stream).
    let covered: usize = mask.iter().flatten().map(|&(s, e)| (e - s) as usize).sum();
    let total_new: usize = new_rows
        .iter()
        .map(|t| unicode_width::UnicodeWidthStr::width(t.as_str()))
        .sum();
    if total_new > 0 && covered * 4 > total_new * 3 {
        return new_rows
            .iter()
            .map(|t| vec![(0, unicode_width::UnicodeWidthStr::width(t.as_str()) as u16)])
            .collect();
    }
    mask
}

/// The char-index ranges of `new`'s characters that have no counterpart
/// in `old` (a diff backtrace: matched characters advance both, old-only
/// characters are deletions to skip, new-only characters form the
/// inserted runs). Operates on whole texts (newlines included), so
/// re-wrapped text stays aligned.
///
/// The alignment runs under [`crate::history::DIFF_DEADLINE`]: the old
/// hand-rolled LCS DP was quadratic in BOTH memory and time, and a
/// multi-KB table renders to ~15K chars per side — 225M cells, a
/// gigabyte of matrix, minutes in a debug build. On expiry similar
/// hands back its best alignment so far, so a deadline-truncated mask
/// simply animates a few extra or missing cells instead of stalling the
/// render.
fn inserted_char_ranges(old: &str, new: &str) -> Vec<(usize, usize)> {
    if old.is_empty() {
        return vec![(0, new.chars().count())];
    }
    let diff = TextDiffConfig::default()
        .timeout(crate::history::DIFF_DEADLINE)
        .diff_chars(old, new);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for op in diff.ops() {
        match op.tag() {
            // Equal advances both, Delete advances only the old text;
            // Insert (and the new side of a Replace) is what the
            // appear effect must reveal.
            DiffTag::Insert | DiffTag::Replace => {
                let range = op.new_range();
                if !range.is_empty() {
                    ranges.push((range.start, range.end));
                }
            }
            DiffTag::Equal | DiffTag::Delete => {}
        }
    }
    ranges.into_iter().filter(|(s, e)| s < e).collect()
}

/// The characters of `old` that have no counterpart in `new` (in
/// reading order) plus the removed fraction (0.0 = nothing removed,
/// 1.0 = everything). The LCS backtrace collects old-only characters —
/// the deletions a rewrite's ghost should backspace away. A near-total
/// replacement usually means the line-index alignment misfired (an
/// inserted block), not a real removal; the caller guards on the
/// fraction.
/// The per-line transition for the ANIMATIONS: the new lines with no
/// equal-content match (they stream in) and the old lines with no match
/// (their text ghosts out). The change marks keep the block-level
/// semantic marking (a whole rendered block lights), but the animations
/// must work at line granularity — a one-cell edit in a large table
/// would otherwise mark the whole table as one block, ballooning the
/// char diffs and hitting the LCS budget. The DP is over LINES (content
/// equality), so it stays cheap; pathological documents fall back to no
/// animation rather than a hang.
///
/// Each removed line carries its backtrace anchor — the new-document
/// index the removal collapsed to (the `j` at the moment the removal is
/// taken). The ghost must render at that position, not at the same
/// numeric index: insertions and earlier deletions shift the new
/// document, and `b + 1` in OLD-line arithmetic was landing
/// mid-document ghosts at the bottom of the new render.
fn line_level_transition(
    old: &[String],
    new: &[String],
) -> (HashSet<usize>, Vec<(usize, usize)>) {
    let o = old.len();
    let n = new.len();
    if o.saturating_mul(n) > LCS_CELL_BUDGET {
        return (HashSet::new(), Vec::new());
    }
    let mut dp = vec![vec![0usize; n + 1]; o + 1];
    for i in (0..o).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut changed = HashSet::new();
    let mut removed: Vec<(usize, usize)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < o && j < n {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            removed.push((i, j));
            i += 1;
        } else {
            changed.insert(j);
            j += 1;
        }
    }
    while i < o {
        removed.push((i, j));
        i += 1;
    }
    while j < n {
        changed.insert(j);
        j += 1;
    }
    (changed, removed)
}

/// The LCS work budget for the diff helpers: beyond this many DP cells
/// the block is too large to diff precisely (a whole-document change
/// would otherwise hang the render). Callers get the safe degraded
/// answer: everything removed / everything new.
const LCS_CELL_BUDGET: usize = 2_000_000;

fn removed_text(old: &str, new: &str) -> (String, f32) {
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    if o.is_empty() {
        return (String::new(), 0.0);
    }
    if n.is_empty() {
        return (old.to_string(), 1.0);
    }
    if o.len().saturating_mul(n.len()) > LCS_CELL_BUDGET {
        // Too large to diff: report everything removed. The rewrite-ghost
        // guard (frac < 0.75) then skips the ghost, so huge blocks fall
        // back to the instant transition instead of hanging the app.
        return (old.to_string(), 1.0);
    }
    let mut dp = vec![vec![0usize; n.len() + 1]; o.len() + 1];
    for i in (0..o.len()).rev() {
        for j in (0..n.len()).rev() {
            dp[i][j] = if o[i] == n[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let mut removed: Vec<char> = Vec::new();
    while i < o.len() && j < n.len() {
        if o[i] == n[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            removed.push(o[i]); // an old-only char: removed
            i += 1;
        } else {
            j += 1; // a new-only char: insertion, not a removal
        }
    }
    removed.extend_from_slice(&o[i..]); // trailing old-only chars
    let frac = removed.len() as f32 / o.len() as f32;
    (removed.into_iter().collect(), frac)
}

/// The display-column ranges of `new`'s characters that have no
/// counterpart in `old` — the per-row view used by the tests (see
/// [`inserted_char_ranges`] for the whole-text diff underneath).
#[cfg(test)]
fn inserted_columns(old: &str, new: &str) -> Vec<(u16, u16)> {
    use unicode_width::UnicodeWidthChar;
    let col_of = |idx: usize| -> u16 {
        new.chars()
            .take(idx)
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(1) as u16)
            .sum()
    };
    inserted_char_ranges(old, new)
        .into_iter()
        .map(|(s, e)| (col_of(s), col_of(e)))
        .collect()
}

/// Arm the landing pulse only after the transition has actually
/// settled: the render itself is instant, but the warp, the ghost
/// backspace, and the streaming reveal keep the screen moving for up
/// to ~1.1 s after it. The pulse marks the END of that motion — the
/// new document fully in place — not the render itself. It rides on
/// top of the rotating border (the wave never pauses) and lasts
/// [`LANDING_PULSE_MS`]; source mode (no frame to flare) uses the same
/// window to brighten its gutter instead. Instant transitions
/// (`--no-fx`, source mode, a same-content re-render) arm no effects,
/// so the check passes on the first paint and the pulse fires as
/// before.
fn begin_landing_pulse_after_draw(app: &mut App) {
    if !app.landing_pulse_pending {
        return;
    }
    // The draw pass above retained only unfinished effects, so whatever
    // is still queued here is genuinely moving the screen.
    let transition_done =
        app.warp_fx.is_none() && app.appear_fx.is_empty() && app.ghost_fx.is_empty();
    if transition_done {
        app.landing_pulse_pending = false;
        if app.config.fx && app.view_active() {
            app.landing_pulse_fx =
                Some(crate::effects::landing_pulse_effect(app.ui_landing_pulse));
        }
        app.landing_pulse_until =
            Some(Instant::now() + Duration::from_millis(crate::effects::LANDING_PULSE_MS as u64));
    }
}

fn render_history_when_settled(app: &mut App) {
    if app
        .history_render_due
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        render_pending_history(app, true);
    }
}

/// The ghost blocks for a history transition: contiguous runs of removed
/// lines become one [`history::DeletedBlock`] whose anchor is the
/// backtrace position the removal collapsed to in the NEW document. A
/// single removed line whose anchor holds a changed line is a rewrite
/// IN PLACE — only the removed characters ghost, BELOW the rewritten
/// line (the unchanged prefix never flickers, and the layout collapses
/// back by exactly the ghost row). The removal fraction guard (<75%)
/// keeps misaligned indexes from ghosting; fully removed lines ghost
/// whole. A full deletion ghosts AT the anchor — the position where the
/// lines used to be — so the collapse pulls the following text up to
/// where the ghost is dissolving.
fn ghost_blocks(
    old_lines: &[String],
    new_lines: &[String],
    changed_lines: &HashSet<usize>,
    removed_lines: Vec<(usize, usize)>,
) -> Vec<history::DeletedBlock> {
    let mut removed_sorted = removed_lines;
    removed_sorted.sort_unstable();
    let mut blocks = Vec::new();
    let mut i = 0usize;
    while i < removed_sorted.len() {
        let (a, anchor) = removed_sorted[i];
        let mut b = a;
        while i + 1 < removed_sorted.len() && removed_sorted[i + 1].0 == b + 1 {
            b += 1;
            i += 1;
        }
        i += 1;
        let old_line = &old_lines[a];
        // A rewrite IN PLACE (the removed line's anchor holds a changed
        // line): ghost only the removed characters. A removed line whose
        // anchor now holds an unrelated line (e.g. a blank where the
        // line used to be) ghosts whole — pairing it would trip the
        // fraction guard and skip the backspace.
        let rewrite = b == a && changed_lines.contains(&anchor);
        let content = match new_lines.get(anchor) {
            Some(new_line) if rewrite => {
                let (removed, frac) = removed_text(old_line, new_line);
                if removed.trim().is_empty() || frac >= 0.75 {
                    continue;
                }
                removed
            }
            _ => old_lines[a..=b].join("\n"),
        };
        blocks.push(history::DeletedBlock {
            anchor: if rewrite {
                (anchor + 1).min(new_lines.len())
            } else {
                anchor.min(new_lines.len())
            },
            content,
            ..Default::default()
        });
    }
    blocks
}

/// Insert deleted Markdown blocks into the new render as dim, non-interactive
/// rows. They remain rendered Markdown, then [`expire_history_ghosts`]
/// rebuilds the current document without them to produce the collapse.
fn insert_history_ghosts(
    view: &mut ViewState,
    deleted: &[history::DeletedBlock],
    highlighter: &Highlighter,
    path: &Path,
    ghost_bg: Color,
) -> Vec<(usize, usize)> {
    // (insert row, row count) per ghost, in processing order (descending
    // anchors). Blocks inserted LATER sit ABOVE and shift earlier rows
    // down; the caller-facing rects are fixed up at the end.
    let mut rects: Vec<(usize, usize)> = Vec::new();
    let mut ordered = deleted.to_vec();
    ordered.sort_by_key(|block| std::cmp::Reverse(block.anchor));
    for block in ordered {
        let source = Source::from_content(path.to_path_buf(), block.content);
        let ghost = ViewState::render(&source, view.width.max(1) as u16, highlighter, Default::default());
        if ghost.rows.is_empty() {
            continue;
        }
        let insert_at = if block.anchor >= view.source_starts.len() {
            view.rows.len()
        } else {
            view.source_starts[block.anchor]
        };
        let count = ghost.rows.len();
        rects.push((insert_at, count));
        for start in &mut view.source_starts {
            if *start >= insert_at {
                *start += count;
            }
        }
        for (offset, mut row) in ghost.rows.into_iter().enumerate() {
            for span in &mut row {
                // Ghost styling: the original color dimmed 55% toward the
                // neutral ghost gray — readable, but clearly "not there
                // anymore". NO BACKGROUND (the deletion band was dropped
                // from the spec) and NO DIM/ITALIC: terminals render them
                // muddy, and synthetic italics smear CJK glyphs.
                let fg = match span.style.fg {
                    Some(c @ Color::Rgb(..)) => lerp_color(c, ghost_bg, 0.55),
                    _ => Color::Gray,
                };
                span.style = span.style.fg(fg);
            }
            // Ghost rows are synthesized too: one `None` attribution per
            // span, parallel to `rows` (see `insert_cards`).
            let ghost_attrs = vec![None; row.len()];
            view.rows.insert(insert_at + offset, row);
            view.row_segments.insert(insert_at + offset, Vec::new());
            view.row_attrs.insert(insert_at + offset, ghost_attrs);
            view.card_rows.insert(insert_at + offset, true);
        }
    }
    // Later (smaller-anchor) ghosts are inserted above and push this
    // ghost down by their row counts.
    let mut shift = 0usize;
    for (row, count) in rects.iter_mut().rev() {
        *row += shift;
        shift += *count;
    }
    rects
}

fn expire_history_ghosts(app: &mut App) {
    // The composer owns the screen while Input is open: collapsing the
    // ghosts rebuilds the view under the comment bar and jumps the
    // terminal cursor. The expiry stays armed (the ghost rows keep
    // their place) until Enter or Esc returns to the view.
    if app.mode == Mode::Input {
        return;
    }
    if app
        .history_ghost_until
        .is_some_and(|until| Instant::now() >= until)
    {
        app.history_ghost_until = None;
        // The ghost rows come out of the view (the collapse). The parked
        // appear effects SURVIVE this rebuild: they were built against
        // exactly this ghost-free layout, so their rows are still valid
        // and the add phase streams in right after (see
        // [`render_pending_history`]).
        app.ghost_fx.clear();
        rebuild_view_preserving_cursor(app);
    }
}

/// View → comment handoff. The view cursor is already a source line; a
/// merged paragraph row can cover several source lines at once, so without
/// a selection we land on the head of the row (what a user means by "this
/// line"). With a view-mode selection active, its exact line range is
/// pinned first and carried over unchanged; the comment cursor sits on its
/// extent, so j/k keep extending from where the user left off.
fn enter_source_mode(app: &mut App) {
    // Both directions hand the exact cursor line over (the line-based
    // navigation makes the view cursor meaningful line-by-line), so a
    // Tab round trip never shifts the line.
    if let Some(sel) = app.selection {
        app.cursor = sel.cursor;
    } else {
        app.cursor = app.view.cursor;
    }
    // A view-mode `n` that landed on a pure deletion carried its focus
    // here (the view keeps a one-line selection for the red `▌` mark). Drop
    // that selection so the focus goes live: the promised "Tab: inspect
    // & comment" arrives with the deleted rows focused, not with the
    // untouched anchor line selected.
    if app.focused_deletion == Some(app.cursor)
        && app
            .selection
            .is_some_and(|sel| sel.range() == (app.cursor, app.cursor))
    {
        app.selection = None;
    }
    app.mode = Mode::Source;
    // The row cache is built during source-mode drawing; build it now so
    // keep_cursor_visible can walk it (line_rows starts empty, and row_of
    // would panic slicing past its end).
    let width = source_content_width(app);
    app.ensure_row_cache(width);
    app.refresh_line_rows();
    // Keep the cursor line's PHYSICAL screen row where it was in view mode:
    // the two modes wrap at different widths, so without this the
    // highlighted line jumps around on every toggle and the eye loses it.
    // View content sits one row lower (its frame's top border); the +1
    // keeps the terminal row identical across the toggle. Clamps at the
    // document edges (an allowed exception).
    let screen = app.view.cursor_row() as isize - app.view.offset as isize + 1;
    let target = app.row_of(app.cursor) as isize - screen;
    let max_off = app.max_offset(app.source_viewport_rows() as u16) as isize;
    app.offset = target.clamp(0, max_off.max(0)) as usize;
    // Every mode switch forces ASCII so j/k navigation is never swallowed
    // by the IME. No-op when --ime off or the helper hasn't been built yet.
    let _ = app.ime_session.force_ascii();
}

/// Comment → view handoff: land on the exact same source line, mirroring
/// [`enter_source_mode`]'s screen-row preservation so toggling never moves
/// the highlighted line. View is the home mode, so this is where Esc in
/// source mode lands too.
fn enter_view_mode(app: &mut App) {
    // The -1 mirrors enter_source_mode's +1: view content sits one row
    // lower (its frame's top border), so the cursor line keeps its
    // PHYSICAL terminal row across the toggle.
    let screen = if app.line_rows.is_empty() {
        0
    } else {
        app.row_of(app.cursor) as isize - app.offset as isize - 1
    };
    app.view.goto_source_line(app.cursor);
    let target = app.view.cursor_row() as isize - screen;
    let max_off = app.view.rows.len().saturating_sub(app.view_viewport_rows()) as isize;
    app.view.offset = target.clamp(0, max_off.max(0)) as usize;
    // The preserved screen row may leave the cursor's block (wrapped lines
    // or a comment card) clipped at the bottom; reveal it.
    app.view.keep_cursor_visible(app.view_viewport_rows());
    app.mode = Mode::View;
    // Every mode switch forces ASCII so j/k navigation is never swallowed
    // by the IME. No-op when --ime off or the helper hasn't been built yet.
    let _ = app.ime_session.force_ascii();
}

/// Which source lines a comment covers → per-line marker flags for the
/// view's gutter. Only comments for `current_file` are considered.
fn view_marker_flags(comments: &[Comment], n_lines: usize, current_file: &Path) -> Vec<bool> {
    let mut marked = vec![false; n_lines];
    if marked.is_empty() {
        return marked;
    }
    let last = n_lines.saturating_sub(1);
    for c in comments {
        if c.file_path != current_file {
            continue;
        }
        let start = (c.start.saturating_sub(1) as usize).min(last);
        let end = (c.end.saturating_sub(1) as usize).min(last);
        marked[start..=end].fill(true);
    }
    marked
}



/// `n`/`N` jump targets: each comment on `current_file` as its own
/// (0-based start, end) pair, sorted by start then end. Overlapping
/// comments stay separate targets, so a jump never selects the merged
/// union of several comments — only the lines one comment covers.
/// Identical ranges (stacked comments on the same lines) count once:
/// the selection would be the same either way.
fn comment_regions(comments: &[Comment], n_lines: usize, current_file: &Path) -> Vec<(usize, usize)> {
    if n_lines == 0 {
        return Vec::new();
    }
    let last = n_lines.saturating_sub(1);
    let mut regions: Vec<(usize, usize)> = comments
        .iter()
        .filter(|c| c.file_path == current_file)
        .map(|c| {
            let start = (c.start.saturating_sub(1) as usize).min(last);
            let end = (c.end.saturating_sub(1) as usize).min(last);
            (start, end)
        })
        .collect();
    regions.sort_unstable();
    regions.dedup();
    regions
}

/// Move the view cursor by one CONTENT line: blank source lines and table
/// delimiter rows (`|---|---|`) are skipped (they render as gap rows —
/// stopping on them is a wasted keypress; the mouse, a selection's j/k
/// extension, and source mode still reach them). A delimiter row carries
/// no information — the table body already shows the columns — so it
/// counts as a gap like a blank. Stays put when only blanks remain in
/// that direction.
fn view_move_cursor(app: &mut App, dir: isize) {
    let n = app.source.len() as isize;
    let mut i = app.view.cursor as isize;
    loop {
        i += dir;
        if i < 0 || i >= n {
            return;
        }
        if !app.source.lines[i as usize].trim().is_empty()
            && !is_table_delimiter_line(&app.view.ghost, i as usize)
        {
            app.view.goto_source_line(i as usize);
            return;
        }
    }
}

/// View mode: the cursor walks the rendered rows; `v` starts a selection
/// (anchored at the paragraph head), j/k extend it while active, `c`
/// comments the selection, `n`/`N` jump to comment blocks, Tab toggles to
/// source mode carrying the selection over.
pub(crate) fn on_view_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut AppTerminal>) {
    let viewport = app.view_viewport_rows();
    match key {
        // The rendered document is the timeline. No diff pane is opened:
        // only the blocks that differ are replaced by the new render while
        // the reader remains anchored around the same heading.
        KeyCode::Left if modifiers.is_empty() => {
            select_history(app, 1);
        }
        KeyCode::Right if modifiers.is_empty() => {
            select_history(app, -1);
        }
        // Alt+j / Alt+k: next/previous difference from the baseline.
        // as F7/`]c`/n, for 40%-keyboard layouts where neither F7 nor
        // `[`/`]` sit on the base layer (j/k are already the movement
        // keys, so Alt+move is the bigger step, like Ctrl+d/Ctrl+u).
        KeyCode::Char('j') if modifiers.contains(KeyModifiers::ALT) => jump_change_mark(app, 1),
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::ALT) => jump_change_mark(app, -1),
        // Shift+↓/↑ and J/K: select-and-move in one key (no `v` first).
        // The Shift+arrow arms must precede the plain arrow arms, which
        // carry no modifier guard. `J`/`K` are matched by character only:
        // some terminals report them with SHIFT set, others do not.
        KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) => select_and_move_view(app, 1),
        KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) => select_and_move_view(app, -1),
        KeyCode::Char('J') => select_and_move_view(app, 1),
        KeyCode::Char('K') => select_and_move_view(app, -1),
        KeyCode::Char('j') | KeyCode::Down => {
            if app.selection.is_some() {
                extend_view_selection(app, 1);
            } else {
                view_move_cursor(app, 1);
                app.view.keep_cursor_visible(viewport);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selection.is_some() {
                extend_view_selection(app, -1);
            } else {
                view_move_cursor(app, -1);
                app.view.keep_cursor_visible(viewport);
            }
        }
        KeyCode::Char('g') => app.view.jump_top(),
        KeyCode::Char('G') => {
            app.view.jump_bottom();
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::PageDown => {
            app.view.move_cursor_display(viewport as isize);
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::PageUp => {
            app.view.move_cursor_display(-(viewport as isize));
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.view.move_cursor_display((viewport / 2) as isize);
            app.view.keep_cursor_visible(viewport);
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.view.move_cursor_display(-((viewport / 2) as isize));
            app.view.keep_cursor_visible(viewport);
        }
        // Ctrl+C is the hard interrupt: the same path as `q` — quit at
        // once with no comments, one confirmation with comments — but
        // written out here rather than delegated, so it never depends on
        // config (Esc's quit needs esc-quit enabled; an interrupt must
        // always do something, even while a send command is stalled).
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            if app.confirm_quit || app.comments.is_empty() {
                app.running = false;
            } else {
                app.confirm_quit = true;
            }
        }
        // Selection start, parallel to source mode: the cursor line.
        KeyCode::Char('v') => {
            app.selection = Some(Selection::new(app.view.cursor));
        }
        KeyCode::Char('c') => {
            // Comment the view selection (or the cursor line — the exact
            // line, matching the line-based navigation) without leaving
            // view mode: the composer is drawn inline in the rendered view
            // and Enter/Esc return here. The draw pass keeps the bar on
            // screen (keep_composer_visible_view) — including on the last
            // line, where the bar extends past the document's last row.
            open_composer(app, Mode::View);
        }
        // Ctrl+n/Ctrl+N and Ctrl+p/Ctrl+P jump between comments: n and
        // p move forward (next), the shifted variants backward (prev).
        // Review jumps come after the Ctrl arms, so the shifted
        // keys stay unambiguous.
        KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, 1),
        KeyCode::Char('N') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, -1),
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, -1),
        KeyCode::Char('P') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, 1),
        // n/N: next/previous difference from the baseline.
        // keys (delta, less, magit), reachable on any layout: no F-keys,
        // no `[`/`]`, no Alt. No modifier guard: some terminals report
        // Shift+N as 'N' WITH the SHIFT flag set, which the is_empty
        // guard would drop.
        KeyCode::Char('n') => jump_change_mark(app, 1),
        KeyCode::Char('N') => jump_change_mark(app, -1),
        KeyCode::Char('a') => {
            mark_seen(app, true);
        }
        KeyCode::Tab => {
            // View is only reachable for Markdown-family files; a source
            // file (e.g. .rs) never leaves source mode.
            if supports_view(app.current_file_path()) {
                enter_source_mode(app);
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
        }
        // Comment management works from the view too (the parallel model:
        // d/y/s need no mode switch).
        KeyCode::Char('d') => delete_comment_at_cursor(app),
        KeyCode::Char('y') => yank_visible(app),
        KeyCode::Char('s') => export_all(app, true),
        KeyCode::Char('q') => request_quit(app),
        KeyCode::Char('r') => {
            if let Some(position) = app.history().map(|history| history.position)
                && position > 0
            {
                select_history(app, -(position as isize));
                render_pending_history(app, false);
            }
            reload_now(app);
        }
        KeyCode::Char('i') => ignore_change(app),
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
            if app.config.reply {
                // The file picker shows temp-file names — meaningless in
                // reply mode; ]/[ moves between messages instead.
                app.flash_err("reply mode — move between messages with ]/[");
            } else {
                open_overlay(app, Overlay::Files, app.current_file_index);
            }
        }
        KeyCode::Char('e') => {
            if app.config.reply {
                // Reply mode: the doc is the agent's message — editing the
                // temp copy would only diverge from the conversation.
                app.flash_err("reply mode — editing disabled");
            } else if app.file_changed {
                // A pending agent edit is up for review; opening the editor
                // now would fold it into one's own edit — the from_editor
                // reload would acknowledge it without it ever appearing in
                // the change marks. Review first (`r`), then edit.
                app.flash_err("file changed — r reload first");
            } else if let Some(t) = terminal {
                open_editor(app, t);
            }
        }
        // `]`/`[` arm the chord: alone they switch files (when the
        // [`CHORD_MS`] window expires, or on the next non-chord key); `c`
        // within the window jumps to the next/previous change instead
        // (the F7 fallback). F7/Shift+F7 jump directly.
        KeyCode::F(7) if modifiers.contains(KeyModifiers::SHIFT) => jump_change_mark(app, -1),
        KeyCode::F(7) => jump_change_mark(app, 1),
        KeyCode::Char(']') => app.pending_chord = Some((Instant::now(), ']')),
        KeyCode::Char('[') => app.pending_chord = Some((Instant::now(), '[')),
        // The knob (only with `--semantic`): `-`/`+` move one point,
        // `<`/`>` ten. `=` is the unshifted alias for `+`, the
        // convention every zoom control uses. The four keys were free on
        // BOTH maps (view and source) before this — nothing else in
        // akapen binds a punctuation key except `]`/`[` and `?`.
        //
        // 同じ 4 本が `on_source_key` にもある。source view に range
        // decoration が通ったので、どちらのモードでも装飾が動く。
        //
        // No `modifiers.is_empty()` guard, for the same reason `J`/`K`/`N`
        // carry none: some terminals report a shifted character WITH the
        // SHIFT flag set, and the guard would silently drop it.
        //
        // ガードが落ちればこれらの腕は無かったことになり、末尾の `_ => {}`
        // に落ちる — `--semantic` を渡していないセッションでは `-` も `+`
        // も `<` も `>` も、この層が存在しなかったときと 1 バイトも違わない
        // 動きをする。「使えない機能があります」という UI を見せないため、
        // 断りの toast すら出さない。
        // 割り当ては `crate::keys` の 1 か所にある。層の無いセッションは
        // `semantic_enabled()` で素通りする（使えないキーを呑み込まない）。
        KeyCode::Char(c)
            if app.semantic_enabled() && crate::keys::semantic_key(c).is_some() =>
        {
            if let Some(action) = crate::keys::semantic_key(c) {
                on_semantic_key(app, action);
            }
        }
        // `l` opens the all-comments list; `t` the document timeline;
        // Ctrl+p opens the file picker; `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
        }
        // **Review の一覧**（`R`）— 校正候補。`l` の隣に置いてあるのは、
        // どちらも「台帳を開く」キーだからである（`crate::keys::REVIEW_OPEN`）。
        // 層も `--lint-cmd` も無いセッションでは他のキーと同じく黙って
        // 落ちる — 使えない機能の断りを見せない、という marks と同じ作法
        // である。**lint だけのセッションでは生きている**（意味層は要らない）。
        KeyCode::Char(crate::keys::REVIEW_OPEN) if app.review_key_live() => {
            open_review(app);
        }
        KeyCode::Char('t') => {
            if app.config.reply {
                app.flash_err("reply mode — history unavailable");
            } else if app.history().is_some_and(|history| history.revisions.len() > 1) {
                let position = app.history().map_or(0, |h| h.position);
                app.timeline_restore = Some(position);
                open_overlay(app, Overlay::Timeline, position);
                keep_overlay_cursor_visible(app);
            }
        }
        KeyCode::Char('?') => {
            open_overlay(app, Overlay::Help, 0);
        }
        // 一番手前の層を 1 枚はがす。順番は `crate::esc::ORDER` の 1 本で、
        // source とも Review の据え付けの一覧とも同じ表を通る。
        KeyCode::Esc => esc::peel(app),
        _ => {}
    }
}

/// Extend the view-mode selection by one SOURCE LINE — byte-for-byte the
/// same semantics as source mode's [`extend_selection`] (the anchor
/// stays, the cursor moves, and the range normalizes — `k` past the
/// anchor grows the range upward). The phrase-precise gray shows the
/// growth even inside merged rows, so the cursor never looks stuck. Tab
/// carries the range unchanged and source mode shows it exactly.
fn extend_view_selection(app: &mut App, dir: isize) {
    let Some(sel) = &mut app.selection else {
        return;
    };
    if app.source.is_empty() {
        return;
    }
    let max = app.source.len() - 1;
    let next = (sel.cursor as isize + dir).clamp(0, max as isize) as usize;
    sel.cursor = next;
    app.view.goto_source_line(next);
    app.view.keep_cursor_visible(app.view_viewport_rows());
}

/// `J`/`K` / Shift+↓↑ in view mode: anchor a selection on the cursor line
/// if none is active, then extend it one line — the state `v` `j` would
/// reach, in one key. From here `j`/`k` keep extending and Esc cancels.
fn select_and_move_view(app: &mut App, dir: isize) {
    if app.source.is_empty() {
        return;
    }
    if app.selection.is_none() {
        app.selection = Some(Selection::new(app.view.cursor));
    }
    extend_view_selection(app, dir);
}

/// `J`/`K` / Shift+↓↑ in source mode (see [`select_and_move_view`]).
fn select_and_move_source(app: &mut App, dir: isize, height: u16) {
    if app.source.is_empty() {
        return;
    }
    if app.selection.is_none() {
        app.selection = Some(Selection::new(app.cursor));
    }
    extend_selection(app, dir, height);
}

/// The source lines that share the view cursor's row: with no selection
/// the cursor band covers the WHOLE row, and a merged paragraph row holds
/// several source lines, so "copy what I see" means all of them. A row
/// with no attribution (a rule, a border) falls back to the cursor line.
pub(crate) fn view_cursor_row_lines(view: &ViewState) -> (usize, usize) {
    let row = view.cursor_row();
    let segs = view.row_segments.get(row).map(|s| s.as_slice()).unwrap_or(&[]);
    let lo = segs.iter().map(|s| s.line).min();
    let hi = segs.iter().map(|s| s.line).max();
    match (lo, hi) {
        (Some(lo), Some(hi)) => (lo.min(view.cursor), hi.max(view.cursor)),
        _ => (view.cursor, view.cursor),
    }
}

/// `y`: copy the selection — or the cursor line — as displayed. Source
/// mode copies the raw Markdown lines; view mode copies the rendered text
/// (Tab to source mode first for the Markdown). Copying the *comments*
/// lives in the comments overlay (`l`, then `y`), where the comments are.
fn yank_visible(app: &mut App) {
    if app.source.is_empty() {
        app.flash_err("nothing to copy");
        return;
    }
    // A live deletion focus (source mode, after `n` landed on a pure
    // deletion) shows the deleted baseline rows, so that is what `y`
    // copies — the same snippet `c` quotes there, not the untouched
    // anchor line the cursor technically sits on.
    if app.mode == Mode::Source
        && let Some(deleted) = app.focused_deletion_content()
    {
        let lines = deleted.lines().count().max(1);
        match export::copy_to_clipboard(&deleted) {
            Ok(()) => app.flash(format!("copied {lines} deleted line(s)")),
            Err(e) => app.flash_err(format!("clipboard failed: {e:#}")),
        }
        return;
    }
    let (start, end) = match app.selection {
        Some(sel) => sel.range(),
        None if app.mode == Mode::View => view_cursor_row_lines(&app.view),
        None => (app.cursor, app.cursor),
    };
    let text = if app.mode == Mode::View {
        yank::view_text(&app.source, &app.highlight, start, end)
    } else {
        yank::source_text(&app.source, start, end)
    };
    let Some(text) = text else {
        app.flash_err("nothing to copy on this line");
        return;
    };
    let lines = end - start + 1;
    match export::copy_to_clipboard(&text) {
        Ok(()) => app.flash(format!("copied {lines} line(s)")),
        Err(e) => app.flash_err(format!("clipboard failed: {e:#}")),
    }
}
/// Move the cursor to the next (`n`) or previous (`N`) comment — the same
/// comment landing in both modes. Each comment is its own target:
/// overlapping comments are jumped to one by one, so the selection never
/// spans the merged union of several comments (stacked comments on the
/// same range count once). The comment becomes the selection (its lines
/// light up), so j/k can extend it and d/c act on the range. The cursor
/// lands on the comment's extent (bottom) — the selection model's
/// invariant is cursor == selection extent.
fn jump_comment(app: &mut App, dir: isize) {
    let visible: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
    let regions = comment_regions(&visible, app.source.len(), app.current_file_path());
    if regions.is_empty() {
        return;
    }
    let cur = if app.mode == Mode::View {
        app.view.cursor as isize
    } else {
        app.cursor as isize
    };
    // Navigate relative to the current selection when there is one (the
    // cursor sits on the extent, which is INSIDE the block — searching
    // from the cursor would re-find the same block); otherwise fall back
    // to the cursor.
    let (low, high) = match app.selection {
        Some(sel) => {
            let (a, b) = sel.range();
            (a as isize, b as isize)
        }
        None => (cur, cur),
    };
    // Targets are individual comments sorted by (start, end), so a jump
    // compares the full pair lexicographically: the next comment is the
    // first one strictly after the current position — overlapping
    // comments come out in line order instead of merging into one block.
    let target = if dir > 0 {
        regions
            .iter()
            .find(|&&(s, e)| (s as isize, e as isize) > (low, high))
            .copied()
    } else {
        regions
            .iter()
            .rev()
            .find(|&&(s, e)| (s as isize, e as isize) < (low, high))
            .copied()
    };
    match target {
        Some((start, end)) => {
            app.selection = Some(Selection {
                anchor: start,
                cursor: end,
            });
            if app.mode == Mode::View {
                app.view.goto_source_line(end);
                app.view.keep_cursor_visible(app.view_viewport_rows());
            } else {
                app.cursor = end;
                app.keep_cursor_visible(app.source_viewport_rows() as u16);
            }
        }
        None => app.flash_err(if dir > 0 {
            "no comments below"
        } else {
            "no comments above"
        }),
    }
}

fn change_targets(app: &App) -> Vec<(usize, usize)> {
    let mut lines: Vec<usize> = app.comparison_changed.iter().copied().collect();
    lines.sort_unstable();
    lines.dedup();
    let mut targets = Vec::new();
    for line in lines {
        match targets.last_mut() {
            Some((_, end)) if line == *end + 1 => *end = line,
            _ => targets.push((line, line)),
        }
    }
    for line in &app.comparison_deleted_before {
        if !targets.iter().any(|(a, b)| line >= a && line <= b) {
            targets.push((*line, *line));
        }
    }
    targets.sort_unstable();
    targets
}

fn active_change_mark_sets(app: &App) -> (HashSet<usize>, HashSet<usize>) {
    (
        app.comparison_changed.clone(),
        app.comparison_deleted_before.clone(),
    )
}

/// Semantic Reading Layer のキー 1 打を捌く。**入口はここ 1 つ**で、
/// 割り当ては [`crate::keys`] にある。
fn on_semantic_key(app: &mut App, action: crate::keys::SemanticKey) {
    use crate::keys::SemanticKey;
    match action {
        SemanticKey::Amount(delta) => adjust_marks_share(app, delta),
        SemanticKey::CycleQuestion(step) => cycle_marks_question(app, step),
        SemanticKey::PickQuestion => open_marks_picker(app),
        SemanticKey::FreeQuestion => open_marks_prompt(app),
        SemanticKey::Focus => press_focus(app),
    }
}

/// **`f`（フォーカス）を押した。**
///
/// 押している間だけ沈む端末と、押すたびに切り替わる端末の両方を、
/// [`crate::focus::Focus`] の 1 つの状態機械が出す（そちらのモジュール注）。
/// ここがするのは「押した」を渡すことと、沈める先が無いときに理由を
/// 言うことだけである。
///
/// 沈めても**つまみ（`-`/`+`/`<`/`>`）と問い（`m`/`M`/`/`）は効いたまま**で
/// ある — 沈んだ状態のまま問いを変えられるのが、この機能のいちばんの
/// 使い道だからで、そのために必要な行はここに 1 行も無い（投影が
/// 切り替わるだけで、キーの経路は元のまま）。
fn press_focus(app: &mut App) {
    // **沈んでいる間は必ず戻せる。** 断りは「沈める先が無いのに沈めよう
    // とした」ときだけで、沈んだあとに問いを変えて 0 本になった画面
    // （見出しだけが残って他は全部沈んでいる）から `f` で出られなく
    // なるのは事故である — そこで断ると Esc しか出口が無くなる。
    if !app.focused() && !app.can_focus() {
        // 0 本のときに沈めると画面が全部沈む。頼まれたのは「他を沈める」
        // であって「全部沈める」ではないので、断って理由を言う。
        app.flash("nothing marked yet — m to pick what to mark");
        return;
    }
    app.press_focus(Instant::now());
}

/// **つまみ。** 上から何 % を光らせるかを `delta` ポイント動かす。
///
/// **ここに遅延の起点は無い。** 起点は問いを決めたとき
/// （[`cycle_marks_question`] / [`open_marks_prompt`]）で、量のつまみでは
/// ない — 問いの無い解析はこの投影では使えないからである。
///
/// だからこの関数は `Provider::analyze` へ到達しない。走るのは
/// `marks::mark` 1 回だけで、新しい本数は読み出しに出る。
fn adjust_marks_share(app: &mut App, delta: i16) {
    // **注釈が無いあいだは動かさない。** 光っている箇所が 0 本なのに
    // % だけが動くと、読み出しの数字が画面と食い違う。断り方は 3 通りで、
    // どれも「なぜ今使えないか」を言う。
    if app.semantic_doc.is_none() {
        if app.semantic_inflight.is_some() {
            // 答え待ち。まだ無いのは事実だが「使えない」とは違う —
            // 数秒後には来る。
            app.flash("analyzing…");
        } else if app.marks_question.is_none() {
            app.flash("nothing to mark for yet — m to pick, / to ask");
        } else {
            // `--semantic`（fixture）でしか来ない: 層は頼まれているのに
            // provider がこの文書を断った（別の文書の fixture、など）。
            app.flash_err("semantic annotation unavailable for this document");
        }
        return;
    }
    app.nudge_marks_share(delta);
}

/// **定型を巡る**（`m` / `M`）。marks モードの遅延の起点の 1 つ。
///
/// 同じ (文書, 問い) は二度 Jev を呼ばない（キャッシュ）ので、環を一周して
/// 戻るのは 0 円である。
fn cycle_marks_question(app: &mut App, step: i16) {
    // **解析は 1 度だけ。** 逆回りを「残り全部ぶん進む」で書くと、1 打で
    // Jev を定型の数だけ呼ぶ（[`App::cycle_marks_question`]）。
    if !app.cycle_marks_question(step as isize) {
        refuse_marks_question(app);
        return;
    }
    if let Some(label) = app.marks_question_label() {
        app.flash(format!("asking: {label}"));
    }
}

/// **定型の選択（popup）を開く**（`m`）。
///
/// 開くだけで、ここでは何も聞かない — 解析が走るのは popup の中で選んだ
/// 瞬間である（`overlay::activate_overlay_selection`）。巡る形が 1 打ごとに
/// Jev を呼んでいたのに対して、popup は**選んだ 1 本だけ**を呼ぶ。
///
/// カーソルはいま聞いている定型に置く。`m` を押して Enter を打てば
/// 「何も変わらない」が既定になるので、覗くのが安全になる。
fn open_marks_picker(app: &mut App) {
    if app.marks_questions.is_none() || app.marks_question_is_fixed() {
        refuse_marks_question(app);
        return;
    }
    let cursor = mark_for_preset_row(app, app.marks_preset);
    open_overlay(app, Overlay::MarkFor, cursor);
}

/// 自由入力のプロンプトを閉じて元のモードへ戻す。
fn close_marks_prompt(app: &mut App) {
    app.marks_prompt = false;
    app.input.clear();
    app.input_cursor = 0;
    app.mode = app.composer_return;
    app.ime_guard = None;
}

/// 問いを変えられない経路で `m` / `/` を押したときの断り。
pub(crate) fn refuse_marks_question(app: &mut App) {
    if matches!(
        app.semantic_source,
        Some(crate::semantic::SemanticSource::Inline(_))
    ) {
        // fixture は 1 つの問いへの答えを固定で持っている。変わった
        // ふりをするより、変わらないと言うほうが正しい。
        app.flash_err("fixture: what to mark is fixed");
    } else {
        app.flash_err("mark for unavailable");
    }
}

/// **自由入力のプロンプトを開く**（`/`、または popup の最下段）。
///
/// 借りるのは `Mode::Input`（打鍵の経路・IME・Esc の取り消し）**だけ**で、
/// **見た目は composer ではない** — 描くのはメッセージ行の 1 行プロンプト
/// である（[`crate::chrome::draw_ask_prompt`]）。
///
/// **2026-09-22 に composer の借用をやめた。** 借りていたときは吹き出しが
/// `comment · 1` と名乗り（バッジだけが `ASK` に変わる）、1 行の問いのために
/// 3 行を文書へ割り込ませていた。問いは 1 行なので 1 行で足りる。
///
/// `input_start` / `input_end` は**アンカーとしては使わない**が、カーソル
/// 行を入れておく — コメントの経路と同じ形にしておけば、`Esc` の後始末
/// （[`cancel_composer`]）が marks の旗を見るだけで済む。
pub(crate) fn open_marks_prompt(app: &mut App) {
    if app.marks_questions.is_none() || app.marks_question_is_fixed() {
        refuse_marks_question(app);
        return;
    }
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    app.marks_prompt = true;
    app.editing_comment = None;
    app.input.clear();
    app.input_cursor = 0;
    app.input_start = line;
    app.input_end = line;
    app.composer_return = app.mode;
    app.mode = Mode::Input;
    app.ime_guard = Some(ime::ImeGuard::enter(app.config.ime));
    app.cursor = line;
}

fn jump_change_mark(app: &mut App, dir: isize) {
    // If an arrow scrub selected a generation that has not rendered yet,
    // materialize it before navigating its baseline-relative marks. A mark
    // must always point into the document the user can actually see.
    if app
        .history()
        .is_some_and(|history| history.position != history.rendered_position)
    {
        render_pending_history(app, false);
    }
    let targets = change_targets(app);
    if targets.is_empty() {
        app.flash("no changes since baseline");
        return;
    }
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    let target = if dir > 0 {
        targets
            .iter()
            .position(|(start, _)| *start > line)
            .unwrap_or(0)
    } else {
        targets
            .iter()
            .rposition(|(_, end)| *end < line)
            .unwrap_or(targets.len() - 1)
    };
    let (start, end) = targets[target];
    // A pure deletion's difference is the red rows above the target line,
    // not the (unchanged) line itself. In source mode the landing focuses
    // those rows — no selection on the anchor line, bright deleted rows,
    // and `c` comments the deletion — instead of highlighting a line the
    // agent never touched. View mode keeps the selection (it has only the
    // red `▌` position mark to point at) but records the focus too, so a Tab
    // into source arrives with the deletion focused (see
    // [`enter_source_mode`]) — the flash promises exactly that.
    let is_deletion = start == end
        && app.comparison_deleted_before.contains(&start)
        && !app.comparison_changed.contains(&start);
    if is_deletion && app.mode == Mode::Source {
        app.selection = None;
        app.focused_deletion = Some(start);
    } else {
        app.focused_deletion = if is_deletion { Some(start) } else { None };
        app.selection = Some(Selection {
            anchor: start,
            cursor: end,
        });
    }
    app.cursor = end;
    app.view.goto_source_line(end);
    if app.mode == Mode::View {
        app.view
            .center_source_range(start, end, app.view_viewport_rows());
    } else {
        app.center_source_range(start, end, app.source_viewport_rows() as u16);
    }
    if is_deletion {
        let removed = app
            .deleted_content_at(start)
            .map(|content| content.split('\n').count())
            .unwrap_or(0);
        // View shows only the red `▌` position mark — the deleted content (and
        // commenting on it) lives in source mode, so point the way there.
        let action = if app.mode == Mode::Source {
            "c: comment on deletion"
        } else {
            "Tab: inspect & comment"
        };
        app.flash(format!(
            "difference {}/{} · {} deleted line{} · {}",
            target + 1,
            targets.len(),
            removed,
            if removed == 1 { "" } else { "s" },
            action
        ));
    } else {
        app.flash(format!(
            "difference {}/{} · L{}{}",
            target + 1,
            targets.len(),
            start + 1,
            if end > start {
                format!("-{}", end + 1)
            } else {
                String::new()
            }
        ));
    }
}

/// **`]m` / `[m`: 次・前のマーク行へ。**
///
/// 飛び先の台帳は `App::marks_lines`（装飾を作り直すたびに更新される
/// 1 本のリスト）で、**スクロールバーの溝に打つ目盛りと同じもの**である
/// — 2 か所で数えると「点の無いところへ飛ぶ」が起きる。
///
/// 環である（最後の次は最初）。`]c` のレビューマークと同じ作法で、
/// 行き止まりを作らない。選択は作らない — マーカーは offer であって
/// 「ここを直せ」ではないので、赤入れのジャンプのように範囲を掴まない。
fn jump_mark(app: &mut App, dir: isize) {
    let targets = app.marks_lines.clone();
    if targets.is_empty() {
        app.flash("no marks — m to pick what to mark");
        return;
    }
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    let target = if dir > 0 {
        targets.iter().position(|&l| l > line).unwrap_or(0)
    } else {
        targets
            .iter()
            .rposition(|&l| l < line)
            .unwrap_or(targets.len() - 1)
    };
    let destination = targets[target];
    app.cursor = destination;
    app.view.goto_source_line(destination);
    // **飛び先が画面に入ること。** view は行が折り返すので、行番号では
    // なく表示行で寄せる必要がある（`center_source_range` は両モードが
    // 持っている同じ約束である）。
    if app.mode == Mode::View {
        app.view
            .center_source_range(destination, destination, app.view_viewport_rows());
    } else {
        app.center_source_range(destination, destination, app.source_viewport_rows() as u16);
    }
    app.flash(format!(
        "mark {}/{} · L{}",
        target + 1,
        targets.len(),
        destination + 1
    ));
}

pub(crate) fn mark_seen(app: &mut App, announce: bool) -> bool {
    if app.config.reply {
        return false;
    }
    let index = app.current_file_index;
    let path = app.current_file_path().to_path_buf();
    let at_now = app.history().is_none_or(|history| history.at_now());
    // `position` is authoritative even while fast scrubbing: app.source may
    // still contain the last rendered generation until the debounce ends.
    let content = app
        .history()
        .and_then(|history| history.current())
        .map(|revision| revision.content.clone())
        .unwrap_or_else(|| app.source.content.clone());
    let baseline_label = (!at_now)
        .then(|| app.history().and_then(|history| history.label()))
        .flatten();
    let result = if let Some(cache) = app.snapshot_cache.clone() {
        app.histories
            .get_mut(index)
            .map(|history| {
                if at_now {
                    history.acknowledge(&path, &content, &cache)
                } else {
                    history.set_baseline(&path, &content, &cache)
                }
            })
            .transpose()
    } else {
        if let Some(history) = app.histories.get_mut(index) {
            history.acknowledge_in_memory(&content);
        }
        Ok(None)
    };
    if let Err(error) = result {
        app.flash_err(format!("baseline failed: {error:#}"));
        return false;
    }
    // Acknowledging or choosing a baseline completes the current review
    // action. Leave SELECT state consistently in both rendered and source
    // modes; Input keeps treating `a` as ordinary text.
    app.selection = None;
    refresh_baseline_marks(app);
    if announce {
        if let Some(label) = baseline_label {
            app.flash(format!("baseline set · {label}"));
        } else {
            app.flash("seen");
        }
    }
    true
}

fn pin_current_snapshot(app: &mut App) {
    let Some(cache) = app.snapshot_cache.clone() else {
        return;
    };
    let path = app.current_file_path().to_path_buf();
    if let Some(history) = app.histories.get(app.current_file_index) {
        let _ = history.pin_current(&path, &cache);
    }
}

/// Flag the view for re-rendering after the resize debounce.
fn mark_view_dirty(app: &mut App) {
    app.view_dirty = true;
    if app.view_dirty_since.is_none() {
        app.view_dirty_since = Some(Instant::now());
    }
}

/// The render width: the content pane width, i.e. the terminal width
/// minus the page's left margin, the 2 columns the frame borders take,
/// and the text column's 1-column pads on each side. The marker column
/// rides the frame's left border, so it reserves no width of its own. The
/// render must wrap at exactly the width the pane displays, or the
/// source-line mapping drifts by a couple of columns. Both call sites
/// (startup and resize) go through this so they can never disagree.
pub(crate) fn view_render_width(terminal_width: u16) -> u16 {
    terminal_width.saturating_sub(2).saturating_sub(1).saturating_sub(2)
}

/// Re-render at the current terminal width, preserving the cursor fraction.
fn rerender_view(app: &mut App) {
    if ratatui::crossterm::terminal::size().is_ok() {
        let fraction = app.view.cursor_fraction();
        // render_current_view derives the render width from the terminal.
        let file_comments: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
        let mut view = render_current_view(app, &file_comments);
        view.goto_fraction(fraction);
        // Reveal the cursor in the fresh view (draw_view no longer
        // auto-scrolls to it every frame).
        view.keep_cursor_visible(app.view_viewport_rows());
        app.view = view;
        // 幅が変わると割合で戻したカーソルは候補の行からずれうる。
        // 一覧を据え付けているなら、選んだ候補へ送り直す。
        crate::review_dock::follow(app);
    }
}

/// Page-move the comment cursor by `delta` display rows (view-mode spec:
/// the cursor rides the page jump and the viewport follows, instead of a
/// viewport-only scroll that leaves the cursor behind off screen), landing
/// on the source line that owns the target row.
fn source_move_cursor_display(app: &mut App, delta: isize, viewport: u16) {
    if app.source.is_empty() || app.line_rows.is_empty() {
        return;
    }
    let target = (app.row_of(app.cursor) as isize + delta).max(0) as usize;
    let mut row = 0usize;
    let mut line = app.source.len() - 1;
    for (i, &rows) in app.line_rows.iter().enumerate() {
        if target < row + rows {
            line = i;
            break;
        }
        row += rows;
    }
    app.cursor = line;
    app.keep_cursor_visible(viewport);
}

/// Source mode: navigate, select, comment, manage, quit.
pub(crate) fn on_source_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut AppTerminal>) {
    // `viewport` is the height keep_cursor_visible must scroll against: the
    // comment pane's real content height (draw_source's inner.height), not
    // the raw terminal height — the title bar and footer take those rows,
    // so scrolling against the terminal height fires the down branch
    // `height - viewport` rows late and hides the cursor in the pane's
    // bottom rows.
    let viewport = app.source_viewport_rows() as u16;
    match key {
        // Source and rendered view share one document timeline. Only the
        // representation changes when Tab is pressed.
        KeyCode::Left if modifiers.is_empty() => {
            select_history(app, 1);
        }
        KeyCode::Right if modifiers.is_empty() => {
            select_history(app, -1);
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            source_move_cursor_display(app, (viewport / 2) as isize, viewport);
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            source_move_cursor_display(app, -((viewport / 2) as isize), viewport);
        }
        // Ctrl+C is the hard interrupt: the same path as `q` — quit at
        // once with no comments, one confirmation with comments — but
        // written out here rather than delegated, so it never depends on
        // config (Esc's quit needs esc-quit enabled; an interrupt must
        // always do something, even while a send command is stalled).
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            if app.confirm_quit || app.comments.is_empty() {
                app.running = false;
            } else {
                app.confirm_quit = true;
            }
        }
        // Alt+j / Alt+k: next/previous difference from the baseline.
        // as F7/`]c`/n, for 40%-keyboard layouts where neither F7 nor
        // `[`/`]` sit on the base layer.
        KeyCode::Char('j') if modifiers.contains(KeyModifiers::ALT) => jump_change_mark(app, 1),
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::ALT) => jump_change_mark(app, -1),
        // Shift+↓/↑ and J/K: select-and-move (see on_view_key).
        KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) => select_and_move_source(app, 1, viewport),
        KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) => select_and_move_source(app, -1, viewport),
        KeyCode::Char('J') => select_and_move_source(app, 1, viewport),
        KeyCode::Char('K') => select_and_move_source(app, -1, viewport),
        KeyCode::Char('j') | KeyCode::Down => {
            if app.selection.is_some() {
                extend_selection(app, 1, viewport);
            } else {
                move_cursor(app, 1, viewport);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selection.is_some() {
                extend_selection(app, -1, viewport);
            } else {
                move_cursor(app, -1, viewport);
            }
        }
        KeyCode::Char('g') => {
            app.cursor = 0;
            app.offset = 0;
        }
        KeyCode::Char('G') => {
            app.cursor = app.source.len().saturating_sub(1);
            app.keep_cursor_visible(viewport);
        }
        KeyCode::Char('v') => {
            app.selection = Some(Selection::new(app.cursor));
        }
        KeyCode::Char('c') => open_composer(app, Mode::Source),
        KeyCode::Char('d') => delete_comment_at_cursor(app),
        KeyCode::Char('s') => export_all(app, true),
        KeyCode::Char('y') => yank_visible(app),
        KeyCode::Tab => {
            // Non-Markdown files are source-only: Tab is a no-op with a
            // toast instead of rendering raw source as fake markdown.
            if supports_view(app.current_file_path()) {
                enter_view_mode(app);
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
        }
        // view と同じ表（`crate::esc::ORDER`）。Esc はモードを切り替えない
        // — 切り替えは Tab の 1 本（反射の Esc で読んでいた位置を失った）。
        KeyCode::Esc => esc::peel(app),
        KeyCode::PageDown => {
            source_move_cursor_display(app, viewport as isize, viewport);
        }
        KeyCode::PageUp => {
            source_move_cursor_display(app, -(viewport as isize), viewport);
        }
        KeyCode::Char('q') => request_quit(app),
        KeyCode::Char('r') => reload_now(app),
        KeyCode::Char('i') => ignore_change(app),
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
            if app.config.reply {
                // The file picker shows temp-file names — meaningless in
                // reply mode; ]/[ moves between messages instead.
                app.flash_err("reply mode — move between messages with ]/[");
            } else {
                open_overlay(app, Overlay::Files, app.current_file_index);
            }
        }
        KeyCode::Char('e') => {
            if app.config.reply {
                // Reply mode: the doc is the agent's message — editing the
                // temp copy would only diverge from the conversation.
                app.flash_err("reply mode — editing disabled");
            } else if app.file_changed {
                // A pending agent edit is up for review; opening the editor
                // now would fold it into one's own edit — the from_editor
                // reload would acknowledge it without it ever appearing in
                // the change marks. Review first (`r`), then edit.
                app.flash_err("file changed — r reload first");
            } else if let Some(t) = terminal {
                open_editor(app, t);
            }
        }
        // Ctrl+n/Ctrl+N and Ctrl+p/Ctrl+P jump between comments: n and
        // p move forward (next), the shifted variants backward (prev).
        // Review jumps come after the Ctrl arms, so the shifted
        // keys stay unambiguous.
        KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, 1),
        KeyCode::Char('N') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, -1),
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, -1),
        KeyCode::Char('P') if modifiers.contains(KeyModifiers::CONTROL) => jump_comment(app, 1),
        // n/N: next/previous difference from the baseline.
        // keys (delta, less, magit), reachable on any layout: no F-keys,
        // no `[`/`]`, no Alt. No modifier guard: some terminals report
        // Shift+N as 'N' WITH the SHIFT flag set, which the is_empty
        // guard would drop.
        KeyCode::Char('n') => jump_change_mark(app, 1),
        KeyCode::Char('N') => jump_change_mark(app, -1),
        KeyCode::Char('a') => {
            mark_seen(app, true);
        }
        // `]`/`[` arm the chord: alone they switch files (when the
        // [`CHORD_MS`] window expires, or on the next non-chord key); `c`
        // within the window jumps to the next/previous change instead
        // (the F7 fallback). F7/Shift+F7 jump directly.
        KeyCode::F(7) if modifiers.contains(KeyModifiers::SHIFT) => jump_change_mark(app, -1),
        KeyCode::F(7) => jump_change_mark(app, 1),
        KeyCode::Char(']') => app.pending_chord = Some((Instant::now(), ']')),
        KeyCode::Char('[') => app.pending_chord = Some((Instant::now(), '[')),
        // The layer's keys — the same guard, and the same lack of a
        // `modifiers.is_empty()` guard as the view map (see
        // `on_view_key`). Bound here because source mode paints the
        // range decorations too: moving the knob visibly changes what is
        // MARKED on THIS screen, not only on the other one. While the
        // decoration layer was view-only these keys were deliberately
        // left out — a key that moved a number in the footer and changed
        // nothing else would have been a lie.
        // 割り当ては `crate::keys` の 1 か所にある。層の無いセッションは
        // `semantic_enabled()` で素通りする（使えないキーを呑み込まない）。
        KeyCode::Char(c)
            if app.semantic_enabled() && crate::keys::semantic_key(c).is_some() =>
        {
            if let Some(action) = crate::keys::semantic_key(c) {
                on_semantic_key(app, action);
            }
        }
        // `l` opens the all-comments list; `t` the document timeline;
        // Ctrl+p opens the file picker; `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
        }
        // **Review の一覧**（`R`）— 校正候補。`l` の隣に置いてあるのは、
        // どちらも「台帳を開く」キーだからである（`crate::keys::REVIEW_OPEN`）。
        // 層も `--lint-cmd` も無いセッションでは他のキーと同じく黙って
        // 落ちる — 使えない機能の断りを見せない、という marks と同じ作法
        // である。**lint だけのセッションでは生きている**（意味層は要らない）。
        KeyCode::Char(crate::keys::REVIEW_OPEN) if app.review_key_live() => {
            open_review(app);
        }
        KeyCode::Char('t') => {
            if app.config.reply {
                app.flash_err("reply mode — history unavailable");
            } else if app.history().is_some_and(|history| history.revisions.len() > 1) {
                let position = app.history().map_or(0, |h| h.position);
                app.timeline_restore = Some(position);
                open_overlay(app, Overlay::Timeline, position);
                keep_overlay_cursor_visible(app);
            }
        }
        KeyCode::Char('?') => {
            open_overlay(app, Overlay::Help, 0);
        }
        _ => {}
    }
}

/// Byte offset of the composer cursor's logical-line start (just after
/// the previous `\n`, or 0).
fn input_line_start(s: &str, cursor: usize) -> usize {
    s[..cursor].rfind('\n').map_or(0, |i| i + 1)
}

/// Byte offset of the composer cursor's logical-line end (just before
/// the next `\n`, or the text end).
fn input_line_end(s: &str, cursor: usize) -> usize {
    s[cursor..].find('\n').map_or(s.len(), |i| cursor + i)
}

/// `from` advanced by up to `col` characters, capped at `to`.
fn advance_chars(s: &str, from: usize, to: usize, col: usize) -> usize {
    s[from..to]
        .char_indices()
        .nth(col)
        .map(|(i, _)| from + i)
        .unwrap_or(to)
}

/// Move the composer cursor one logical line up (`dir < 0`) or down,
/// keeping the character column (clamped to the target line's length).
/// At the first/last line the cursor jumps to the text start/end — the
/// standard textarea fallback.
fn input_move_line(s: &str, cursor: usize, dir: isize) -> usize {
    let start = input_line_start(s, cursor);
    let col = s[start..cursor].chars().count();
    if dir < 0 {
        if start == 0 {
            return 0;
        }
        // `start - 1` is the previous line's `\n`; its line starts after
        // the newline before THAT.
        let prev_start = input_line_start(s, start - 1);
        advance_chars(s, prev_start, start - 1, col)
    } else {
        let end = input_line_end(s, cursor);
        if end == s.len() {
            return s.len();
        }
        let next_start = end + 1;
        let next_end = input_line_end(s, next_start);
        advance_chars(s, next_start, next_end, col)
    }
}

/// Discard the draft and return to the mode the composer was opened
/// from — the shared Esc / Ctrl+C cancel. An interrupt must never type
/// into the draft: raw mode delivers Ctrl+C as a key event, so without
/// this arm the IME-safe catch-all below would insert a `c` instead.
fn cancel_composer(app: &mut App) {
    // marks の自由入力は composer を借りているだけ。コメントの後始末
    // （再編集の復帰・「comment cancelled」）はどれも当たらない。
    if app.marks_prompt {
        close_marks_prompt(app);
        app.flash("ask cancelled");
        return;
    }
    app.input.clear();
    app.input_cursor = 0;
    app.mode = app.composer_return;
    app.ime_guard = None;
    // A re-edit rendered the view without the edited card
    // (open_composer / the Tab flip); cancelling puts it back.
    if app.editing_comment.take().is_some() {
        replace_view_preserving_cursor(app);
    }
    app.flash("comment cancelled");
}

/// Input mode: printable keys insert at the text cursor (IME-safe);
/// arrows / Home / End (and Ctrl+a / Ctrl+e) move it, Backspace / Delete
/// edit around it, Ctrl+j inserts a newline, Enter confirms, Esc cancels.
fn on_input_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    // Guard: `input` can be replaced wholesale (tests set it directly;
    // future features might too). Keep the cursor inside it, on a char
    // boundary, before any positional edit.
    let mut cur = app.input_cursor.min(app.input.len());
    while !app.input.is_char_boundary(cur) {
        cur -= 1;
    }
    app.input_cursor = cur;
    match key {
        KeyCode::Enter => {
            let text = app.input.trim().to_string();
            // marks モードの自由入力。composer を借りているだけなので、
            // コメントの経路（アンカー・スナップショット・再編集）には
            // 1 行も入らない。
            if app.marks_prompt {
                if text.is_empty() {
                    app.flash_err("empty — nothing asked (Esc to cancel)");
                    return;
                }
                close_marks_prompt(app);
                if app.ask_marks_free(&text) {
                    app.flash(format!("asking: {text}"));
                }
                return;
            }
            if text.is_empty() {
                app.flash_err("empty comment — not added (Esc to cancel)");
                return;
            }
            // While `n`'s deletion focus is live (the composer keeps the
            // cursor on the anchor line with no selection), the comment is
            // about the deletion: quote the deleted baseline text instead
            // of the anchor line the composer happens to sit on.
            let deletion_snippet = app.focused_deletion_content();
            if let Some(idx) = app.editing_comment {
                // Re-edit: replace the text and refresh its source snippet.
                if let Some(c) = app.comments.get_mut(idx) {
                    c.text = text;
                    c.lines = deletion_snippet
                        .unwrap_or_else(|| app.source.snippet(c.start, c.end));
                }
                app.flash(format!("comment updated ({} total)", app.comments.len()));
            } else {
                let lines = deletion_snippet.unwrap_or_else(|| {
                    app.source
                        .snippet(app.input_start as u32 + 1, app.input_end as u32 + 1)
                });
                let revision = app.current_revision_context();
                app.comments.push(Comment {
                    file_path: app.current_file_path().to_path_buf(),
                    start: app.input_start as u32 + 1,
                    end: app.input_end as u32 + 1,
                    lines,
                    revision,
                    anchor: None,
                    text,
                });
                app.flash(format!("comment added ({} total)", app.comments.len()));
            }
            pin_current_snapshot(app);
            app.editing_comment = None;
            // The view folds cards into its layout; add the new one so a
            // later Tab to view shows it in place.
            replace_view_preserving_cursor(app);
            // Return to where the composer was opened from: source mode
            // (card visible) or view mode (markers update in place).
            app.mode = app.composer_return;
            // The selection was consumed by the comment: clear it so a
            // following j/k moves the cursor instead of extending a range.
            app.selection = None;
            // Back to ASCII so j/k is never captured by the IME.
            app.ime_guard = None;
        }
        KeyCode::Esc => cancel_composer(app),
        // 改行はコメントだけのもの。**問いは 1 行である**（1 行プロンプト
        // には 2 行めを置く場所が無く、Jev へ送る文面にも改行は要らない）。
        // 黙って呑まずに理由を言う — `?` ヘルプが marks の行に
        // `^j newline` を出していないのと揃う。
        KeyCode::Char(c)
            if modifiers.contains(KeyModifiers::CONTROL) && c == 'j' && app.marks_prompt =>
        {
            app.flash_err("ask is one line");
        }
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'j' => {
            app.input.insert(app.input_cursor, '\n');
            app.input_cursor += 1;
        }
        // Readline-style line jumps (Ctrl+a/e mirror Home/End).
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'a' => {
            app.input_cursor = input_line_start(&app.input, app.input_cursor);
        }
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'e' => {
            app.input_cursor = input_line_end(&app.input, app.input_cursor);
        }
        // Ctrl+C cancels like Esc — caught before the catch-all `Char(c)`
        // arm below, which would otherwise insert a `c` into the draft.
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c == 'c' => {
            cancel_composer(app);
        }
        KeyCode::Home => {
            app.input_cursor = input_line_start(&app.input, app.input_cursor);
        }
        KeyCode::End => {
            app.input_cursor = input_line_end(&app.input, app.input_cursor);
        }
        KeyCode::Left => {
            if let Some((i, _)) = app.input[..app.input_cursor].char_indices().next_back() {
                app.input_cursor = i;
            }
        }
        KeyCode::Right => {
            if let Some(ch) = app.input[app.input_cursor..].chars().next() {
                app.input_cursor += ch.len_utf8();
            }
        }
        KeyCode::Up => {
            app.input_cursor = input_move_line(&app.input, app.input_cursor, -1);
        }
        KeyCode::Down => {
            app.input_cursor = input_move_line(&app.input, app.input_cursor, 1);
        }
        KeyCode::Tab => {
            // The composer is mode-independent: Tab flips the pane under it
            // (rendered view ⇄ raw source) while typing continues — the
            // target range stays pinned, only the representation changes.
            // Non-Markdown files are source-only: the flip to view is a
            // no-op there.
            if app.composer_return == Mode::View {
                enter_source_mode(app);
                app.composer_return = Mode::Source;
            } else if supports_view(app.current_file_path()) {
                enter_view_mode(app);
                app.composer_return = Mode::View;
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
            // enter_*_mode set the mode for the plain toggle; composing
            // continues, so stay in Input.
            app.mode = Mode::Input;
            // Flipping to view while re-editing: the view may still carry
            // the edited card (it was rendered before the edit began, or
            // while the composer sat over source mode) — hide it now.
            if app.composer_return == Mode::View && app.editing_comment.is_some() {
                replace_view_preserving_cursor(app);
            }
        }
        // Plain characters only: a Ctrl/Alt chord must never silently
        // type into the draft (Ctrl+w/u/k are readline keys elsewhere,
        // Alt+j/k are review jumps in the other modes — inserting their
        // letters would betray the modifier intent). SHIFT is still
        // accepted: some terminals report capital letters WITH the SHIFT
        // flag (see the n/N arms), so an empty-modifier guard would
        // break uppercase typing on them.
        KeyCode::Char(c) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            app.input.insert(app.input_cursor, c);
            app.input_cursor += c.len_utf8();
        }
        KeyCode::Backspace => {
            if let Some((i, _)) = app.input[..app.input_cursor].char_indices().next_back() {
                app.input.remove(i);
                app.input_cursor = i;
            }
        }
        KeyCode::Delete if app.input_cursor < app.input.len() => {
            app.input.remove(app.input_cursor);
        }
        _ => {}
    }
}

/// Open the inline composer anchored to the current selection (or cursor
/// line). Shared by source-mode `c` and view-mode `c` (which selects the
/// line first).
fn open_composer(app: &mut App, return_to: Mode) {
    if app.source.is_empty() {
        app.flash_err("empty file — nothing to comment");
        return;
    }
    let (start, end) = match app.selection {
        Some(s) => s.range(),
        None => {
            let line = if app.mode == Mode::View {
                app.view.cursor
            } else {
                app.cursor
            };
            (line, line)
        }
    };
    // An EXACT range match flips the composer into re-edit mode: the
    // comment's text is prefilled and Enter replaces it instead of adding
    // a stacked duplicate. Any other range adds a new comment.
    let current = app.current_file_path().to_path_buf();
    let revision = app.current_revision_context();
    let edit_idx = app.comments.iter().position(|c| {
        c.file_path == current
            && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
            && (c.start.saturating_sub(1) as usize) == start
            && (c.end.saturating_sub(1) as usize) == end
    });
    app.editing_comment = edit_idx;
    app.input = edit_idx
        .and_then(|i| app.comments.get(i))
        .map(|c| c.text.clone())
        .unwrap_or_default();
    // The text cursor starts at the end (prefilled text stays intact;
    // arrows/Home/End move into it).
    app.input_cursor = app.input.len();
    app.input_start = start;
    app.input_end = end;
    app.composer_return = return_to;
    app.mode = Mode::Input;
    // Editing from view mode: re-render right away so the edited comment's
    // card disappears (the composer replaces it — no stacked bars). This
    // must run AFTER the mode flip: visible_cards excludes the edited
    // card only while Input is active.
    if edit_idx.is_some() && return_to == Mode::View {
        replace_view_preserving_cursor(app);
    }
    // Optionally force Japanese for typing (--ime=jp); always back to
    // ASCII on close (dropped below) so j/k is never captured.
    app.ime_guard = Some(ime::ImeGuard::enter(app.config.ime));
    // Keep the inline input box on screen (first approximation on the
    // pre-composer layout; the draw pass's per-frame nudge —
    // keep_composer_visible / keep_composer_visible_view — keeps the
    // whole bar visible as the text grows, including on the last line).
    app.cursor = end;
    app.keep_cursor_visible(app.source_viewport_rows() as u16);
}

fn extend_selection(app: &mut App, delta: isize, height: u16) {
    let Some(sel) = &mut app.selection else {
        return;
    };
    if app.source.is_empty() {
        return;
    }
    let max = app.source.len() - 1;
    let next = (sel.cursor as isize + delta).clamp(0, max as isize) as usize;
    sel.cursor = next;
    app.cursor = next;
    app.keep_cursor_visible(height);
}

/// Delete every comment whose anchored range contains the cursor line.
fn delete_comment_at_cursor(app: &mut App) {
    // Both modes delete at their own cursor (the two cursors are the same
    // source line right after a Tab, but drift apart as each mode moves).
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    let before = app.comments.len();
    let current_file = app.current_file_path().to_path_buf();
    let revision = app.current_revision_context();
    app.comments.retain(|c| {
        !(c.file_path == current_file
            && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
            && c.covers(line))
    });
    let removed = before - app.comments.len();
    if removed > 0 {
        // The view folds the cards into its layout; drop the deleted ones.
        replace_view_preserving_cursor(app);
        app.flash(format!("deleted {removed} comment(s)"));
    } else {
        app.flash_err("no comment on this line");
    }
}

/// Quit; with unsent comments, require a second `q` (Esc cancels).
fn request_quit(app: &mut App) {
    if app.comments.is_empty() {
        app.running = false;
        return;
    }
    if app.confirm_quit {
        app.running = false;
    } else {
        app.confirm_quit = true;
        // The persistent prompt banner (prompt_message) carries the
        // message; a duplicate toast is not needed.
    }
}

/// Export all comments. `y` in the comments overlay copies to the
/// clipboard; `s` (body or overlay) additionally
/// delivers via `--send-cmd` (e.g. a herdr pane) and clears the slate —
/// but ONLY on success: a
/// failed send keeps the comments for a retry, and nothing is queued for
/// stdout (terminal output after quit was confusing; pipe via the send
/// command instead, e.g. `--send-cmd "cat >> review.txt"`).
/// `s` / `y` で送る文面。**`y` も `s` も同じ文面である**（貼る先も LLM
/// だから、コピーと送信で中身が違うと「コピーで試した」が試しにならない）。
///
/// review コメントがあれば、書き換えの契約を先頭に足す（4 節「一番怖い
/// ところ」、[`review_contract::compose`]）。無ければ整形した本文のまま —
/// 赤入れだけの送信は段階 1 までと 1 バイトも変わらない。
pub(crate) fn export_text(app: &App) -> String {
    let body = if app.config.reply {
        export::format_all_reply(&app.comments)
    } else {
        export::format_all(&app.comments)
    };
    review_contract::compose(
        body,
        &app.comments,
        app.review_rules.as_ref(),
        &review_contract::contract_text(),
        &review_contract::lint_contract_text(),
    )
}

/// **送れたあとの片付け** — コメントを全部消し、消した本数を返す。
/// `--send-cmd` と `--send-agent` の両方の成功がここを通る。
///
/// 消す前に、送った review / lint コメントを覚える
/// （[`App::note_review_sent`]）。その候補は一覧で `✓` のまま（Sent）で、
/// `a` がもう一度同じ指示を作ることは無い。
pub(crate) fn clear_sent_comments(app: &mut App) -> usize {
    let count = app.comments.len();
    app.note_review_sent();
    app.comments.clear();
    replace_view_preserving_cursor(app);
    count
}

pub(crate) fn export_all(app: &mut App, send: bool) {
    if app.comments.is_empty() {
        app.flash_err("no comments yet");
        return;
    }
    let text = export_text(app);
    let mut parts: Vec<String> = Vec::new();
    let mut had_error = false;
    match export::copy_to_clipboard(&text) {
        Ok(()) => parts.push(format!("copied {} comment(s)", app.comments.len())),
        Err(e) => {
            parts.push(format!("clipboard failed: {e:#}"));
            had_error = true;
        }
    }
    if send {
        match app.config.send_cmd.clone() {
            Some(cmd) => match export::send_command(&cmd, &text) {
                Ok(()) => {
                    let count = clear_sent_comments(app);
                    parts.push(format!("sent via {cmd} · {count} comment(s) cleared"));
                }
                Err(e) => {
                    parts.push(format!("send failed: {e:#} · comments kept"));
                    had_error = true;
                }
            },
            // `--send-agent`: resolve the sole herdr agent in the current
            // tab (else workspace) and submit directly — argv, no shell,
            // so quotes in the export can never break the delivery.
            None if app.config.send_agent => {
                match export::resolve_agent_pane() {
                    Ok(target) => match export::send_to_agent(&target, &text) {
                        Ok(()) => {
                            let count = clear_sent_comments(app);
                            parts.push(format!(
                                "sent to {target} · {count} comment(s) cleared"
                            ));
                        }
                        Err(e) => {
                            parts.push(format!("send failed: {e:#} · comments kept"));
                            had_error = true;
                        }
                    },
                    Err(e) => {
                        parts.push(format!("no agent: {e:#} · comments kept"));
                        had_error = true;
                    }
                }
            }
            None => {
                parts.push("no --send-cmd/--send-agent — nothing sent (y copies)".into());
                had_error = true;
            }
        }
    }
    let joined = parts.join(" · ");
    if had_error {
        app.flash_err(joined);
    } else {
        app.flash(joined);
    }
}

fn move_cursor(app: &mut App, delta: isize, height: u16) {
    if app.source.is_empty() {
        return;
    }
    let max = app.source.len() - 1;
    let next = (app.cursor as isize + delta).clamp(0, max as isize) as usize;
    app.cursor = next;
    app.keep_cursor_visible(height);
}


#[cfg(test)]
mod bar_tests;

#[cfg(test)]
mod state_tests;

#[cfg(test)]
mod marks_tests;

#[cfg(test)]
mod review_anchor_tests;
#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod review_dock_tests;
#[cfg(test)]
mod review_undo_tests;

#[cfg(test)]
mod mouse_tests;

#[cfg(test)]
mod mouse_view_tests;

#[cfg(test)]
mod history_animation_tests;

/// Range decoration in SOURCE mode: `docs/design/range-attribution-plan.md`'s
/// Phase 1 (attribution) and Phase 2 (decoration) applied to the
/// raw-Markdown screen, where the rendered view had them already. The
/// tests here run [`build_rows`] — the real source-mode drawer — and read
/// the styles off the rows it emits.
#[cfg(test)]
mod source_decoration_tests;
#[cfg(test)]
mod color_scheme_tests;
#[cfg(test)]
mod review_render_tests;
