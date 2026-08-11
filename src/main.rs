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
mod view;



use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ratatui::Frame;
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
use unicode_width::UnicodeWidthStr;

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
use crate::view::{
    is_table_delimiter_line, scroll_offset_at, scroll_offset_drag, scroll_thumb, GutterCell,
    ViewState,
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
/// `ratatui::init()`, so every early error return (a failed
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
}

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

    let mut terminal = ratatui::init();
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
    for ((state, history), path) in file_states
        .iter_mut()
        .zip(histories.iter())
        .zip(files.iter())
    {
        if let Some(reviewed) = history.reviewed_content.as_deref() {
            let (changed, deleted) = history::review_transition(
                supports_view(path),
                reviewed,
                &state.source.content,
            );
            state.review_changed = changed;
            state.review_deleted_before = deleted;
            state.comparison_changed = state.review_changed.clone();
            state.comparison_deleted_before = state.review_deleted_before.clone();
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
    // pattern, like Hermes/pi.dev): the composer draws its own `▏` glyph,
    // and the IME still anchors its inline composition window to the
    // logical cursor position we keep publishing via set_cursor_position.
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

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    // Paint the initial frame before waiting for input.
    terminal.draw(|f| draw(f, app))?;
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
        if event::poll(Duration::from_millis(TICK_MS))? {
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
        terminal.draw(|f| draw(f, app))?;
        begin_history_frame_flash_after_draw(app);
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
        }
        if !app.running {
            return Ok(());
        }
    }
}

fn on_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut ratatui::DefaultTerminal>) {
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
            line_rows + card_rows + composer_rows
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
        return;
    };
    let (changed, deleted) = history::review_transition(
        supports_view(app.current_file_path()),
        reviewed,
        &app.source.content,
    );
    app.comparison_changed = changed;
    app.comparison_deleted_before = deleted;
}

/// Move only the lightweight history cursor. The rendered Markdown remains
/// untouched until input settles, so holding an arrow can scan dozens of
/// revisions without paying the renderer cost for intermediate choices.
fn select_history(app: &mut App, delta: isize) -> bool {
    if app.config.reply {
        app.flash_err("history unavailable in reply mode");
        return false;
    }
    let index = app.current_file_index;
    let moved = app
        .histories
        .get_mut(index)
        .is_some_and(|history| history.move_by(delta));
    if !moved {
        let Some(history) = app.histories.get(index) else {
            return false;
        };
        // The lightweight cursor can reach an edge before its Markdown has
        // rendered. Keep the useful yellow timeline label visible while the
        // debounce catches up; reporting a boundary error here would hide
        // the destination throughout a held-arrow scrub.
        if history.rendered_position != history.position {
            if let Some(label) = history.label() {
                app.flash(label);
            }
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

    let label = {
        let history = &app.histories[index];
        history.label().unwrap_or_default()
    };
    app.history_render_due = Some(Instant::now() + HISTORY_RENDER_DEBOUNCE);
    // A previous landing pulse must not bleed into the first frame of the
    // next selected document.
    app.history_frame_flash_until = None;
    app.history_frame_flash_pending = false;
    app.flash(label);
    true
}

/// Render the final revision selected by [`select_history`]. This is the
/// only expensive half of time travel and runs once after the arrow stops.
fn render_pending_history(app: &mut App, animate: bool) -> bool {
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
    if let Some(history) = app.histories.get_mut(index) {
        history.rendered_position = position;
    }
    if app.source.content == content {
        refresh_comparison_marks(app);
        app.history_frame_flash_pending = true;
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
    let (changed_blocks, deleted_blocks) =
        history::block_transition(&old_lines, &new_source.lines);
    app.source = new_source;
    refresh_comparison_marks(app);
    app.history_changed = changed_blocks;
    app.history_changed_until = Some(Instant::now() + Duration::from_millis(450));
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
    if animate && !source_mode && !deleted_blocks.is_empty() {
        insert_history_ghosts(
            &mut view,
            &deleted_blocks,
            &app.highlight,
            &path,
        );
        app.history_ghost_until = Some(Instant::now() + Duration::from_millis(650));
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
    app.history_frame_flash_pending = true;
    true
}

/// Start the landing pulse only after one complete terminal paint of the
/// newly rendered document. This keeps the pulse a completion signal rather
/// than part of the document transition itself.
fn begin_history_frame_flash_after_draw(app: &mut App) {
    if app.history_frame_flash_pending {
        app.history_frame_flash_pending = false;
        app.history_frame_flash_until = Some(Instant::now() + Duration::from_millis(250));
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

/// Insert deleted Markdown blocks into the new render as dim, non-interactive
/// rows. They remain rendered Markdown, then [`expire_history_ghosts`]
/// rebuilds the current document without them to produce the collapse.
fn insert_history_ghosts(
    view: &mut ViewState,
    deleted: &[history::DeletedBlock],
    highlighter: &Highlighter,
    path: &Path,
) {
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
        for start in &mut view.source_starts {
            if *start >= insert_at {
                *start += count;
            }
        }
        for (offset, mut row) in ghost.rows.into_iter().enumerate() {
            for span in &mut row {
                span.style = span
                    .style
                    .fg(Color::Gray)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC);
            }
            view.rows.insert(insert_at + offset, row);
            view.row_segments.insert(insert_at + offset, Vec::new());
            view.card_rows.insert(insert_at + offset, true);
        }
    }
}

fn expire_history_ghosts(app: &mut App) {
    if app
        .history_ghost_until
        .is_some_and(|until| Instant::now() >= until)
    {
        app.history_ghost_until = None;
        replace_view_preserving_cursor(app);
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
pub(crate) fn on_view_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut ratatui::DefaultTerminal>) {
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
            // file (e.g. .rs) never leaves source mode. A mode flip
            // cancels a pending reload confirmation: the prompt's context
            // is the pane the user was looking at.
            app.confirm_reload = false;
            if supports_view(app.current_file_path()) {
                enter_source_mode(app);
            } else {
                app.flash_err("view mode unavailable — not a Markdown file");
            }
        }
        // Comment management works from the view too (the parallel model:
        // d/y/s need no mode switch).
        KeyCode::Char('d') => delete_comment_at_cursor(app),
        KeyCode::Char('y') => export_all(app, false),
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
        // `l` opens the all-comments list; Ctrl+p opens the file picker;
        // `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
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
            } else if app.confirm_edit {
                app.confirm_edit = false;
                app.flash("edit cancelled");
            } else if app.confirm_reload {
                app.confirm_reload = false;
                app.flash("reload cancelled");
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
    app.selection = Some(Selection {
        anchor: start,
        cursor: end,
    });
    app.cursor = end;
    app.view.goto_source_line(end);
    if app.mode == Mode::View {
        app.view
            .center_source_range(start, end, app.view_viewport_rows());
    } else {
        app.center_source_range(start, end, app.source_viewport_rows() as u16);
    }
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
pub(crate) fn on_source_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers, terminal: Option<&mut ratatui::DefaultTerminal>) {
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
        KeyCode::Char('y') => export_all(app, false),
        KeyCode::Tab => {
            // Non-Markdown files are source-only: Tab is a no-op with a
            // toast instead of rendering raw source as fake markdown.
            // A mode flip cancels a pending reload confirmation: the
            // prompt's context is the pane the user was looking at.
            app.confirm_reload = false;
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
            } else if app.confirm_edit {
                app.confirm_edit = false;
                app.flash("edit cancelled");
            } else if app.confirm_reload {
                app.confirm_reload = false;
                app.flash("reload cancelled");
            } else if app.selection.take().is_some() {
                app.flash("selection cancelled");
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
        // `l` opens the all-comments list; Ctrl+p opens the file picker;
        // `?` opens the full key reference.
        KeyCode::Char('l') => {
            open_overlay(app, Overlay::Comments, 0);
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
            if let Some(idx) = app.editing_comment {
                // Re-edit: replace the text and refresh its source snippet.
                if let Some(c) = app.comments.get_mut(idx) {
                    c.text = text;
                    c.lines = app.source.snippet(c.start, c.end);
                }
                app.flash(format!("comment updated ({} total)", app.comments.len()));
            } else {
                let lines = app
                    .source
                    .snippet(app.input_start as u32 + 1, app.input_end as u32 + 1);
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
        KeyCode::Char(c) => {
            app.input.insert(app.input_cursor, c);
            app.input_cursor += c.len_utf8();
        }
        KeyCode::Backspace => {
            if let Some((i, _)) = app.input[..app.input_cursor].char_indices().next_back() {
                app.input.remove(i);
                app.input_cursor = i;
            }
        }
        KeyCode::Delete => {
            if app.input_cursor < app.input.len() {
                app.input.remove(app.input_cursor);
            }
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
            && c.revision == revision
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
        !(c.file_path == current_file && c.revision == revision && c.covers(line))
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

/// Export all comments. `y` copies to the clipboard; `s` additionally
/// delivers via `--send-cmd` (e.g. a herdr pane) and clears the slate —
/// but ONLY on success: a
/// failed send keeps the comments for a retry, and nothing is queued for
/// stdout (terminal output after quit was confusing; pipe via the send
/// command instead, e.g. `--send-cmd "cat >> review.txt"`).
fn export_all(app: &mut App, send: bool) {
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
    // The permanent message line sits between the body and the footer:
    // prompts and toasts render there, so the body (and the view mode's
    // frame) always closes cleanly — nothing floats over the border rows
    // anymore. The line stays even when empty, so the layout never
    // shifts.
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let [title, body, message, footer] = layout.areas(f.area());
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
    // The message line floats above everything (overlays included): a
    // notification never displaces content. The persistent prompt wins
    // over the transient toast — an action that demands the user must
    // not be hidden behind a message that will expire on its own.
    draw_message(f, message, app);
}








/// View mode: the native render with a row cursor. No per-frame
/// keep_cursor_visible here — like source mode, the wheel scrolls the
/// viewport only and the cursor is an absolute position that may sit off
/// screen; keyboard navigation, clicks, and the mode handoffs reveal it.
fn review_flags(marks: &HashSet<usize>, line_count: usize) -> Vec<bool> {
    (0..line_count).map(|line| marks.contains(&line)).collect()
}

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
    let glowing = if app
        .history_changed_until
        .is_some_and(|until| Instant::now() < until)
    {
        review_flags(&app.history_changed, n)
    } else {
        vec![false; n]
    };
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
    let frame_flashing = app
        .history_frame_flash_until
        .is_some_and(|until| Instant::now() < until);
    let page_border = if frame_flashing {
        app.ui_history_frame_flash
    } else if app.is_historical() {
        app.ui_history_border
    } else {
        app.ui_border
    };
    let border_style = if frame_flashing {
        Style::default().fg(page_border).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(page_border)
    };
    let (mut text, mut gutter) = app.view.visible_text_with_glow(
        inner.height as usize,
        &marked,
        &changed,
        &glowing,
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
        // Terminal-cursor position inside the bar: on the `▏` glyph (the
        // macOS IME anchors its inline composition window here), sharing
        // the drawer's row math via composer_cursor_pos.
        let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
        // Top rule at `start_row`, text rows from `start_row + 1`.
        let row = start_row + 1 + crow;
        if row < inner.height as usize {
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
    // running the cursor/selection band to the page edge.
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
    // the bar and follows the `▏` glyph.
    if let Some((col, row)) = composer_cursor {
        f.set_cursor_position(Position {
            x: inner.x + col,
            y: inner.y + row,
        });
    }
}

/// Build the visible source-mode rows: `[status][number] ` + content, each
/// source line wrapped to `content_width`, continuation rows bare content.
/// Selected lines get the selection background; the cursor line a `>` marker.
/// While the composer is open, also returns where the terminal cursor should
/// sit inside it (body-relative column/row), so the real bar cursor (and the
/// macOS IME's inline composition window) renders inside the bar.
fn build_rows(app: &App, height: u16, content_width: u16) -> (Text<'static>, Option<(u16, u16)>) {
    let mut out: Vec<Line> = Vec::new();
    let mut composer_cursor: Option<(u16, u16)> = None;
    let width = content_width as usize;
    // Compute baseline → displayed-generation marks once for this frame.
    // Green means present/changed; red means deleted before a line.
    let (scoped_added, scoped_deleted) = active_review_mark_sets(app);
    let history_glow_active = app
        .history_changed_until
        .is_some_and(|until| Instant::now() < until);
    let history_landing_pulse = app
        .history_frame_flash_until
        .is_some_and(|until| Instant::now() < until);
    // Comment bars span the whole pane (gutter included), pi.dev-style.
    let full_width = (content_width + app.gutter_cols) as usize;
    // The current file's cards (the edited one is hidden while composing).
    let cards = visible_cards(app);
    let mut row = 0usize;
    for idx in 0..app.source.len() {
        let line_rows = app.rows_of(idx);
        if row + line_rows <= app.offset {
            row += line_rows;
            continue;
        }
        if row >= app.offset + height as usize {
            break;
        }
        let selected = app.selection.is_some_and(|s| s.contains(idx));
        let revision = app.current_revision_context();
        let commented = app.comments.iter().any(|c| {
            c.covers(idx)
                && c.file_path == app.current_file_path()
                && c.revision == revision
        });
        let added = scoped_added.contains(&idx);
        let deleted_before = scoped_deleted.contains(&idx);
        let is_cursor = idx == app.cursor;
        // `▌` is a current changed line; `▀` is a deletion position.
        let cursor_mark = if is_cursor {
            ">"
        } else if added {
            "▌"
        } else if deleted_before {
            "▀"
        } else {
            " "
        };
        // The cursor line gets the same calm DarkGray background as view
        // mode (text colors untouched — a full-row reversal was fatiguing
        // and clashed with the syntax highlighting). A selected row shares
        // the background; the `>` marker keeps the cursor visible at the
        // selection edge. Present/changed review lines get a restrained
        // green background; pure deletions use only their red position mark.
        let cursor_bg = is_cursor || selected;
        let history_glow_bg = history_glow_active
            && app.history_changed.contains(&idx)
            && !cursor_bg;
        let changed_bg = added && !cursor_bg && !history_glow_bg;
        let deleted_fg = deleted_before && !cursor_bg && !history_glow_bg && !changed_bg;
        let gutter_style = if cursor_bg {
            Style::default().bg(app.ui_selected_bg)
        } else if history_glow_bg {
            Style::default().bg(app.ui_history_glow_bg)
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
        } else if history_glow_bg {
            Style::default().fg(Color::DarkGray).bg(app.ui_history_glow_bg)
        } else if commented && !is_cursor {
            Style::default().fg(Color::Yellow)
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else if deleted_fg {
            Style::default().fg(Color::Red)
        } else {
            gutter_style
        };
        // Source has no permanent frame, so its completion pulse uses the
        // stable line-number rail instead. It starts only after the newly
        // selected source has already been painted once.
        if history_landing_pulse {
            num_style = num_style
                .fg(app.ui_history_frame_flash)
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
            let fg = if added {
                Color::LightGreen
            } else if deleted_before {
                Color::LightRed
            } else {
                Color::LightCyan
            };
            let s = Style::default().fg(fg).add_modifier(Modifier::BOLD);
            if cursor_bg { s.bg(app.ui_selected_bg) } else { s }
        } else if history_glow_bg {
            Style::default()
                .fg(Color::DarkGray)
                .bg(app.ui_history_glow_bg)
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else if deleted_fg {
            Style::default().fg(Color::Red)
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
                // block.
                let indent_style = if cursor_bg {
                    Style::default().bg(app.ui_selected_bg)
                } else if history_glow_bg {
                    Style::default().bg(app.ui_history_glow_bg)
                } else if changed_bg {
                    Style::default().bg(app.ui_changed_bg)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(
                    " ".repeat(app.gutter_cols as usize),
                    indent_style,
                ));
            }
            for f in frags {
                let style = if cursor_bg {
                    f.style.bg(app.ui_selected_bg)
                } else if history_glow_bg {
                    f.style.bg(app.ui_history_glow_bg)
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
            if cursor_bg || history_glow_bg || changed_bg {
                let used: usize = spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let bg = if cursor_bg {
                    app.ui_selected_bg
                } else if history_glow_bg {
                    app.ui_history_glow_bg
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
            // Terminal-cursor position inside the bar: on the `▏` glyph.
            // The macOS IME draws its inline composition window at this
            // spot too. composer_cursor_pos shares the drawer's row math,
            // so the anchor can never drift from the glyph.
            let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
            // Top rule at `start_row`, text rows from `start_row + 1`.
            composer_cursor = Some((ccol as u16, (start_row + 1 + crow) as u16));
        }
        row += line_rows;
    }
    (Text::from(out), composer_cursor)
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

/// The composer's wrapped body rows as plain strings: the input with the
/// `▏` cursor glyph inserted at `cursor` (byte offset), split into
/// logical lines and wrapped to `full_width`. The drawer, the row count,
/// and the terminal-cursor anchor all derive from THIS, so the layout,
/// the glyph, and the IME window can never disagree.
fn composer_body_rows(text: &str, cursor: usize, full_width: usize) -> Vec<String> {
    let mut display = text.to_string();
    display.insert(cursor.min(display.len()), '▏');
    let mut rows = Vec::new();
    for logical in display.split('\n') {
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

/// The `▏` glyph's position inside the composer body: (body row, display
/// column). Row 0 is the first text row under the top rule.
fn composer_cursor_pos(text: &str, cursor: usize, full_width: usize) -> (usize, usize) {
    let rows = composer_body_rows(text, cursor, full_width);
    // The cursor glyph is the (n+1)-th `▏` in the display text, where n
    // is the count of `▏` the user typed BEFORE the cursor (the glyph is
    // inserted at `cursor`, so everything before it keeps its order). A
    // plain find would grab the user's own glyph when the text contains
    // `▏` before the cursor; rfind would grab it when one follows the
    // cursor — counting picks the cursor glyph in both cases, so the IME
    // anchor never drifts from the rendered cursor.
    let before = text[..cursor.min(text.len())].matches('▏').count();
    let mut seen = 0;
    for (i, row) in rows.iter().enumerate() {
        let mut from = 0;
        while let Some(pos) = row[from..].find('▏') {
            let abs = from + pos;
            seen += 1;
            if seen == before + 1 {
                return (i, UnicodeWidthStr::width(&row[..abs]));
            }
            from = abs + '▏'.len_utf8();
        }
    }
    (rows.len().saturating_sub(1), 0)
}

/// Display rows the composer bar occupies (top rule + wrapped input +
/// bottom rule). Must match [`composer_lines`].
pub(crate) fn composer_line_count(text: &str, cursor: usize, full_width: usize) -> usize {
    composer_body_rows(text, cursor, full_width).len().max(1) + 2
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
/// composing. The `▏` cursor glyph sits at the insertion point (the
/// hardware cursor stays hidden — see run(); this is the modern-TUI
/// pattern used by Hermes/pi.dev, language-independent and immune to the
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
    for piece in composer_body_rows(text, cursor, full_width) {
        lines.push(Line::from(Span::raw(piece)));
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
        let text_row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text_row.ends_with('▏'), "cursor glyph on the last text row");
        let bottom: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            bottom.chars().filter(|&c| c == '─').count(),
            80,
            "bottom rule spans the full pane"
        );
    }

    #[test]
    fn composer_glyph_follows_the_cursor() {
        // ▏ renders at the cursor position, not pinned to the end.
        let lines = composer_lines("abc", 1, 0, 0, 80, false);
        let row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(row, "a▏bc");
        assert_eq!(composer_cursor_pos("abc", 1, 80), (0, 1));
        // Multi-line: the glyph (and the IME anchor) lands on the second
        // logical line's row.
        assert_eq!(composer_cursor_pos("ab\ncd", 4, 80), (1, 1));
        // CJK before the cursor counts display width, not chars.
        assert_eq!(composer_cursor_pos("あい", 3, 80), (0, 2));
        // A user `▏` in the text must not displace the cursor anchor:
        // with the cursor past it the rendered glyph is the LAST `▏`;
        // with the cursor before it (arrows can move there) it is the
        // FIRST — the anchor follows the cursor either way.
        assert_eq!(composer_cursor_pos("x▏y", 5, 80), (0, 3));
        assert_eq!(composer_cursor_pos("x▏y", 1, 80), (0, 1));
        assert_eq!(composer_cursor_pos("x▏y", 4, 80), (0, 2));
    }

    #[test]
    fn composer_tabs_expand_so_the_cursor_column_matches() {
        // The ▏ column is measured on the wrapped text; a pasted tab
        // expands to the terminal's 8-column stop, so the cursor lands
        // after the visible text instead of after a 0-width tab.
        let lines = composer_lines("a\tb", 3, 0, 0, 80, false);
        let text_row: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text_row, "a       b▏");
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
    fn deleted_block_is_inserted_dimly_then_can_be_rebuilt_away() {
        let highlight = Highlighter::new(None, false);
        let source = Source::from_content("doc.md".into(), "# Next\n\ntext\n".into());
        let mut view = ViewState::render(&source, 60, &highlight);
        let original_rows = view.rows.len();
        let original_start = view.source_starts[0];
        insert_history_ghosts(
            &mut view,
            &[history::DeletedBlock {
                anchor: 0,
                content: "## Gone\n\nold text".into(),
            }],
            &highlight,
            Path::new("doc.md"),
        );
        assert!(view.rows.len() > original_rows);
        assert!(view.source_starts[0] > original_start);
        assert!(
            view.rows[..view.source_starts[0]]
                .iter()
                .flatten()
                .all(|span| span.style.add_modifier.contains(Modifier::DIM))
        );
    }
}
