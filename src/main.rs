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
mod effects;
mod export;
mod highlight;
mod history;
mod ime;
mod overlay;
mod reload;
mod render;
mod snapshot;
mod source;
mod theme;
mod timeline;
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
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use tachyonfx::EffectRenderer;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::*;
use crate::chrome::*;
use crate::comment::{Comment, Selection};
use crate::config::{Action, Config};
use crate::highlight::{Highlighter, Span as HiSpan, syntax_for, wrap_spans};
use crate::history::DocumentHistory;
use crate::overlay::*;
use crate::reload::*;
use crate::snapshot::SnapshotCache;
use crate::source::Source;
use similar::{DiffTag, TextDiffConfig};
use crate::view::{
    is_table_delimiter_line, lerp_color, scroll_offset_at, scroll_offset_drag, scroll_thumb,
    GutterCell, ViewState,
};







fn main() -> Result<()> {
    match Config::from_env()? {
        Action::Help => {
            println!(
                "akapen — read markdown rendered, comment on source lines\n\
                 \n\
                 usage: akapen <file...> [--send-cmd <cmd> | --send-agent] [--reply]
                 \x20                         [--theme <name>]\n\
                 \x20                         [--ime <off|ascii|jp>] [--light|--dark]\n\
                 \x20                         [--callback <cmd>]\n\
                 \n\
                 \x20 --send-cmd <cmd>  pipe `s` export to a shell command via stdin\n\
                 \x20 --send-agent      send `s` export to the sole herdr agent in this tab\n\
                 \x20                   (else the sole workspace agent; needs herdr on PATH)\n\
                 \x20 --reply           quote the snippet without file/line\n\
                 \x20                   references; external changes auto-reload\n\
                 \x20                   (instant reply to an agent message; see scripts/akp)\n\
                 \x20 --theme <name>    syntect theme name or path to a .tmTheme file\n\
                 \x20 --ime <off|ascii|jp> input-source control around the composer\n\
                 \x20                   (default ascii; needs swiftc on macOS)\n\
                 \x20 --light           force light mode (default: auto-detect\n\
                 \x20                   the terminal background via OSC 11)\n\
                 \x20 --dark            force dark mode\n\
                 \x20 --no-fx           disable the animated time-machine frame\n\
                 \x20                   (the rotating gradient border while browsing the past)\n\
                 \x20 --no-cursor-anchor stop publishing the hidden cursor position at\n\
                 \x20                   the composer's caret (calms cursor-following\n\
                 \x20                   terminal shaders; the IME composition window\n\
                 \x20                   then loses its anchor)\n\
                 \x20 --callback <cmd>  shell command to spawn on exit\n\
                 \x20                   (e.g. return to a file-picker after quit)\n\
                 \x20 --esc-quit <auto|always|never> whether Esc may quit\n\
                 \x20                   (default auto: only with --callback;\n\
                 \x20                   always = unconditionally, never = Esc\n\
                 \x20                   stays a pure cancel)\n\
                 \n\
                 keys:\n\
                 \x20 view/source:  j/k scroll, Left/Right time travel, g/G top/bottom, PgUp/PgDn, Ctrl+u/Ctrl+d,\n\
                 \x20                v select, c comment, s send, y copy, d delete, e edit,\n\
                 \x20                r reload, a acknowledge/set baseline, n/F7/]c next difference, ]/[ files,\n\
                 \x20                l comments, Ctrl+o files, Tab source mode\n\
                 \x20 source mode:  j/k move, v select, c comment, s send,\n\
                 \x20                y copy, d delete, e edit, r reload, ^n/^p next comment,\n\
                 \x20                a acknowledge/set baseline, n/F7/]c next difference, ]/[ files,\n\
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
        Action::Run(config) => run(config),
    }
}





/// When stdin is not a terminal (xargs gives children /dev/null; scripts
/// redirect it), rebind fd 0 to a real tty so crossterm's event reader
/// can initialize. On macOS this must be the actual pty slave: /dev/tty
/// is a synthetic node that kqueue (mio) rejects with EINVAL, and
/// crossterm's own /dev/tty fallback therefore fails with "Failed to
/// initialize input reader".
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
    // terminal node itself — epoll etc. accept it, so crossterm works.
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
/// alternate screen. `run()` creates it immediately after
/// the no-blink terminal init, so every early error return (a failed
/// `terminal.size()`, an I/O error in `event_loop`) still leaves the
/// shell usable. ratatui 0.30's `Terminal` has no Drop-based restore and
/// the panic hook only fires on panics, so a plain `?` would otherwise
/// exit the process with the terminal stuck in raw mode and echo off.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), Show);
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
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
    app.review_changed = std::mem::take(&mut fs.review_changed);
    app.review_deleted_before = std::mem::take(&mut fs.review_deleted_before);
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
}

impl NoBlinkBackend<std::io::Stdout> {
    /// Enter raw mode + the alternate screen and return a terminal whose
    /// cursor never becomes visible (the analogue of `ratatui::init()`
    /// plus the no-blink promise). The caller's own guard restores the
    /// terminal on exit.
    pub(crate) fn init() -> Result<AppTerminal> {
        ratatui::crossterm::terminal::enable_raw_mode()?;
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::cursor::Hide,
            ratatui::crossterm::terminal::EnterAlternateScreen
        );
        let backend = NoBlinkBackend {
            inner: CrosstermBackend::new(std::io::stdout()),
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
        self.inner.draw(content)
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
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
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

    let mut terminal = NoBlinkBackend::init()?;
    // From here on the terminal is in raw mode + alternate screen; the
    // guard's Drop restores it on every return path, early or normal.
    let terminal_guard = TerminalGuard;
    // Light/dark resolution: --light/--dark win, else the terminal's
    // background is queried (OSC 11). Needs raw mode (init enables it)
    // and must run before the event loop consumes input; unanswerable
    // terminals fall back to dark.
    let light = config
        .light
        .unwrap_or_else(|| theme::detect_light().unwrap_or(false));
    // The syntax theme: --theme wins; a missing or unresolvable name
    // falls back to a default matching light/dark (highlight.rs).
    let highlight = Highlighter::new(config.theme.as_deref(), light);
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
            render_view_with_cards(&source, view_render_width(size.width), &highlight, &[])
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
        if let Some(reviewed) = history.reviewed_content.as_deref() {
            let (changed, deleted, blocks) =
                history::comparison_transition(reviewed, &state.source.content);
            state.review_changed = changed;
            state.review_deleted_before = deleted;
            state.comparison_changed = state.review_changed.clone();
            state.comparison_deleted_before = state.review_deleted_before.clone();
            state.comparison_deleted_blocks = blocks;
        }
    }
    app.histories = histories;
    app.snapshot_cache = snapshot_cache;
    app.file_states = file_states;
    activate_first_file(&mut app);
    // Command mode always runs in ASCII so j/k etc. are never swallowed by
    // the IME. The first attempt may no-op while the helper compiles on
    // first run; the event loop retries until it sticks.
    app.ime_forced = app.ime_session.force_ascii();
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
    let res = event_loop(&mut terminal, &mut app);
    // The guard's Drop performs the whole shutdown (cursor, mouse capture,
    // raw mode, alternate screen). Drop it explicitly BEFORE spawning the
    // callback so the callback inherits a clean terminal; on early error
    // returns the same Drop runs at scope exit instead.
    drop(terminal_guard);
    if let Some(cmd) = &app.config.callback {
        let _ = Command::new("sh").arg("-c").arg(cmd).spawn();
    }
    res
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
/// the kernel releases the controlling terminal. Crossterm never notices
/// — the pty master can stay open (herdr keeps it for scrollback), so
/// reads just block and draws keep succeeding, and no signal arrives.
/// Once the terminal is released, `/dev/tty` stops opening (ENXIO), the
/// only reliable "session is dead" signal from inside the process.
fn controlling_terminal_alive() -> bool {
    std::fs::OpenOptions::new().read(true).open("/dev/tty").is_ok()
}

/// Whether stdin's writer is gone (POLLHUP): a pipe-based virtual
/// terminal (e.g. a herdr plugin pane) whose owner closed. Reads would
/// return EOF forever — crossterm never surfaces that as an event.
fn stdin_hung_up() -> bool {
    let mut pfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: 0,
        revents: 0,
    };
    // SAFETY: poll(2) on fd 0, which is open here (crossterm owns it).
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

    let area = prev.area;
    let mut wrote = false;
    for y in 0..area.height {
        for x in 0..area.width.saturating_sub(1) {
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
        let tick = if app.has_active_fx() { FX_TICK_MS } else { TICK_MS };
        if event::poll(Duration::from_millis(tick))? {
            for _ in 0..MAX_EVENTS_PER_FRAME {
                if !event::poll(Duration::ZERO)? {
                    break;
                }
                match event::read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        let history_direction = history_key_direction(app, key.code, key.modifiers);
                        if let Some(direction) = history_direction {
                            select_history(app, direction);
                        } else if key.kind == KeyEventKind::Press {
                            render_pending_history(app, true);
                            on_key(app, key.code, key.modifiers, Some(terminal));
                        }
                    }
                    // Releasing an arrow does not force an immediate render:
                    // the short settle window intentionally groups quick
                    // taps and holds into one A→D document transition.
                    Event::Key(key) if key.kind == KeyEventKind::Release => {}
                    Event::Mouse(mouse) => {
                        render_pending_history(app, true);
                        on_mouse(app, mouse);
                    }
                    Event::Resize(..) => mark_view_dirty(app),
                    _ => {}
                }
            }
        }
        // Expire a pending `]`/`[` chord into its default file switch.
        expire_pending_chord(app);
        render_history_when_settled(app);
        expire_history_ghosts(app);
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
    // Overlay intercepts its own keys first.
    if app.overlay.is_some() {
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
                jump_review_mark(app, if bracket == ']' { 1 } else { -1 });
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
    // With an overlay open the mouse drives the overlay only: a click
    // outside the panel closes it (modal dismiss), a click on an entry
    // selects it, and the wheel moves the selection (j/k semantics).
    // Nothing touches the content underneath.
    if app.overlay.is_some() {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
                let panel = overlay_panel(Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                });
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
                    let max = help_rows(app.esc_quit_enabled(), app.config.reply, false)
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
                    let max = help_rows(app.esc_quit_enabled(), app.config.reply, false)
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
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
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
            // list, `1/3 files` the file picker, and the path copies the
            // full path to the clipboard. The hit regions come from the
            // same layout math draw_title uses.
            if mouse.row == 0 {
                let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
                match title_hit_at(app, w, mouse.column) {
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
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
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
) -> ViewState {
    let mut view = ViewState::render(source, columns, highlighter);
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
            view.rows.insert(insert_at + i, spans);
            card_rows.insert(insert_at + i, true);
            view.row_segments.insert(insert_at + i, Vec::new());
        }
    }
    view.card_rows = card_rows;
}

/// Render the current complete document with its inline comment cards.
pub(crate) fn render_current_view(app: &App, comments: &[Comment]) -> ViewState {
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let width = view_render_width(w);
    render_view_with_cards(&app.source, width, &app.highlight, comments)
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

/// Recompute the active file's one, cumulative review mark set. Git and the
/// currently displayed historical generation do not affect this baseline.
pub(crate) fn refresh_review_marks(app: &mut App) {
    let index = app.current_file_index;
    let reviewed = app
        .histories
        .get(index)
        .and_then(|history| history.reviewed_content.clone());
    let Some(reviewed) = reviewed else {
        app.review_changed.clear();
        app.review_deleted_before.clear();
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
        app.comparison_deleted_blocks.clear();
        return;
    };
    let (changed, deleted) = history::review_transition(
        supports_view(app.current_file_path()),
        &reviewed,
        &app.histories[index].revisions[0].content,
    );
    app.review_changed = changed;
    app.review_deleted_before = deleted;
    refresh_comparison_marks(app);
}

/// Recompute baseline-relative marks for the complete document currently
/// rendered on screen. This is distinct from the durable NOW review set:
/// historical generations can be inspected without changing what remains
/// unreviewed in the working document.
fn refresh_comparison_marks(app: &mut App) {
    let Some(reviewed) = app
        .history()
        .and_then(|history| history.reviewed_content.as_deref())
    else {
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
        app.comparison_deleted_blocks.clear();
        app.focused_deletion = None;
        return;
    };
    let (changed, deleted, blocks) =
        history::comparison_transition(reviewed, &app.source.content);
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
            ViewState::render(&old_source, view.width.max(1) as u16, highlighter).rows
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
/// (their text ghosts out). The review marks keep the block-level
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
        let ghost = ViewState::render(&source, view.width.max(1) as u16, highlighter);
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
            view.rows.insert(insert_at + offset, row);
            view.row_segments.insert(insert_at + offset, Vec::new());
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
        KeyCode::Char('j') if modifiers.contains(KeyModifiers::ALT) => jump_review_mark(app, 1),
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::ALT) => jump_review_mark(app, -1),
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
        KeyCode::Char('n') => jump_review_mark(app, 1),
        KeyCode::Char('N') => jump_review_mark(app, -1),
        KeyCode::Char('a') => {
            acknowledge_review(app, true);
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
                // the review marks. Review first (`r`), then edit.
                app.flash_err("file changed — r reload first");
            } else if let Some(t) = terminal {
                open_editor(app, t);
            }
        }
        // `]`/`[` arm the chord: alone they switch files (when the
        // [`CHORD_MS`] window expires, or on the next non-chord key); `c`
        // within the window jumps to the next/previous change instead
        // (the F7 fallback). F7/Shift+F7 jump directly.
        KeyCode::F(7) if modifiers.contains(KeyModifiers::SHIFT) => jump_review_mark(app, -1),
        KeyCode::F(7) => jump_review_mark(app, 1),
        KeyCode::Char(']') => app.pending_chord = Some((Instant::now(), ']')),
        KeyCode::Char('[') => app.pending_chord = Some((Instant::now(), '[')),
        // `l` opens the all-comments list; `t` the document timeline;
        // Ctrl+p opens the file picker; `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
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
        KeyCode::Esc => {
            // View is the home mode: Esc cancels the quit confirmation
            // first, then a pending selection (parallel to source mode).
            // With esc-quit enabled the confirmation is the topmost
            // layer — Esc closes it for real, like a second q — and
            // with nothing pending Esc falls through to the quit path.
            if app.confirm_quit {
                if app.esc_quit_enabled() {
                    app.running = false;
                } else {
                    app.confirm_quit = false;
                    app.flash("quit cancelled");
                }
            } else if app.selection.take().is_some() {
                app.flash("selection cancelled");
            } else if app.esc_quit_enabled() {
                request_quit(app);
            }
        }
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

fn review_targets(app: &App) -> Vec<(usize, usize)> {
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

fn active_review_mark_sets(app: &App) -> (HashSet<usize>, HashSet<usize>) {
    (
        app.comparison_changed.clone(),
        app.comparison_deleted_before.clone(),
    )
}

fn jump_review_mark(app: &mut App, dir: isize) {
    // If an arrow scrub selected a generation that has not rendered yet,
    // materialize it before navigating its baseline-relative marks. A mark
    // must always point into the document the user can actually see.
    if app
        .history()
        .is_some_and(|history| history.position != history.rendered_position)
    {
        render_pending_history(app, false);
    }
    let targets = review_targets(app);
    if targets.is_empty() {
        app.flash("no differences from review baseline");
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

pub(crate) fn acknowledge_review(app: &mut App, announce: bool) -> bool {
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
        app.flash_err(format!("review checkpoint failed: {error:#}"));
        return false;
    }
    // Acknowledging or choosing a baseline completes the current review
    // action. Leave SELECT state consistently in both rendered and source
    // modes; Input keeps treating `a` as ordinary text.
    app.selection = None;
    refresh_review_marks(app);
    if announce {
        if let Some(label) = baseline_label {
            app.flash(format!("review baseline set · {label}"));
        } else {
            app.flash("reviewed");
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
        KeyCode::Char('j') if modifiers.contains(KeyModifiers::ALT) => jump_review_mark(app, 1),
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::ALT) => jump_review_mark(app, -1),
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
        KeyCode::Esc => {
            // The quit confirmation is the most urgent state: Esc resolves
            // it before anything else (a pending selection otherwise
            // swallows the first Esc and the prompt feels stuck). With
            // esc-quit enabled the confirmation is the topmost layer —
            // Esc closes it for real, like a second q — and with nothing
            // pending Esc falls through to the quit path. Esc never
            // switches modes — Tab is the one toggle (a mode flip from a
            // reflexive Esc lost the reading position).
            if app.confirm_quit {
                if app.esc_quit_enabled() {
                    app.running = false;
                } else {
                    app.confirm_quit = false;
                    app.flash("quit cancelled");
                }
            } else if app.selection.take().is_some() {
                app.flash("selection cancelled");
            } else if app.deletion_focus().is_some() {
                app.focused_deletion = None;
                app.flash("deletion focus cancelled");
            } else if app.esc_quit_enabled() {
                request_quit(app);
            }
        }
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
                // the review marks. Review first (`r`), then edit.
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
        KeyCode::Char('n') => jump_review_mark(app, 1),
        KeyCode::Char('N') => jump_review_mark(app, -1),
        KeyCode::Char('a') => {
            acknowledge_review(app, true);
        }
        // `]`/`[` arm the chord: alone they switch files (when the
        // [`CHORD_MS`] window expires, or on the next non-chord key); `c`
        // within the window jumps to the next/previous change instead
        // (the F7 fallback). F7/Shift+F7 jump directly.
        KeyCode::F(7) if modifiers.contains(KeyModifiers::SHIFT) => jump_review_mark(app, -1),
        KeyCode::F(7) => jump_review_mark(app, 1),
        KeyCode::Char(']') => app.pending_chord = Some((Instant::now(), ']')),
        KeyCode::Char('[') => app.pending_chord = Some((Instant::now(), '[')),
        // `l` opens the all-comments list; `t` the document timeline;
        // Ctrl+p opens the file picker; `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
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
pub(crate) fn export_all(app: &mut App, send: bool) {
    if app.comments.is_empty() {
        app.flash_err("no comments yet");
        return;
    }
    let text = if app.config.reply {
        export::format_all_reply(&app.comments)
    } else {
        export::format_all(&app.comments)
    };
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
                    let count = app.comments.len();
                    app.comments.clear();
                    replace_view_preserving_cursor(app);
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
                            let count = app.comments.len();
                            app.comments.clear();
                            replace_view_preserving_cursor(app);
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

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    // The real per-frame delta advances the tachyonfx effect timers.
    let now = Instant::now();
    let last_tick = app
        .last_draw
        .map(|t| now.saturating_duration_since(t))
        .unwrap_or(Duration::from_millis(16));
    app.last_draw = Some(now);
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ]);
    let [title, body, footer] = layout.areas(f.area());
    draw_title(f, title, app);
    match app.mode {
        Mode::Input => {
            if app.composer_return == Mode::View {
                draw_view(f, body, app)
            } else {
                draw_source(f, body, app)
            }
        }
        Mode::View => draw_view(f, body, app),
        Mode::Source => draw_source(f, body, app),
    }
    draw_footer(f, footer, app);
    if app.overlay.is_some() {
        draw_overlay(f, app);
    }
    // The browsing timeline bar: drawn after the footer so its state
    // words own the bottom row. It covers the footer, the frame's
    // bottom border (the axis row replaces it), and — on wide terminals
    // — the last content row (the times row). The message row floats
    // one row above it, and the border effect skips the covered row.
    let timeline_on = crate::timeline::timeline_active(app);
    if timeline_on {
        draw_timeline_bar(f, app);
    }
    // The message row floats above everything (overlays included): a
    // notification never displaces content. It renders on the row just
    // above the footer — the frame's bottom border in view mode (the
    // corners stay), the last content row in source mode — so the
    // layout never shifts. The persistent prompt wins over the
    // transient toast: an action that demands the user must not be
    // hidden behind a message that will expire on its own.
    draw_message(f, app);
    // tachyonfx effects repaint after the static UI: the time-machine
    // frame (view mode, browsing the past, `--fx` on, outside the
    // render-complete flash), the appear/ghost scatter effects of the
    // last history render, and the toast fade on the message row (only
    // while the toast is the top message).
    if app.config.fx && app.view_active() {
        crate::effects::set_timeline_bar_visible(timeline_on);
        crate::effects::set_time_depth(travel_depth(app));
        // Same geometry draw_view builds: one column off the left edge,
        // the body's full height.
        let frame = Rect {
            x: body.x + 1,
            y: body.y,
            width: body.width.saturating_sub(1),
            height: body.height,
        };
        if app.is_historical() {
            // The sky is interior-only and never blinks off: the border
            // rotation and the landing pulse repaint only the frame's
            // own cells, so the stars keep twinkling underneath.
            if let Some(effect) = app.starfield_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
            // The border rotation runs continuously through the whole
            // transition; the landing pulse rides ON TOP of it once the
            // replacement settles, so the wave never pauses.
            if let Some(effect) = app.time_machine_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
            if let Some(effect) = app.landing_pulse_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
        }
        // The scatter effects live on the text column (view-relative
        // rows mapped through the current scroll offset); finished
        // effects drop themselves.
        let text_x = frame.x + 2;
        let text_w = frame.width.saturating_sub(4);
        let offset = app.view.offset as isize;
        let viewport = frame.height.saturating_sub(2) as isize;
        let rect_for =
            |row: usize, height: usize| -> Option<Rect> {
                let top = frame.y as isize + 1;
                let y = top + row as isize - offset;
                if y + height as isize <= top || y >= top + viewport {
                    return None; // entirely off-screen
                }
                let y0 = y.max(top) as u16;
                let y1 = (y + height as isize).min(top + viewport).max(y0 as isize + 1) as u16;
                Some(Rect {
                    x: text_x,
                    y: y0,
                    width: text_w,
                    height: (y1 - y0).max(1),
                })
            };
        // The composer owns the text column while it is open: the
        // scatter effects AND the warp pause behind it (render_effect is
        // skipped, so their timers hold) — a transition that was under
        // way when the comment bar opened freezes until Enter/Esc
        // returns to the view. The border rotation keeps flying (it
        // lives in the frame's own cells, never over the text).
        let composing = app.mode == Mode::Input && app.composer_return == Mode::View;
        if !composing {
            // Changed blocks that have scrolled off-screen drop their
            // effects rather than hold them: left un-rendered their
            // timers never advance, so a transition whose changed blocks
            // are all off-screen would never settle (the landing pulse
            // waits on `appear_fx`/`ghost_fx` staying empty) and
            // scrolling the block back in would suddenly start its
            // stream mid-transition. Off-screen changes simply land
            // already-rendered.
            app.appear_fx
                .retain(|(row, height, fx)| !fx.done() && rect_for(*row, *height).is_some());
            for (row, height, effect) in app.appear_fx.iter_mut() {
                if let Some(rect) = rect_for(*row, *height) {
                    f.render_effect(effect, rect, last_tick);
                }
            }
            app.ghost_fx
                .retain(|(row, height, fx)| !fx.done() && rect_for(*row, *height).is_some());
            for (row, height, effect) in app.ghost_fx.iter_mut() {
                if let Some(rect) = rect_for(*row, *height) {
                    f.render_effect(effect, rect, last_tick);
                }
            }
            // The generation warp flies over everything in the frame —
            // content, scatter effects, starfield — the way a window
            // crosses in front of the room. Paused while the composer is
            // open: at its innermost scale the bottom ring crosses the
            // lower text rows, where the comment bar lives.
            if let Some(effect) = app.warp_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
        }
    }
    // The warp is a one-shot flight: drop it when it completes, or when
    // the view goes away mid-flight (left un-rendered it would never
    // finish and would hold the tick rate high forever).
    if app.warp_fx.as_ref().is_some_and(|fx| fx.done())
        || !(app.config.fx && app.view_active())
    {
        app.warp_fx = None;
    }
    // The landing pulse is a one-shot beat too: drop it when it
    // completes, or when the view leaves browsing mid-pulse (left
    // un-rendered it would never finish).
    if app.landing_pulse_fx.as_ref().is_some_and(|fx| fx.done())
        || !(app.config.fx && app.view_active() && app.is_historical())
    {
        app.landing_pulse_fx = None;
    }
    // The timeline bar's own slide-in/out, rendered over its rows (the
    // static bar is drawn above, the shader wipes and reveals it).
    if app.config.fx && timeline_on
        && let Some(effect) = app.timeline_fx.as_mut()
        && !effect.done()
    {
        let rect = timeline_bar_rect(f.area());
        f.render_effect(effect, rect, last_tick);
    }
    if app.timeline_fx.as_ref().is_some_and(|fx| fx.done()) || !timeline_on {
        app.timeline_fx = None;
    }
    if app.status.is_some()
        && prompt_message(app).is_none()
        && let Some(effect) = app.toast_fx.as_mut()
    {
        let row = Rect {
            x: 0,
            y: f.area().height.saturating_sub(if timeline_on { 3 } else { 2 }),
            width: f.area().width,
            height: 1,
        };
        f.render_effect(effect, row, last_tick);
    }
    // Keep the completed frame for the wide-char residue pass: the next
    // draw's wrapper compares it against the freshly drawn frame to blank
    // the right halves of wide characters the diff skipped (see
    // [`clear_wide_char_residue`]).
    app.last_frame = Some(f.buffer_mut().clone());
}








/// The rect of the browsing timeline bar: the bottom two rows, full
/// width — the same rows [`draw_timeline_bar`] paints.
fn timeline_bar_rect(area: Rect) -> Rect {
    let rows = crate::timeline::TIMELINE_BAR_ROWS.min(area.height);
    Rect {
        x: 0,
        y: area.height.saturating_sub(rows),
        width: area.width,
        height: rows,
    }
}

/// The browsing timeline bar: two rows, bottom-anchored — the state
/// words on the footer row (`HERE` at the current revision, `NOW` at
/// the right edge) and the axis on the row
/// above (`●` LOCAL / `◼` COMMIT / `◆` the current point / `▮`
/// baseline, dim left of the review baseline). While a history step is
/// fresh, the scrubber tooltip travels with the `◆` on the message row
/// above the axis (see [`timeline_tooltip_layout`]).
/// Drawn over the static UI before the effects; the slide effect
/// animates it.
/// How deep into the timeline the traveler is: 0 = just behind NOW,
/// 1 = the oldest revision. The LIVE position feeds it (not the
/// rendered one), so the effects already deepen while an arrow is held.
/// The frame effects and the timeline bar's marker colors both scale
/// their drama with it.
fn travel_depth(app: &App) -> f32 {
    app.history()
        .filter(|history| history.revisions.len() > 1)
        .map(|history| history.position as f32 / (history.revisions.len() - 1) as f32)
        .unwrap_or(0.0)
}

fn draw_timeline_bar(f: &mut Frame, app: &App) {
    let width = f.area().width as usize;
    let height = f.area().height;
    if width < 60 || height < 4 {
        return;
    }
    let Some(history) = app.history() else {
        return;
    };
    let Some(layout) = crate::timeline::layout_timeline(history, width) else {
        return;
    };
    let words_row = height.saturating_sub(1);
    let axis_row = height.saturating_sub(2);
    // The scrubber's family colors ride the same depth shift as the
    // frame: the deeper the traveler, the further LOCAL's pink and
    // COMMIT's periwinkle sink toward violet — the bar and the frame
    // read as one instrument. `--no-fx` keeps the static colors.
    let depth_color = |c: Color| -> Color {
        if app.config.fx {
            crate::view::time_machine_depth_shift(app.ui_light, c, travel_depth(app))
        } else {
            c
        }
    };

    // Row 2: the axis — markers over the line. The line is dim left of
    // the review baseline (reviewed history is behind you) and normal
    // from the baseline to NOW (the unreviewed stretch); before the
    // first acknowledgement the whole axis reads normally.
    {
        let by_col: std::collections::HashMap<usize, &crate::timeline::TimelinePoint> =
            layout.points.iter().map(|p| (p.col, p)).collect();
        let past = Style::default().fg(Color::DarkGray);
        let future = Style::default().fg(Color::Gray);
        let mut cells: Vec<(char, Style)> = Vec::with_capacity(width);
        for x in 0..width {
            let cell = match by_col.get(&x) {
                Some(p) if p.current => (
                    '◆',
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Some(p) => match p.kind {
                    // NOW always marks the right edge; a baseline
                    // sitting there needs no marker of its own (the
                    // whole axis reads dim — everything is reviewed).
                    crate::timeline::PointKind::Now => (
                        '●',
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    crate::timeline::PointKind::Local if p.baseline => (
                        '▮',
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    crate::timeline::PointKind::Commit if p.baseline => (
                        '▮',
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    crate::timeline::PointKind::Local => (
                        '●',
                        Style::default().fg(depth_color(crate::view::TIMELINE_LOCAL_COLOR)),
                    ),
                    crate::timeline::PointKind::Commit => (
                        '◼',
                        Style::default().fg(depth_color(crate::view::TIMELINE_COMMIT_COLOR)),
                    ),
                },
                None => (
                    '─',
                    match layout.baseline_col {
                        Some(base_col) if x < base_col => past,
                        _ => future,
                    },
                ),
            };
            cells.push(cell);
        }
        // Merge consecutive identical cells into spans.
        let mut spans: Vec<Span> = Vec::new();
        let mut i = 0;
        while i < cells.len() {
            let (ch, style) = cells[i];
            let mut j = i + 1;
            while j < cells.len() && cells[j] == (ch, style) {
                j += 1;
            }
            spans.push(Span::styled(ch.to_string().repeat(j - i), style));
            i = j;
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: 0,
                y: axis_row,
                width: f.area().width,
                height: 1,
            },
        );
    }

    // Row 3: the state words — nothing else. The `t` affordance is
    // already advertised where it is learned (the normal footer shows
    // `t detail` whenever a timeline exists, and `?` documents the
    // scrub keys), so repeating it here only crowded the NOW corner.
    {
        let mut items: Vec<(usize, String, Style)> = layout
            .words
            .iter()
            .map(|(start, text)| {
                // The "you are here" pair (HERE / NOW) reads bright.
                let style = Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
                (*start, text.clone(), style)
            })
            .collect();
        items.sort_by_key(|(start, _, _)| *start);
        let mut spans: Vec<Span> = Vec::new();
        let mut pos = 0usize;
        for (start, text, style) in items {
            let start = start.min(width);
            if start > pos {
                spans.push(Span::raw(" ".repeat(start - pos)));
            }
            let len = text.len();
            spans.push(Span::styled(text, style));
            pos = start + len;
        }
        if pos < width {
            spans.push(Span::raw(" ".repeat(width - pos)));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: 0,
                y: words_row,
                width: f.area().width,
                height: 1,
            },
        );
    }

    // The scrubber tooltip: the revision readout on the message row,
    // riding over the `◆`. It yields the row entirely
    // while a prompt or toast owns it (a centered banner over a longer
    // tooltip would leave stray fragments at its sides).
    if height >= 5
        && app.status.is_none()
        && prompt_message(app).is_none()
        && let Some((start, parts)) = timeline_tooltip_layout(app, width)
    {
        // The exit dissolve is the drawer's own: over the deadline's
        // final stretch a growing fraction of cells is simply NOT
        // overdrawn, so the document underneath shows through. (A
        // post-hoc tachyonfx dissolve could only blank the band's own
        // cells — its background strip kept sitting over the page.)
        // `--no-fx` never enters the window: its deadline excludes the
        // dissolve, so the band stays whole and pops off.
        let gone = app
            .timeline_tooltip_until
            .map(|until| {
                let remaining = until
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u32;
                if !app.config.fx || remaining >= crate::effects::TOOLTIP_DISSOLVE_MS {
                    0.0
                } else {
                    1.0 - remaining as f32 / crate::effects::TOOLTIP_DISSOLVE_MS as f32
                }
            })
            .unwrap_or(0.0);
        let y = height.saturating_sub(3);
        let buf = f.buffer_mut();
        let mut x = start;
        for (text, style) in parts {
            // Contiguous visible runs render together; a hidden cell
            // breaks the run and leaves the underlying cell untouched.
            let mut run = String::new();
            let mut run_x = x;
            for ch in text.chars() {
                let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                // A stable per-column hash (Knuth multiplicative) sets
                // each cell's dissolve threshold, so cells wink out in
                // a fixed spatially-random order as `gone` rises — no
                // flicker from re-randomizing every frame. Wide chars
                // live or die whole, keyed on their leading column.
                let keep = gone == 0.0
                    || (x.wrapping_mul(2_654_435_761) >> 7) % 1000 >= (gone * 1000.0) as usize;
                if keep {
                    if run.is_empty() {
                        run_x = x;
                    }
                    run.push(ch);
                } else if !run.is_empty() {
                    buf.set_string(run_x as u16, y, &run, style);
                    run.clear();
                }
                x += w;
            }
            if !run.is_empty() {
                buf.set_string(run_x as u16, y, &run, style);
            }
        }
    }
}

/// The scrubber tooltip's anchor and content: `(start column, styled
/// parts)` — `BASELINE · ` when the point is the review baseline, the
/// provenance glyph in its (depth-shifted) family color, `id · age`
/// bright, and the summary dim. LOCAL's long deterministic sentence
/// ("akapen local snapshot: uncommitted state captured …") collapses
/// to "local snapshot" — the age next to it already says when.
/// The band is centered over the `◆` and travels with it, clamped to
/// the terminal edges (one-column margin) so it can never spill; the
/// summary may use whatever width is left and is clipped only by the
/// screen. Every part rides the nebula band ([`tooltip_band_bg`]) —
/// the row floats over document text, and glyphs on the terminal's own
/// background would blend into it.
/// `None` once the hold window has expired or while the cursor sits at
/// NOW.
fn timeline_tooltip_layout(app: &App, width: usize) -> Option<(usize, Vec<(String, Style)>)> {
    app.timeline_tooltip_until
        .filter(|until| Instant::now() < *until)?;
    let history = app.history()?;
    let revision = history.current()?;
    let col = crate::timeline::layout_timeline(history, width)?
        .points
        .iter()
        .find(|p| p.current)?
        .col;
    let (glyph, family) = match revision.source {
        crate::history::RevisionSource::Now => return None,
        crate::history::RevisionSource::Local => ('●', crate::view::TIMELINE_LOCAL_COLOR),
        crate::history::RevisionSource::Git => ('◼', crate::view::TIMELINE_COMMIT_COLOR),
    };
    let glyph_color = if app.config.fx {
        crate::view::time_machine_depth_shift(app.ui_light, family, travel_depth(app))
    } else {
        family
    };
    // The row floats over document text. A black band vanished on dark
    // terminals (their background IS black), so the band wears the
    // time-machine nebula instead: a deep indigo clearly distinct from
    // both the terminal background and the page — the readout is an
    // instrument of the machine, not a line of the document.
    let light = app.ui_light;
    let on_band = Style::default().bg(crate::view::tooltip_band_bg(light));
    let mut parts: Vec<(String, Style)> = Vec::new();
    if history.baseline_position() == Some(history.position) {
        parts.push((
            " BASELINE ·".to_string(),
            on_band
                .fg(crate::view::tooltip_band_baseline_fg(light))
                .add_modifier(Modifier::BOLD),
        ));
    }
    parts.push((format!(" {glyph} "), on_band.fg(glyph_color)));
    let mut head = revision.short_id.clone();
    if let Some(then_ms) = revision.timestamp_ms {
        head.push_str(&format!(
            " · {}",
            crate::history::relative_age(crate::snapshot::now_ms(), then_ms)
        ));
    }
    parts.push((
        head,
        on_band
            .fg(crate::view::tooltip_band_fg(light))
            .add_modifier(Modifier::BOLD),
    ));
    let summary = match revision.source {
        crate::history::RevisionSource::Local => "local snapshot".to_string(),
        _ => revision.summary.clone(),
    };
    let used: usize = parts
        .iter()
        .map(|(text, _)| UnicodeWidthStr::width(text.as_str()))
        .sum();
    let budget = width.saturating_sub(2); // one-column margin each side
    if !summary.is_empty() && budget > used + 3 {
        let clipped = clip_title_label(&summary, budget - used - 3);
        parts.push((
            format!(" · {clipped}"),
            on_band.fg(crate::view::tooltip_band_dim_fg(light)),
        ));
    }
    // The band's closing pad, so the text never ends flush against the
    // page text to its right.
    parts.push((" ".to_string(), on_band));
    // Centered over the ◆, clamped so the band never spills over either
    // edge (right clamp first, then the left margin wins for bands as
    // wide as the screen).
    let total: usize = parts
        .iter()
        .map(|(text, _)| UnicodeWidthStr::width(text.as_str()))
        .sum();
    let start = col
        .saturating_sub(total / 2)
        .min(width.saturating_sub(total + 1))
        .max(1);
    Some((start, parts))
}

/// screen; keyboard navigation, clicks, and the mode handoffs reveal it.
fn review_flags(marks: &HashSet<usize>, line_count: usize) -> Vec<bool> {
    (0..line_count).map(|line| marks.contains(&line)).collect()
}

/// View mode: the native render with a row cursor. No per-frame
/// keep_cursor_visible here — like source mode, the wheel scrolls the
/// viewport only and the cursor is an absolute position that may sit off
/// screen until the next j/k.
fn draw_view(f: &mut Frame, area: Rect, app: &mut App) {
    // View mode always draws the frame: the bordered "page" is the reading
    // mode's visual signature (source mode is the frameless raw editor).
    // The page floats one column off the screen's left edge — a margin so
    // the markers riding the border never touch the terminal edge (the
    // title and footer strips stay full-width).
    let margin = 1;
    let frame = Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin),
        height: area.height,
    };
    // The frame's border is one cell: the reading pane's calm edge.
    let m = 1;
    let inner = Rect {
        x: frame.x + m,
        y: frame.y + m,
        width: frame.width.saturating_sub(m * 2),
        height: frame.height.saturating_sub(m * 2),
    };
    // The text column floats one column off each border: the marker on
    // the left border needs a breath before the text (`> print(...)`), and
    // the right edge gets the same gap, so the paragraph reads as a set
    // column instead of a full-bleed block. The pads live INSIDE the
    // rendered rows (see [`ViewState::visible_text`]); `content` is the
    // width the rows (and the bars) are built at.
    let content = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    // Which source lines carry comments → a per-line flag for the view's
    // marker column (drawn over the frame's left border, so no render
    // width is reserved and the layout never shifts when the first comment
    // is added). Every line of a multi-line comment is flagged; the column
    // renders one marker on each line's first display row only, so the
    // range reads as a clean column instead of a noisy band.
    let visible: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
    let marked = view_marker_flags(&visible, app.source.len(), app.current_file_path());
    // The selection is the LINE range itself (comment-identical);
    // `visible_text` resolves it to the exact spans via the phrase
    // segments.
    let sel = app.selection.map(|s| s.range());
    // View and source read the same baseline → displayed-generation marks.
    let (changed_set, deleted_set) = active_review_mark_sets(app);
    let n = app.source.len();
    let changed = review_flags(&changed_set, n);
    let deleted = review_flags(&deleted_set, n);
    let emphasized = vec![false; n];
    // The composer opened from view mode (`c` in view) is drawn inline
    // right under the cursor line, so the comment can be typed without
    // leaving the rendered view. While it is open it is part of the
    // layout: keep it on screen BEFORE the visible window is built, so
    // the splice below lands on the adjusted offset. The bar extends the
    // view's scrollable extent — a comment on the LAST line would
    // otherwise clamp the scroll at the document's last row and hide the
    // bar — and it grows as you type. Input mode has no scroll keys, so a
    // per-frame nudge cannot fight the user.
    let composing = app.mode == Mode::Input && app.composer_return == Mode::View;
    if composing {
        app.keep_composer_visible_view(inner.height as usize);
    }
    // The frame border reads as the history state while browsing; the
    // landing pulse (an effect) flares over it and settles without ever
    // replacing this color, and the rotation never pauses for it.
    let page_border = if app.is_historical() {
        app.ui_history_border
    } else {
        app.ui_border
    };
    let border_style = Style::default().fg(page_border);
    let (mut text, mut gutter) = app.view.visible_text_with_glow(
        inner.height as usize,
        &marked,
        &changed,
        &[], // the transient glow was retired: the streaming reveal and
             // the frame flash already mark what changed
        &deleted,
        &emphasized,
        sel,
        app.ui_selected_bg,
        app.ui_history_glow_bg,
        border_style,
    );
    if composing {
        let full_width = content.width as usize;
        // Below the selection's rendered block (its END line's block — an
        // upward selection anchors at the range bottom, and a merged last
        // line anchors below the whole paragraph).
        let anchor = view_composer_anchor(app);
        let start_row = anchor.saturating_sub(app.view.offset) + 1;
        let lines = composer_lines(
            &app.input,
            app.input_cursor,
            app.input_start,
            app.input_end,
            full_width,
            app.editing_comment.is_some(),
        );
        if start_row <= text.lines.len() {
            let n = lines.len();
            // The bar floats in the text column like every other row: one
            // pad on each side.
            let padded: Vec<Line> = lines
                .into_iter()
                .map(|line| {
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(line.spans);
                    spans.push(Span::raw(" "));
                    Line::from(spans)
                })
                .collect();
            text.lines.splice(start_row..start_row, padded);
            // The composer rows carry no marker: keep the marker column
            // aligned with the text rows (the border shows through).
            gutter.splice(
                start_row..start_row,
                std::iter::repeat_n(GutterCell::border(border_style), n),
            );
        }
        // Terminal-cursor position inside the bar: on the block caret (the
        // macOS IME anchors its inline composition window here), sharing
        // the drawer's row math via composer_cursor_pos. The hardware
        // cursor is hidden, but the POSITION is published every frame for
        // that anchor — terminals running cursor-following shaders
        // (Ghostty's cursor_blaze etc.) blaze around the motion, so
        // `--no-cursor-anchor` stops publishing (the caret is
        // akapen-drawn and unaffected; only the IME anchor is lost).
        let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
        // Top rule at `start_row`, text rows from `start_row + 1`.
        let row = start_row + 1 + crow;
        if app.config.cursor_anchor && row < inner.height as usize {
            f.set_cursor_position(Position {
                x: content.x + ccol as u16,
                y: inner.y + row as u16,
            });
        }
    }
    // A calm gray frame: the border marks the reading pane without
    // competing with the rendered content. A mid gray — ANSI bright-black
    // sank into the background.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    // The text already starts at `offset` (see [`ViewState::visible_text`]),
    // so no Paragraph scroll is needed — the renderer only ever builds the
    // visible window.
    let p = Paragraph::new(text).block(block);
    f.render_widget(p, frame);
    // The marker column rides the frame's left border: `>` on the cursor
    // row, `▌` on marked rows, the border's `│` everywhere else. Written
    // over the border cells after the frame, so a marker replaces the
    // border glyph in place; the selection background extends over it,
    // running the cursor/selection band to the page edge. (The
    // time-machine gradient is a tachyonfx effect rendered after this
    // pass — it repaints only border-glyph cells, so markers survive.)
    let buf = f.buffer_mut();
    for (i, cell) in gutter.iter().take(inner.height as usize).enumerate() {
        if let Some(c) = buf.cell_mut((frame.x, inner.y + i as u16)) {
            c.set_symbol(cell.glyph);
            c.set_style(cell.style);
        }
    }
    // The scrollbar sits one column inside the frame's right border: a
    // `▐` thumb, mirroring the marker column on the left. The thumb
    // tracks the VIEWPORT offset (wheel scroll moves the viewport only),
    // so it always reflects what is on screen; when the content fits, no
    // scrollbar is drawn and the border stays clean.
    if let Some((start, len)) =
        scroll_thumb(app.view.rows.len(), inner.height as usize, app.view.offset)
    {
        let thumb_fg = app.ui_scrollbar;
        let right = frame.x + frame.width - 2;
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((right, inner.y + i as u16)) {
                // Preserve the cell's current bg (the right pad's bg on
                // selected/cursor rows) so the band runs unbroken.
                let bg = c.style().bg;
                c.set_symbol("▐");
                let mut style = Style::default().fg(thumb_fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
    }
}




















/// Source mode: status/gutter + highlighted content, wrapped by our own
/// width-aware algorithm (ratatui Wrap would double-wrap or misalign CJK).
fn draw_source(f: &mut Frame, area: Rect, app: &mut App) {
    // Source mode never draws a frame: every column goes to the source
    // (the framed view is the reading mode's visual signature). The
    // rightmost column is the scrollbar's track.
    let m = 0;
    let inner = Rect {
        x: area.x + m,
        y: area.y + m,
        width: area.width.saturating_sub(m * 2),
        height: area.height.saturating_sub(m * 2),
    };
    let gutter_cols = 1 + app.source.gutter_width as u16 + 1;
    app.gutter_cols = gutter_cols;
    let content_width = inner.width.saturating_sub(gutter_cols + 1);
    if app.source.is_empty() {
        f.render_widget(Paragraph::new("(empty file)"), area);
        return;
    }
    app.ensure_row_cache(content_width);
    app.refresh_line_rows();
    // While the composer is open, keep its bar on screen: the bar's
    // height is folded into line_rows (it grows as you type) and Input
    // mode has no scrolling keys, so a per-frame nudge cannot fight the
    // user — without it a comment on the last line left the bar below
    // the pane. Wheel/keys keep their own keep_cursor_visible discipline.
    if app.mode == Mode::Input {
        app.keep_composer_visible(inner.height as usize);
    }
    // The cursor is an absolute file position: wheel scroll moves the
    // viewport only, so don't yank the offset back every frame. Keyboard
    // j/k, c, and G call keep_cursor_visible themselves (herdr-review
    // style: scroll away, the cursor stays where it was).

    let (text, composer_cursor) = build_rows(app, inner.height, content_width);
    f.render_widget(Paragraph::new(text), area);
    // The scrollbar rides the pane's own right edge (source mode draws no
    // frame, so unlike view mode there is no border to sit inside — the
    // thumb goes all the way to the last column). The thumb appears only
    // when the content overflows the viewport; it tracks the viewport
    // offset (wheel scroll moves the viewport only), so it always reflects
    // what is on screen.
    if let Some((start, len)) = scroll_thumb(
        app.line_rows.iter().sum(),
        inner.height as usize,
        app.offset,
    ) {
        let thumb_fg = app.ui_scrollbar;
        let right = area.x + area.width - 1;
        let buf = f.buffer_mut();
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((right, inner.y + i as u16)) {
                // Preserve the cell's current bg (the gutter's bg on
                // selected/cursor rows) so the band runs unbroken.
                let bg = c.style().bg;
                c.set_symbol("▐");
                let mut style = Style::default().fg(thumb_fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
    }
    // The logical cursor position is still published every frame even
    // though the hardware cursor is hidden: the macOS IME anchors its
    // inline composition window to this spot, so conversion stays inside
    // the bar and follows the block caret. `--no-cursor-anchor` stops the
    // publishing for terminals whose shaders blaze around cursor motion.
    if app.config.cursor_anchor
        && let Some((col, row)) = composer_cursor
    {
        f.set_cursor_position(Position {
            x: inner.x + col,
            y: inner.y + row,
        });
    }
}

/// Build the visible source-mode rows: `[status][number] ` + content, each
/// source line wrapped to `content_width`, continuation rows bare content.
/// The rows are exactly the viewport window `[offset, offset + height)`:
/// a band straddling the top edge is trimmed to its visible tail, so the
/// screen never drifts from the row math (`keep_cursor_visible`, the
/// scrollbar, and the mouse mapping all count in display rows).
/// Selected lines get the selection background; the cursor line a `>` marker.
/// While the composer is open, also returns where the terminal cursor should
/// sit inside it (body-relative column/row), so the real bar cursor (and the
/// macOS IME's inline composition window) renders inside the bar.
fn build_rows(app: &App, height: u16, content_width: u16) -> (Text<'static>, Option<(u16, u16)>) {
    let mut out: Vec<Line> = Vec::new();
    let mut composer_cursor: Option<(u16, u16)> = None;
    let width = content_width as usize;
    // Baseline → displayed-generation marks. Green `▌` marks a line that
    // is present and changed; the baseline text of every change renders
    // inline as red `▌` deleted rows (see `deleted_block_lines`), so
    // source mode needs no red `▌` position mark — that stays view-only.
    let scoped_added = &app.comparison_changed;
    let landing_pulse = app
        .landing_pulse_until
        .is_some_and(|until| Instant::now() < until);
    // Comment bars span the whole pane (gutter included), pi.dev-style.
    let full_width = (content_width + app.gutter_cols) as usize;
    // The current file's cards (the edited one is hidden while composing).
    let cards = visible_cards(app);
    let mut row = 0usize;
    // Rows of the first emitted band that lie ABOVE the scroll offset:
    // the offset lands mid-band whenever a wrapped line, a comment card,
    // or a deleted block straddles the pane's top edge, but the loop
    // emits whole bands — the surplus is trimmed after the loop.
    let mut skip: Option<usize> = None;
    for idx in 0..app.source.len() {
        let line_rows = app.rows_of(idx);
        if row + line_rows <= app.offset {
            row += line_rows;
            continue;
        }
        if row >= app.offset + height as usize {
            break;
        }
        if skip.is_none() {
            skip = Some(app.offset.saturating_sub(row));
        }
        // The baseline text deleted at this position renders first: red
        // `▌` rows above their anchor line. Blocks that fell past the
        // last line (an EOF deletion) render below its text instead.
        //
        // Lighting rule: the difference is the DIFF PAIR, so deleted rows
        // light up (bright red on the DarkGray cursor band) whenever their
        // anchor line is the cursor line or inside the selection — an
        // `n`-landed rewrite lights old and new content as one block, and
        // j/k passing the anchor lights its deletion too. `n`'s
        // pure-deletion focus additionally moves the `>` glyph onto the
        // first deleted row and renders the (untouched) anchor line as an
        // ordinary line — two cursor bands would fight the eye.
        let deletion_focused = app.deletion_focus() == Some(idx);
        let deletion_lit = deletion_focused
            || idx == app.cursor
            || app.selection.is_some_and(|s| s.contains(idx));
        let (deleted_above, deleted_below) = app.deleted_blocks_at(idx);
        for (i, block) in deleted_above.iter().enumerate() {
            out.extend(deleted_block_lines(
                app,
                &block.content,
                width,
                full_width,
                deletion_lit,
                deletion_focused && i == 0,
            ));
        }
        let selected = app.selection.is_some_and(|s| s.contains(idx));
        let revision = app.current_revision_context();
        let commented = app.comments.iter().any(|c| {
            c.covers(idx)
                && c.file_path == app.current_file_path()
                && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
        });
        let added = scoped_added.contains(&idx);
        let is_cursor = idx == app.cursor && !deletion_focused;
        // `▌` marks a current changed line; deletions are their own rows.
        let cursor_mark = if is_cursor {
            ">"
        } else if added {
            "▌"
        } else {
            " "
        };
        // The cursor line gets the same calm DarkGray background as view
        // mode (text colors untouched — a full-row reversal was fatiguing
        // and clashed with the syntax highlighting). A selected row shares
        // the background; the `>` marker keeps the cursor visible at the
        // selection edge. Present/changed review lines get a restrained
        // green background.
        let cursor_bg = is_cursor || selected;
        let changed_bg = added && !cursor_bg;
        let gutter_style = if cursor_bg {
            Style::default().bg(app.ui_selected_bg)
        } else if changed_bg {
            Style::default().bg(app.ui_changed_bg)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Selected lines: the number turns the composer's cyan — the
        // pending range reads at a glance and matches the color it will be
        // typed under. Commented lines: yellow (no `#` marker), so the
        // gutter shows at a glance which lines carry comments.
        let mut num_style = if selected {
            Style::default().fg(Color::Cyan).bg(app.ui_selected_bg)
        } else if commented && !is_cursor {
            Style::default().fg(Color::Yellow)
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else {
            gutter_style
        };
        // Source has no permanent frame, so its completion signal uses
        // the stable line-number rail instead — the same window as the
        // view mode's landing pulse. It starts only after the newly
        // selected source has already been painted once.
        if landing_pulse {
            num_style = num_style
                .fg(app.ui_landing_pulse)
                .add_modifier(Modifier::BOLD);
        }
        let num = Span::styled(
            format!("{:>width$} ", idx + 1, width = app.source.gutter_width),
            num_style,
        );
        let wrapped = wrap_spans(&app.spans[idx], width);
        // The cursor glyph is bold — it must be findable at a glance
        // (yellow is the comment marker's color), same as view mode. Its
        // color inherits the review mark under it. Mark-less rows keep the
        // classic LightCyan.
        let mark_style = if is_cursor {
            let fg = if added { Color::LightGreen } else { Color::LightCyan };
            let s = Style::default().fg(fg).add_modifier(Modifier::BOLD);
            if cursor_bg { s.bg(app.ui_selected_bg) } else { s }
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else {
            gutter_style
        };
        for (k, frags) in wrapped.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if k == 0 {
                spans.push(Span::styled(cursor_mark, mark_style));
                spans.push(num.clone());
            } else {
                // Continuation rows of a wrapped line: indent by the gutter
                // width so the text stays aligned under the first row's
                // text instead of starting at column 0 under the line
                // number. The reversed/selected background spans the whole
                // continuation row so the cursor/selection reads as one
                // block. A changed line's `▌` repeats on every wrapped row
                // (same color family as the first-row mark), so the
                // left-edge mark runs unbroken — a wrapped changed line or
                // a run of adjacent changed lines reads as one solid band,
                // not a scattered strip of first-row marks.
                let indent_style = if cursor_bg {
                    Style::default().bg(app.ui_selected_bg)
                } else if changed_bg {
                    Style::default().bg(app.ui_changed_bg)
                } else {
                    Style::default()
                };
                // `added` implies one of the bands above, so the mark cell
                // always sits on a painted background. The color mirrors
                // the first-row mark: LightGreen under the cursor glyph,
                // Green on the changed band, and the selection band's
                // plain style when the line is only selected.
                let (mark, mark_style) = if added {
                    if is_cursor {
                        (
                            "▌",
                            Style::default()
                                .fg(Color::LightGreen)
                                .bg(app.ui_selected_bg),
                        )
                    } else if changed_bg {
                        ("▌", Style::default().fg(Color::Green).bg(app.ui_changed_bg))
                    } else {
                        ("▌", Style::default().bg(app.ui_selected_bg))
                    }
                } else {
                    (" ", Style::default())
                };
                spans.push(Span::styled(mark, mark_style));
                spans.push(Span::styled(
                    " ".repeat(app.gutter_cols as usize - 1),
                    indent_style,
                ));
            }
            for f in frags {
                let style = if cursor_bg {
                    f.style.bg(app.ui_selected_bg)
                } else if changed_bg {
                    f.style.bg(app.ui_changed_bg)
                } else {
                    f.style
                };
                spans.push(Span::styled(f.text.clone(), style));
            }
            // A cursor/selection/changed row's background runs to the
            // RIGHT EDGE of the pane (same calm color, pi.dev-style band)
            // — the text extent alone made the selection ragged and easy
            // to misread. This also covers blank lines (no fragments)
            // with no extra glyph.
            if cursor_bg || changed_bg {
                let used: usize = spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let bg = if cursor_bg {
                    app.ui_selected_bg
                } else {
                    app.ui_changed_bg
                };
                if used < full_width {
                    spans.push(Span::styled(
                        " ".repeat(full_width - used),
                        Style::default().bg(bg),
                    ));
                }
            }
            out.push(Line::from(spans));
        }
        for (i, block) in deleted_below.iter().enumerate() {
            out.extend(deleted_block_lines(
                app,
                &block.content,
                width,
                full_width,
                deletion_lit,
                deletion_focused && deleted_above.is_empty() && i == 0,
            ));
        }
        // Inline comment bars: one per comment ending on this line, stacked
        // (current file only; the one being re-edited is hidden under the
        // edit composer).
        for c in cards
            .iter()
            .filter(|c| c.end as usize - 1 == idx)
        {
            out.extend(comment_bar_lines(c, full_width));
        }
        // The composer bar while typing, under the target range's last line.
        if app.mode == Mode::Input && app.input_end == idx {
            let start_row = out.len();
            out.extend(composer_lines(
                &app.input,
                app.input_cursor,
                app.input_start,
                app.input_end,
                full_width,
                app.editing_comment.is_some(),
            ));
            // Terminal-cursor position inside the bar: on the block caret.
            // The macOS IME draws its inline composition window at this
            // spot too. composer_cursor_pos shares the drawer's row math,
            // so the anchor can never drift from the caret.
            let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
            // Top rule at `start_row`, text rows from `start_row + 1`.
            composer_cursor = Some((ccol as u16, (start_row + 1 + crow) as u16));
        }
        row += line_rows;
    }
    // Trim the top band's rows above the offset and cap at the viewport.
    // Without the trim everything on screen sits `offset - band_start`
    // rows LOWER than the row math believes, and a cursor computed to be
    // on the pane's last row renders below the bottom edge — off screen.
    let skip = skip.unwrap_or(0).min(out.len());
    if skip > 0 {
        out.drain(..skip);
    }
    out.truncate(height as usize);
    // The composer-cursor row was recorded in emitted-band coordinates:
    // shift it by the trim, and drop it once it leaves the viewport (the
    // IME anchor must never point at a row that is not on screen).
    let composer_cursor = composer_cursor.and_then(|(col, r)| {
        let r = (r as usize).checked_sub(skip)?;
        (r < height as usize).then_some((col, r as u16))
    });
    (Text::from(out), composer_cursor)
}

/// The rows one deleted block paints: per baseline line, a red `▌` mark
/// and a blank number column on the first wrapped row (the line has no
/// number in the displayed document), a gutter-width indent on
/// continuation rows, and the text in red on the full-row red band —
/// the diff-pair partner of the green `▌` changed band. Display-only
/// rows: no selection or comment treatment applies. While `n`'s deletion
/// focus is on the block (`focused`), the rows take the cursor's own
/// visual language — bright red text on the DarkGray cursor band running
/// the full pane width, with the `>` glyph on the first row when this
/// block leads the focused set (`cursor_glyph`). The row count must
/// match [`crate::app::deleted_block_rows`] — same per-line [`wrap_spans`]
/// at the same width.
fn deleted_block_lines(
    app: &App,
    content: &str,
    width: usize,
    full_width: usize,
    focused: bool,
    cursor_glyph: bool,
) -> Vec<Line<'static>> {
    // Deleted rows are the diff pair of the green `▌` changed band: the
    // static state is a full-row red band (same restraint, same extent).
    // While lit, the gray cursor band replaces the red band across the
    // whole row and the text brightens.
    let text_style = if focused {
        Style::default()
            .fg(Color::LightRed)
            .bg(app.ui_selected_bg)
    } else {
        Style::default().fg(Color::Red).bg(app.ui_deleted_bg)
    };
    let mark_style = if focused {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD)
            .bg(app.ui_selected_bg)
    } else {
        Style::default().fg(Color::Red).bg(app.ui_deleted_bg)
    };
    let fill_style = if focused {
        Style::default().bg(app.ui_selected_bg)
    } else {
        Style::default().bg(app.ui_deleted_bg)
    };
    let mut out = Vec::new();
    let mut first_row = true;
    for line in content.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: line.to_string(),
                style: text_style,
            }],
            width,
        );
        for (k, frags) in wrapped.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if k == 0 {
                let mark = if cursor_glyph && first_row { ">" } else { "▌" };
                spans.push(Span::styled(mark, mark_style));
                spans.push(Span::styled(
                    " ".repeat(app.source.gutter_width + 1),
                    fill_style,
                ));
            } else {
                // The red `▌` repeats on every wrapped row, exactly like a
                // green changed line's continuation rows: the left-edge
                // mark runs unbroken down the whole block.
                spans.push(Span::styled("▌", mark_style));
                spans.push(Span::styled(
                    " ".repeat(app.gutter_cols as usize - 1),
                    fill_style,
                ));
            }
            for f in frags {
                spans.push(Span::styled(f.text.clone(), f.style));
            }
            // The band runs to the pane's right edge, exactly like the
            // cursor/selection band on ordinary rows — and the green `▌`
            // band on changed rows: the red band is its diff-pair
            // partner, so a rewrite reads as one matched block. The
            // focused state keeps the same extent (the gray cursor band
            // replaces the red one).
            let used: usize = spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if used < full_width {
                spans.push(Span::styled(
                    " ".repeat(full_width - used),
                    fill_style,
                ));
            }
            out.push(Line::from(spans));
            first_row = false;
        }
    }
    out
}

/// Display rows a saved-comment bar occupies under its anchor line
/// (top rule + wrapped body + bottom rule). Must match
/// [`comment_bar_lines`].
pub(crate) fn card_line_count(c: &Comment, full_width: usize) -> usize {
    let body: usize = c
        .text
        .split('\n')
        .map(|l| {
            wrap_spans(
                &[HiSpan {
                    text: l.to_string(),
                    style: Style::default(),
                }],
                full_width,
            )
            .len()
        })
        .sum();
    body.max(1) + 2
}

/// The composer's wrapped body rows as plain strings: the input split
/// into logical lines and wrapped to `full_width`, WITHOUT any cursor
/// glyph. The body is stable no matter where the insertion point sits, so
/// moving the cursor can never reflow the bar. (The old design embedded a
/// `▏` glyph in the text before wrapping: the near-invisible 1/8-width
/// sliver read as a half-width space, and wide-char + glyph interplay at
/// width boundaries flipped the row segmentation — "a half-width space
/// slips in and it shifts".) The caret is painted afterwards by
/// [`cursor_caret_line`] at the column [`composer_cursor_pos`] reports.
fn composer_body_rows(text: &str, full_width: usize) -> Vec<String> {
    // The cursor glyph is deliberately NOT part of this wrap: the body
    // must be stable no matter where the insertion point sits, so moving
    // the cursor can never reflow the bar. (Embedding the `▏` in the text
    // before wrapping made a wide char + glyph interplay leave a stray
    // blank cell at width boundaries, and flipped the segmentation as the
    // cursor crossed a wrap — "a half-width space slips in and it shifts".)
    // [`composer_lines`] overlays the glyph afterwards at the column
    // [`composer_cursor_pos`] reports, so the IME anchor still lands on it.
    let mut rows = Vec::new();
    for logical in text.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            rows.push(row.iter().map(|s| s.text.as_str()).collect::<String>());
        }
    }
    rows
}

/// The caret's cell inside the composer body: (body row, display
/// column). Row 0 is the first text row under the top rule. Computed by
/// wrapping the PREFIX `text[..cursor]` with the same wrap the body uses,
/// so the insertion point — and the IME anchor — exactly tracks the drawn
/// glyph without reflowing the wrapped rows. A prefix that ends flush at
/// the width boundary puts the cursor at the start of the next row.
fn composer_cursor_pos(text: &str, cursor: usize, full_width: usize) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    // Guard: `cursor` must be on a char boundary to slice the prefix.
    let cursor = (0..=cursor)
        .rev()
        .find(|&c| text.is_char_boundary(c))
        .unwrap_or(0);
    let prefix = &text[..cursor];
    let mut rows: Vec<String> = Vec::new();
    for logical in prefix.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            rows.push(row.iter().map(|s| s.text.as_str()).collect::<String>());
        }
    }
    let Some(last) = rows.last() else {
        return (0, 0);
    };
    let col = UnicodeWidthStr::width(last.as_str());
    let row = rows.len() - 1;
    // An insertion point flush at the width boundary sits on the NEXT row
    // (the character after it would wrap there). [`composer_lines`] appends
    // a caret-only row when that next row does not exist (input ending
    // exactly at the boundary) so the caret always has a cell.
    if col >= full_width {
        (row + 1, 0)
    } else {
        (row, col)
    }
}

/// The composer's body rows plus the caret cell, with a caret-only
/// trailing row appended when the insertion point lands flush at the
/// width boundary and there is no next row. Both [`composer_lines`] and
/// [`composer_line_count`] go through here, so height and render always
/// agree.
fn composer_body_with_caret(
    text: &str,
    cursor: usize,
    full_width: usize,
) -> (Vec<String>, (usize, usize)) {
    let mut body = composer_body_rows(text, full_width);
    let (mut crow, ccol) = composer_cursor_pos(text, cursor, full_width);
    if crow >= body.len() {
        body.push(String::new());
        crow = body.len() - 1;
    }
    (body, (crow, ccol))
}

/// Render the cursor as a background block over the character at the
/// insertion point (classic block caret) instead of inserting a `▏`
/// glyph. The old glyph was a LEFT ONE EIGHTH BLOCK — a 1/8-wide sliver
/// that most terminal fonts render as a nearly invisible gap, so the
/// caret read as "a half-width space slipped in" and the insertion point
/// was untraceable (`テストコ▏メント。` → `コ` and `メ` looked
/// disconnected). A background block on the existing character needs no
/// glyph: nothing can render as a stray space, nothing shifts, and the
/// caret is unmistakable.
fn cursor_caret_line(row: &str, col: usize) -> Line<'static> {
    // Composer cyan on black type: the block caret inverts the char under
    // it, readable in dark and light themes alike.
    let caret = Style::default().fg(Color::Black).bg(Color::Cyan);
    let mut width = 0usize;
    let mut caret_at = row.len();
    for (byte, ch) in row.char_indices() {
        if width >= col {
            caret_at = byte;
            break;
        }
        width += UnicodeWidthChar::width(ch).unwrap_or(0);
        caret_at = byte + ch.len_utf8();
    }
    if caret_at < row.len() {
        // Block over the character sitting at the insertion point.
        let ch = row[caret_at..].chars().next().unwrap();
        let end = caret_at + ch.len_utf8();
        Line::from(vec![
            Span::raw(row[..caret_at].to_string()),
            Span::styled(ch.to_string(), caret),
            Span::raw(row[end..].to_string()),
        ])
    } else {
        // End of the line: a thin underline caret (a filled block cell
        // there reads as a bulky full-width square — the "ダサい" initial
        // cursor on an empty draft). Cyan underline on the empty cell
        // where the next character — or the IME composition — lands.
        Line::from(vec![
            Span::raw(row.to_string()),
            Span::styled(
                " ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ])
    }
}

/// Display rows the composer bar occupies (top rule + wrapped input +
/// bottom rule). Independent of the cursor position: the body is wrapped
/// without the cursor glyph, so the height never changes as you move the
/// insertion point (and never differs between count and render).
pub(crate) fn composer_line_count(text: &str, cursor: usize, full_width: usize) -> usize {
    let (body, _) = composer_body_with_caret(text, cursor, full_width);
    body.len().max(1) + 2
}

/// A saved comment as a full-width bar: top and bottom rules only (no side
/// borders, no background fill), so it reads like a speech bubble whose
/// rules run to both screen edges. The rule color shows the state — saved
/// comments are dark gray with a yellow title, composing is cyan.
fn comment_bar_lines(c: &Comment, full_width: usize) -> Vec<Line<'static>> {
    let rule = Style::default().fg(Color::DarkGray);
    let title = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let label = format!(" comment · {} ", c.range_label());
    let fill = "─".repeat(full_width.saturating_sub(label.width()));
    let mut lines = vec![Line::from(vec![
        Span::styled(label, title),
        Span::styled(fill, rule),
    ])];
    for logical in c.text.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            let piece: String = row.iter().map(|s| s.text.as_str()).collect();
            lines.push(Line::from(Span::raw(piece)));
        }
    }
    lines.push(Line::from(Span::styled("─".repeat(full_width), rule)));
    lines
}

/// The inline comment input bar: the same top/bottom rules in cyan while
/// composing. The block caret sits at the insertion point, painted over
/// the character there (the hardware cursor stays hidden — see run();
/// this is the modern-TUI pattern, language-independent and immune to the
/// IME commit drift of a real cursor). `editing` flips the label to
/// `edit` when the composer is replacing an existing comment.
fn composer_lines(
    text: &str,
    cursor: usize,
    start: usize,
    end: usize,
    full_width: usize,
    editing: bool,
) -> Vec<Line<'static>> {
    let rule = Style::default().fg(Color::Cyan);
    let title = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let word = if editing { "edit" } else { "comment" };
    let label = if start == end {
        format!(" {word} · {} ", start + 1)
    } else {
        format!(" {word} · {}-{} ", start + 1, end + 1)
    };
    let fill = "─".repeat(full_width.saturating_sub(label.width()));
    let mut lines = vec![Line::from(vec![
        Span::styled(label, title),
        Span::styled(fill, rule),
    ])];
    // Wrap the text without any glyph, then paint a background block
    // caret over the character at the insertion point — the body rows stay
    // stable as the cursor moves, and the caret (unlike a glyph) can never
    // read as a stray space or shift the layout.
    let (body, (cursor_row, cursor_col)) =
        composer_body_with_caret(text, cursor, full_width);
    for (i, piece) in body.into_iter().enumerate() {
        if i == cursor_row {
            lines.push(cursor_caret_line(&piece, cursor_col));
        } else {
            lines.push(Line::from(Span::raw(piece)));
        }
    }
    lines.push(Line::from(Span::styled("─".repeat(full_width), rule)));
    lines
}

#[cfg(test)]
mod bar_tests {
    use super::*;
    use crate::comment::Comment;

    fn test_comment(text: &str) -> Comment {
        Comment {
            file_path: "/tmp/x.md".into(),
            start: 1,
            end: 2,
            text: text.into(),
            lines: text.into(),
            revision: None,
        }
    }

    #[test]
    fn comment_bar_lines_have_rules_only() {
        // Top rule (title + fill), text row, bottom rule. No side borders,
        // no background fill — the rules run to both screen edges.
        let lines = comment_bar_lines(&test_comment("テスト"), 80);
        assert_eq!(lines.len(), 3, "top rule + text + bottom rule");
        let label = " comment · 1-2 ";
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.starts_with(label), "top rule starts with the title");
        assert_eq!(
            top.chars().filter(|&c| c == '─').count(),
            80 - label.chars().count(),
            "top rule fill spans the rest of the pane"
        );
        let bottom: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            bottom.chars().filter(|&c| c == '─').count(),
            80,
            "bottom rule spans the full pane"
        );
        assert!(lines.iter().all(|l| l.width() <= 80));
    }

    #[test]
    fn composer_lines_have_rules_and_cursor() {
        let text = "テスト";
        let lines = composer_lines(text, text.len(), 0, 0, 80, false);
        assert_eq!(lines.len(), 3, "top rule + input + bottom rule");
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.starts_with(" comment · 1 "), "add label: {top}");
        // Re-edit mode flips the label to `edit`.
        let lines = composer_lines(text, text.len(), 2, 4, 80, true);
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.starts_with(" edit · 3-5 "), "edit label: {top}");
        // No glyph is inserted into the text: the body row holds the raw
        // input plus the end-of-text caret as a filled cell.
        let text_row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text_row, "テスト ", "body + filled caret cell, no glyph");
        let pos = caret_in(&lines[1]).expect("block caret on the last text row");
        assert_eq!(pos.1, " ", "end-of-text caret fills the next cell");
        let bottom: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            bottom.chars().filter(|&c| c == '─').count(),
            80,
            "bottom rule spans the full pane"
        );
    }

    /// The styled caret of a composer line: (display column of the caret,
    /// its content). The block caret is fg Black on bg Cyan; the
    /// end-of-line caret is a cyan underline.
    fn caret_in(line: &Line<'static>) -> Option<(usize, String)> {
        let caret = |span: &Span<'static>| {
            span.style.bg == Some(Color::Cyan)
                || (span.style.fg == Some(Color::Cyan)
                    && span
                        .style
                        .add_modifier
                        .contains(Modifier::UNDERLINED))
        };
        let mut col = 0usize;
        for span in &line.spans {
            if caret(span) {
                return Some((col, span.content.to_string()));
            }
            col += UnicodeWidthStr::width(span.content.as_ref());
        }
        None
    }

    #[test]
    fn cursor_movement_does_not_reflow_the_composer() {
        // The caret is painted over the wrapped body, never part of the
        // wrap: moving the insertion point must leave the rows identical
        // and only slide the caret. (The old embedding re-wrapped on
        // every move and left a stray half-width blank at width
        // boundaries.)
        let text = "あいうえおかきくけこさしすせそ".to_string();
        let width = 24;
        let mut bases: Vec<String> = Vec::new();
        for cursor in [3, 9, 12, 15, 18, 21, 30] {
            let rows = composer_lines(&text, cursor, 0, 0, width, false);
            let body_lines: Vec<&Line<'_>> = rows[1..rows.len() - 1].iter().collect();
            let carets: Vec<_> = body_lines
                .iter()
                .filter_map(|l| caret_in(l))
                .collect();
            assert_eq!(carets.len(), 1, "cursor={cursor}: exactly one caret");
            let (crow, ccol) = composer_cursor_pos(&text, cursor, width);
            // The caret sits on body row `crow` at display column `ccol`
            // (or on the caret-only trailing row when the input ends
            // flush at the width boundary — composer_lines already
            // appended it, so rows[1 + crow] exists).
            let (c_crow, c_ccol) = if crow >= body_lines.len() {
                (body_lines.len() - 1, 0)
            } else {
                (crow, ccol)
            };
            let on_row = caret_in(body_lines[c_crow])
                .expect("caret on the reported row")
                .0;
            assert_eq!(on_row, c_ccol, "cursor={cursor}: caret column matches");
            let body: String = body_lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            bases.push(body);
        }
        assert!(
            bases.iter().all(|b| *b == bases[0]),
            "the wrapped body is stable across cursor moves:\n{:?}",
            bases
        );
    }

    #[test]
    fn composer_caret_follows_the_cursor() {
        // The block caret covers the character at the insertion point.
        let lines = composer_lines("abc", 1, 0, 0, 80, false);
        assert_eq!(caret_in(&lines[1]), Some((1, "b".to_string())));
        // End of text: a filled block cell after the last character.
        let lines = composer_lines("abc", 3, 0, 0, 80, false);
        assert_eq!(caret_in(&lines[1]), Some((3, " ".to_string())));
        assert_eq!(composer_cursor_pos("abc", 1, 80), (0, 1));
        // Multi-line: the caret (and the IME anchor) lands on the second
        // logical line's row.
        assert_eq!(composer_cursor_pos("ab\ncd", 4, 80), (1, 1));
        // CJK before the cursor counts display width, not chars.
        assert_eq!(composer_cursor_pos("あい", 3, 80), (0, 2));
        // A user `▏` in the text is just a width-1 character: the caret
        // sits before/after it exactly where the cursor is, with no
        // glyph-counting ambiguity.
        assert_eq!(composer_cursor_pos("x▏y", 5, 80), (0, 3));
        assert_eq!(composer_cursor_pos("x▏y", 1, 80), (0, 1));
        assert_eq!(composer_cursor_pos("x▏y", 4, 80), (0, 2));
    }

    #[test]
    fn composer_tabs_expand_so_the_cursor_column_matches() {
        // The caret column is measured on the wrapped text; a pasted tab
        // expands to the terminal's 8-column stop, so the cursor lands
        // after the visible text instead of after a 0-width tab.
        let lines = composer_lines("a\tb", 3, 0, 0, 80, false);
        let text_row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text_row, "a       b ", "tab expanded + end caret cell");
        assert_eq!(caret_in(&lines[1]), Some((9, " ".to_string())));
    }

    #[test]
    fn line_counts_match_bar_height() {
        let c = test_comment("テスト");
        assert_eq!(card_line_count(&c, 80), comment_bar_lines(&c, 80).len());
        let text = "テスト";
        assert_eq!(
            composer_line_count(text, text.len(), 80),
            composer_lines(text, text.len(), 0, 0, 80, false).len()
        );
        // Mid-text cursor: the count and the render still agree.
        assert_eq!(
            composer_line_count(text, 3, 80),
            composer_lines(text, 3, 0, 0, 80, false).len()
        );
        // Wrapped body: long text adds rows to both the count and the render.
        let c = test_comment(&"あ".repeat(100));
        assert_eq!(card_line_count(&c, 40), comment_bar_lines(&c, 40).len());
    }

    #[test]
    fn index_at_abs_maps_bars_to_their_line() {
        // rows: 行1(1), 行2(1), 行3(1)+バー(3)=4, 行4(1), 行5(1)
        let rows = vec![1, 1, 4, 1, 1];
        // offset=0: absolute rows 0..8
        assert_eq!(index_at_abs(&rows, 0, 0), Some(0), "line 1");
        assert_eq!(index_at_abs(&rows, 0, 1), Some(1), "line 2");
        assert_eq!(index_at_abs(&rows, 0, 2), Some(2), "line 3");
        assert_eq!(index_at_abs(&rows, 0, 3), Some(2), "bar top -> line 3");
        assert_eq!(index_at_abs(&rows, 0, 5), Some(2), "bar bottom -> line 3");
        assert_eq!(index_at_abs(&rows, 0, 6), Some(3), "line 4");
        assert_eq!(index_at_abs(&rows, 0, 7), Some(4), "line 5");
        assert_eq!(index_at_abs(&rows, 0, 8), None, "past the end");
    }

    #[test]
    fn index_at_abs_is_stable_across_scroll_offsets() {
        // Regression: the mapping used to compare the viewport-relative
        // display_row against absolute row spans, so after scrolling
        // (offset > 0) a click landed `offset` lines too high — worst at
        // the scroll stops (top/bottom).
        let rows = vec![1, 1, 4, 1, 1, 1, 1];
        // The same absolute row must map to the same line at any offset.
        for offset in 0..4 {
            // Absolute row 6 is line 4 (offset 0: abs 6 -> idx 3).
            assert_eq!(
                index_at_abs(&rows, offset, 6),
                Some(3),
                "abs row 6 at offset {offset}"
            );
            // Absolute row 4 (bar body) belongs to line 3.
            assert_eq!(
                index_at_abs(&rows, offset, 4),
                Some(2),
                "abs row 4 at offset {offset}"
            );
        }
        // Scrolled to the bottom stop: last viewport row maps to the last
        // line, not `offset` lines above it.
        let last_abs = rows.iter().sum::<usize>() - 1;
        assert_eq!(
            index_at_abs(&rows, 4, last_abs),
            Some(rows.len() - 1),
            "bottom stop maps to the last line"
        );
    }
}

#[cfg(test)]
mod state_tests;

#[cfg(test)]
mod mouse_tests {
    use super::*;
    use crate::comment::Comment;
    use crate::config::{Config, EscQuit};
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::view::ViewState;
    use std::io::Write;

    fn test_app() -> App {
        // A tempdir, not a fixed name: the fixed $TMPDIR path used to
        // collide when several worktrees ran cargo test at once (each
        // process truncated the file under the others), flaking the
        // mouse-mapping tests. The dir may drop after load — the source
        // is already in memory.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("row_at_test.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=8 {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 3; // 1 + gutter_width(1) + 1
        app.ensure_row_cache(75);
        app.comments.push(Comment {
            file_path: path.clone(),
            start: 5,
            end: 5,
            text: "テスト".into(),
            lines: "テスト".into(),
            revision: None,
        });
        app.refresh_line_rows();
        app
    }

    #[test]
    fn wrapped_continuation_rows_indent_under_the_gutter() {
        // A long first line wraps; continuation rows must start with the
        // gutter width of spaces so text stays aligned under the first
        // row's text instead of under the line number.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrap_indent_test.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", "あ".repeat(40)).unwrap(); // 80 cols -> wraps at 57
        writeln!(f, "short").unwrap();
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 57, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(57);
        app.refresh_line_rows();
        let (text, _) = build_rows(&app, 20, 57);
        let row0: String = text.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let row1: String = text.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            row0.starts_with(">1 "),
            "first row shows the gutter, got {row0:?}"
        );
        assert!(
            row1.starts_with("   "),
            "continuation row indents by the gutter width, got {row1:?}"
        );
        assert!(
            !row1.starts_with('>'),
            "no gutter markers on continuation rows, got {row1:?}"
        );
    }

    /// An app whose line 1 wraps into 2 display rows at width 57,
    /// followed by `short_lines` one-row lines ("line2"..).
    fn wrap_app(short_lines: usize) -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrap_scroll_test.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", "あ".repeat(40)).unwrap(); // 80 cols -> 2 rows at 57
        for i in 2..=short_lines + 1 {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 57, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 1 + app.source.gutter_width as u16 + 1;
        app.ensure_row_cache(57);
        app.refresh_line_rows();
        app
    }

    fn row_string(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn mid_band_offset_trims_the_top_rows() {
        // The scroll offset lands on line 1's SECOND wrapped row: the
        // window must start at that continuation row, not re-emit the
        // whole band from the pane top (which shifted everything down
        // and pushed the bottom row — the cursor's — off screen).
        let mut app = wrap_app(9);
        assert_eq!(app.rows_of(0), 2, "premise: line 1 wraps into 2 rows");
        app.offset = 1;
        let (text, _) = build_rows(&app, 5, 57);
        assert_eq!(text.lines.len(), 5, "exactly the viewport window");
        let row0 = row_string(&text.lines[0]);
        assert!(
            !row0.contains(" 1 "),
            "the band's first row is above the offset and must be \
             trimmed, got {row0:?}"
        );
        assert!(
            row_string(&text.lines[1]).contains("line2"),
            "line 2 sits right under the continuation row"
        );
        assert!(
            row_string(&text.lines[4]).contains("line5"),
            "the window's last row is offset+height-1, not clipped away"
        );
    }

    #[test]
    fn cursor_stays_inside_the_window_scrolling_down() {
        // j all the way down: every step keeps the cursor band inside
        // [offset, offset+viewport) even while a wrapped line straddles
        // the top edge (the original bug: the cursor drifted below the
        // bottom edge and came back only at band boundaries).
        let mut app = wrap_app(9);
        let viewport = 4u16;
        for _ in 0..app.source.len() {
            move_cursor(&mut app, 1, viewport);
            let start = app.row_of(app.cursor);
            let end = start + app.rows_of(app.cursor);
            assert!(
                start >= app.offset && end <= app.offset + viewport as usize,
                "cursor band [{start},{end}) escaped the window \
                 [{},{})",
                app.offset,
                app.offset + viewport as usize
            );
            // And the rendered window agrees: the `>` glyph is on screen.
            let (text, _) = build_rows(&app, viewport, 57);
            assert!(
                text.lines.iter().any(|l| row_string(l).starts_with('>')),
                "the rendered window must contain the cursor glyph"
            );
        }
    }

    #[test]
    fn keep_cursor_visible_prefers_the_band_start_when_taller_than_viewport() {
        // A card stack taller than the viewport: the whole band cannot
        // fit, so the band's START (the `>` row) wins over its end.
        let mut app = test_app();
        app.comments[0].text = "x\n".repeat(30).trim_end().to_string();
        app.refresh_line_rows();
        app.cursor = 4; // the commented line
        let viewport = 10u16;
        assert!(
            app.rows_of(4) > viewport as usize,
            "premise: the band overflows the viewport"
        );
        app.keep_cursor_visible(viewport);
        assert_eq!(
            app.offset,
            app.row_of(4),
            "the band start (cursor row) stays on the top row"
        );
    }

    #[test]
    fn click_on_bar_maps_to_anchor_line() {
        let app = test_app();
        // 8 lines; line 5 (idx=4) carries a 3-row bar below it:
        // row0..3 = lines 1-4, row4 = line 5, rows 5-7 = the bar,
        // row8 = line 6, row9 = line 7.
        assert_eq!(source_line_at(&app, 75, 4), Some(4), "line 5");
        assert_eq!(source_line_at(&app, 75, 5), Some(4), "bar top rule");
        assert_eq!(source_line_at(&app, 75, 6), Some(4), "bar text");
        assert_eq!(
            source_line_at(&app, 75, 7),
            Some(4),
            "bar bottom rule"
        );
        assert_eq!(source_line_at(&app, 75, 8), Some(5), "line 6");
        assert_eq!(source_line_at(&app, 75, 9), Some(6), "line 7");
    }

    #[test]
    fn click_maps_correctly_without_comments() {
        let mut app = test_app();
        app.comments.clear();
        app.refresh_line_rows();
        assert_eq!(source_line_at(&app, 75, 4), Some(4));
        assert_eq!(source_line_at(&app, 75, 5), Some(5));
    }

    /// A source-mode app with `n` single-line rows (no wrapping at any
    /// sane width), so the scrollbar track exists and `line_rows` sums to
    /// exactly `n`. 1000 lines overflow any reasonable terminal height.
    fn scrollbar_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scroll.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=1000 {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(source_content_width(&app));
        app.refresh_line_rows();
        app
    }

    /// A view-mode app over `n` single-row paragraphs (blank lines keep
    /// the markdown renderer from merging them), so the scrollbar track
    /// exists. The mode is the one `.md` files start in: view.
    fn scrollbar_view_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=1000 {
            writeln!(f, "line{i}\n").unwrap();
        }
        let config = Config {
            files: vec![path],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        App::new(config, source, highlight, view, false)
    }

    #[test]
    fn scrollbar_track_click_jumps_and_grabs() {
        let mut app = scrollbar_app();
        let total: usize = app.line_rows.iter().sum();
        let viewport = app.source_viewport_rows();
        assert!(total > viewport, "the track must exist");
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let col = w - 1;
        let down = |row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let up = |row: u16| MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // A click at content row 5 jumps the viewport there and grabs the
        // thumb (the click row, the offset it landed on).
        let expected = scroll_offset_at(total, viewport, 5).unwrap();
        on_mouse(&mut app, down(1 + 5));
        assert_eq!(app.offset, expected, "click jumps the viewport");
        assert_eq!(app.scrollbar_drag, Some((5, expected)), "thumb grabbed");
        // The title row is not the track: the click falls through.
        on_mouse(&mut app, down(0));
        assert_eq!(app.offset, expected, "title row is not the track");
        // Release ends the grab.
        on_mouse(&mut app, up(1 + 5));
        assert_eq!(app.scrollbar_drag, None, "release clears the grab");
    }

    #[test]
    fn scrollbar_drag_scrubs_and_clamps() {
        let mut app = scrollbar_app();
        let total: usize = app.line_rows.iter().sum();
        let viewport = app.source_viewport_rows();
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let col = w - 1;
        let ev = |kind: MouseEventKind, row: u16, c: u16| MouseEvent {
            kind,
            column: c,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Press at track row 5, drag to track row 15: the offset follows.
        on_mouse(&mut app, ev(MouseEventKind::Down(MouseButton::Left), 6, col));
        let start = app.offset;
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 16, col));
        assert_eq!(
            app.offset,
            scroll_offset_drag(total, viewport, 5, start, 15).unwrap(),
            "drag scrubs the thumb"
        );
        // The pointer may leave the column while grabbed.
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 11, 30));
        assert_eq!(
            app.offset,
            scroll_offset_drag(total, viewport, 5, start, 10).unwrap(),
            "a grabbed drag scrubs from anywhere"
        );
        // Dragging past the track end clamps to the bottom.
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 200, col));
        assert_eq!(
            app.offset,
            scroll_offset_drag(total, viewport, 5, start, viewport - 1).unwrap(),
            "past the track end clamps"
        );
        // Release; a later drag no longer scrubs (and the normal drag
        // selection has no anchor to start from).
        on_mouse(&mut app, ev(MouseEventKind::Up(MouseButton::Left), 16, col));
        let before = app.offset;
        on_mouse(&mut app, ev(MouseEventKind::Drag(MouseButton::Left), 10, col));
        assert_eq!(app.offset, before, "no grab, no scrub");
    }

    #[test]
    fn scrollbar_works_in_view_mode_too() {
        // View mode: the track rides the frame's right border, one column
        // inside it (the right pad) and one row lower (title + frame top
        // border).
        let mut app = scrollbar_view_app();
        let total = app.view.rows.len();
        let viewport = app.view_viewport_rows();
        assert!(total > viewport, "the track must exist");
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let col = w - 2;
        let ev = |kind: MouseEventKind, row: u16| MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Click at content row 5 (screen row 2 + 5).
        let expected = scroll_offset_at(total, viewport, 5).unwrap();
        on_mouse(&mut app, ev(MouseEventKind::Down(MouseButton::Left), 2 + 5));
        assert_eq!(app.view.offset, expected, "view-mode click jumps the viewport");
        assert!(app.scrollbar_drag.is_some());
        // Release ends the grab.
        on_mouse(&mut app, ev(MouseEventKind::Up(MouseButton::Left), 2 + 5));
        assert_eq!(app.scrollbar_drag, None);
    }
}

#[cfg(test)]
mod mouse_view_tests {
    use super::*;
    use crate::config::EscQuit;
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::view::ViewState;

    fn real_app() -> App {
        let path = "testdata/full.md";
        let config = Config {
            files: vec![path.into()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
        };
        let source = Source::load(path.into()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::View;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app
    }

    fn mouse(kind: MouseEventKind, row: u16, col: u16) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn tab_flips_the_pane_under_the_composer() {
        // 入力中の Tab は composer を開いたまま下のペインを切り替える。
        // 対象範囲と入力途中のテキストは無傷。
        let mut app = real_app();
        app.view.cursor = 5;
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        let (start, end) = (app.input_start, app.input_end);
        on_input_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Input, "composing continues");
        assert_eq!(app.composer_return, Mode::Source, "the pane flipped to source");
        assert_eq!(app.input, "x", "typed text survives");
        assert_eq!((app.input_start, app.input_end), (start, end), "target range pinned");
        on_input_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.composer_return, Mode::View, "and back");
        // Enter は現在のペインに戻る。
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.comments.len(), 1);
    }

    #[test]
    fn mouse_cannot_touch_the_selection_while_composing() {
        // c でコメント入力中: クリック/ドラッグはカーソル・選択を変えない
        // （コメント対象範囲は composer を開いた時点で確定している）。
        let mut app = real_app();
        app.view.cursor = 5;
        app.selection = Some(Selection { anchor: 4, cursor: 5 });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        let (sel, cur) = (app.selection, app.view.cursor);
        on_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 10, 0));
        on_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 14, 0));
        assert_eq!(app.selection, sel, "クリック/ドラッグで選択が変わらない");
        assert_eq!(app.view.cursor, cur, "カーソルも動かない");
    }

    #[test]
    fn mouse_click_selects_the_phrase_line_and_drag_extends_by_lines() {
        let app = real_app();
        // インラインリンク行 (0-based 44) の表示行。マウス行 = 表示行 + 2
        // (タイトルバー + view の枠の上辺)。
        let display = app.view.source_starts[44];
        let mouse_row = display as u16 + 2;
        let text: String = app.view.rows[display].iter().map(|s| s.text.as_str()).collect();
        let segs: Vec<(usize, usize, usize)> = app.view.row_segments[display]
            .iter().map(|s| (s.line, s.start, s.end)).collect();
        println!("display {display} text={text:?} segs={segs:?}");
        // 句1（先頭列）→ line 44 / 句2（タイトル付き句内）→ line 45。
        // 列は左枠 1 + ガター 2 の右から数える。
        let mut a = real_app();
        on_mouse(&mut a, mouse(MouseEventKind::Down(MouseButton::Left), mouse_row, 0));
        let first = a.view.cursor;
        let mut b = real_app();
        on_mouse(&mut b, mouse(MouseEventKind::Down(MouseButton::Left), mouse_row, 56));
        let second = b.view.cursor;
        println!("click 句1 → {first}, 句2(col55) → {second}");
        assert_eq!(first, 44, "句1（インラインリンク）は line 44");
        assert_eq!(second, 45, "句2（タイトル付き）は line 45");
        // ドラッグ: 句1 から 2 表示行下まで → 行範囲 (anchor 44, end > 44)。
        let mut c = real_app();
        on_mouse(&mut c, mouse(MouseEventKind::Down(MouseButton::Left), mouse_row, 0));
        on_mouse(&mut c, mouse(MouseEventKind::Drag(MouseButton::Left), mouse_row + 2, 0));
        let sel = c.selection.map(|s| s.range());
        println!("drag 句1→2行下: selection={sel:?} cursor={}", c.view.cursor);
        let (a, b) = sel.unwrap();
        assert_eq!(a, 44);
        assert!(b > a, "ドラッグで複数行が選択される");
    }
}

#[cfg(test)]
mod history_animation_tests {
    use super::*;

    #[test]
    fn ghost_blocks_anchor_at_the_collapsed_position_not_below() {
        // Deleting B and C from [A,B,C,D]: the ghost must sit where the
        // lines were — between A and D — not below D or at the bottom
        // of the document (old `(b + 1)` arithmetic was out of bounds
        // and clamped to the last row).
        let old = vec!["A".into(), "B".into(), "C".into(), "D".into()];
        let new = vec!["A".into(), "D".into()];
        let (changed, removed) = line_level_transition(&old, &new);
        let blocks = ghost_blocks(&old, &new, &changed, removed);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "B\nC");
        assert_eq!(blocks[0].anchor, 1, "the run collapses to new index 1");
    }

    #[test]
    fn ghost_blocks_anchor_survives_an_insertion_above() {
        // [A,B,C,D] → [A,X,B,D]: C is deleted while X is inserted at
        // index 1. The ghost belongs between B and D (new index 3) —
        // NOT at the same numeric index (2), which old-index arithmetic
        // would land between X and B.
        let old = vec!["A".into(), "B".into(), "C".into(), "D".into()];
        let new = vec!["A".into(), "X".into(), "B".into(), "D".into()];
        let (changed, removed) = line_level_transition(&old, &new);
        let blocks = ghost_blocks(&old, &new, &changed, removed);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "C");
        assert_eq!(blocks[0].anchor, 3, "after B, before D");
    }

    #[test]
    fn ghost_blocks_at_eof_stay_at_the_bottom() {
        let old = vec!["A".into(), "B".into(), "C".into()];
        let new = vec!["A".into()];
        let (changed, removed) = line_level_transition(&old, &new);
        let blocks = ghost_blocks(&old, &new, &changed, removed);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "B\nC");
        assert_eq!(blocks[0].anchor, 1, "clamped to the new line count");
    }

    #[test]
    fn ghost_blocks_rewrite_ghosts_below_the_rewritten_line() {
        // "brave world" → "world": only the removed characters ghost,
        // anchored BELOW the rewritten line (the original design: the
        // removed text backspaces away, then the layout collapses).
        let old = vec!["A".into(), "brave world".into(), "C".into()];
        let new = vec!["A".into(), "world".into(), "C".into()];
        let (changed, removed) = line_level_transition(&old, &new);
        assert!(changed.contains(&1), "the rewritten line streams in");
        let blocks = ghost_blocks(&old, &new, &changed, removed);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "brave ");
        assert_eq!(blocks[0].anchor, 2, "below the rewritten line");
    }

    #[test]
    fn deleted_block_is_inserted_dimly_then_can_be_rebuilt_away() {
        let highlight = Highlighter::new(None, false);
        let source = Source::from_content("doc.md".into(), "# Next\n\ntext\n".into());
        let mut view = ViewState::render(&source, 60, &highlight);
        let original_rows = view.rows.len();
        let original_start = view.source_starts[0];
        let rects = insert_history_ghosts(
            &mut view,
            &[history::DeletedBlock {
                anchor: 0,
                content: "## Gone\n\nold text".into(),
            }],
            &highlight,
            Path::new("doc.md"),
            Color::Rgb(70, 73, 88),
        );
        assert!(view.rows.len() > original_rows);
        assert!(view.source_starts[0] > original_start);
        assert!(
            view.rows[..view.source_starts[0]]
                .iter()
                .flatten()
                .all(|span| span.style.bg.is_none()),
            "ghost rows carry no background (the deletion band was dropped)"
        );
        // The ghost's view-relative rect: inserted at the anchor's row,
        // exactly as tall as the rows it added (the dissolve target).
        assert_eq!(rects, vec![(0, view.rows.len() - original_rows)]);
    }

    #[test]
    fn changed_blocks_become_staggered_scatter_effects() {
        // Contiguous changed lines merge into one scatter block; isolated
        // lines stand alone. The rects come from the view's source-line
        // mapping, so wrapped/merged rows are covered as one block.
        let highlight = Highlighter::new(None, false);
        let source = Source::from_content("doc.md".into(), "a\n\nb\n\nc\n\nd\n\ne\n\nf\n".into());
        let view = ViewState::render(&source, 60, &highlight);
        let changed = HashSet::from([0usize, 1, 4]);
        let old_lines: Vec<String> =
            vec!["x".into(), "".into(), "b".into(), "".into(), "e".into(), "".into()];
        let fx = appear_effects(&view, &changed, &old_lines, &highlight, Path::new("doc.md"), 0);
        assert_eq!(fx.len(), 2, "lines 0-1 merge into one block");
        assert_eq!((fx[0].0, fx[0].1), (0, 2), "block 1 covers rows 0-1");
        assert_eq!(fx[1].0, view.source_starts[4], "block 2 starts at line 4's row");
        assert_eq!(fx[1].1, 1, "block 2 is one row tall");
    }

    #[test]
    fn inserted_columns_mark_only_the_new_characters() {
        // A mid-line insertion: only "brave " is new.
        assert_eq!(inserted_columns("hello world", "hello brave world"), vec![(6, 12)]);
        // Nothing new: no mask at all.
        assert_eq!(inserted_columns("same", "same"), Vec::<(u16, u16)>::new());
        // Empty old text: everything is new.
        assert_eq!(inserted_columns("", "abc"), vec![(0, 3)]);
        // A deletion alone reveals nothing (the shrink is the ghost's job).
        assert_eq!(inserted_columns("abc", "ac"), Vec::<(u16, u16)>::new());
        // CJK: one inserted double-width character spans two columns.
        assert_eq!(inserted_columns("abc", "a日c"), vec![(1, 3)]);
    }

    #[test]
    fn appear_mask_survives_a_wrap_shift() {
        // An insertion pushes "foo" onto the next line: the old block
        // wraps as "hello world foo / bar baz", the new as "hello brave
        // world / foo bar baz". Row 0 masks only "brave ", and the
        // re-wrapped "foo" — unchanged text — is NOT masked (regression:
        // per-row index alignment masked the whole shifted row).
        let highlight = Highlighter::new(None, false);
        let old = "hello world foo bar baz";
        let new = "hello brave world foo bar baz";
        let old_view = ViewState::render(
            &Source::from_content("doc.md".into(), format!("{old}\n")),
            20,
            &highlight,
        );
        let new_view = ViewState::render(
            &Source::from_content("doc.md".into(), format!("{new}\n")),
            20,
            &highlight,
        );
        assert_eq!(new_view.rows.len(), 2, "the new block wraps to two rows");
        let row0 = new_view.source_starts[0];
        let mask = appear_mask(&new_view, row0, new_view.rows.len(), &old_view.rows);
        assert_eq!(mask[0], vec![(6, 12)], "only the inserted run masks");
        assert!(mask[1].is_empty(), "wrapped-around text stays put");
    }
}
