//! The big end-to-end state tests: key handling across every mode,
//! overlays, prompts/toasts, title/footer chrome, reloads, and the
//! composer (moved wholesale from main.rs).

/// State-machine tests: mode transitions, selection, composer, deletion,
/// and quit confirmation. These run the real key handlers against a built
/// App (no TTY), so the event-loop logic is exercised the same way a user
/// would — only the rendering is bypassed.
use crate::*;

use std::path::{Path, PathBuf};

/// The contiguous runs of caret cells in a rendered buffer, as
/// (row, col_start, col_end). The block caret is fg Black on bg Cyan and
/// the end-of-line caret a cyan underline — the same colors the footer's
/// mode label uses — so the title (0) and footer (last) strips are
/// excluded; only the body area counts.
fn caret_runs(buf: &ratatui::buffer::Buffer) -> Vec<(usize, usize, usize)> {
    let w = buf.area.width as usize;
    let h = buf.area.height as usize;
    let is_caret = |c: &ratatui::buffer::Cell| {
        c.style().bg == Some(ratatui::style::Color::Cyan)
            || (c.style().fg == Some(ratatui::style::Color::Cyan)
                && c
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::UNDERLINED))
    };
    let mut cells: Vec<(usize, usize)> = buf
        .content
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            let row = i / w;
            row > 0 && row + 1 < h && is_caret(c)
        })
        .map(|(i, _)| (i / w, i % w))
        .collect();
    cells.sort_unstable();
    let mut runs = Vec::new();
    for (row, col) in cells {
        match runs.last_mut() {
            Some((r0, _c0, c1)) if *r0 == row && *c1 == col => *c1 = col + 1,
            _ => runs.push((row, col, col + 1)),
        }
    }
    runs
}

use crate::comment::Selection;
    use crate::config::{Config, EscQuit};
    use ratatui::backend::Backend;
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::source::Source;
    use crate::view::ViewState;
    use std::io::Write;

    /// A fresh app over a temp file with `n` lines ("line1"..), in `mode`.
    fn make_app(n: usize, mode: Mode) -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=n {
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
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        let mut history = DocumentHistory::load(&app.files[0], &app.source.content, 0);
        history.acknowledge_in_memory(&app.source.content);
        app.histories = vec![history];
        app.mode = mode;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app
    }

    /// Like [`make_app`] but returns the tempdir too, so tests that rewrite
    /// the file on disk can keep it alive (make_app's tempdir is dropped
    /// when it returns, deleting the file).
    fn make_app_keep(n: usize, mode: Mode) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=n {
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
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        let mut history = DocumentHistory::load(&app.files[0], &app.source.content, 0);
        history.acknowledge_in_memory(&app.source.content);
        app.histories = vec![history];
        app.mode = mode;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        (app, dir)
    }

    /// Simulate the [`CHORD_MS`] window elapsing, then run the event
    /// loop's expiry — the tests press `]`/`[` and expire synchronously,
    /// so the timestamp must be backdated past the window first.
    pub(crate) fn expire_chord(app: &mut App) {
        if let Some((_, bracket)) = app.pending_chord {
            app.pending_chord =
                Some((Instant::now() - CHORD_MS - Duration::from_millis(1), bracket));
        }
        expire_pending_chord(app);
    }

    fn add_comment(app: &mut App, start: usize, end: usize, text: &str) {
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: start as u32,
            end: end as u32,
            lines: app.source.snippet(start as u32, end as u32),
            revision: None,
            text: text.into(),
        });
    }

    #[test]
    fn quit_without_comments_exits_immediately() {
        let mut app = make_app(5, Mode::Source);
        request_quit(&mut app);
        assert!(!app.running);
    }

    #[test]
    fn quit_with_comments_requires_a_second_q() {
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        assert!(app.running, "first q only arms the confirmation");
        assert!(app.confirm_quit);
        request_quit(&mut app);
        assert!(!app.running, "second q quits");
    }

    #[test]
    fn esc_cancels_quit_confirmation() {
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running);
        assert!(!app.confirm_quit, "Esc clears the confirmation");
    }

    #[test]
    fn ctrl_c_quits_like_q_in_view_and_source_modes() {
        // No comments: Ctrl+C exits at once (Esc would need esc-quit
        // enabled — an interrupt must never be a no-op).
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, None);
        assert!(!app.running, "source: no comments, quits at once");
        let mut app = make_app(5, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, None);
        assert!(!app.running, "view: no comments, quits at once");
        // With comments: first Ctrl+C arms the confirmation, second quits.
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, None);
        assert!(app.running, "first Ctrl+C only arms the confirmation");
        assert!(app.confirm_quit);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, None);
        assert!(!app.running, "second Ctrl+C confirms the quit");
        // Same from view mode.
        let mut app = make_app(5, Mode::View);
        add_comment(&mut app, 2, 2, "note");
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, None);
        assert!(app.running && app.confirm_quit);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, None);
        assert!(!app.running);
    }

    #[test]
    fn ctrl_c_cancels_the_composer_like_esc() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "abc".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.mode, Mode::Source, "back to the composer's origin");
        assert!(app.input.is_empty(), "the draft is discarded");
        assert!(app.comments.is_empty(), "nothing was added");
        assert_eq!(app.input_cursor, 0);
        assert!(app.ime_guard.is_none());
        // The interrupt key must never reach the IME-safe catch-all: a
        // `c` inserted into the draft would be exactly this bug.
        assert!(!app.input.contains('c'));
    }

    #[test]
    fn composer_drops_unbound_ctrl_and_alt_chords() {
        // A Ctrl/Alt chord with no composer meaning must not silently
        // type its letter: Ctrl+w/u/k are readline keys elsewhere and
        // Alt+j/k are review jumps in the other modes — inserting them
        // would betray the modifier intent. Only the explicitly bound
        // chords (Ctrl+j/a/e/c) act; everything else is dropped.
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Char('j'), KeyModifiers::ALT);
        on_input_key(&mut app, KeyCode::Char('k'), KeyModifiers::ALT);
        assert!(app.input.is_empty(), "ctrl/alt chords never type: {:?}", app.input);
        // The explicitly bound chords still work alongside the guard.
        on_input_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.input, "\n", "Ctrl+j still inserts a newline");
    }

    #[test]
    fn composer_accepts_shift_flagged_capitals() {
        // Some terminals report capital letters WITH the SHIFT flag set
        // (the reason the n/N review arms skip the empty-modifier guard).
        // The composer's chord guard must therefore reject only
        // CONTROL/ALT — an empty-modifier check would break uppercase
        // typing on those terminals.
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('N'), KeyModifiers::SHIFT);
        assert_eq!(app.input, "N", "SHIFT-flagged capitals still type");
    }

    #[test]
    fn flash_err_beeps_once_per_error_message() {
        // The BEL decision comes from the current status: the identical
        // error still on screen within STATUS_SECS skips the bell, and
        // anything else rings it (the toast itself refreshes either way).
        let mut app = make_app(5, Mode::Source);
        assert!(!is_repeat_error(&app.status, "boom"), "no toast yet");
        app.flash_err("boom");
        assert!(
            is_repeat_error(&app.status, "boom"),
            "the fresh toast makes the next identical error a repeat"
        );
        app.flash_err("boom");
        assert!(
            is_repeat_error(&app.status, "boom"),
            "the toast was refreshed, not left stale"
        );
        // An info toast with the same text is not an error repeat.
        app.flash("boom");
        assert!(!is_repeat_error(&app.status, "boom"));
        // A different message always beeps.
        assert!(!is_repeat_error(&app.status, "bang"));
        // After STATUS_SECS the same message is fresh again.
        let stale = Some((
            "boom".into(),
            Instant::now() - STATUS_SECS - Duration::from_millis(1),
            true,
        ));
        assert!(!is_repeat_error(&stale, "boom"));
    }

    #[test]
    fn esc_quits_with_callback_in_auto_mode() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "auto + callback: Esc quits with no comments");
    }

    #[test]
    fn esc_quits_with_callback_in_view_mode() {
        let mut app = make_app(5, Mode::View);
        app.config.callback = Some("fzf".into());
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "auto + callback: Esc quits from view too");
    }

    #[test]
    fn esc_arms_then_confirms_the_quit_with_callback() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        add_comment(&mut app, 2, 2, "note");
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running);
        assert!(app.confirm_quit, "first Esc arms the confirmation");
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "armed Esc confirms the quit, not cancels");
    }

    #[test]
    fn esc_never_quits_with_callback_in_never_mode() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        app.config.esc_quit = EscQuit::Never;
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running, "never: Esc stays a pure cancel");
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        assert!(app.confirm_quit);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running);
        assert!(!app.confirm_quit, "never: armed Esc still cancels");
    }

    #[test]
    fn esc_always_quits_without_callback() {
        let mut app = make_app(5, Mode::View);
        app.config.esc_quit = EscQuit::Always;
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.running, "always: Esc quits even without callback");
    }

    #[test]
    fn esc_cancels_selection_before_quitting() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        });
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.running, "a pending selection cancels first");
        assert!(app.selection.is_none());
    }

    #[test]
    fn overlay_esc_closes_the_overlay_not_the_app() {
        let mut app = make_app(5, Mode::Source);
        app.config.callback = Some("fzf".into());
        open_overlay(&mut app, Overlay::Help, 0);
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.running);
        assert!(app.overlay.is_none(), "overlay Esc never quits");
    }

    #[test]
    fn prompt_message_reflects_the_esc_binding() {
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 2, 2, "note");
        request_quit(&mut app);
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — q to quit, Esc to cancel")
        );
        app.config.callback = Some("fzf".into());
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — Esc/q to quit")
        );
    }

    #[test]
    fn view_viewport_rows_match_the_frame_layout() {
        // The message row reuses the frame's bottom border (view mode)
        // or the last content row (source mode) — no dedicated row, so
        // the viewport math stays the plain title+footer subtraction.
        // It must match draw_view's inner.height, or keep_cursor_visible
        // would fight the offset.
        let app = make_app(5, Mode::View);
        assert_eq!(
            app.view_viewport_rows() as u16,
            app.terminal_height() - 4,
            "frame's two borders + title + footer"
        );
    }

    #[test]
    fn source_viewport_rows_match_the_frame_layout() {
        // Source mode never draws the frame, so only title + footer are
        // subtracted (must match draw_source's inner.height).
        let app = make_app(5, Mode::Source);
        assert_eq!(
            app.source_viewport_rows() as u16,
            app.terminal_height() - 2,
            "title + footer"
        );
    }

    #[test]
    fn help_rows_reflect_the_esc_binding() {
        let rows = help_rows(false, false, false, false);
        assert!(
            rows.iter().any(|(l, k)| *l == "quit" && *k == "q quit · Esc cancel"),
            "default help advertises Esc as cancel"
        );
        let rows = help_rows(true, false, false, false);
        assert!(
            rows.iter().any(|(l, k)| *l == "quit" && *k == "Esc/q quit"),
            "esc-quit help advertises Esc/q as quit"
        );
    }

    #[test]
    fn help_advertises_review_navigation() {
        let rows = help_rows(false, false, true, false);
        assert!(rows
            .iter()
            .any(|(l, k)| *l == "compare" && k.contains("a acknowledge")));
        let rows = help_rows(false, true, true, false);
        assert!(!rows.iter().any(|(l, _)| *l == "compare"));
    }

    #[test]
    fn reply_mode_help_hides_file_navigation_and_edit() {
        // Reply mode: messages replace files — no file switching, no
        // edit, and reloads are automatic.
        let rows = help_rows(false, true, false, false);
        assert!(
            !rows.iter().any(|(l, _)| *l == "file"),
            "no file navigation row in reply mode"
        );
        assert!(
            !rows.iter().any(|(_, k)| k.contains("e edit")),
            "no edit in reply mode"
        );
        assert!(
            rows.iter()
                .any(|(l, k)| *l == "msg" && k.contains("]/[")),
            "reply help advertises message navigation"
        );
        assert!(
            rows.iter()
                .any(|(l, k)| *l == "reload" && k.contains("auto-reload")),
            "reply help advertises auto-reload"
        );
        // Non-reply mode keeps them.
        let rows = help_rows(false, false, false, false);
        assert!(rows.iter().any(|(l, _)| *l == "file"));
        assert!(rows.iter().any(|(_, k)| k.contains("e edit")));
    }

    #[test]
    fn reply_mode_title_shows_label_not_temp_path() {
        // The doc is a temp copy of the agent's message; the path is
        // noise in reply mode — the title says what the pane is for.
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        assert_ne!(title_metrics(&app, 80).path, "reply");
        app.config.reply = true;
        assert_eq!(title_metrics(&app, 80).path, "reply");
    }

    #[test]
    fn q_in_view_mode_quits_when_there_are_no_comments() {
        let mut app = make_app(5, Mode::View);
        on_view_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE, None);
        assert!(!app.running);
    }

    #[test]
    fn delete_removes_every_comment_covering_the_cursor() {
        let mut app = make_app(10, Mode::Source);
        add_comment(&mut app, 3, 5, "range");
        add_comment(&mut app, 7, 7, "single");
        app.cursor = 4; // inside the 3-5 range
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1);
        assert_eq!(app.comments[0].start, 7);
    }

    #[test]
    fn delete_without_a_comment_on_the_line_flashes() {
        let mut app = make_app(10, Mode::Source);
        add_comment(&mut app, 3, 5, "range");
        app.cursor = 9;
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "nothing deleted");
        assert!(app.status.is_some(), "a flash message explains the miss");
    }

    #[test]
    fn composer_on_the_last_line_stays_on_screen() {
        // A comment on the LAST line used to push its bar off the bottom
        // of the pane: the open-time keep_cursor_visible ran on the
        // pre-composer row layout (the bar was not folded into line_rows
        // yet), so nothing scrolled and the bar rendered below the fold,
        // invisible. The draw pass folds the bar in and nudges the
        // offset so the bar's bottom rule sits at the pane bottom.
        let mut app = make_app(25, Mode::Source);
        app.cursor = 24; // last line
        app.offset = app.max_offset(22); // last line sits at the pane bottom
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        // The draw pass: fold the bar into line_rows, then nudge.
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app.keep_composer_visible(22);
        let start = app.row_of(24);
        let end = start + app.rows_of(24);
        assert_eq!(end, 28, "last line + top rule + body + bottom rule");
        assert_eq!(app.offset, 6, "scrolled so the bar's bottom row is visible");
        assert!(start >= app.offset, "the commented line is still visible");
        assert!(end <= app.offset + 22, "the whole bar is inside the pane");
        // As the input wraps to more rows, the per-frame nudge keeps the
        // bar's bottom rule at the pane bottom (before, a growing bar
        // clipped — nothing re-scrolled while typing).
        app.input = "x".repeat(200); // 3 wrapped body rows at full width 78
        app.input_cursor = app.input.len();
        app.refresh_line_rows();
        app.keep_composer_visible(22);
        let end = app.row_of(24) + app.rows_of(24);
        assert_eq!(end, 30, "5-row bar after wrapping");
        assert_eq!(app.offset, 8);
        assert!(end <= app.offset + 22, "the grown bar still fits");
    }

    #[test]
    fn source_composer_on_the_last_line_renders_in_the_buffer() {
        // End-to-end: with the cursor on the last line and the pane
        // scrolled to the bottom, the composer bar must actually be
        // painted inside the terminal buffer (the user-visible regression:
        // the bar and its text were below the fold, invisible).
        let mut app = make_app(25, Mode::Source);
        app.cursor = 24; // last line
        app.offset = app.max_offset(18); // pane bottom (TestBackend 20 rows − 2)
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "最終行のコメント".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // Wide glyphs (CJK) occupy two cells, the second a spacer; strip
        // spaces so multi-cell sequences compare as one string.
        let content: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .filter(|&c| c != ' ')
            .collect();
        assert!(
            content.contains("comment·25"),
            "the composer bar is visible for the last line"
        );
        assert!(
            content.contains("最終行のコメント"),
            "the typed comment text is visible"
        );
        assert!(!caret_runs(buf).is_empty(), "the block caret is in the bar");
        assert!(
            content.contains("line24"),
            "the commented line itself is still on screen"
        );
    }

    #[test]
    fn enter_confirms_the_comment_and_returns_to_source_mode() {
        let mut app = make_app(8, Mode::Source);
        app.cursor = 1;
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        }); // select 0-based 1..=2
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (1, 2));
        for ch in "テスト".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.input, "テスト");
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.comments.len(), 1);
        let c = &app.comments[0];
        assert_eq!((c.start, c.end), (2, 3), "1-based inclusive range");
        assert_eq!(c.text, "テスト");
        assert_eq!(c.lines, "line2\nline3", "verbatim snippet");
        assert!(app.selection.is_none(), "the selection was consumed");
        assert!(app.ime_guard.is_none(), "the IME guard is dropped");
    }

    #[test]
    fn enter_with_blank_input_is_rejected() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        app.input = "   ".into();
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Input, "stays in the composer");
        assert!(app.comments.is_empty());
    }

    #[test]
    fn esc_cancels_the_composer() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        app.input = "hello".into();
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Source);
        assert!(app.input.is_empty(), "the draft is discarded");
        assert!(app.comments.is_empty());
        assert!(app.ime_guard.is_none());
    }

    #[test]
    fn composer_cursor_moves_and_edits_mid_text() {
        // Arrows move the text cursor; insertion, Backspace, and Delete
        // act at the cursor instead of only at the end (the re-edit
        // prefill was uneditable without this).
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "abc".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(app.input, "abXc", "insert at the cursor");
        on_input_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.input, "abc", "Backspace deletes before the cursor");
        on_input_key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.input, "ab", "Delete removes at the cursor");
        // Home / End and their readline twins Ctrl+a / Ctrl+e.
        on_input_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('>'), KeyModifiers::NONE);
        assert_eq!(app.input, ">ab");
        on_input_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(app.input, ">ab!");
        on_input_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.input, "ab!");
        // Left at the start and Right at the end are clamped no-ops.
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, 0);
        on_input_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn composer_cursor_handles_multibyte_and_lines() {
        // Multibyte chars move as one unit; Up/Down cross logical lines
        // keeping the character column.
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "日本語".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        for ch in "二行目".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.input, "日本語\n二行目");
        // Left steps over one CJK char (3 bytes), never mid-char.
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(app.input, "日本X語\n二行目", "Up kept the char column");
        // Cursor sits after X (col 3). Two Lefts → col 1, Down keeps it.
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Char('Y'), KeyModifiers::NONE);
        assert_eq!(app.input, "日本X語\n二Y行目", "Down kept the char column");
        // Up at the first line goes to the text start; Down at the last
        // line to the text end.
        on_input_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, 0);
        on_input_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn ctrl_j_and_backspace_edit_the_buffer() {
        let mut app = make_app(5, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "ab".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        on_input_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(app.input, "ab\nc");
        on_input_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.input, "ab\n");
    }

    #[test]
    fn v_starts_a_selection_and_j_k_extend_it() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 3;
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection::new(3)));
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 4));
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 3));
        // Extending past the top clamps at line 0.
        for _ in 0..5 {
            on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        }
        assert_eq!(app.selection.unwrap().range(), (0, 3));
    }

    #[test]
    fn j_k_move_the_cursor_without_a_selection() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 3;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 4);
        assert!(app.selection.is_none());
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 3);
        // Moving past the last line clamps.
        app.cursor = 9;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 9);
    }

    #[test]
    fn c_anchors_the_composer_to_the_selection_end() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 5;
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (5, 6));
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.cursor, 6, "cursor follows the selection end");
    }

    #[test]
    fn c_on_an_empty_file_flashes_instead_of_opening() {
        let mut app = make_app(0, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert!(app.status.is_some());
    }

    #[test]
    fn esc_cancels_the_selection_and_never_switches_modes() {
        // Tab is the ONE mode toggle: a reflexive Esc must not flip the
        // mode (it used to, and the reading position got lost).
        let mut app = make_app(10, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "Esc only clears the selection");
        assert!(app.selection.is_none());
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "a second Esc stays in source mode");
    }

    #[test]
    fn tab_returns_to_view_on_the_same_source_line() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 7;
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 7, "exact line handoff");
        // The view reveals the cursor (draw_view no longer auto-scrolls).
        let row = app.view.cursor_row();
        let offset = app.view.offset;
        let viewport = app.view_viewport_rows();
        assert!(
            row >= offset && row < offset + viewport,
            "cursor revealed (row {row}, offset {offset}, viewport {viewport})"
        );
    }

    #[test]
    fn view_j_skips_table_delimiter_rows_like_blanks() {
        // A table's delimiter row renders nothing and carries no
        // information (the body shows the columns) — j/k skip it exactly
        // like a blank line: from the header row the cursor lands on the
        // body, never on the delimiter.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "| A | B |\n|---|---|\n| a | b |\n").unwrap();
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::View;
        assert_eq!(
            app.view.ghost[1].as_deref(),
            Some("|---|---|"),
            "the delimiter renders nothing (ghost entry)"
        );
        // Down from the header row: the delimiter is skipped, the body is
        // the first stop.
        app.view.cursor = 0;
        view_move_cursor(&mut app, 1);
        assert_eq!(
            app.view.cursor, 2,
            "j lands on the body row, past the delimiter"
        );
        // Up from the body: the delimiter is skipped again.
        view_move_cursor(&mut app, -1);
        assert_eq!(app.view.cursor, 0, "k lands back on the header row");
    }

    #[test]
    fn view_j_still_stops_on_informational_invisible_lines() {
        // The skip is for the delimiter row only: ref-defs and fences
        // carry information, so j/k keep stopping on them (the ghost
        // paints the raw source there).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "段落\n\n[ref1]: https://example.com\n\n```rust\nfn main() {}\n```\n",
        )
        .unwrap();
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::View;
        app.view.cursor = 0;
        view_move_cursor(&mut app, 1);
        assert_eq!(app.view.cursor, 2, "j stops on the ref-def (informational)");
        view_move_cursor(&mut app, 1);
        assert_eq!(app.view.cursor, 4, "j stops on the opening fence");
    }

    #[test]
    fn view_v_starts_a_selection_in_view_mode() {
        // Headings render one row each (no blank rows, no paragraph merge),
        // so display-row movement maps 1:1 to source lines.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=10 {
            writeln!(f, "# line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::View;
        // Deterministic 1:1 row mapping (headings etc. render with extra
        // blank rows that would skew the display-row walk).
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.move_cursor_display(6);
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(
            app.mode, Mode::View,
            "v starts a selection in view; Tab switches modes"
        );
        assert_eq!(app.selection, Some(Selection::new(6)));
        // j/k extend the selection, parallel to source mode.
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (6, 7));
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (6, 6));
        // Esc clears the selection without leaving view mode.
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
        assert_eq!(app.mode, Mode::View);
    }

    #[test]
    fn view_v_lands_on_the_head_of_a_merged_paragraph_row() {
        // The renderer joins a paragraph's soft-wrapped lines into one rendered
        // row; display-row navigation leaves the view cursor on the row's
        // LAST source line, but the selection must cover the whole row —
        // the head through the row's last line (lines 2-4 merge onto row 1,
        // so the selection is 2-4, 1-based).
        let mut app = make_app(10, Mode::View);
        app.view.rows = vec![vec![]; 4];
        app.view.source_starts = vec![0, 1, 1, 1, 2, 3, 3, 3, 3, 3];
        app.view.cursor = 3; // last source line of the merged group
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        // Model A: the selection is the cursor line (line-granular, same as
        // source mode); the gray may show the whole row, but the comment
        // covers exactly the selected line.
        assert_eq!(app.selection, Some(Selection::new(3)));
    }

    #[test]
    fn view_selection_extends_by_lines_like_source_mode() {
        // j/k step by source lines — byte-for-byte the same semantics as
        // source mode (1:1 rows here, so a line is a display row).
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..8).map(|_| vec![]).collect();
        app.view.source_starts = (0..8).collect();
        app.view.goto_source_line(3);
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection::new(3)), "v selects the cursor line");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 4), "j extends one display row");
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (3, 3), "k shrinks back");
        // k past the anchor extends up.
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.unwrap().range(), (2, 3), "k extends up past the anchor");
        // Esc clears the selection.
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
    }

    #[test]
    fn view_c_comments_the_selection_and_switches_modes() {
        let mut app = make_app(10, Mode::View);
        // A two-line selection, then c: the composer anchors to the whole
        // range and the app lands in Input.
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input_start, 2);
        assert_eq!(app.input_end, 4, "the selection is not overwritten");
        // Without a selection, c falls back to the cursor line (1:1 row
        // mapping so the paragraph head is the line itself).
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(5);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input_start, 5);
        assert_eq!(app.input_end, 5);
    }

    #[test]
    fn view_c_opens_the_composer_directly() {
        let mut app = make_app(10, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input, "c opens the composer right away");
        assert_eq!(
            app.selection, None,
            "bare c does not manufacture a selection — the target line stays visible"
        );
        assert_eq!(app.input_start, 0);
        assert_eq!(app.input_end, 0);
    }

    #[test]
    fn view_c_on_an_empty_file_flashes_instead_of_entering_input() {
        let mut app = make_app(0, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View, "empty file: stays in view mode");
        assert!(app.status.is_some(), "flashes a message");
    }

    #[test]
    fn view_c_enters_and_returns_without_leaving_view_mode() {
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(3);
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        });
        // c opens the composer inline (still "from view": Enter/Esc return
        // to View, not Comment).
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.composer_return, Mode::View);
        assert_eq!(app.input_start, 2);
        assert_eq!(app.input_end, 4);
        // Esc cancels and returns to view, keeping the selection.
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View, "Esc returns to view mode");
        assert!(app.selection.is_some(), "cancel keeps the selection");
        // c again, this time confirming: the comment lands and view stays.
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View, "Enter returns to view mode");
        assert_eq!(app.comments.len(), 1);
        assert_eq!(app.comments[0].start, 3, "anchored to the selection");
        assert_eq!(app.comments[0].end, 5);
        assert!(app.selection.is_none(), "the selection was consumed");
    }

    #[test]
    fn source_c_still_returns_to_source_mode() {
        let mut app = make_app(10, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.composer_return, Mode::Source);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Source, "source-mode c still lands in comment");
        assert_eq!(app.comments.len(), 1);
    }

    #[test]
    fn view_n_next_jumps_between_commented_lines() {
        let mut app = make_app(10, Mode::View);
        let cur = app.current_file_path().to_path_buf();
        app.comments.push(Comment {
            file_path: cur.clone(),
            start: 3,
            end: 3,
            lines: "line3".into(),
            revision: None,
            text: "c1".into(),
        });
        app.comments.push(Comment {
            file_path: cur,
            start: 7,
            end: 7,
            lines: "line7".into(),
            revision: None,
            text: "c2".into(),
        });
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 2, "n jumps to the first comment below");
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 6, "n jumps to the next comment");
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 2, "N jumps back");
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 2, "N stays put when no comment above");
        assert!(app.status.is_some(), "and flashes a message");
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 6, "n resumes from where it stopped");
    }

    #[test]
    fn mode_switch_keeps_the_cursor_screen_row() {
        // view: cursor row 20, scrolled to offset 10 → cursor at content
        // row 10, PHYSICAL terminal row 12 (title + the view frame's top
        // border). Source mode draws no frame (content starts at physical
        // row 1), so its offset lands the cursor at content row 11 — the
        // same physical row. The round trip restores the view offset.
        // A 1:1 row mapping (each source line its own rendered row) keeps
        // the math exact; make_app's plain lines would merge into a
        // paragraph in the rendered view.
        let mut app = make_app(60, Mode::View);
        app.view.rows = (0..60).map(|_| vec![]).collect();
        app.view.source_starts = (0..60).collect();
        app.view.goto_source_line(20);
        app.view.offset = 10;
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.cursor, 20);
        assert_eq!(
            app.offset,
            9,
            "comment content row 11 (20 − 9) = the view's physical row"
        );
        // Back to view (Tab) — which mirrors the same physical row.
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 20);
        assert_eq!(app.view.offset, 10, "the round trip restores the view offset");
    }

    #[test]
    fn mode_switch_carries_the_selection_over() {
        // A view selection survives the Tab toggle: selection is one state
        // shared by both modes (the parallel model).
        let mut app = make_app(10, Mode::View);
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 5,
        });
        app.view.goto_source_line(5);
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: 2,
                cursor: 5
            }),
            "the selection is carried into source mode"
        );
        assert_eq!(app.cursor, 5, "the comment cursor sits on the extent");
        // Esc clears the carried selection but never switches modes; Tab
        // returns to view with the selection gone.
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "Esc clears the selection only");
        assert!(app.selection.is_none());
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
    }

    #[test]
    fn mode_switch_clamps_at_the_edges() {
        // Cursor scrolled off-screen above in view (offset 40 > row 30): the
        // preserved screen row would push the offset past the scrollable
        // max, so it clamps (the allowed edge exception).
        let mut app = make_app(60, Mode::View);
        app.view.rows = (0..60).map(|_| vec![]).collect();
        app.view.source_starts = (0..60).collect();
        app.view.goto_source_line(30);
        app.view.offset = 40;
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        let max = app.max_offset(app.source_viewport_rows() as u16);
        assert_eq!(app.offset, max, "offset clamps at the scrollable max");
    }

    #[test]
    fn view_marker_gutter_flags_commented_rows() {
        // Merged rows: lines 2-4 share one rendered row. A comment on line 2
        // must mark the shared row even though it maps to the group's last
        // source line.
        let view = ViewState {
            rows: vec![vec![], vec![], vec![], vec![]],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 1, 1, 1],
            ..Default::default()
        };
        let marked = vec![false, true, false, false]; // line 1 (0-based) commented
        let (_, gutter) = view.visible_text(
            10,
            &marked,
            &[],
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            ratatui::style::Style::default(),
        );
        // Row 1 (source lines 1-3 merged) carries the marker glyph.
        assert_eq!(gutter[1].glyph, "▌", "merged row shows the marker");
        // Row 0 is unmarked; the cursor (line 0) shows `>` instead.
        assert_ne!(gutter[0].glyph, "▌", "unmarked row shows no marker");
    }

    #[test]
    fn view_marker_flags_cover_the_whole_range() {
        // A multi-line comment (3-5) flags every covered line; a
        // single-line comment (9) flags its own line.
        let comments = vec![
            Comment {
                file_path: "d.md".into(),
                start: 3,
                end: 5,
                lines: String::new(),
                revision: None,
                text: "c1".into(),
            },
            Comment {
                file_path: "d.md".into(),
                start: 9,
                end: 9,
                lines: String::new(),
                revision: None,
                text: "c2".into(),
            },
        ];
        let marked = view_marker_flags(&comments, 10, Path::new("d.md"));
        assert!(marked[2] && marked[3] && marked[4], "every covered line is flagged");
        assert!(!marked[1] && !marked[5], "lines outside the range are clean");
        assert!(marked[8], "single-line comment flags its own line");
        assert_eq!(marked.len(), 10);
        assert!(view_marker_flags(&comments, 0, Path::new("d.md")).is_empty());
    }

    #[test]
    fn n_n_jump_to_comment_block_heads() {
        // Overlapping ranges stay separate jump targets: A(2-5) and
        // B(4-6) overlap but are jumped to one by one, so the selection
        // never becomes their merged union; C(8-9) is the next target.
        let mut app = make_app(10, Mode::View);
        let cur = app.current_file_path().to_path_buf();
        let comments = vec![
            Comment {
                file_path: cur.clone(),
                start: 2,
                end: 5,
                lines: String::new(),
                revision: None,
                text: "c1".into(),
            },
            Comment {
                file_path: cur.clone(),
                start: 4,
                end: 6,
                lines: String::new(),
                revision: None,
                text: "c2".into(),
            },
            Comment {
                file_path: cur,
                start: 8,
                end: 9,
                lines: String::new(),
                revision: None,
                text: "c3".into(),
            },
        ];
        assert_eq!(
            comment_regions(&comments, 10, app.current_file_path()),
            vec![(1, 4), (3, 5), (7, 8)],
            "overlapping comments stay separate targets"
        );
        app.comments = comments;
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 4, "cursor lands on the first comment's extent");
        assert_eq!(
            app.selection,
            Some(Selection { anchor: 1, cursor: 4 }),
            "only the first comment becomes the selection"
        );
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 5, "n steps into the overlapping comment");
        assert_eq!(app.selection, Some(Selection { anchor: 3, cursor: 5 }));
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 8, "n skips to the last comment");
        assert_eq!(app.selection, Some(Selection { anchor: 7, cursor: 8 }));
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 8, "n at the last comment flashes");
        assert!(app.status.is_some());
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 5, "N returns to the overlapping comment");
        assert_eq!(app.selection, Some(Selection { anchor: 3, cursor: 5 }));
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 4, "N returns to the first comment");
        assert_eq!(app.selection, Some(Selection { anchor: 1, cursor: 4 }));
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 4, "N at the first comment flashes");
        // Clearing the selection falls back to cursor-based navigation.
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.view.cursor, 8, "n from a cleared cursor still finds the next comment");
    }

    #[test]
    fn reply_mode_reload_skips_diff_and_badge() {
        // --reply: the doc is a single agent message; each refresh replaces
        // the whole thing, so a diff would mark everything as changed.
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        app.config.reply = true;
        let mut f = std::fs::File::create(app.current_file_path()).unwrap();
        writeln!(f, "new message line 1").unwrap();
        writeln!(f, "new message line 2").unwrap();
        assert!(reload_source(&mut app, false).is_ok());
        assert_eq!(app.source.len(), 2, "new content is loaded");
        assert!(app.review_changed.is_empty(), "reply mode has no review marks");
        assert!(!app.file_changed, "reload clears the pending prompt");
    }

    #[test]
    fn reload_replaces_source_and_clamps_anchors() {
        let (mut app, _dir) = make_app_keep(10, Mode::Source);
        app.cursor = 9;
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 9,
            end: 10,
            lines: String::new(),
            revision: None,
            text: "c".into(),
        });
        // The agent rewrites the file down to 7 lines.
        let mut f = std::fs::File::create(app.current_file_path()).unwrap();
        for i in 1..=7 {
            writeln!(f, "line{i}").unwrap();
        }
        assert!(reload_source(&mut app, false).is_ok());
        assert_eq!(app.source.len(), 7);
        assert_eq!(app.cursor, 6, "cursor clamps to the new last line");
        assert_eq!(app.comments.len(), 1, "the comment stays on its generation");
        assert!(!app.file_changed, "the pending prompt clears on reload");
        assert!(app.selection.is_none(), "selection is cleared");
    }

    #[test]
    fn r_reloads_and_i_ignores_a_pending_change() {
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        // Simulate a detected external edit.
        app.file_changed = true;
        app.file_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        // The agent appends a line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(app.current_file_path())
            .unwrap();
        writeln!(f, "line6").unwrap();
        // r reloads and clears the prompt.
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert_eq!(app.source.len(), 6, "r replaces the in-memory source");
        assert!(!app.file_changed, "r clears the pending prompt");
        assert!(app.review_changed.contains(&5), "the new line needs review");
        // A fresh change can be ignored with i.
        app.file_changed = true;
        on_source_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE, None);
        assert!(!app.file_changed, "i marks the change as seen");
        assert_eq!(
            app.last_loaded_stamp, app.file_stamp,
            "ignored state counts as loaded"
        );
    }

    #[test]
    fn mixed_anchor_red_bar_survives_the_full_draw_path() {
        // End-to-end doubt check: the mixed anchor's red bar is written
        // over the frame's left BORDER cells after the paragraph render
        // — this asserts the actual terminal buffer, so any later pass
        // clobbering the marker column (border repaints, effects,
        // scrollbar) would fail here. Line index 2 is both changed and a
        // deletion anchor; line index 4 is changed only.
        let (mut app, _dir) = make_app_keep(6, Mode::View);
        // Blank-line-separated paragraphs: consecutive lines would merge
        // into ONE rendered group and put every mark on one row.
        std::fs::write(app.current_file_path(), "p1\n\np2\n\np3\n").unwrap();
        reload_source(&mut app, false).unwrap();
        app.histories[0].acknowledge_in_memory(&app.source.content);
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
        // Source lines: 0 "p1", 1 "", 2 "p2", 3 "", 4 "p3". p2 is both
        // changed and a deletion anchor; p3 is changed only.
        app.comparison_changed.insert(2);
        app.comparison_deleted_before.insert(2);
        app.comparison_changed.insert(4);
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // The marker column rides the left border at x = 1 (the page
        // floats one column off the screen edge) and view rows start at
        // y = 2 (title strip + frame border). Rendered rows: p1 (cursor,
        // y=2), blank (y=3), p2 (y=4), blank (y=5), p3 (y=6).
        let mixed = &buf[(1u16, 4u16)];
        assert_eq!(mixed.symbol(), "▌", "the anchor row keeps the bar shape");
        assert_eq!(mixed.style().fg, Some(Color::Red), "deletion red wins on the anchor row");
        let changed = &buf[(1u16, 6u16)];
        assert_eq!(changed.symbol(), "▌", "changed-only line keeps the bar");
        assert_eq!(changed.style().fg, Some(Color::Green));
        let plain = &buf[(1u16, 3u16)];
        assert_eq!(plain.symbol(), "│", "unmarked rows keep the border glyph");
    }

    #[test]
    fn acknowledge_clears_review_marks_and_moves_the_baseline() {
        let (mut app, _dir) = make_app_keep(3, Mode::Source);
        std::fs::write(app.current_file_path(), "line1\nchanged\nline3\n").unwrap();
        reload_source(&mut app, false).unwrap();
        assert!(!app.review_changed.is_empty());
        app.review_changed.clear();
        app.review_changed.insert(1);
        app.comparison_changed.clear();
        app.comparison_changed.insert(1);
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 1, "n lands on the unreviewed block");

        on_source_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE, None);

        assert!(app.review_changed.is_empty());
        assert!(app.review_deleted_before.is_empty());
        assert!(app.selection.is_none(), "a leaves source SELECT state");
        assert_eq!(
            app.histories[0].reviewed_content.as_deref(),
            Some(app.source.content.as_str())
        );
    }

    #[test]
    fn acknowledge_leaves_view_select_state() {
        let mut app = make_app(3, Mode::View);
        app.selection = Some(Selection {
            anchor: 0,
            cursor: 2,
        });

        on_view_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE, None);

        assert!(app.selection.is_none());
        assert_eq!(app.mode, Mode::View);
    }

    #[test]
    fn acknowledge_on_a_historical_generation_selects_it_as_the_baseline() {
        let mut app = make_app(3, Mode::Source);
        let old = "old first line\nline2\nline3\n";
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: old.into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;

        on_source_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE, None);

        assert_eq!(app.histories[0].reviewed_content.as_deref(), Some(old));
        assert!(app.review_changed.contains(&0));
        assert_eq!(app.histories[0].position, 1, "selecting a baseline does not leave the past");
    }

    #[test]
    fn historical_generation_shows_and_navigates_baseline_relative_marks() {
        let mut app = make_app(3, Mode::Source);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());

        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);

        assert_eq!(app.source.lines[0], "old first line");
        assert!(!app.comparison_changed.is_empty());
        assert!(app.review_changed.is_empty(), "NOW review state remains independent");
        // Line-granular review marks: only the changed line (0) is marked,
        // not the whole merged block (the block-level intent was dropped).
        assert_eq!(app.selection.unwrap().range(), (0, 0));
    }

    #[test]
    fn review_count_measures_groups_instead_of_source_lines() {
        let mut app = make_app(10, Mode::Source);
        app.review_changed = [1, 2, 5].into_iter().collect();
        app.review_deleted_before = [0, 2, 8].into_iter().collect();

        assert_eq!(
            app.file_review_count(0),
            4,
            "one changed block counts once and a deletion at the same anchor is not doubled"
        );
    }

    #[test]
    fn review_navigation_wraps_and_can_select_the_only_mark_under_the_cursor() {
        let mut app = make_app(3, Mode::Source);
        app.review_changed.insert(0);
        app.comparison_changed.insert(0);
        app.cursor = 0;

        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);

        assert_eq!(app.selection.unwrap().range(), (0, 0));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn source_review_range_centers_as_far_as_document_edges_allow() {
        let mut app = make_app(20, Mode::Source);

        app.center_source_range(7, 9, 5);
        assert_eq!(app.offset, 6);
        app.center_source_range(19, 19, 5);
        assert_eq!(app.offset, 15);
    }

    #[test]
    fn reload_pins_live_comments_to_the_previous_local_generation() {
        let (mut app, dir) = make_app_keep(3, Mode::Source);
        let cache = SnapshotCache::at(dir.path().join("snapshot-cache"));
        app.histories = vec![
            DocumentHistory::load_cached(
                app.current_file_path(),
                &app.source.content,
                0,
                &cache,
            )
            .unwrap(),
        ];
        app.snapshot_cache = Some(cache.clone());
        add_comment(&mut app, 2, 2, "keep this context");

        std::fs::write(app.current_file_path(), "line1\nchanged\nline3\n").unwrap();
        reload_source(&mut app, false).unwrap();

        let revision = app.comments[0]
            .revision
            .as_deref()
            .expect("the former NOW becomes a LOCAL generation");
        assert!(revision.starts_with("local:"));
        let cached = cache.load(app.current_file_path()).unwrap();
        assert!(
            cached
                .snapshots
                .iter()
                .any(|snapshot| snapshot.pinned && snapshot.content.contains("line2"))
        );
    }

    #[test]
    fn reload_drops_former_now_cards_from_the_new_now_layout() {
        // The rebase above attaches the comment to the old LOCAL
        // generation; the reload's own re-render must NOT fold that card
        // into the new NOW's layout — a stale card lingered until the
        // next full re-render (regression: render_view_with_cards got
        // ALL comments instead of the current revision's visible cards).
        let (mut app, dir) = make_app_keep(3, Mode::Source);
        let cache = SnapshotCache::at(dir.path().join("snapshot-cache"));
        app.histories = vec![
            DocumentHistory::load_cached(
                app.current_file_path(),
                &app.source.content,
                0,
                &cache,
            )
            .unwrap(),
        ];
        app.snapshot_cache = Some(cache.clone());
        add_comment(&mut app, 2, 2, "keep this context");

        std::fs::write(app.current_file_path(), "line1\nchanged\nline3\n").unwrap();
        reload_source(&mut app, false).unwrap();

        // The card belongs to the old LOCAL generation: invisible at NOW,
        // and the reloaded view's layout holds no card row.
        assert!(
            visible_cards(&app).is_empty(),
            "the rebased comment is not visible at the new NOW"
        );
        assert!(
            !app.view.card_rows.iter().any(|&card| card),
            "no card row in the reloaded view layout"
        );
        // Browsing to the old generation brings the card back.
        app.histories[0].position = 1;
        assert_eq!(
            visible_cards(&app).len(),
            1,
            "the card rides its LOCAL generation"
        );
    }

    #[test]
    fn r_on_non_utf8_file_toasts_an_error_and_keeps_content() {
        // A binary write or a mid-write agent edit breaks UTF-8; `r` must
        // not fail silently (the ⚡ prompt staying up forever with a dead
        // `r` was the bug) — it toasts the reason and keeps the content.
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        std::fs::write(app.current_file_path(), [0xff, 0xfe, b'a', b'\n']).unwrap();
        // Simulate the poll's detection of the external edit.
        app.file_changed = true;
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        let (msg, _, is_error) = app.status.as_ref().expect("a toast fires");
        assert!(*is_error, "the failure is an error toast: {msg}");
        assert!(msg.contains("reload failed"), "toast names the failure: {msg}");
        assert!(msg.contains("reading"), "toast names the file read: {msg}");
        assert_eq!(app.source.len(), 5, "the old content stays");
        assert!(app.file_changed, "the pending prompt stays up for a retry");
        // A successful reload after the file recovers clears the error.
        std::fs::write(
            app.current_file_path(),
            "line1\nline2\nline3\nline4\nline5\nline6\n",
        )
        .unwrap();
        on_source_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE, None);
        assert_eq!(app.source.len(), 6, "r retries after the file recovers");
        assert!(!app.file_changed);
    }

    #[test]
    fn reload_skips_identical_content_but_handles_touches() {
        let (mut app, _dir) = make_app_keep(5, Mode::Source);
        // touch the file: same content, new mtime
        let file = app.current_file_path().to_path_buf();
        std::fs::write(&file, std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(reload_source(&mut app, false).is_ok(), "a touch is handled");
    }

    #[test]
    fn poll_arms_the_reload_on_stamp_mismatch() {
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        // Simulate the startup stamp from an earlier version of the file.
        app.last_loaded_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        app.file_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        poll_file_change(&mut app);
        assert!(
            app.reload_pending.is_some(),
            "a stamp mismatch arms the debounced reload"
        );
        assert!(
            app.file_stamp.is_some_and(|(_, len)| len > 0),
            "the current on-disk stamp is recorded"
        );
    }

    #[test]
    fn esc_cancels_the_quit_confirmation_in_both_modes() {
        // Source mode: q → confirm, Esc cancels it (even with a selection
        // active — the confirmation outranks the selection).
        let mut app = make_app(5, Mode::Source);
        app.selection = Some(Selection::new(2));
        app.comments.push(Comment {
            file_path: "d.md".into(),
            start: 1,
            end: 1,
            lines: String::new(),
            revision: None,
            text: "c".into(),
        });
        on_source_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE, None);
        assert!(app.confirm_quit);
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.confirm_quit, "Esc cancels the prompt first");
        assert!(
            app.selection.is_some(),
            "the selection survives; Esc did not double as selection-clear"
        );
        // View mode: q → confirm, Esc cancels (view has no other Esc role).
        let mut app = make_app(5, Mode::View);
        app.comments.push(Comment {
            file_path: "d.md".into(),
            start: 1,
            end: 1,
            lines: String::new(),
            revision: None,
            text: "c".into(),
        });
        on_view_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE, None);
        assert!(app.confirm_quit);
        on_view_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(!app.confirm_quit);
        assert_eq!(app.mode, Mode::View, "stays in view mode");
    }

    #[test]
    fn source_mode_n_n_jumps_select_the_block() {
        // Two blocks: [1-3] (one multi-line comment) and [6] (a stacked
        // single-line comment). n/N in source mode land on the block
        // extent and the whole block becomes the selection, exactly like
        // view mode.
        let mut app = make_app(10, Mode::Source);
        let cur = app.current_file_path().to_path_buf();
        app.comments.push(Comment {
            file_path: cur.clone(),
            start: 2,
            end: 4,
            lines: String::new(),
            revision: None,
            text: "c1".into(),
        });
        app.comments.push(Comment {
            file_path: cur.clone(),
            start: 7,
            end: 7,
            lines: String::new(),
            revision: None,
            text: "c2".into(),
        });
        app.comments.push(Comment {
            file_path: cur,
            start: 7,
            end: 7,
            lines: String::new(),
            revision: None,
            text: "c3".into(),
        });
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, 3, "cursor lands on the block extent");
        assert_eq!(
            app.selection,
            Some(Selection { anchor: 1, cursor: 3 }),
            "the block [1-3] is selected"
        );
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, 6, "stacked comments count as one block");
        assert_eq!(app.selection, Some(Selection { anchor: 6, cursor: 6 }));
        // N returns to the previous block, re-selecting it.
        app.selection = Some(Selection::new(6));
        on_source_key(&mut app, KeyCode::Char('N'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, 3, "N returns to the previous block extent");
        assert_eq!(app.selection, Some(Selection { anchor: 1, cursor: 3 }));
    }

    #[test]
    fn view_composer_renders_inline_in_the_rendered_view() {
        // Draw the app while the view-origin composer is open and check the
        // buffer: the composer bar must appear inside the rendered view
        // (title under the cursor line), not a mode switch.
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(3);
        on_view_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            content.contains("comment · 4"),
            "the composer bar renders inside the view"
        );
        assert!(!caret_runs(buf).is_empty(), "the block caret is in the bar");
    }

    #[test]
    fn view_composer_on_the_last_line_stays_on_screen() {
        // Commenting the LAST line used to clamp the view's scroll at the
        // document's last row, pushing the composer bar below the pane —
        // invisible. The bar extends the scrollable extent (max_off grows
        // by the bar's height), so the nudge can always reveal it.
        let mut app = make_app(25, Mode::View);
        app.view.rows = (0..25).map(|_| vec![]).collect();
        app.view.source_starts = (0..25).collect();
        app.view.goto_source_line(24);
        app.view.offset = app.view.rows.len().saturating_sub(20); // pane bottom
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input_end, 24);
        app.keep_composer_visible_view(20);
        assert_eq!(view_composer_anchor(&app), 24, "anchor at the last row");
        let start_row = view_composer_anchor(&app).saturating_sub(app.view.offset) + 1;
        assert_eq!(
            start_row + 3,
            20,
            "the whole 3-row bar fits, ending at the pane bottom (start_row {start_row})"
        );
    }

    #[test]
    fn view_composer_typing_scrolls_to_keep_the_bar_on_screen() {
        // As the input wraps to more rows, the per-frame nudge scrolls the
        // view so the bar's bottom rule stays at the pane bottom (before,
        // only the open-time scroll existed — a growing bar clipped).
        let mut app = make_app(25, Mode::View);
        app.view.rows = (0..25).map(|_| vec![]).collect();
        app.view.source_starts = (0..25).collect();
        app.view.goto_source_line(24);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        app.input = "x".repeat(200);
        app.input_cursor = app.input.len();
        app.keep_composer_visible_view(20);
        let h = composer_line_count(&app.input, app.input_cursor, view_content_width(&app));
        assert_eq!(h, 5, "top rule + 3 wrapped rows + bottom rule");
        let start_row = view_composer_anchor(&app).saturating_sub(app.view.offset) + 1;
        assert_eq!(
            start_row + h,
            20,
            "the bar's bottom rule sits at the pane bottom (start_row {start_row})"
        );
    }

    #[test]
    fn view_composer_on_the_last_line_renders_in_the_buffer() {
        // End-to-end: commenting the LAST line in view mode must paint the
        // composer bar inside the terminal buffer (the regression: the
        // scroll clamp at the document's last row left the bar below the
        // pane, invisible).
        let mut app = make_app(25, Mode::View);
        app.view.rows = (0..25).map(|_| vec![]).collect();
        app.view.source_starts = (0..25).collect();
        app.view.goto_source_line(24);
        app.view.offset = app.view.rows.len().saturating_sub(16); // pane bottom
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        for ch in "最終行".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // Wide glyphs (CJK) occupy two cells, the second a spacer; strip
        // spaces so multi-cell sequences compare as one string.
        let content: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .filter(|&c| c != ' ')
            .collect();
        assert!(
            content.contains("comment·25"),
            "the composer bar is visible for the last line"
        );
        assert!(
            content.contains("最終行"),
            "the typed comment text is visible"
        );
        assert!(!caret_runs(buf).is_empty(), "the block caret is in the bar");
    }

    #[test]
    fn view_folds_comment_cards_into_the_layout() {
        // Headings render one row each, so the mapping is deterministic: a
        // comment on lines 2-3 inserts its card after line 3's row and
        // shifts the source-line mapping for everything below.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=8 {
            writeln!(f, "# line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let comments = vec![Comment {
            file_path: "d.md".into(),
            start: 2,
            end: 3,
            lines: String::new(),
            revision: None,
            text: "card body".into(),
        }];
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let width = view_render_width(w);
        let view = render_view_with_cards(&source, width, &highlight, &comments, Default::default());
        let card_h = view.card_rows.iter().filter(|&&b| b).count();
        assert!(card_h >= 3, "title + body + rule rows");
        let base = ViewState::render(&source, width, &highlight, Default::default());
        assert_eq!(
            view.source_starts[3],
            base.source_starts[3] + card_h,
            "rows below the comment end shift by the card height"
        );
        let first_card = view.card_rows.iter().position(|&b| b).unwrap();
        let card_text: String = view
            .card_rows
            .iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .flat_map(|(i, _)| view.rows[i].iter().map(|s| s.text.as_str()))
            .collect::<String>();
        assert!(card_text.contains("card body"), "the card text is in the view");
        // Card rows float in the text column like every other row: one
        // pad on each side (the title keeps its own leading space).
        let (text, gutter) = view.visible_text(
            10,
            &[],
            &[],
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            ratatui::style::Style::default(),
        );
        let row: String = text.lines[first_card]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            row.trim_start().starts_with("comment · 2-3 "),
            "card row starts with its title: {row:?}"
        );
        assert_eq!(
            gutter[first_card].glyph, "│",
            "card rows keep the plain border"
        );
    }

    #[test]
    fn comment_added_from_view_folds_the_card_in() {
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        on_input_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View);
        assert!(
            app.view.card_rows.iter().any(|&b| b),
            "the new card is folded into the view layout"
        );
        // Deleting the comment removes the card again.
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None); // to source mode
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert!(app.comments.is_empty());
        assert!(
            !app.view.card_rows.iter().any(|&b| b),
            "the card is removed with the comment"
        );
    }

    #[test]
    fn merged_followers_keep_their_row_after_a_card() {
        // A comment whose end line is merged with the following lines
        // (one paragraph row): the card is inserted after the paragraph's
        // row, so the merged followers' text stays ABOVE the card — their
        // source-line mapping must not shift into the card (the regression:
        // the cursor landed on a card row, losing the `>` marker and the
        // selection highlight, and mouse clicks resolved to wrong lines).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        // One 4-line paragraph (no blank lines): all four lines merge
        // into a single rendered row, then a blank + a heading.
        writeln!(f, "para one").unwrap();
        writeln!(f, "para two").unwrap();
        writeln!(f, "para three").unwrap();
        writeln!(f, "para four").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "# after").unwrap();
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        // Comment on line 1 (0-based), the paragraph's second line.
        let comments = vec![Comment {
            file_path: "d.md".into(),
            start: 2,
            end: 2,
            lines: String::new(),
            revision: None,
            text: "card".into(),
        }];
        let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let width = view_render_width(w);
        let mut view = render_view_with_cards(&source, width, &highlight, &comments, Default::default());
        let card_h = view.card_rows.iter().filter(|&&b| b).count();
        assert!(card_h >= 3);
        // The paragraph (lines 0-3) shares row 0; the card follows it;
        // the blank (line 4) and the heading (line 5) shift down by the
        // card height.
        assert_eq!(view.source_starts[0], 0);
        assert_eq!(view.source_starts[1], 0, "merged follower keeps the paragraph row");
        assert_eq!(view.source_starts[2], 0, "merged follower keeps the paragraph row");
        assert_eq!(view.source_starts[3], 0, "merged follower keeps the paragraph row");
        assert_eq!(view.source_starts[4], 1 + card_h, "the blank shifts below the card");
        assert_eq!(view.source_starts[5], 2 + card_h, "lines below the card shift");
        // The cursor on a merged follower stays on the paragraph row: the
        // `>` marker and the selection band are visible again (the
        // regression: the cursor row resolved into the card).
        view.cursor = 1;
        let (_, gutter) = view.visible_text(
            10,
            &[false; 6],
            &[],
            &[],
            &[],
            Some((1, 1)),
            Color::Rgb(88, 91, 112),
            ratatui::style::Style::default(),
        );
        assert_eq!(gutter[0].glyph, ">", "cursor on the merged follower marks row 0");
    }

    #[test]
    fn block_last_row_spans_a_merged_paragraph() {
        // Lines a/b/c merge into one paragraph wrapped over rows 0-2, a
        // blank owns row 3, a heading row 4. Any line of the paragraph
        // anchors at the group's last row (2); the blank and the heading
        // are their own blocks.
        let starts = vec![0, 0, 0, 3, 4];
        assert_eq!(block_last_row(&starts, 5, 0), 2);
        assert_eq!(block_last_row(&starts, 5, 2), 2);
        assert_eq!(block_last_row(&starts, 5, 3), 3);
        assert_eq!(block_last_row(&starts, 5, 4), 4);
    }

    #[test]
    fn block_last_row_stays_inside_an_html_block() {
        // 1:1 lines (an HTML block renders one row per source line, no
        // blanks in between): the block of each line is exactly its own
        // row, so a card for a comment ending mid-block sits right under
        // its end line — the same place source mode shows it — instead of
        // dropping below the whole non-blank run (regression: comment
        // 492-498 rendered its card under </div> at line 501).
        let starts = vec![0, 1, 2, 3, 4, 5];
        assert_eq!(block_last_row(&starts, 6, 2), 2);
        assert_eq!(block_last_row(&starts, 6, 5), 5, "last line anchors at the last row");
    }

    #[test]
    fn view_card_sits_under_its_end_line_inside_an_html_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "# t\n\n<div>\n<p>x</p>\n<span>y</span>\n</div>\n",
        )
        .unwrap();
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(None, false);
        // The comment ends on the <p> line (1-based 4, 0-based 3).
        let comments = vec![Comment {
            file_path: "d.md".into(),
            start: 3,
            end: 4,
            lines: String::new(),
            revision: None,
            text: "mid-block".into(),
        }];
        let base = ViewState::render(&source, 60, &highlight, Default::default());
        let view = render_view_with_cards(&source, 60, &highlight, &comments, Default::default());
        let first_card = view.card_rows.iter().position(|&b| b).unwrap();
        assert_eq!(
            first_card,
            base.source_starts[3] + 1,
            "the card sits directly under the <p> line's row, not under </div>"
        );
    }

    #[test]
    fn composer_anchors_at_the_selection_range_bottom() {
        // Downward selection: anchor 2, cursor 5 → input_end 5.
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(5);
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 5,
        });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input_end, 5);
        assert_eq!(view_composer_anchor(&app), 5, "below line 5's block");
        // Upward selection: anchor 6, cursor 2 → the range max is still 6.
        let mut app = make_app(10, Mode::View);
        app.view.rows = (0..10).map(|_| vec![]).collect();
        app.view.source_starts = (0..10).collect();
        app.view.goto_source_line(2);
        app.selection = Some(Selection {
            anchor: 6,
            cursor: 2,
        });
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input_end, 6, "range max, not the cursor");
        assert_eq!(view_composer_anchor(&app), 6);
    }

    #[test]
    fn g_and_g_jump_to_the_ends() {
        let mut app = make_app(10, Mode::Source);
        app.cursor = 5;
        app.offset = 4;
        on_source_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE, None);
        assert_eq!((app.cursor, app.offset), (0, 0));
        on_source_key(&mut app, KeyCode::Char('G'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 9);
    }

    #[test]
    fn page_keys_ride_the_cursor_and_clamp() {
        // View-mode spec ported to source mode: the page keys move the
        // CURSOR by a page (Ctrl+u/d a half page) and the viewport follows
        // — no more viewport-only scroll that strands the cursor off
        // screen. Clamps at both document edges.
        let mut app = make_app(50, Mode::Source);
        let viewport = app.source_viewport_rows();
        on_source_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE, None);
        assert_eq!(app.cursor, viewport.min(49), "PageDown jumps the cursor a page");
        assert_cursor_visible(&app, viewport);
        for _ in 0..20 {
            on_source_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE, None);
        }
        assert_eq!(app.cursor, 49, "clamped at the last line");
        assert!(app.offset <= app.max_offset(app.source_viewport_rows() as u16));
        for _ in 0..40 {
            on_source_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE, None);
        }
        assert_eq!(app.cursor, 0, "PageUp clamps at the top");
        assert_eq!(app.offset, 0);
        on_source_key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, viewport / 2, "Ctrl+d jumps half a page");
        on_source_key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL, None);
        assert_eq!(app.cursor, 0, "Ctrl+u jumps back");
    }

    #[test]
    fn view_s_y_d_are_wired() {
        // The export/delete keys work from the view without a mode switch.
        // With no comments they flash instead of exporting (and the early
        // return keeps the test off the real clipboard).
        let mut app = make_app(10, Mode::View);
        on_view_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "s flashes 'no comments yet'");
        app.status = None;
        on_view_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "d flashes 'no comment on this line'");
    }

    #[test]
    fn view_jk_skips_blank_lines() {
        // The view's j/k stop only on content lines — blanks render as gap
        // rows and stopping there is a wasted keypress. The mouse, a
        // selection's j/k extension, and source mode still reach blanks.
        let mut app = make_app(7, Mode::View);
        app.source.lines = vec![
            "a".into(),
            String::new(),
            String::new(),
            "b".into(),
            String::new(),
            "c".into(),
            String::new(),
        ];
        app.view.rows = (0..7).map(|_| vec![]).collect();
        app.view.source_starts = (0..7).collect();
        app.view.goto_source_line(0);
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 3, "j skips the blank run onto b");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 5, "j skips onto c");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 5, "only a trailing blank below: stays put");
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 3, "k skips back over the blanks");
        // With a selection active, j/k extend line by line (blanks
        // included — a range must stay contiguous).
        app.selection = Some(Selection::new(3));
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(
            app.selection.unwrap().range(),
            (3, 4),
            "selection extension still walks every line"
        );
    }

    /// The cursor's display-row range under the current layout cache.
    fn cursor_row_range(app: &App) -> (usize, usize) {
        let start = app.row_of(app.cursor);
        (start, start + app.rows_of(app.cursor))
    }

    /// The cursor row range must sit inside the window the renderer paints
    /// (`[offset, offset + viewport)`), or the cursor line is off-screen.
    fn assert_cursor_visible(app: &App, viewport: usize) {
        let (start, end) = cursor_row_range(app);
        assert!(
            start >= app.offset,
            "cursor row {start} above the viewport top {} (cursor {} offset {})",
            app.offset,
            app.cursor,
            app.offset
        );
        assert!(
            end <= app.offset + viewport,
            "cursor row {end} below the viewport bottom {} (cursor {} offset {})",
            app.offset + viewport,
            app.cursor,
            app.offset
        );
    }

    #[test]
    fn j_spam_keeps_the_cursor_row_in_view() {
        // Regression: keep_cursor_visible was fed the raw terminal height,
        // so the down branch (end > offset + height) fired `height - viewport`
        // rows late — the cursor hid in the pane's bottom rows while the
        // viewport never caught up. The up branch (start < offset) is
        // height-independent, which is why only downward movement broke.
        let mut app = make_app(200, Mode::Source);
        let viewport = app.source_viewport_rows();
        for _ in 0..250 {
            on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
            assert_cursor_visible(&app, viewport);
        }
        assert_eq!(app.cursor, 199, "the cursor reached the last line");
    }

    #[test]
    fn one_j_or_k_yanks_a_scrolled_away_cursor_back_into_view() {
        // Wheel-style scrolling moves the viewport alone — the cursor keeps
        // its absolute file position and can sit far outside the window.
        // The next keyboard move must pull the viewport back in one step.
        let mut app = make_app(100, Mode::Source);
        // Cursor far above the viewport: one j lands it inside.
        app.cursor = 30;
        app.offset = 80;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 31);
        assert_cursor_visible(&app, app.source_viewport_rows());
        // Cursor far below the viewport: one k lands it inside — this
        // direction is the one that used to fail (height-dependent branch).
        app.cursor = 90;
        app.offset = 10;
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 89);
        assert_cursor_visible(&app, app.source_viewport_rows());
    }

    /// A session app over two temp files (a.md viewable, b.rs not).
    fn make_session() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("a.md");
        let rs_path = dir.path().join("b.rs");
        std::fs::write(&md_path, "# a\n\nline2\nline3\n").unwrap();
        std::fs::write(&rs_path, "fn main() {}\n").unwrap();
        let config = Config {
            files: vec![md_path, rs_path],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(None, false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        // Simulate run()'s per-file modes: a.md → View, b.rs → Source.
        app.file_states = vec![
            FileState {
                mode: Mode::View,
                ..Default::default()
            },
            FileState {
                mode: Mode::Source,
                ..Default::default()
            },
        ];
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        (app, dir)
    }

    #[test]
    fn supports_view_matches_markdown_family_only() {
        assert!(supports_view(Path::new("doc.md")));
        assert!(supports_view(Path::new("doc.MARKDOWN")));
        assert!(supports_view(Path::new("doc.mdx")));
        assert!(!supports_view(Path::new("doc.rs")));
        assert!(!supports_view(Path::new("doc.toml")));
        assert!(!supports_view(Path::new("doc")));
    }

    #[test]
    fn session_switches_files_with_brackets() {
        let (mut app, _dir) = make_session();
        assert_eq!(app.current_file_index, 0);
        assert_eq!(app.mode, Mode::View);
        // ] → next file (b.rs, source-only). The bracket alone falls back
        // to the file switch when the chord window expires.
        on_view_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        expire_chord(&mut app);
        assert_eq!(app.current_file_index, 1);
        assert_eq!(app.mode, Mode::Source, "b.rs opens in source mode");
        // Tab is a no-op on the source-only file.
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source, "Tab cannot enter view for .rs");
        assert!(app.status.is_some(), "a toast explains the restriction");
        // [ → back to a.md, view mode restored.
        on_source_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        expire_chord(&mut app);
        assert_eq!(app.current_file_index, 0);
        assert_eq!(app.mode, Mode::View, "per-file mode restored");
    }


    #[test]
    fn ctrl_o_opens_the_file_picker_and_enter_switches() {
        let (mut app, _dir) = make_session();
        // Ctrl+o opens the Files overlay, cursor on the current file.
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL, None);
        assert_eq!(app.overlay, Some(Overlay::Files));
        assert_eq!(app.overlay_cursor, 0, "starts on the current file");
        // j moves to b.rs; Enter switches and closes.
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_cursor, 1);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "Enter closes the picker");
        assert_eq!(app.current_file_index, 1);
        assert_eq!(app.mode, Mode::Source);
    }

    #[test]
    fn esc_and_q_close_the_overlay_without_quitting() {
        let (mut app, _dir) = make_session();
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL, None);
        assert!(app.overlay.is_some());
        on_overlay_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "q closes the picker, not the app");
        assert!(app.running, "the app keeps running");
        // Same for the comments list via l.
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Comments));
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn l_lists_all_comments_across_files_and_d_deletes() {
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 2,
            end: 2,
            lines: "line2".into(),
            revision: None,
            text: "on a".into(),
        });
        app.comments.push(Comment {
            file_path: b.clone(),
            start: 1,
            end: 1,
            lines: "fn main".into(),
            revision: None,
            text: "on b".into(),
        });
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Comments));
        // The list is sorted by path: b.rs < a.md? No — the list sorts by
        // file_path; the temp dir names are random, so just check both
        // entries are reachable and delete works.
        on_overlay_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 1, "d deletes the selected comment");
        assert_eq!(app.overlay, Some(Overlay::Comments), "the list stays open");
    }

    #[test]
    fn comments_overlay_enter_jumps_to_file_and_line() {
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        app.comments.push(Comment {
            file_path: a,
            start: 3,
            end: 3,
            lines: "line3".into(),
            revision: None,
            text: "note".into(),
        });
        // Start on b.rs (index 1); jump via the list back to a.md line 3.
        on_source_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        expire_chord(&mut app);
        assert_eq!(app.current_file_index, 1);
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "Enter closes the list");
        assert_eq!(app.current_file_index, 0, "jumped back to a.md");
        assert_eq!(app.mode, Mode::View, "a.md restored to view mode");
        assert_eq!(app.view.cursor, 2, "cursor lands on the comment line (0-based)");
    }

    #[test]
    fn comments_overlay_enter_restores_a_historical_revision() {
        // A comment anchored to a past revision (made while browsing the
        // timeline) must restore that revision before jumping: its line
        // numbers belong to that document, not the live tree, and its
        // card only renders under that revision. Landing on NOW would
        // point the selection at the wrong lines with no card in sight.
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        let path = app.files[0].clone();
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "shorter version".into(),
            content: "line1\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        let revision = app.histories[0].revisions[1].context().unwrap();
        app.comments.push(Comment {
            file_path: path,
            start: 2,
            end: 3,
            lines: "line2\nline3".into(),
            revision: Some(revision.clone()),
            text: "on the past".into(),
        });
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "Enter closes the list");
        assert_eq!(
            app.histories[0].position, 1,
            "the comment's revision is restored"
        );
        assert_eq!(
            app.source.content, "line1\nline2\nline3\n",
            "the document shows the comment's revision"
        );
        let (a, b) = app.selection.unwrap().range();
        assert_eq!((a, b), (1, 2), "selection lands on the comment's lines");
        assert!(
            visible_cards(&app)
                .iter()
                .any(|c| c.revision.as_deref() == Some(revision.as_str())),
            "the comment's card is visible under its revision"
        );
    }

    #[test]
    fn comments_overlay_enter_restores_a_legacy_bare_local_revision() {
        // 旧形式（`local:<id>` 裸）で保存済みのコメントは、新形式 context
        // （id — 説明文）の世代と identity 部分だけで照合され、移行なしで復元できる。
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        let path = app.files[0].clone();
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:legacyid".into()),
            short_id: "legacy".into(),
            summary:
                "akapen local snapshot: uncommitted state captured 2026-08-16T05:03:22Z \
                 (not a git object)"
                    .into(),
            content: "line1\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        // アップグレード前のセッションで保存された裸の id を持つコメント。
        app.comments.push(Comment {
            file_path: path,
            start: 2,
            end: 3,
            lines: "line2\nline3".into(),
            revision: Some("local:legacyid".into()),
            text: "on the past".into(),
        });
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "Enter closes the list");
        assert_eq!(
            app.histories[0].position, 1,
            "the legacy bare id still restores the new-format generation"
        );
        assert_eq!(
            app.source.content, "line1\nline2\nline3\n",
            "the document shows the comment's revision"
        );
        assert!(
            visible_cards(&app).iter().any(|c| {
                crate::history::same_revision(c.revision.as_deref(), Some("local:legacyid"))
            }),
            "the card renders under the new-format context"
        );
    }

    #[test]
    fn comments_overlay_enter_restores_a_revision_on_another_file() {
        // The comment list spans every file; jumping to a historical
        // comment on ANOTHER file must restore that file's revision, not
        // just switch and land on the wrong lines.
        let (mut app, _dir) = make_session();
        let b = app.files[1].clone();
        app.histories = vec![
            DocumentHistory {
                revisions: vec![history::Revision {
                    id: None,
                    short_id: "now".into(),
                    summary: "working tree".into(),
                    content: "# a\n\nline2\nline3\n".into(),
                    source: history::RevisionSource::Now,
                    timestamp_ms: None,
                }],
                position: 0,
                rendered_position: 0,
                reviewed_id: None,
                reviewed_content: None,
            },
            DocumentHistory {
                revisions: vec![
                    history::Revision {
                        id: None,
                        short_id: "now".into(),
                        summary: "working tree".into(),
                        content: "fn main() {}\n".into(),
                        source: history::RevisionSource::Now,
                        timestamp_ms: None,
                    },
                    history::Revision {
                        id: Some("local:old".into()),
                        short_id: "old".into(),
                        summary: "older shape".into(),
                        content: "fn old() {}\n".into(),
                        source: history::RevisionSource::Local,
                        timestamp_ms: None,
                    },
                ],
                position: 0,
                rendered_position: 0,
                reviewed_id: None,
                reviewed_content: None,
            },
        ];
        let revision = app.histories[1].revisions[1].context().unwrap();
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "fn old() {}".into(),
            revision: Some(revision.clone()),
            text: "old fn".into(),
        });
        // a.md (index 0) has no comments, so the first selectable entry
        // is b.rs's comment.
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
        assert_eq!(app.current_file_index, 1, "switched to the comment's file");
        assert_eq!(
            app.histories[1].position, 1,
            "the target file's revision is restored"
        );
        assert_eq!(
            app.source.content, "fn old() {}\n",
            "the target file shows that revision"
        );
        assert_eq!(app.selection.unwrap().range(), (0, 0));
    }

    #[test]
    fn comments_overlay_enter_falls_back_when_the_revision_is_stale() {
        // A comment whose revision no longer exists in the history (e.g.
        // a pruned local snapshot) falls back to a plain line jump in
        // the current document instead of panicking or mis-seeking.
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        let path = app.files[0].clone();
        app.comments.push(Comment {
            file_path: path,
            start: 2,
            end: 2,
            lines: "line2".into(),
            revision: Some("local:pruned".into()),
            text: "ghost".into(),
        });
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
        assert_eq!(
            app.histories[0].position, 0,
            "no seek without a matching revision"
        );
        assert_eq!(
            app.selection.unwrap().range(),
            (1, 1),
            "plain line jump in the current document"
        );
    }

    #[test]
    fn comments_overlay_enter_returns_to_now_for_a_live_comment() {
        // Selecting a live (revision-less) comment while browsing the
        // past returns to NOW: the comment lives in the working tree, so
        // the jump must show it there, not in the wrong document.
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        let path = app.files[0].clone();
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "shorter".into(),
            content: "line1\nline2\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        // Browse into the past, then open the list from there.
        app.histories[0].position = 1;
        app.history_render_due = Some(std::time::Instant::now());
        render_pending_history(&mut app, false);
        assert_eq!(app.source.content, "line1\nline2\n", "browsing the past");
        app.comments.push(Comment {
            file_path: path,
            start: 3,
            end: 3,
            lines: "line3".into(),
            revision: None,
            text: "live".into(),
        });
        on_view_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.histories[0].position, 0, "back to NOW");
        assert_eq!(
            app.source.content, "line1\nline2\nline3\nline4\nline5\n",
            "the working tree is rendered"
        );
        assert_eq!(app.selection.unwrap().range(), (2, 2));
    }

    #[test]
    fn reload_preserves_comments_on_all_files() {
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "on a".into(),
        });
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "y".into(),
            revision: None,
            text: "on b".into(),
        });
        std::fs::write(&a, "# a\n\nchanged\n").unwrap();
        assert!(reload_source(&mut app, false).is_ok());
        assert_eq!(app.comments.len(), 2);
        assert!(app.comments.iter().any(|comment| comment.text == "on a"));
        assert!(app.comments.iter().any(|comment| comment.text == "on b"));
    }

    #[test]
    fn switching_back_rerenders_a_stale_width_view() {
        // A resize while a file sits in the background leaves its saved
        // view at the old wrap width; switching back must re-render it.
        let (mut app, _dir) = make_session();
        app.view.width = 10; // simulate a stale render width
        on_view_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        expire_chord(&mut app);
        on_source_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        expire_chord(&mut app);
        assert_ne!(
            app.view.width, 10,
            "the restored view re-renders at the current width"
        );
    }

    #[test]
    fn unique_suffix_keeps_unique_basenames_short() {
        let all = vec![
            PathBuf::from("testdata/a.md"),
            PathBuf::from("testdata/b.md"),
            PathBuf::from("src/c.md"),
        ];
        assert_eq!(unique_suffix(&all[0], &all), "a.md");
        assert_eq!(unique_suffix(&all[1], &all), "b.md");
        assert_eq!(unique_suffix(&all[2], &all), "c.md");
    }

    #[test]
    fn unique_suffix_disambiguates_colliding_basenames() {
        let all = vec![
            PathBuf::from("docs/design.md"),
            PathBuf::from("src/design.md"),
            PathBuf::from("src/main.rs"),
        ];
        // design.md collides: the parent dir joins the row.
        assert_eq!(unique_suffix(&all[0], &all), "docs/design.md");
        assert_eq!(unique_suffix(&all[1], &all), "src/design.md");
        // main.rs is unique: bare basename.
        assert_eq!(unique_suffix(&all[2], &all), "main.rs");
        // Component-wise matching: `a.md` must not collide with `ba.md`.
        let all2 = vec![PathBuf::from("x/a.md"), PathBuf::from("ba.md")];
        assert_eq!(unique_suffix(&all2[0], &all2), "a.md");
    }

    #[test]
    fn common_parent_finds_the_shared_dir() {
        let same = vec![
            PathBuf::from("testdata/a.md"),
            PathBuf::from("testdata/b.md"),
        ];
        assert_eq!(common_parent(&same).as_deref(), Some("testdata/"));
        // Deeper common ancestor is reported fully.
        let deep = vec![
            PathBuf::from("a/b/x.md"),
            PathBuf::from("a/b/y.md"),
        ];
        assert_eq!(common_parent(&deep).as_deref(), Some("a/b/"));
        // Different dirs: no common parent.
        let mixed = vec![
            PathBuf::from("testdata/a.md"),
            PathBuf::from("src/b.md"),
        ];
        assert_eq!(common_parent(&mixed), None);
        // Cwd-relative single files: parent is empty.
        let flat = vec![PathBuf::from("a.md"), PathBuf::from("b.md")];
        assert_eq!(common_parent(&flat), None);
    }

    #[test]
    fn comments_overlay_groups_rows_by_file() {
        // Two comments on a.md, one on b.rs: the overlay must show each
        // file once as a header (basename + count) and one line per
        // comment — never the full temp path on every row.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "one".into(),
        });
        app.comments.push(Comment {
            file_path: a,
            start: 2,
            end: 2,
            lines: "x".into(),
            revision: None,
            text: "two".into(),
        });
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "y".into(),
            revision: None,
            text: "three".into(),
        });
        app.overlay = Some(Overlay::Comments);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("a.md (2)"), "group header with count: {content}");
        assert!(content.contains("b.rs (1)"), "second group header: {content}");
        // Rows are short (range + body), not the old full-path location
        // format (`.../a.md:1`).
        assert!(
            !content.contains("a.md:1"),
            "the full-path location is not repeated per comment: {content}"
        );
        // Both comments on a.md still show their line ranges.
        assert!(content.contains("1  one"), "row 1: {content}");
        assert!(content.contains("2  two"), "row 2: {content}");
    }

    #[test]
    fn title_comment_indicator_appears_only_with_comments() {
        // The top-right `▌ N` lights up when the first comment exists and
        // disappears when the comments are cleared (e.g. after `s`).
        let (mut app, _dir) = make_session();
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        assert!(
            !capture(&mut app).contains("▌ 1"),
            "no indicator without comments"
        );
        let a = app.files[0].clone();
        app.comments.push(Comment {
            file_path: a,
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "c".into(),
        });
        assert!(capture(&mut app).contains("▌ 1"), "indicator appears with comments");
        app.comments.clear();
        assert!(
            !capture(&mut app).contains("▌ 1"),
            "indicator disappears when cleared"
        );
    }

    #[test]
    fn toast_floats_over_content_not_footer() {
        // A toast is an overlay at the BOTTOM of the body (row 22, one
        // above the footer — the classic message-line position): the
        // footer keeps the badge + hints, and nothing scrolls.
        let (mut app, _dir) = make_session();
        // The tachyonfx fade (from black) is a separate concern; the
        // steady-state colors are tested with it off.
        app.config.fx = false;
        app.flash("hello toast");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let row22: String = buf.content[22 * 80..23 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            row22.contains("hello toast"),
            "toast floats at the bottom of the body: {row22}"
        );
        let last: String = buf.content[23 * 80..24 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            !last.contains("hello toast"),
            "toast is not in the footer: {last}"
        );
        assert!(last.contains("VIEW"), "footer keeps the mode badge: {last}");
        // An info toast renders yellow.
        let yellow = buf.content[22 * 80..23 * 80]
            .iter()
            .any(|c| c.style().fg == Some(Color::Yellow));
        assert!(yellow, "info toasts are yellow");
    }

    #[test]
    fn error_toast_is_red_and_beeps() {
        // Can't-do feedback: a red toast at the bottom (the BEL beep
        // itself is emitted to stdout at flash time).
        let (mut app, _dir) = make_session();
        app.config.fx = false; // steady-state colors, no fade
        app.flash_err("cannot do that");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let row22: String = buf.content[22 * 80..23 * 80]
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(row22.contains("cannot do that"));
        let red = buf.content[22 * 80..23 * 80]
            .iter()
            .any(|c| c.style().fg == Some(Color::Red));
        assert!(red, "error toasts are red");
    }


    #[test]
    fn fx_effects_are_created_only_with_the_fx_flag() {
        // `--fx` (default) arms the time-machine frame at startup and a
        // fade effect for every toast; `--no-fx` leaves both uncreated.
        let (app, _dir) = make_session();
        assert!(
            app.time_machine_fx.is_some(),
            "the time-machine frame effect exists with --fx"
        );
        assert!(
            app.starfield_fx.is_some(),
            "the starfield exists with --fx too"
        );
        // A fresh App built with `--no-fx` from the start has no frame
        // effect, and its toasts never fade.
        let (mut app, _dir) = make_session();
        app.config.fx = false;
        app.time_machine_fx = None;
        app.flash("hello");
        assert!(app.toast_fx.is_none(), "--no-fx: no toast fade either");
    }

    #[test]
    fn title_ys_hint_reads_the_room() {
        // The y/s explainer shows only what is actionable: nothing with
        // no comments, `y copy` only without --send-cmd, both with it.
        // Still low priority: it vanishes when the path needs the room.
        let (mut app, _dir) = make_session();
        let capture = |app: &mut App, width: u16| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .take(width as usize)
                .collect()
        };
        // No comments: neither key does anything — no explainer at all.
        assert!(!capture(&mut app, 80).contains("y copy"));
        assert!(!capture(&mut app, 80).contains("s send"));
        // With comments but no --send-cmd: only the copy key is shown.
        app.comments.push(Comment {
            file_path: app.files[0].clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "c".into(),
        });
        assert!(capture(&mut app, 80).contains("y copy"));
        assert!(!capture(&mut app, 80).contains("s send"));
        // With --send-cmd: both.
        app.config.send_cmd = Some("true".to_string());
        assert!(capture(&mut app, 80).contains("y copy · s send"));
        // The explainer still yields to a narrow screen.
        assert!(!capture(&mut app, 30).contains("y copy"));
    }

    #[test]
    fn title_clicks_open_overlays() {
        // Title bar buttons: `▌ N` → comment list, `1/3 files` → file
        // picker (cursor on the current file). Body clicks do neither.
        let (mut app, _dir) = make_session();
        app.comments.push(Comment {
            file_path: app.files[0].clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "c".into(),
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let m = title_metrics(&app, 80);
        // Comment counter → comment list.
        on_mouse(&mut app, click(0, m.indicator_x + 1));
        assert_eq!(app.overlay, Some(Overlay::Comments));
        app.overlay = None;
        // File counter → file picker, cursor on the current file.
        on_mouse(&mut app, click(0, m.file_count_x + 1));
        assert_eq!(app.overlay, Some(Overlay::Files));
        assert_eq!(app.overlay_cursor, app.current_file_index);
        app.overlay = None;
        // A body click (row 1) opens nothing.
        on_mouse(&mut app, click(1, 5));
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn title_file_counter_shows_position() {
        // `1/3 files` reflects the current file index; hidden for a
        // single-file session.
        let (mut app, _dir) = make_session();
        assert_eq!(
            title_metrics(&app, 80).file_count,
            " 1/2 files",
            "position is 1-based"
        );
        app.current_file_index = 1;
        assert_eq!(title_metrics(&app, 80).file_count, " 2/2 files");
        // Single file: no counter.
        app.files = vec![app.files[0].clone()];
        app.current_file_index = 0;
        assert_eq!(title_metrics(&app, 80).file_count, "");
    }

    #[test]
    fn title_reply_mode_counts_messages_not_files() {
        // Reply mode: the files are recent agent messages — the counter
        // says msgs so ]/[ navigation reads as message navigation.
        let (mut app, _dir) = make_session();
        app.config.reply = true;
        assert_eq!(
            title_metrics(&app, 80).file_count,
            " 1/2 msgs",
            "reply mode labels the counter with messages"
        );
        app.current_file_index = 1;
        assert_eq!(title_metrics(&app, 80).file_count, " 2/2 msgs");
    }

    #[test]
    fn title_esc_close_badge_tracks_esc_quit() {
        let (mut app, _dir) = make_session();
        // Default (no callback): no badge — Esc is not a close key.
        assert_eq!(title_metrics(&app, 80).esc_close, "");
        // With the esc-quit affordance live, the filled badge appears at
        // the top-right, right of the comment counter.
        app.config.callback = Some("fzf".into());
        let m = title_metrics(&app, 80);
        assert_eq!(m.esc_close, " esc close ");
        assert_eq!(m.esc_close_x + m.esc_close_w, 80, "flush right");
        assert!(
            m.indicator_x + m.indicator_w <= m.esc_close_x,
            "badge sits right of the comment counter"
        );
        // While composing, Esc cancels the composer instead: hide it.
        app.mode = Mode::Input;
        assert_eq!(title_metrics(&app, 80).esc_close, "");
    }

    #[test]
    fn title_esc_close_badge_keeps_room_over_the_path() {
        let (mut app, _dir) = make_session();
        app.config.callback = Some("fzf".into());
        let m = title_metrics(&app, 20);
        assert_eq!(m.esc_close, " esc close ", "badge survives narrow widths");
        assert!(
            m.path_w + m.esc_close_w <= 20,
            "the path yields to the badge: {m:?}"
        );
    }

    #[test]
    fn files_overlay_shows_the_change_bolt() {
        // The picker carries the title bar's ⚡ for files whose on-disk
        // state differs from what was loaded — background files included
        // (the poll loop only watches the current file).
        let (mut app, _dir) = make_session();
        // a.md (current): seen stamp matches disk → no ⚡.
        let meta = std::fs::metadata(&app.files[0]).unwrap();
        app.last_loaded_stamp = Some((meta.modified().unwrap(), meta.len()));
        // b.rs (background): a stale seen stamp → ⚡.
        app.file_states[1].last_loaded_stamp = Some((SystemTime::UNIX_EPOCH, 0));
        assert!(!file_externally_changed(&app, 0));
        assert!(file_externally_changed(&app, 1));
        app.overlay = Some(Overlay::Files);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("b.rs ⚡"), "changed file carries ⚡: {content}");
        assert!(!content.contains("a.md ⚡"), "unchanged file stays clean: {content}");
        // A file with no recorded stamp (load state unknown) shows no ⚡.
        app.file_states[1].last_loaded_stamp = None;
        assert!(!file_externally_changed(&app, 1));
    }

    #[test]
    fn overlay_mouse_selects_entries() {
        // File picker: clicking an entry row moves the overlay cursor;
        // the title row does nothing; the content underneath is untouched.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let view_cursor = app.view.cursor;
        on_mouse(&mut app, click(panel.y + 2, panel.x + 5)); // first entry
        assert_eq!(app.overlay_cursor, 0);
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5)); // second entry
        assert_eq!(app.overlay_cursor, 1);
        on_mouse(&mut app, click(panel.y + 1, panel.x + 5)); // title row
        assert_eq!(app.overlay_cursor, 1, "title row is not an entry");
        assert_eq!(
            app.view.cursor, view_cursor,
            "the content cursor is untouched while an overlay is open"
        );
    }

    #[test]
    fn overlay_mouse_selects_comments_skipping_headers() {
        // Comments list: rows are title, header(a), c0, c1, header(b), c2
        // — clicking a comment row selects it; a header row does nothing.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "one".into(),
        });
        app.comments.push(Comment {
            file_path: a,
            start: 2,
            end: 2,
            lines: "x".into(),
            revision: None,
            text: "two".into(),
        });
        app.comments.push(Comment {
            file_path: b,
            start: 1,
            end: 1,
            lines: "y".into(),
            revision: None,
            text: "three".into(),
        });
        app.overlay = Some(Overlay::Comments);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5)); // c0 (first comment row)
        assert_eq!(app.overlay_cursor, 0);
        on_mouse(&mut app, click(panel.y + 4, panel.x + 5)); // c1
        assert_eq!(app.overlay_cursor, 1);
        on_mouse(&mut app, click(panel.y + 6, panel.x + 5)); // c2 (after header b)
        assert_eq!(app.overlay_cursor, 2);
        on_mouse(&mut app, click(panel.y + 2, panel.x + 5)); // header row
        assert_eq!(app.overlay_cursor, 2, "header rows are not selectable");
    }

    #[test]
    fn overlay_wheel_moves_selection() {
        // The wheel moves the overlay cursor (j/k semantics) while an
        // overlay is open, clamped to the entry count.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let wheel = |kind: MouseEventKind| MouseEvent {
            kind,
            column: 0,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.overlay_cursor, 1);
        on_mouse(&mut app, wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.overlay_cursor, 0);
        on_mouse(&mut app, wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.overlay_cursor, 0, "clamped at the top");
    }

    #[test]
    fn footer_hints_are_labeled_and_offer_help() {
        // The footer shows `L{line}/{total}` (1-based, cursor-anchored), a
        // few labeled actions, and `? help` for the full reference.
        let mut app = make_app(10, Mode::View);
        assert!(footer_hints(&app).contains("L1/10"), "cursor line 1 of 10");
        assert!(footer_hints(&app).contains("j/k scroll"));
        assert!(footer_hints(&app).contains("c comment"));
        assert!(footer_hints(&app).contains("? help"));
        app.view.goto_source_line(4);
        assert!(footer_hints(&app).contains("L5/10"));
        app.selection = Some(Selection::new(3));
        assert!(footer_hints(&app).contains("4–4"));
        assert!(footer_hints(&app).contains("j/k extend"));
        assert!(
            footer_hints(&app).contains("Esc cancel"),
            "the selection state spells out the way out: {}",
            footer_hints(&app)
        );
        let mut app2 = make_app(10, Mode::Source);
        assert!(footer_hints(&app2).contains("L1/10"));
        app2.cursor = 9;
        assert!(footer_hints(&app2).contains("L10/10"));
        assert!(footer_hints(&app2).contains("v select"));
        assert!(footer_hints(&app2).contains("? help"));
        // An empty file reports L0/0 instead of an out-of-range line.
        let app3 = make_app(0, Mode::Source);
        assert!(footer_hints(&app3).contains("L0/0"));
    }

    #[test]
    fn the_title_keeps_only_the_tiny_position_badge_while_browsing() {
        // Browsing history must not duplicate the generation readout:
        // the full label (provenance · id · age · summary) lives in the
        // scrubber tooltip next to the timeline's ◆, the title keeps
        // only the `◆ 1/2` position badge (the fallback identity for
        // narrow terminals where the bar never appears), and the footer
        // keeps only the short navigation affordance.
        let mut app = make_app(3, Mode::Source);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;

        // The footer names the direction, never the generation.
        let hints = footer_hints(&app);
        assert!(hints.contains("← older · newer →"), "nav affordance stays: {hints}");
        assert!(
            !hints.contains("LOCAL") && !hints.contains("local snapshot"),
            "no generation label in the footer: {hints}"
        );
        // The title's state slot carries the tiny badge, nothing more.
        let m = title_metrics(&app, 80);
        assert_eq!(m.change, " ◆ 1/2 ", "title keeps only the badge: {}", m.change);
        assert!(
            !m.change.contains("LOCAL") && !m.change.contains("base"),
            "no long label in the title: {}",
            m.change
        );
        assert!(
            m.path.contains("doc"),
            "the path keeps the room the label used to take: {}",
            m.path
        );
    }

    #[test]
    fn timeline_bar_appears_while_browsing_and_slides_out_at_now() {
        let mut app = make_app(3, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        // At NOW the bar is hidden.
        assert!(!crate::timeline::timeline_visible(&app));
        assert!(app.timeline_fx.is_none());
        // First step into the past: the bar appears and slides in.
        assert!(select_history(&mut app, 1));
        assert!(crate::timeline::timeline_visible(&app));
        assert!(app.timeline_fx.is_some(), "slide-in effect created");
        assert!(app.timeline_exit_until.is_none());
        // Returning to NOW: the bar lingers for the slide-out, then dies.
        // The linger window is injectable — the 200 ms default raced
        // under parallel test load (the thread could stall between
        // arming and asserting), so widen it to make this deterministic.
        app.timeline_exit_ms = Duration::from_secs(5);
        assert!(select_history(&mut app, -1));
        assert!(!crate::timeline::timeline_visible(&app));
        assert!(
            app.timeline_exit_until.is_some(),
            "slide-out window armed"
        );
        assert!(
            crate::timeline::timeline_active(&app),
            "lingering while the slide-out plays"
        );
        assert!(app.timeline_fx.is_some(), "slide-out effect created");
    }

    #[test]
    fn timeline_bar_has_no_effects_without_fx() {
        let mut app = make_app(3, Mode::View);
        app.config.fx = false;
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        assert!(select_history(&mut app, 1));
        assert!(crate::timeline::timeline_visible(&app));
        assert!(app.timeline_fx.is_none(), "no effects with --no-fx");
        assert!(select_history(&mut app, -1));
        assert!(app.timeline_exit_until.is_none(), "no linger without fx");
        assert!(!crate::timeline::timeline_active(&app));
    }

    #[test]
    fn timeline_overlay_scrubs_and_restores_on_esc() {
        let mut app = make_app(3, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "oldest".into(),
            content: "oldest first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Git,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        // Open via `t`.
        on_view_key(&mut app, KeyCode::Char('t'), KeyModifiers::empty(), None);
        assert_eq!(app.overlay, Some(Overlay::Timeline));
        assert_eq!(app.overlay_cursor, 1);
        assert_eq!(app.timeline_restore, Some(1));
        // j scrubs older; the document render is scheduled behind it.
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::empty());
        assert_eq!(app.histories[0].position, 2);
        assert_eq!(app.overlay_cursor, 2);
        // k back toward NOW.
        on_overlay_key(&mut app, KeyCode::Char('k'), KeyModifiers::empty());
        assert_eq!(app.histories[0].position, 1);
        // Esc restores the position the list opened at, and closes.
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(app.overlay, None);
        assert_eq!(app.histories[0].position, 1);
        assert!(
            app.history_render_due.is_some(),
            "the restored position re-renders"
        );
    }

    #[test]
    fn timeline_overlay_enter_keeps_the_selected_position() {
        let mut app = make_app(3, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "oldest".into(),
            content: "oldest first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Git,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        on_view_key(&mut app, KeyCode::Char('t'), KeyModifiers::empty(), None);
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::empty());
        assert_eq!(app.histories[0].position, 2);
        // Enter confirms the scrubbed position.
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(app.overlay, None);
        assert_eq!(app.histories[0].position, 2);
    }

    #[test]
    fn timeline_overlay_t_is_gated_and_toggles() {
        // A one-point timeline has no list: `t` is a no-op.
        let mut app = make_app(3, Mode::View);
        on_view_key(&mut app, KeyCode::Char('t'), KeyModifiers::empty(), None);
        assert_eq!(app.overlay, None);
        // With history, `t` opens and closes again (keeping the position).
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        on_view_key(&mut app, KeyCode::Char('t'), KeyModifiers::empty(), None);
        assert_eq!(app.overlay, Some(Overlay::Timeline));
        on_overlay_key(&mut app, KeyCode::Char('t'), KeyModifiers::empty());
        assert_eq!(app.overlay, None);
        assert_eq!(app.histories[0].position, 1, "t close keeps the position");
    }

    #[test]
    fn timeline_bar_renders_the_axis_over_the_bottom_rows() {
        // Golden-ish frame check: while browsing, the timeline bar owns
        // the bottom three rows (2-row mode at 80 columns: words + axis;
        // the times row joins at 100+). The NOW anchor sits at the right
        // edge, the current point is marked.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "oldest".into(),
            content: "oldest first line\nline2\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Git,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        // Owned snapshot: the buffer borrow must not outlive the redraw.
        let frame_text =
            |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| -> Vec<String> {
                let buf = terminal.backend().buffer();
                buf.content
                    .chunks(80)
                    .map(|row| {
                        row.iter()
                            .map(|c| c.symbol().chars().next().unwrap_or(' '))
                            .collect()
                    })
                    .collect()
            };
        let frame = frame_text(&terminal);
        let axis = &frame[22];
        assert!(axis.ends_with('●'), "NOW at the right edge: {axis}");
        assert!(axis.contains('◆'), "the current point on the axis: {axis}");
        assert!(axis.contains('◼'), "commit marker on the axis: {axis}");
        assert!(
            axis.find('◆').unwrap_or(0) < axis.rfind('●').unwrap_or(0),
            "the current point sits left of NOW: {axis}"
        );
        let words = &frame[23];
        assert!(words.contains("HERE"), "words row: {words}");
        assert!(words.trim_end().ends_with("NOW"), "NOW at the right edge: {words}");
        // The footer hints are gone while the bar owns the row.
        assert!(!words.contains("VIEW"), "no mode badge under the bar: {words}");
        // At NOW the same frame has no bar: the footer hints return.
        app.histories[0].position = 0;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let frame = frame_text(&terminal);
        let footer = &frame[23];
        assert!(footer.contains("VIEW"), "footer badge returns: {footer}");
        let footer = &frame[23];
        assert!(footer.contains("VIEW"), "footer badge returns: {footer}");
    }

    #[test]
    fn scrubber_tooltip_floats_over_the_axis_and_yields_to_toasts() {
        // A history step floats the revision readout (glyph · id · age ·
        // summary) on the message row above the axis; a toast owns that
        // row outright, and an expired hold removes the tooltip.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "old subject".into(),
            content: "old first line\nline2\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Git,
            timestamp_ms: Some(crate::snapshot::now_ms().saturating_sub(2 * 3_600_000)),
        });
        assert!(select_history(&mut app, 1));
        assert!(app.timeline_tooltip_until.is_some());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let frame_text =
            |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| -> Vec<String> {
                let buf = terminal.backend().buffer();
                buf.content
                    .chunks(80)
                    .map(|row| {
                        row.iter()
                            .map(|c| c.symbol().chars().next().unwrap_or(' '))
                            .collect()
                    })
                    .collect()
            };
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let frame = frame_text(&terminal);
        // words 23 · axis 22 · tooltip on the message row 21.
        let tooltip = &frame[21];
        assert!(tooltip.contains("◼ cafe"), "provenance and id: {tooltip}");
        assert!(tooltip.contains("2h ago"), "relative age: {tooltip}");
        assert!(tooltip.contains("old subject"), "summary: {tooltip}");
        // The readout floats over document text: it sits on the nebula
        // band (never the terminal's own background) so it reads as a
        // time-machine instrument, not a line of the page.
        let x = tooltip.chars().position(|c| c == '◼').unwrap() as u16;
        assert_eq!(
            terminal.backend().buffer()[(x, 21)].bg,
            crate::view::tooltip_band_bg(false),
            "the tooltip rides the nebula band"
        );
        // A toast (an edge error) owns the row; the tooltip yields
        // entirely instead of leaving fragments beside the banner.
        app.flash_err("oldest document version");
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let frame = frame_text(&terminal);
        let row = &frame[21];
        assert!(row.contains("oldest document version"), "toast wins: {row}");
        assert!(!row.contains("cafe"), "no tooltip fragments: {row}");
        // Mid-dissolve the band must thin out, not linger as a solid
        // strip: hidden cells are not overdrawn at all, so the document
        // (and the terminal background) shows through them. The old
        // tachyonfx dissolve blanked only the glyphs and left the whole
        // background strip sitting over the page.
        app.status = None;
        app.toast_fx = None;
        let band_cells = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
            let buf = terminal.backend().buffer();
            (0..80)
                .filter(|&x| buf[(x, 21)].bg == crate::view::tooltip_band_bg(false))
                .count()
        };
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let full = band_cells(&terminal);
        assert!(full > 0, "the band is on screen during the hold");
        // The dissolve fraction rides the wall clock (deadline minus
        // now), so under parallel test load a single draw can slip past
        // the 150 ms target and see an empty (or still-full) band.
        // Retry until one draw lands inside the window; a systematic
        // regression (band never thins, or vanishes wholesale) fails
        // every attempt.
        let mut dissolving = full;
        for _ in 0..20 {
            app.timeline_tooltip_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(150));
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            dissolving = band_cells(&terminal);
            if dissolving > 0 && dissolving < full {
                break;
            }
        }
        assert!(
            dissolving < full,
            "mid-dissolve the band thins out: {dissolving} of {full} cells remain"
        );
        assert!(dissolving > 0, "mid-dissolve some of the band still shows");
        // The hold expires: the readout leaves the screen.
        app.timeline_tooltip_until = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(1));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let frame = frame_text(&terminal);
        assert!(!frame[21].contains("cafe"), "expired tooltip is gone: {}", frame[21]);
        assert_eq!(
            band_cells(&terminal),
            0,
            "no background strip survives the deadline"
        );
    }

    #[test]
    fn scrubber_tooltip_clamps_at_the_edges_instead_of_spilling() {
        // The band rides the ◆. Next to NOW the ◆ sits near the right
        // edge; a long summary must clamp inside the screen (a spilled
        // Rect would panic the buffer indexing) and clip with an
        // ellipsis instead of crawling out.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "a very long subject line that cannot possibly fit whole \
                      into an eighty column terminal next to the other parts"
                .into(),
            content: "old first line\nline2\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Git,
            timestamp_ms: Some(crate::snapshot::now_ms()),
        });
        assert!(select_history(&mut app, 1));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let row: String = terminal
            .backend()
            .buffer()
            .content
            .chunks(80)
            .nth(21)
            .unwrap()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(row.contains("◼ cafe"), "the readout still shows: {row}");
        assert!(row.contains('…'), "the summary clips with an ellipsis: {row}");
    }

    #[test]
    fn ghost_effects_drop_when_the_next_revision_has_no_deletions() {
        // A revision without deletions leaves the previous ghost list
        // stale: its row numbers refer to the OLD view, and rendering
        // it against the new view used to underflow the backspace
        // pacing (total - 1 - order) in ghost_effect. The render must
        // drop the stale effects.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:del".into()),
            short_id: "del".into(),
            summary: "deleted a line".into(),
            content: "line1\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:add".into()),
            short_id: "add".into(),
            summary: "added a line".into(),
            content: "line1\nline3\nline4\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        // Render the deletion revision: ghosts appear.
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(!app.ghost_fx.is_empty(), "deletions ghost out");
        assert!(app.history_ghost_until.is_some());
        // Move to a revision that only adds lines: no ghosts, and the
        // stale ones must be gone (their rows belong to the old view).
        app.histories[0].position = 2;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(
            app.ghost_fx.is_empty(),
            "no deletions in the new revision, no stale ghosts"
        );
    }

    #[test]
    fn the_add_phase_waits_for_the_delete_phase() {
        // A revision that BOTH removes and adds lines plays as two
        // phases: the scatter-in for the new text is parked behind the
        // ghost phase (backspace + collapse), so the reveal starts only
        // after [`crate::effects::GHOST_PHASE_MS`] — the new text
        // materializes in its final position and never moves. The
        // `fx::delay` wrapper makes the effect's total timer span the
        // delete phase plus the reveal.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:mix".into()),
            short_id: "mix".into(),
            summary: "removes line2/line4, adds line6".into(),
            content: "line1\nline3\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(!app.ghost_fx.is_empty(), "deletions ghost out");
        assert!(!app.appear_fx.is_empty(), "additions scatter in");
        let (_, _, fx) = &app.appear_fx[0];
        let total = fx.timer().map(|t| t.duration().as_millis()).unwrap_or(0);
        assert!(
            total > crate::effects::GHOST_PHASE_MS as u128,
            "the add phase waits for the delete phase (got {total} ms)"
        );

        // A pure-add revision has no delete phase: the stream starts
        // immediately, so the effect spans only its own reveal.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:add".into()),
            short_id: "add".into(),
            summary: "added a line".into(),
            content: "line1\nline2\nline3\nline4\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(app.ghost_fx.is_empty(), "no deletions, no ghost phase");
        let (_, _, fx) = &app.appear_fx[0];
        let total = fx.timer().map(|t| t.duration().as_millis()).unwrap_or(0);
        assert!(
            total <= crate::effects::GHOST_PHASE_MS as u128,
            "pure adds stream in immediately (got {total} ms)"
        );
    }

    #[test]
    fn the_collapse_preserves_the_parked_add_phase() {
        // The add phase is parked behind the delete phase (see
        // `the_add_phase_waits_for_the_delete_phase`). The ghost collapse
        // rebuilds the view to the same ghost-free layout the parked
        // effects were built against — it must NOT drop them, or the new
        // text would never stream in and only the backspace would show.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:mix".into()),
            short_id: "mix".into(),
            summary: "removes line2/line4, adds line6".into(),
            content: "line1\nline3\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(!app.appear_fx.is_empty(), "the add phase is parked");
        let parked = app.appear_fx.len();
        // The ghost phase expires: the layout collapses, the parked
        // effects must survive to stream in.
        app.history_ghost_until = Some(Instant::now() - Duration::from_millis(1));
        expire_history_ghosts(&mut app);
        assert_eq!(
            app.appear_fx.len(),
            parked,
            "the collapse keeps the parked add phase"
        );
        assert!(app.ghost_fx.is_empty(), "the ghosts themselves are gone");
        assert!(app.history_ghost_until.is_none());
    }

    #[test]
    fn the_ghost_collapse_waits_for_the_composer() {
        // Typing a comment right after a generation move lands the
        // composer inside the ghost phase (deletions backspace out for
        // ~550 ms before the layout folds). The collapse rebuilds the
        // view — if it fires while the composer is open, the rows below
        // the comment's anchor move and the terminal cursor jumps.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:del".into()),
            short_id: "del".into(),
            summary: "deleted a line".into(),
            content: "line1\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(!app.ghost_fx.is_empty(), "deletions ghost out");
        let rows_with_ghosts = app.view.rows.len();
        // Comment the cursor line while the ghosts are still up.
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input, "composer open");
        let anchor_while_typing = view_composer_anchor(&app);
        // The ghost phase expires mid-typing.
        app.history_ghost_until = Some(Instant::now() - Duration::from_millis(1));
        expire_history_ghosts(&mut app);
        assert_eq!(
            view_composer_anchor(&app),
            anchor_while_typing,
            "the collapse must not move the composer while typing"
        );
        assert_eq!(
            app.view.rows.len(),
            rows_with_ghosts,
            "ghost rows stay until the composer closes"
        );
        // Once the composer closes, the collapse runs and folds the
        // ghosts away.
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::View, "composer closed");
        expire_history_ghosts(&mut app);
        assert!(app.history_ghost_until.is_none());
        assert!(
            app.view.rows.len() < rows_with_ghosts,
            "the collapse folds the ghost rows after the composer"
        );
    }

    #[test]
    fn the_landing_pulse_fires_immediately_for_instant_transitions() {
        // `--no-fx` renders without effects: there is nothing to settle,
        // so the completion signal fires on the first paint after the
        // render. The frame has no effect to ride (`--no-fx`), so only
        // the timing window arms — source mode's gutter signal — and no
        // pulse effect is created.
        let mut app = make_app(5, Mode::View);
        app.config.fx = false;
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "older".into(),
            content: "line1\nline3\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(app.warp_fx.is_none(), "--no-fx arms no effects");
        assert!(app.landing_pulse_pending);
        begin_landing_pulse_after_draw(&mut app);
        assert!(!app.landing_pulse_pending);
        assert!(
            app.landing_pulse_until.is_some(),
            "an instant transition signals immediately"
        );
        assert!(
            app.landing_pulse_fx.is_none(),
            "--no-fx arms no pulse effect (the frame stays static)"
        );
    }

    #[test]
    fn a_comment_op_interrupting_the_transition_drops_the_pulse() {
        // A comment op (Enter with a card, `d` delete, …) rebuilds the
        // view and drops the transition's scatter effects mid-flight: the
        // screen will not settle into the new generation on its own, so
        // the landing pulse must not fire for a transition that never
        // landed.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:add".into()),
            short_id: "add".into(),
            summary: "added a line".into(),
            content: "line1\nline2\nline3\nline4\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(app.landing_pulse_pending);
        replace_view_preserving_cursor(&mut app);
        assert!(
            !app.landing_pulse_pending,
            "the interrupted transition never pulses"
        );
        begin_landing_pulse_after_draw(&mut app);
        assert!(app.landing_pulse_until.is_none());
        assert!(app.landing_pulse_fx.is_none());
    }

    #[test]
    fn the_generation_warp_flies_on_every_real_journey() {
        // Traveling to a different generation arms the warp — in both
        // directions — and `--no-fx` never flies.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "older draft".into(),
            content: "line1\nline2\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(app.warp_fx.is_some(), "diving into the past warps");
        // Coming back to NOW is a journey too.
        app.warp_fx = None;
        app.histories[0].position = 0;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(app.warp_fx.is_some(), "returning to NOW warps too");
        // --no-fx: the render happens, the flight does not.
        app.warp_fx = None;
        app.config.fx = false;
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        assert!(app.warp_fx.is_none(), "--no-fx: no warp");
    }

    #[test]
    fn footer_badge_shows_the_state_not_just_the_mode() {
        // The badge leads the footer and flips with the transient states:
        // SELECT while a selection is active (both modes — the selection
        // is shared), COMMENT while the composer is open. Esc is the way
        // out of both, spelled out in the hints.
        let footer_badge = |app: &mut App| -> String {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            let row: String = buf.content[23 * 80..24 * 80]
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect();
            row[..8].to_string()
        };
        let mut app = make_app(10, Mode::View);
        assert!(footer_badge(&mut app).contains("VIEW"), "view shows VIEW");
        app.selection = Some(Selection::new(3));
        assert!(footer_badge(&mut app).contains("SELECT"), "selection flips the badge");
        assert!(
            !footer_badge(&mut app).contains("VIEW"),
            "the badge is not the plain mode badge anymore"
        );
        app.mode = Mode::Source;
        assert!(
            footer_badge(&mut app).contains("SELECT"),
            "the selection badge carries over to source mode"
        );
        // While composing, the selection is still held (it is consumed on
        // Enter) — the badge must show COMMENT, not SELECT.
        app.mode = Mode::Input;
        assert!(
            footer_badge(&mut app).contains("COMMENT"),
            "composing shows COMMENT"
        );
        assert!(
            footer_hints(&app).contains("Enter confirm"),
            "the composer hint keeps the confirm/cancel pair"
        );
        assert!(footer_hints(&app).contains("Esc cancel"));
    }

    #[test]
    fn question_mark_opens_help_overlay() {
        let mut app = make_app(10, Mode::View);
        on_view_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Help));
        // The reference fits the panel (24-row terminal): j/k are no-ops
        // — a list that fits never scrolls. Esc / q / ? close it.
        on_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_cursor, 0, "no scroll when the content fits");
        on_overlay_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(app.overlay, None, "? toggles the help closed");
        on_source_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Help));
        on_overlay_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn overlay_click_outside_closes() {
        // Modal dismiss: with an overlay open, a click outside the panel
        // closes it; inside on a title row it stays open.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Outside (top-left corner): closes.
        on_mouse(&mut app, click(0, 0));
        assert_eq!(app.overlay, None, "outside click dismisses");
        // Inside on the title row: stays open.
        app.overlay = Some(Overlay::Files);
        on_mouse(&mut app, click(panel.y + 1, panel.x + 5));
        assert_eq!(app.overlay, Some(Overlay::Files), "inside title row keeps it");
        // Outside below the panel: closes again.
        on_mouse(&mut app, click(panel.y + panel.height + 2, 5));
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn prompt_lives_on_the_message_row_above_the_footer() {
        // The q-confirmation dialog renders on the bottom message row
        // (the row just above the footer — vim's message-line position;
        // the frame's bottom border in view mode, the last content row
        // in source mode), the same row as the toast: the footer keeps
        // the badge + hints, and the title keeps its file-centric
        // identity. No row is reserved — the message borrows an existing
        // layout row, so nothing ever shifts.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "c".into(),
        });
        request_quit(&mut app); // arms the confirmation
        assert!(app.confirm_quit);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // Rows in the 80×24 layout: title 0, body 1..22, footer 23 — the
        // message renders on body's last row (22).
        let row_text = |row: usize| -> String {
            buf.content[row * 80..(row + 1) * 80]
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        assert!(
            row_text(22).contains("unsent comments — q to quit"),
            "prompt on the message row, one row above the footer"
        );
        assert!(
            !row_text(0).contains("unsent comments"),
            "no dialog on the title row"
        );
        assert!(
            !row_text(23).contains("unsent comments"),
            "no dialog left in the footer"
        );
        assert!(row_text(23).contains("SOURCE"), "footer keeps the badge");
    }

    #[test]
    fn prompt_outranks_the_toast_on_the_message_row() {
        // A prompt demands an action and must not be missed: while one is
        // pending, the message row shows it even if a toast is also
        // live — the toast would expire on its own, the prompt waits for
        // the user. Once the prompt is answered, the toast shows again.
        let mut app = make_app(10, Mode::Source);
        app.confirm_quit = true;
        app.file_changed = true; // a lower-priority prompt is pending too
        app.flash("copied 3 comment(s)");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        fn render_message(
            terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
            app: &mut App,
        ) -> String {
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            buf.content[22 * 80..23 * 80]
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        }
        let shown = render_message(&mut terminal, &mut app);
        assert!(
            shown.contains("unsent comments — q to quit"),
            "prompt wins over the toast: {shown}"
        );
        assert!(
            !shown.contains("copied 3 comment(s)"),
            "toast suppressed while a prompt is pending: {shown}"
        );
        app.confirm_quit = false;
        app.file_changed = false; // clear the lower-priority prompt too
        let shown = render_message(&mut terminal, &mut app);
        assert!(
            shown.contains("copied 3 comment(s)"),
            "toast visible once the prompt is answered: {shown}"
        );
    }

    #[test]
    fn time_machine_frame_paints_the_rotating_gradient() {
        // Browsing the past with --fx (the default): the frame's border
        // cells are repainted into the time-machine gradient (a
        // purple→pink family — never the static border colors), and the
        // gradient varies around the loop. `--no-fx` and the present
        // (NOW) keep the static borders untouched.
        let historical = |fx: bool| -> App {
            let mut app = make_app(3, Mode::View);
            app.config.fx = fx;
            app.histories[0].revisions.push(history::Revision {
                id: Some("local:old".into()),
                short_id: "old".into(),
                summary: "local snapshot".into(),
                content: "old first line\nline2\nline3\n".into(),
                source: history::RevisionSource::Local,
                timestamp_ms: None,
            });
            app.histories[0].position = 1;
            app
        };
        let draw_corner = |app: &mut App| -> Color {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal
                .backend()
                .buffer()
                .cell((1, 1)) // the frame's top-left corner (margin + body top)
                .unwrap()
                .style()
                .fg
                .expect("the border cell has a color")
        };
        let mut app = historical(true);
        let corner = draw_corner(&mut app);
        assert!(
            matches!(corner, Color::Rgb(..)),
            "the animated border is truecolor: {corner:?}"
        );
        assert_ne!(corner, app.ui_border, "not the plain border color");
        assert_ne!(corner, app.ui_history_border, "not the static history color");
        // The gradient varies around the loop: the right border's middle
        // cell is a different point of the wave than the corner.
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let right = terminal
            .backend()
            .buffer()
            .cell((78, 10))
            .unwrap()
            .style()
            .fg
            .expect("the right border cell has a color");
        assert_ne!(right, corner, "the gradient differs around the frame");
        // --no-fx: the static history border color comes back.
        let mut app = historical(false);
        assert_eq!(draw_corner(&mut app), app.ui_history_border);
        // At NOW the frame stays calm even with fx on.
        let mut app = make_app(3, Mode::View);
        assert_eq!(draw_corner(&mut app), app.ui_border);
    }

    #[test]
    fn prompt_priority_quit_over_change() {
        let mut app = make_app(10, Mode::Source);
        app.confirm_quit = true;
        app.file_changed = true;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — q to quit, Esc to cancel")
        );
        app.confirm_quit = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("file changed — r reload first · i ignore")
        );
        app.file_changed = false;
        assert_eq!(prompt_message(&app).as_deref(), None);
    }

    #[test]
    fn overlay_double_click_switches_file() {
        // Double-clicking a file entry = Enter: it switches and closes.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let row = panel.y + 3; // second file entry
        // Widen the double-click window so the second click is
        // deterministic: the wall-clock comparison used to flake under
        // load (a stalled thread let 400ms elapse between the two
        // clicks).
        app.double_click_ms = Duration::from_secs(3600);
        on_mouse(&mut app, click(row, panel.x + 5));
        assert_eq!(app.overlay_cursor, 1);
        assert_eq!(app.overlay, Some(Overlay::Files), "single click selects only");
        on_mouse(&mut app, click(row, panel.x + 5));
        assert_eq!(app.overlay, None, "double-click activates");
        assert_eq!(app.current_file_index, 1, "switched to the second file");
    }

    #[test]
    fn overlay_double_click_jumps_to_comment() {
        // Double-clicking a comment row = Enter: jumps to the file+line.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        app.comments.push(Comment {
            file_path: a,
            start: 3,
            end: 3,
            lines: "line3".into(),
            revision: None,
            text: "note".into(),
        });
        // Start on b.rs (index 1), open the comment list.
        on_source_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        expire_chord(&mut app);
        assert_eq!(app.current_file_index, 1);
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let row = panel.y + 3; // title + header + first comment row
        // Widen the double-click window (see overlay_double_click_switches_file).
        app.double_click_ms = Duration::from_secs(3600);
        on_mouse(&mut app, click(row, panel.x + 5));
        on_mouse(&mut app, click(row, panel.x + 5));
        assert_eq!(app.overlay, None);
        assert_eq!(app.current_file_index, 0, "jumped back to a.md");
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 2, "cursor on the comment line");
    }

    #[test]
    fn overlay_double_click_requires_same_row() {
        // Two clicks on different entries never activate.
        let (mut app, _dir) = make_session();
        app.overlay = Some(Overlay::Files);
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, click(panel.y + 2, panel.x + 5));
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5)); // different entry
        assert_eq!(app.overlay, Some(Overlay::Files), "different rows do not activate");
        assert_eq!(app.overlay_cursor, 1);
    }

    #[test]
    fn overlay_double_click_state_resets_on_reopen() {
        // Regression: a double-click activates and closes the overlay; a
        // single click in a freshly reopened session on the same row must
        // only select — never inherit the old double-click's second half.
        let (mut app, _dir) = make_session();
        let panel = overlay_panel(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Session 1: double-click the second file → switches there.
        open_overlay(&mut app, Overlay::Files, 0);
        // Widen the double-click window (see overlay_double_click_switches_file).
        app.double_click_ms = Duration::from_secs(3600);
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5));
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5));
        assert_eq!(app.current_file_index, 1, "double-click switched");
        assert_eq!(app.overlay, None);
        // Session 2 (within the double-click window): a single click on
        // the same row selects only.
        open_overlay(&mut app, Overlay::Files, 1);
        on_mouse(&mut app, click(panel.y + 3, panel.x + 5));
        assert_eq!(
            app.overlay,
            Some(Overlay::Files),
            "a fresh session's single click only selects"
        );
        assert_eq!(app.current_file_index, 1, "the file did not change");
    }

    #[test]
    fn c_on_the_exact_range_edits_the_comment() {
        // Selecting exactly the range an existing comment covers and
        // pressing c re-edits it: the composer is prefilled and Enter
        // replaces instead of stacking a duplicate.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: "line3\nline4\nline5".into(),
            revision: None,
            text: "old".into(),
        });
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        }); // 1-based 3-5
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input, "old", "the comment text is prefilled");
        assert_eq!(app.editing_comment, Some(0));
        // Replace the text and confirm.
        app.input.clear();
        for ch in "new".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 1, "replaced, not added");
        assert_eq!(app.comments[0].text, "new");
        assert_eq!(app.mode, Mode::Source);
        assert!(app.editing_comment.is_none());
    }

    #[test]
    fn c_on_a_different_range_still_adds() {
        // A range that does not exactly match an existing comment adds a
        // new comment, with an empty composer.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: String::new(),
            revision: None,
            text: "existing".into(),
        });
        app.selection = Some(Selection {
            anchor: 6,
            cursor: 7,
        }); // 1-based 7-8: no exact match
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.input, "", "no prefill for a different range");
        assert_eq!(app.editing_comment, None);
        for ch in "x".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 2, "a second comment is added");
        assert_eq!(app.comments[1].text, "x");
    }

    #[test]
    fn esc_cancels_the_edit_unchanged() {
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 3,
            lines: String::new(),
            revision: None,
            text: "old".into(),
        });
        app.selection = Some(Selection::new(2));
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.input, "old");
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.comments[0].text, "old", "Esc leaves the comment untouched");
        assert!(app.editing_comment.is_none());
        assert_eq!(app.mode, Mode::Source);
    }

    #[test]
    fn s_with_failed_send_keeps_comments() {
        // A failed send (target unknown / command error) is NOT a
        // delivery: the comments stay for a retry.
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("false".to_string()); // always exits non-zero
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "comments kept on send failure");
        // Quitting now still triggers the unsent-comments confirmation.
        request_quit(&mut app);
        assert!(app.confirm_quit, "kept comments still guard the quit");
    }

    #[test]
    fn s_with_ok_send_clears_the_slate() {
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("cat > /dev/null".to_string()); // succeeds
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert!(app.comments.is_empty(), "delivered → cleared");
    }

    #[test]
    fn s_with_a_chatty_send_cmd_keeps_its_output_off_the_screen_and_toast() {
        // A send command that prints (e.g. `herdr agent prompt` echoing
        // its JSON reply) must not litter the alternate screen: the
        // child's stdout/stderr are piped, not inherited — that half is
        // guaranteed by construction in pipe_and_wait and cannot be
        // observed here. What CAN be asserted: the send counts as
        // delivered, and the output does not surface in the toast.
        let mut app = make_app(5, Mode::Source);
        // The words are assembled at run time so the toast's `sent via
        // <cmd>` echo of the command line cannot match them by accident.
        app.config.send_cmd =
            Some("cat >/dev/null; echo noi''se; echo hi''ss >&2".to_string());
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert!(app.comments.is_empty(), "delivered → cleared");
        let (msg, _, is_err) = app.status.clone().expect("a toast confirms the send");
        assert!(!is_err, "a chatty success is still a success: {msg}");
        assert!(!msg.contains("noise") && !msg.contains("hiss"), "{msg}");
    }

    #[test]
    fn s_with_failed_send_shows_the_childs_stderr_in_the_toast() {
        // A failing send command's last stderr line is the reason the
        // red toast gives, so the user learns *why* nothing was sent.
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("echo 'agent p9 not found' >&2; exit 1".to_string());
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "comments kept on send failure");
        let (msg, _, is_err) = app.status.clone().expect("a toast reports the failure");
        assert!(is_err, "{msg}");
        assert!(msg.contains("send failed"), "{msg}");
        assert!(msg.contains("agent p9 not found"), "{msg}");
    }

    #[test]
    fn s_without_send_cmd_is_a_no_op() {
        // Without a target, `s` has nothing to send: it copies to the
        // clipboard (y's job) and keeps the comments — no clearing, no
        // hidden stdout output after quit.
        let mut app = make_app(5, Mode::Source);
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "comments kept without a target");
        assert!(app.status.is_some(), "a hint explains why nothing was sent");
    }

    #[test]
    fn overlay_y_copies_comments_without_sending() {
        // Copying the comments moved into the comments overlay (`l` then
        // `y`). It is copy-only: even with --send-cmd configured it must
        // not deliver, and the comments stay. (No comments → the early
        // return keeps the test off the real clipboard.)
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("cat > /dev/null".to_string());
        app.overlay = Some(Overlay::Comments);
        on_comments_overlay_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(app.status.is_some(), "y flashes 'no comments yet'");
        assert!(app.comments.is_empty());
        assert_eq!(app.overlay, Some(Overlay::Comments), "the list stays open");
    }

    #[test]
    fn overlay_s_with_nothing_flashes_and_stays() {
        // Only a SUCCESSFUL send (comments cleared) closes the list; a
        // flashed "no comments yet" leaves it where it was.
        let mut app = make_app(5, Mode::Source);
        app.overlay = Some(Overlay::Comments);
        on_comments_overlay_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.status.is_some(), "s flashes 'no comments yet'");
        assert_eq!(app.overlay, Some(Overlay::Comments));
    }

    #[test]
    fn body_y_on_an_empty_document_flashes() {
        // `y` in the body copies the line/selection as shown. With no
        // document there is nothing to copy — and the early return keeps
        // the test off the real clipboard.
        let mut app = make_app(0, Mode::Source);
        on_source_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "y flashes 'nothing to copy'");
        assert_eq!(app.comments.len(), 0);
        let mut app = make_app(0, Mode::View);
        on_view_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE, None);
        assert!(app.status.is_some());
    }

    #[test]
    fn shift_j_k_select_and_move_in_both_modes() {
        // J anchors on the cursor line and extends one line down; K from
        // there shrinks back; a further K past the anchor grows upward.
        // The plain j then keeps extending, exactly as after `v`.
        let mut app = make_app(6, Mode::Source);
        app.cursor = 2;
        on_source_key(&mut app, KeyCode::Char('J'), KeyModifiers::SHIFT, None);
        assert_eq!(app.selection.map(|s| s.range()), Some((2, 3)));
        assert_eq!(app.cursor, 3);
        on_source_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT, None);
        assert_eq!(app.selection.map(|s| s.range()), Some((2, 4)));
        on_source_key(&mut app, KeyCode::Char('K'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('K'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('K'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 2)), "K past the anchor grows upward");
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection.map(|s| s.range()), Some((2, 2)), "plain j extends the live selection");
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert!(app.selection.is_none(), "Esc cancels");

        let mut app = make_app(6, Mode::View);
        app.view.goto_source_line(1);
        on_view_key(&mut app, KeyCode::Up, KeyModifiers::SHIFT, None);
        assert_eq!(app.selection.map(|s| s.range()), Some((0, 1)), "Shift+↑ anchors and grows upward");
        on_view_key(&mut app, KeyCode::Char('J'), KeyModifiers::SHIFT, None);
        on_view_key(&mut app, KeyCode::Char('J'), KeyModifiers::SHIFT, None);
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 2)));
        assert_eq!(app.view.cursor, 2);
    }

    #[test]
    fn view_y_without_selection_targets_the_whole_cursor_row() {
        // make_app's lines form ONE paragraph, rendered as one merged row;
        // the cursor band covers that whole row, so `y` must copy every
        // line in it — not just the cursor's own phrase.
        let app = make_app(3, Mode::View);
        assert_eq!(view_cursor_row_lines(&app.view), (0, 2));
        let mut app = make_app(3, Mode::View);
        app.view.goto_source_line(1);
        assert_eq!(view_cursor_row_lines(&app.view), (0, 2), "any line of the row → the row");
    }

    #[test]
    fn plain_arrows_still_move_without_selecting() {
        let mut app = make_app(6, Mode::Source);
        on_source_key(&mut app, KeyCode::Down, KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 1);
        assert!(app.selection.is_none(), "an unshifted arrow never starts a selection");
    }

    #[test]
    fn overlay_scrolls_only_when_overflowing() {
        // A 24-row terminal gives the panel ~11 visible rows. A short
        // list never scrolls; a long one scrolls to keep the cursor in
        // view.
        let (mut app, _dir) = make_session(); // 2 files
        app.overlay = Some(Overlay::Files);
        on_files_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_offset, 0, "content fits → no scroll");
        // Grow the list past the visible window.
        for i in 0..15 {
            app.files.push(PathBuf::from(format!("f{i:02}.md")));
        }
        app.overlay_cursor = 0;
        for _ in 0..13 {
            on_files_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(app.overlay_cursor, 13);
        assert!(app.overlay_offset > 0, "overflow → the offset follows");
        let visible = overlay_visible_rows();
        assert!(
            app.overlay_offset <= 13 && 13 < app.overlay_offset + visible,
            "the cursor row stays inside the visible window"
        );
        // k scrolls back up and the offset follows.
        for _ in 0..13 {
            on_files_overlay_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        }
        assert_eq!(app.overlay_cursor, 0);
        assert_eq!(app.overlay_offset, 0);
    }

    #[test]
    fn comments_overlay_delete_uses_sorted_order() {
        // The overlay cursor indexes the SORTED list; delete must remove
        // the highlighted comment even when add order interleaves files.
        let (mut app, _dir) = make_session();
        let a = app.files[0].clone();
        let b = app.files[1].clone();
        // Add order: a1, b1, a2 → sorted: a1, a2, b1.
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "a1".into(),
        });
        app.comments.push(Comment {
            file_path: b.clone(),
            start: 1,
            end: 1,
            lines: "x".into(),
            revision: None,
            text: "b1".into(),
        });
        app.comments.push(Comment {
            file_path: a.clone(),
            start: 2,
            end: 2,
            lines: "x".into(),
            revision: None,
            text: "a2".into(),
        });
        app.overlay = Some(Overlay::Comments);
        // j × 2 → sorted index 2 = b1 (raw index 1).
        on_comments_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        on_comments_overlay_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.overlay_cursor, 2);
        on_comments_overlay_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 2);
        assert!(
            app.comments.iter().all(|c| c.text != "b1"),
            "the highlighted (b.rs) comment was deleted, not a.md's"
        );
    }

    #[test]
    fn editing_hides_the_stacked_card() {
        // While re-editing, the old card (`comment · …`) is hidden — the
        // edit composer replaces it, so `comment` and `edit` never stack.
        let mut app = make_app(10, Mode::Source);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: "line3\nline4\nline5".into(),
            revision: None,
            text: "old".into(),
        });
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        });
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.editing_comment, Some(0));
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        let during = capture(&mut app);
        assert!(during.contains("edit · 3-5"), "the edit composer shows: {during}");
        assert!(
            !during.contains("comment · 3-5"),
            "the old card is hidden while editing: {during}"
        );
        // Confirm: the card returns with the new text.
        app.input.clear();
        for ch in "new".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let after = capture(&mut app);
        assert!(after.contains("comment · 3-5"), "the card returns: {after}");
        assert!(after.contains("new"), "with the updated text");
    }

    #[test]
    fn view_mode_editing_hides_the_stacked_card() {
        // Re-editing from VIEW mode: the folded card must disappear under
        // the edit composer (regression: the re-render used to run before
        // the Input flip, so visible_cards never excluded the card), and
        // Esc puts it back.
        let mut app = make_app(10, Mode::View);
        app.comments.push(Comment {
            file_path: app.current_file_path().to_path_buf(),
            start: 3,
            end: 5,
            lines: "line3\nline4\nline5".into(),
            revision: None,
            text: "old".into(),
        });
        replace_view_preserving_cursor(&mut app); // fold the card in
        app.selection = Some(Selection {
            anchor: 2,
            cursor: 4,
        }); // 1-based 3-5: exact match → re-edit
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.editing_comment, Some(0));
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        let during = capture(&mut app);
        assert!(during.contains("edit · 3-5"), "the edit composer shows: {during}");
        assert!(
            !during.contains("comment · 3-5"),
            "the old card is hidden while editing in view: {during}"
        );
        // Esc cancels: the card returns with its original text.
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let after = capture(&mut app);
        assert!(
            after.contains("comment · 3-5"),
            "the card returns on cancel: {after}"
        );
    }

    #[test]
    fn history_selection_defers_markdown_render_until_settled() {
        let mut app = make_app(1, Mode::View);
        app.histories = vec![crate::history::DocumentHistory {
            revisions: vec![
                crate::history::Revision {
                    id: None,
                    short_id: "now".into(),
                    summary: "working tree".into(),
                    content: "line1\n".into(),
                    source: crate::history::RevisionSource::Now,
                    timestamp_ms: None,
                },
                crate::history::Revision {
                    id: Some("abc".into()),
                    short_id: "abc".into(),
                    summary: "old".into(),
                    content: "# Old\n".into(),
                    source: crate::history::RevisionSource::Git,
                    timestamp_ms: None,
                },
            ],
            position: 0,
            rendered_position: 0,
            reviewed_id: None,
            reviewed_content: Some("line1\n".into()),
        }];
        select_history(&mut app, 1);
        assert_eq!(app.histories[0].position, 1);
        assert_eq!(app.source.content, "line1\n", "selection is immediate but cheap");
        assert!(app.history_render_due.is_some());
        assert!(
            app.timeline_tooltip_until.is_some(),
            "a history step arms the scrubber tooltip"
        );
        assert!(app.status.is_none(), "no per-step toast anymore");
        select_history(&mut app, 1);
        assert!(
            app.status.is_none() && app.timeline_tooltip_until.is_some(),
            "the edge stays quiet (tooltip only) until that revision is rendered"
        );
        render_history_when_settled(&mut app);
        assert_eq!(app.source.content, "line1\n", "renderer waits for the debounce");
        app.history_render_due = Some(std::time::Instant::now());
        render_history_when_settled(&mut app);
        assert_eq!(app.source.content, "# Old\n");
        assert!(app.history_render_due.is_none());
        assert!(app.landing_pulse_pending);
        assert!(
            app.landing_pulse_until.is_none() && app.landing_pulse_fx.is_none(),
            "the pulse does not overlap the first paint of the rendered document"
        );
        // The transition is still animating (warp + ghost + stream): the
        // pulse waits for the screen to settle instead of firing into
        // the motion.
        begin_landing_pulse_after_draw(&mut app);
        assert!(
            app.landing_pulse_pending,
            "the pulse waits for the transition to settle"
        );
        assert!(app.landing_pulse_until.is_none());
        // Once the transition effects complete, the next paint pulses:
        // view mode with `--fx` arms the pulse effect itself.
        app.warp_fx = None;
        app.appear_fx.clear();
        app.ghost_fx.clear();
        begin_landing_pulse_after_draw(&mut app);
        assert!(!app.landing_pulse_pending);
        assert!(app.landing_pulse_until.is_some());
        assert!(
            app.landing_pulse_fx.is_some(),
            "view mode with --fx arms the pulse effect"
        );
        select_history(&mut app, 1);
        assert!(
            matches!(app.status.as_ref(), Some((message, _, true)) if message == "oldest document version"),
            "the edge becomes an error only after its document is visible"
        );
        select_history(&mut app, -1);
        select_history(&mut app, -1);
        assert!(
            app.status.is_none() && app.timeline_tooltip_until.is_some(),
            "the present boundary also stays quiet until it is rendered"
        );
        app.history_render_due = Some(std::time::Instant::now());
        render_history_when_settled(&mut app);
        select_history(&mut app, -1);
        assert!(
            matches!(app.status.as_ref(), Some((message, _, true)) if message == "already at the present"),
            "the present boundary becomes an error after its document is visible"
        );
    }

    #[test]
    fn source_mode_shares_the_markdown_timeline_and_tab_keeps_the_revision() {
        let mut app = make_app(1, Mode::Source);
        app.histories = vec![crate::history::DocumentHistory {
            revisions: vec![
                crate::history::Revision {
                    id: None,
                    short_id: "now".into(),
                    summary: "working tree".into(),
                    content: "line1\n".into(),
                    source: crate::history::RevisionSource::Now,
                    timestamp_ms: None,
                },
                crate::history::Revision {
                    id: Some("abc".into()),
                    short_id: "abc".into(),
                    summary: "old".into(),
                    content: "# Old\n\nsource history\n".into(),
                    source: crate::history::RevisionSource::Git,
                    timestamp_ms: None,
                },
            ],
            position: 0,
            rendered_position: 0,
            reviewed_id: None,
            reviewed_content: Some("line1\n".into()),
        }];

        assert_eq!(
            history_key_direction(&app, KeyCode::Left, KeyModifiers::NONE),
            Some(1)
        );
        on_source_key(&mut app, KeyCode::Left, KeyModifiers::NONE, None);
        assert_eq!(app.histories[0].position, 1);
        assert_eq!(app.source.content, "line1\n", "source scrubbing is deferred too");
        app.history_render_due = Some(std::time::Instant::now());
        render_history_when_settled(&mut app);
        assert_eq!(app.source.content, "# Old\n\nsource history\n");

        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.source.content, "# Old\n\nsource history\n");
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.histories[0].position, 1);
    }

    #[test]
    fn source_history_uses_the_gutter_landing_pulse() {
        let mut app = make_app(3, Mode::Source);
        app.cursor = 0;
        app.landing_pulse_until = Some(std::time::Instant::now() + Duration::from_secs(1));
        app.gutter_cols = 4;
        app.ensure_row_cache(60);
        app.refresh_line_rows();

        let (text, _) = build_rows(&app, 10, 60);
        assert_eq!(
            text.lines[2].spans[1].style.fg,
            Some(app.ui_landing_pulse),
            "the line-number rail pulses after landing"
        );
        assert!(
            !text.lines[2]
                .spans
                .iter()
                .any(|span| span.style.bg == Some(app.ui_history_glow_bg)),
            "the neutral glow was retired: the streaming reveal and the\
             frame flash already mark what changed"
        );
    }

    /// Set the review baseline to `baseline` and recompute marks + layout,
    /// as a reload/ack would. NOW stays what `make_app` wrote.
    fn set_baseline(app: &mut App, baseline: &str) {
        app.histories[0].reviewed_content = Some(baseline.to_string());
        refresh_review_marks(app);
        app.refresh_line_rows();
    }

    fn row_text(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn deleted_baseline_rows_render_inline_in_source() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        // The two deleted rows are folded into line2's band: layout total
        // and the renderer agree row for row.
        let total: usize = app.line_rows.iter().sum();
        assert_eq!(total, 5, "3 source lines + 2 inline deleted rows");
        let (text, _) = build_rows(&app, 30, 75);
        assert_eq!(text.lines.len(), 5);
        assert!(row_text(&text.lines[0]).contains("line1"));
        assert!(row_text(&text.lines[1]).contains("GONE A"));
        assert!(row_text(&text.lines[2]).contains("GONE B"));
        assert!(row_text(&text.lines[3]).contains("line2"));

        // A deleted row: red `▌` mark, blank number column, red text on
        // the red band — the diff-pair partner of the green changed band.
        let deleted = &text.lines[1];
        assert_eq!(deleted.spans[0].content.as_ref(), "▌");
        assert_eq!(deleted.spans[0].style.fg, Some(Color::Red));
        assert_eq!(deleted.spans[0].style.bg, Some(app.ui_deleted_bg));
        assert!(
            deleted.spans[1].content.chars().all(|c| c == ' '),
            "deleted rows have no line number: {:?}",
            deleted.spans[1].content
        );
        let body = &deleted.spans[2];
        assert_eq!(body.style.fg, Some(Color::Red));
        assert_eq!(body.style.bg, Some(app.ui_deleted_bg));
        assert!(!body.style.add_modifier.contains(Modifier::DIM));
        // The red band runs to the pane's right edge, exactly like the
        // green `▌` changed band (gutter + content width).
        let total: usize = deleted
            .spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        assert_eq!(total, 78, "deleted rows band the full pane width");
        assert_eq!(
            deleted.spans.last().unwrap().style.bg,
            Some(app.ui_deleted_bg),
            "the right-edge fill carries the red band"
        );
        // No `▀` glyph remains in source mode (the deletion position
        // mark is a red `▌` in view mode only).
        assert!(
            text.lines.iter().all(|l| row_text(l).chars().all(|c| c != '▀')),
            "the deletion position mark is view-only now"
        );
    }

    #[test]
    fn wrapped_changed_and_deleted_rows_carry_the_mark_on_every_row() {
        let mut app = make_app(2, Mode::Source);
        // Both the current line 2 (changed, green) and its deleted
        // baseline text (red) are long enough to wrap to three rows at
        // the 75-column content width.
        let now = format!("line1\n{}\n", "Y".repeat(160));
        app.source.content = now.clone();
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.comparison_changed = [1usize].into_iter().collect();
        app.comparison_deleted_blocks = vec![history::DeletedBlock {
            anchor: 1,
            content: "X".repeat(160),
        }];
        app.ensure_row_cache(75);
        app.refresh_line_rows();

        let (text, _) = build_rows(&app, 30, 75);
        // Rows: line1 (cursor row) | deleted block (3 rows) | line2 (3 rows).
        assert_eq!(text.lines.len(), 7);
        assert_eq!(text.lines[0].spans[0].content.as_ref(), ">");
        // Every deleted row — including wrap continuations — carries the
        // red `▌` on the red band, so the left-edge mark runs unbroken.
        for row in &text.lines[1..4] {
            assert_eq!(row.spans[0].content.as_ref(), "▌", "deleted continuation rows keep the red mark");
            assert_eq!(row.spans[0].style.bg, Some(app.ui_deleted_bg));
        }
        // Every changed row — continuations included — carries the green
        // `▌` on the green band.
        for row in &text.lines[4..7] {
            assert_eq!(row.spans[0].content.as_ref(), "▌", "changed continuation rows keep the green mark");
            assert_eq!(row.spans[0].style.bg, Some(app.ui_changed_bg));
        }
        // The gutter stays aligned on continuation rows: mark + indent
        // equals the first row's mark + line number.
        let first_gutter: usize = text.lines[4]
            .spans
            .iter()
            .take(2)
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        let cont_gutter: usize = text.lines[5]
            .spans
            .iter()
            .take(2)
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        assert_eq!(cont_gutter, first_gutter, "continuation rows keep the gutter width");

        // Focused: the whole block lights on the cursor band and the
        // continuation rows keep the `▌` glyph (`>` stays on the first
        // row only). A pure deletion — the anchor line is ordinary.
        app.cursor = 1;
        app.focused_deletion = Some(1);
        app.comparison_changed.clear();
        let (text, _) = build_rows(&app, 30, 75);
        assert_eq!(text.lines[1].spans[0].content.as_ref(), ">");
        for row in &text.lines[2..4] {
            assert_eq!(row.spans[0].content.as_ref(), "▌");
            assert_eq!(row.spans[0].style.bg, Some(app.ui_selected_bg));
        }
    }

    #[test]
    fn rewritten_lines_show_their_baseline_text() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nold-two\nline3\n");

        // A same-length rewrite is a full diff pair: the baseline text
        // renders as a red row above the green rewritten line…
        let (text, _) = build_rows(&app, 30, 75);
        assert_eq!(text.lines.len(), 4);
        assert!(row_text(&text.lines[1]).contains("old-two"));
        assert!(row_text(&text.lines[2]).contains("line2"));
        assert!(app.comparison_changed.contains(&1));
        // …while the net-deletion position set (view mode's red `▌`, the
        // review badge) intentionally stays empty for rewrites.
        assert!(app.comparison_deleted_before.is_empty());
    }

    #[test]
    fn eof_deletion_renders_below_the_last_line() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nline2\nline3\ntail gone\n");

        let (text, _) = build_rows(&app, 30, 75);
        assert_eq!(text.lines.len(), 4);
        assert!(row_text(&text.lines[2]).contains("line3"));
        assert!(
            row_text(&text.lines[3]).contains("tail gone"),
            "an EOF deletion renders below the last line, not above it"
        );
    }

    #[test]
    fn mouse_maps_deleted_rows_to_their_anchor_line() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        // Bands: line1 = row 0; line2 = rows 1-3 (two deleted rows + its
        // text); line3 = row 4. A click on a deleted row selects the
        // anchor line, same attribution as comment bars.
        assert_eq!(source_line_at(&app, 75, 0), Some(0));
        assert_eq!(source_line_at(&app, 75, 1), Some(1));
        assert_eq!(source_line_at(&app, 75, 2), Some(1));
        assert_eq!(source_line_at(&app, 75, 3), Some(1));
        assert_eq!(source_line_at(&app, 75, 4), Some(2));
    }

    #[test]
    fn n_focuses_pure_deletions_instead_of_selecting_the_anchor_line() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 1, "the cursor lands on the anchor line");
        assert!(
            app.selection.is_none(),
            "the (unchanged) anchor line is not selected"
        );
        assert_eq!(app.deletion_focus(), Some(1));
        assert!(
            matches!(app.status.as_ref(), Some((message, _, false)) if message.contains("deleted")),
            "the flash names the deletion: {:?}",
            app.status
        );

        // The cursor's visual language moves onto the deleted rows: `>`
        // on the first row, bright red text on the cursor band, and the
        // anchor line below renders as an ordinary line meanwhile.
        let (text, _) = build_rows(&app, 30, 75);
        assert_eq!(text.lines[1].spans[0].content.as_ref(), ">");
        assert_eq!(text.lines[2].spans[0].content.as_ref(), "▌");
        let body = &text.lines[1].spans[2];
        assert_eq!(body.style.fg, Some(Color::LightRed));
        assert_eq!(body.style.bg, Some(app.ui_selected_bg));
        assert!(!body.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(
            text.lines[3].spans[0].content.as_ref(),
            " ",
            "the anchor line carries no cursor glyph while the focus is live"
        );
        assert_ne!(
            text.lines[3].spans[0].style.bg,
            Some(app.ui_selected_bg),
            "no second cursor band on the anchor line"
        );

        // Leaving the anchor line dissolves the focus back to the static
        // red band, and the cursor glyph returns to the cursor line.
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.deletion_focus(), None);
        let (text, _) = build_rows(&app, 30, 75);
        let body = &text.lines[1].spans[2];
        assert_eq!(body.style.bg, Some(app.ui_deleted_bg));
        assert!(!body.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(text.lines[1].spans[0].content.as_ref(), "▌");
    }

    #[test]
    fn c_on_a_focused_deletion_quotes_the_deleted_text() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (1, 1));
        app.input = "なぜ消した?".into();
        app.input_cursor = app.input.len();
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

        let comment = app.comments.last().expect("comment added");
        assert_eq!((comment.start, comment.end), (2, 2), "anchored to the anchor line");
        assert_eq!(
            comment.lines, "GONE A\nGONE B",
            "the snippet quotes the deleted baseline text, not the anchor line"
        );
    }

    #[test]
    fn esc_dismisses_the_deletion_focus_before_quitting() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.deletion_focus(), Some(1));
        on_source_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.deletion_focus(), None);
        assert!(app.running, "Esc consumed the focus instead of quitting");
    }

    #[test]
    fn rewrite_pair_lights_as_one_block_when_selected() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nold-two\nline3\n");

        // `n` lands on the rewritten line: the selection covers the green
        // line, and the paired old content above lights up with it — the
        // diff pair is one focused block.
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert!(app.selection.is_some(), "rewrites keep the line selection");
        let (text, _) = build_rows(&app, 30, 75);
        let old_body = &text.lines[1].spans[2];
        assert_eq!(old_body.style.fg, Some(Color::LightRed));
        assert_eq!(old_body.style.bg, Some(app.ui_selected_bg));
        assert!(!old_body.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(
            text.lines[1].spans[0].content.as_ref(),
            "▌",
            "no `>` on rewrite old rows — the real cursor is on the new line"
        );
    }

    #[test]
    fn cursor_on_the_anchor_line_lights_its_deletion() {
        let mut app = make_app(3, Mode::Source);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        // Plain j/k (no n, no focus): standing on the anchor line lights
        // the deleted rows above it.
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 1);
        assert_eq!(app.deletion_focus(), None, "j/k does not create a focus");
        let (text, _) = build_rows(&app, 30, 75);
        let body = &text.lines[1].spans[2];
        assert_eq!(body.style.fg, Some(Color::LightRed));
        assert_eq!(body.style.bg, Some(app.ui_selected_bg));
        assert_eq!(
            text.lines[3].spans[0].content.as_ref(),
            ">",
            "without a focus the cursor glyph stays on the anchor line"
        );
    }

    #[test]
    fn view_n_on_a_deletion_hands_the_focus_to_source_via_tab() {
        let mut app = make_app(3, Mode::View);
        set_baseline(&mut app, "line1\nGONE A\nGONE B\nline2\nline3\n");

        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert!(
            matches!(app.status.as_ref(),
                Some((message, _, false)) if message.contains("Tab: inspect & comment")),
            "the view flash points to source mode: {:?}",
            app.status
        );
        assert!(app.selection.is_some(), "view keeps its anchor selection");

        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(
            app.deletion_focus(),
            Some(1),
            "Tab arrives in source with the deletion focused, as promised"
        );
        assert!(app.selection.is_none());
    }

    #[test]
    fn clip_if_needed_only_marks_real_overflows() {
        assert_eq!(clip_if_needed("short", 24), "short");
        assert_eq!(clip_if_needed("testdata", 24), "testdata");
        let long = "a".repeat(30);
        let out = clip_if_needed(&long, 10);
        assert_eq!(UnicodeWidthStr::width(out.as_str()), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn depth_reaches_the_frame_through_draw() {
        // Full-path probe: draw() feeds travel_depth into the border
        // effect every frame, so a deeper generation shows a deeper
        // frame. Sampled on the TOP border (the left border column x=1
        // carries the cursor/comment markers, which are not border
        // glyphs and stay out of the effect).
        let border_color = |app: &mut App, position: usize| -> Color {
            app.histories[0].position = position;
            app.histories[0].rendered_position = position;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            buf[(40, 1)].fg
        };
        let mut app = make_app(5, Mode::View);
        // Three revisions so position 1 = mid (depth 0.5), 2 = oldest
        // (depth 1.0): the same border cell must sink between them.
        app.histories[0].revisions.push(crate::history::Revision {
            id: Some("local:mid".into()),
            short_id: "mid".into(),
            summary: "middle".into(),
            content: "line1\nline2\nline3\n".into(),
            source: crate::history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].revisions.push(crate::history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "oldest".into(),
            content: "line1\n".into(),
            source: crate::history::RevisionSource::Local,
            timestamp_ms: None,
        });
        let mid = border_color(&mut app, 1);
        let old = border_color(&mut app, 2);
        eprintln!("mid={mid:?} old={old:?}");
        assert_ne!(mid, old, "the frame color must sink with depth");
    }

    #[test]
    fn composer_cursor_is_stable_while_typing_in_the_time_machine() {
        // Typing a comment while browsing history must keep the terminal
        // cursor glued to the `▏` glyph. Dump the cursor position across
        // frames and flag any jump that isn't the expected rightward
        // drift of the text cursor.
        let cursor_pos = |app: &mut App| -> (u16, u16) {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.backend_mut().get_cursor_position().unwrap().into()
        };
        let mut app = make_app(5, Mode::View);
        // A generation with deletions AND additions (ghost phase runs).
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:mix".into()),
            short_id: "mix".into(),
            summary: "removes line2, adds line6".into(),
            content: "line1\nline3\nline4\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        // Comment the cursor line while the transition is still playing.
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        let mut prev = cursor_pos(&mut app);
        eprintln!("start: {prev:?}");
        for ch in "hello world".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
            let pos = cursor_pos(&mut app);
            eprintln!("after {ch}: {pos:?}");
            // The cursor moves right as text grows; a jump UP or a jump
            // of more than 2 rows signals a layout fight.
            let (px, py) = prev;
            let (x, y) = pos;
            assert!(
                y >= py && (y - py) <= 2 && x >= px.saturating_sub(2),
                "cursor jumped after '{ch}': {prev:?} -> {pos:?}"
            );
            prev = pos;
        }
    }

    #[test]
    fn composer_cursor_stays_put_at_the_document_bottom() {
        // A comment on the LAST line while browsing history: the bar
        // extends past the document and the per-frame nudge must keep
        // the cursor still while the text wraps — no upward jumps.
        let cursor_pos = |app: &mut App| -> (u16, u16) {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.backend_mut().get_cursor_position().unwrap().into()
        };
        let mut app = make_app(12, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:add".into()),
            short_id: "add".into(),
            summary: "added a line".into(),
            content: (1..=13).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n") + "\n",
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        // Move the view cursor to the last line and comment it.
        app.view.goto_source_line(11);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        let mut prev = cursor_pos(&mut app);
        eprintln!("start: {prev:?}");
        for ch in "a very long comment that wraps across several rows of the narrow bar".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
            let pos = cursor_pos(&mut app);
            let (px, py) = prev;
            let (x, y) = pos;
            if y == py {
                // Same row: the text cursor only ever moves right.
                assert!(x >= px, "cursor went left after '{ch}': {prev:?} -> {pos:?}");
            } else {
                // A wrap moves the cursor down one row and back to the
                // text start — never up, never more than one row.
                assert!(
                    y > py && y - py <= 1,
                    "cursor jumped rows after '{ch}': {prev:?} -> {pos:?}"
                );
            }
            prev = pos;
        }
        eprintln!("end: {prev:?}");
    }

    #[test]
    fn composer_cursor_is_stable_in_source_mode_time_machine() {
        // Source mode is the one place the timeline bar takes a content
        // row (keep_cursor_out_of_timeline nudges the scroll): typing a
        // comment while browsing must not make the cursor fight that.
        let cursor_pos = |app: &mut App| -> (u16, u16) {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.backend_mut().get_cursor_position().unwrap().into()
        };
        let mut app = make_app(10, Mode::Source);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:del".into()),
            short_id: "del".into(),
            summary: "removes line2".into(),
            content: "line1\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n".into(),
            source: history::RevisionSource::Local,
            timestamp_ms: None,
        });
        app.histories[0].position = 1;
        app.history_render_due = Some(Instant::now());
        assert!(render_pending_history(&mut app, true));
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        let mut prev = cursor_pos(&mut app);
        eprintln!("start: {prev:?}");
        for ch in "hello source comment".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
            let pos = cursor_pos(&mut app);
            let (px, py) = prev;
            let (x, y) = pos;
            if y == py {
                assert!(x >= px, "cursor went left after '{ch}': {prev:?} -> {pos:?}");
            } else {
                assert!(
                    y > py && y - py <= 1,
                    "cursor jumped rows after '{ch}': {prev:?} -> {pos:?}"
                );
            }
            prev = pos;
        }
        eprintln!("end: {prev:?}");
    }

    #[test]
    fn no_cursor_anchor_stops_publishing_the_position() {
        // The composer publishes the hidden hardware cursor position so
        // the macOS IME anchors its composition window at the `▏` — but
        // terminals with cursor-following shaders blaze around that
        // motion. `--no-cursor-anchor` must keep the position untouched.
        let published = |app: &mut App| -> Option<(u16, u16)> {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let pos = terminal.backend_mut().get_cursor_position().unwrap();
            let pos: (u16, u16) = pos.into();
            // The test backend reports the last published position; with
            // nothing published it stays at the default (0,0).
            (pos != (0, 0)).then_some(pos)
        };
        // Anchor on (default): the position is published inside the bar.
        let mut app = make_app(5, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert!(published(&mut app).is_some(), "the anchor publishes the position");
        // Anchor off: nothing is published, the `▏` glyph still draws.
        let mut app = make_app(5, Mode::View);
        app.config.cursor_anchor = false;
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert!(
            published(&mut app).is_none(),
            "--no-cursor-anchor stops publishing the position"
        );
    }

    #[test]
    fn the_composer_keeps_the_hardware_cursor_hidden() {
        // The composer publishes the (hidden) cursor position every frame
        // for the macOS IME anchor. ratatui's draw re-SHOWS the cursor
        // whenever a frame publishes a position, so akapen re-hides it
        // right after every draw — a visible hardware cursor next to the
        // `▏` glyph makes cursor-following terminal shaders (Ghostty's
        // cursor_blaze) animate around it.
        let mut app = make_app(5, Mode::View);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        // The premise: ratatui's position publish shows the cursor — the
        // exact behavior the event loop's re-hide compensates for.
        assert!(
            terminal.backend().cursor_visible(),
            "premise: a publishing draw shows the hardware cursor"
        );
        // The event loop re-hides it right after every draw.
        terminal.backend_mut().hide_cursor().unwrap();
        assert!(
            !terminal.backend().cursor_visible(),
            "the hardware cursor stays hidden while composing"
        );
        // The position is still published for the IME even though the
        // cursor is hidden (visibility and position are separate).
        let pos = terminal.backend_mut().get_cursor_position().unwrap();
        assert_ne!(pos, ratatui::layout::Position::new(0, 0));
    }

    #[test]
    fn view_composer_ime_anchor_sits_on_the_cursor_glyph() {
        // In view mode the composer bar's rows get a leading pad (
        // `Span::raw(" ")` on each side in draw_view). The published
        // terminal-cursor position — the macOS IME's inline-composition
        // anchor — must land ON the \u{258f} `▏` glyph, not on the pad
        // cell to its left. A one-column miss makes the Japanese
        // conversion window read as "a half-width space in front, then it
        // shifts" as the cursor moves back.
        let (mut app, _dir) = make_app_keep(5, Mode::View);
        app.mode = Mode::Input;
        app.composer_return = Mode::View;
        app.input = "あい".to_string();
        app.input_cursor = 3; // after the first full-width char
        app.input_start = 0;
        app.input_end = 0;
        app.ensure_row_cache(75);
        app.refresh_line_rows();

        let mut t =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        // Find the block caret run (fg Black on bg Cyan; title/footer
        // excluded).
        let runs = caret_runs(buf);
        assert_eq!(runs.len(), 1, "exactly one caret run in the body");
        let (row, col, _end) = runs[0];
        let pos = t.backend_mut().get_cursor_position().unwrap();
        assert_eq!(
            (pos.x as usize, pos.y as usize),
            (col, row),
            "the IME anchor must sit exactly on the caret"
        );
    }

    #[test]
    fn composer_cursor_hovers_on_the_glyph_when_wrapped_and_in_source() {
        // Probe the two other composer arrangements: a long comment that
        // WRAPS to multiple body rows, and the source-mode inline bar
        // (which lives after the gutter, no side pad). In both, exactly
        // one `▏` must render and the IME anchor must sit on it.
        let check = |app: &mut App, backend_width: u16| {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
                backend_width,
                24,
            ))
            .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            let buf = t.backend().buffer();
            let runs = caret_runs(buf);
            assert_eq!(runs.len(), 1, "a single caret run, no leftover");
            let (row, col, _end) = runs[0];
            let pos = t.backend_mut().get_cursor_position().unwrap();
            assert_eq!(
                (pos.x as usize, pos.y as usize),
                (col, row),
                "IME anchor on the caret (view wrapping / source mode)"
            );
        };

        // View mode, a comment long enough to wrap within a 20-col bar.
        let (mut app, _dir) = make_app_keep(1, Mode::View);
        app.mode = Mode::Input;
        app.composer_return = Mode::View;
        app.input = "あいうえおかきくけこさしすせそ".to_string();
        app.input_cursor = 9; // mid-word, after 3 full-width chars
        app.input_start = 0;
        app.input_end = 0;
        app.ensure_row_cache(20);
        app.refresh_line_rows();
        check(&mut app, 30);

        // Source mode: bar after the gutter, no side pad.
        let (mut app, _dir) = make_app_keep(1, Mode::Source);
        app.mode = Mode::Input;
        app.composer_return = Mode::Source;
        app.input = "あいうえおかきくけこ".to_string();
        app.input_cursor = 9;
        app.input_start = 0;
        app.input_end = 0;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        check(&mut app, 60);
    }

/// A toy terminal backend that applies ratatui's diff cells onto a
/// persistent screen, like a real terminal — TestBackend re-draws the full
/// buffer each frame, so it can never show a diff-only artifact
/// (afterimages when cells fail to clear). This one replays the emitted
/// cells, so a stale-glyph bug surfaces here.
struct VirtualTerm {
    screen: Vec<ratatui::buffer::Cell>,
    w: u16,
    h: u16,
    cursor: (u16, u16),
    /// Every cell ratatui emitted per draw (a fresh append per draw()).
    diff_log: Vec<(u16, u16, String)>,
}

impl VirtualTerm {
    fn new(w: u16, h: u16) -> Self {
        let n = (w as usize) * (h as usize);
        Self {
            screen: (0..n)
                .map(|_| ratatui::buffer::Cell::default())
                .collect(),
            w,
            h,
            cursor: (0, 0),
            diff_log: Vec::new(),
        }
    }
}

impl ratatui::backend::Backend for VirtualTerm {
    type Error = std::io::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        for (x, y, c) in content {
            let i = (y as usize) * (self.w as usize) + (x as usize);
            self.screen[i] = c.clone();
            self.diff_log.push((x, y, c.symbol().to_string()));
        }
        Ok(())
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        Ok(ratatui::layout::Position::new(self.cursor.0, self.cursor.1))
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        p: P,
    ) -> Result<(), Self::Error> {
        let p = p.into();
        self.cursor = (p.x, p.y);
        Ok(())
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        for c in &mut self.screen {
            *c = ratatui::buffer::Cell::default();
        }
        Ok(())
    }
    fn clear_region(&mut self, _t: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        Ok(())
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        Ok(ratatui::layout::Size::new(self.w, self.h))
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        Ok(ratatui::backend::WindowSize {
            columns_rows: ratatui::layout::Size::new(self.w, self.h),
            pixels: ratatui::layout::Size::new(0, 0),
        })
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn backspace_leaves_no_afterimage_in_the_diff() {
    let (mut app, _dir) = make_app_keep(5, Mode::View);
    app.mode = Mode::Input;
    app.composer_return = Mode::View;
    app.ensure_row_cache(75);
    app.refresh_line_rows();
    let mut terminal = ratatui::Terminal::new(VirtualTerm::new(20, 24)).unwrap();
    let w = 20usize;

    // Frame 1: a comment long enough to WRAP (20-wide bar), caret at end.
    app.input = "あいうえおかきくけこさしすせそ".to_string();
    app.input_cursor = 45;
    app.input_start = 0;
    app.input_end = 0;
    app.ensure_row_cache(20);
    app.refresh_line_rows();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let frame1: Vec<String> = terminal
        .backend()
        .screen
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect();
    assert!(
        frame1
            .iter()
            .filter(|l| l.contains('あ') || l.contains('そ'))
            .count()
            >= 2,
        "the bar must wrap for this test:\n{}",
        frame1
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{i:2}|{l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Backspace across every wrap boundary, drawing (and diff-replaying)
    // after each keystroke — a real terminal sees exactly these frames.
    for _ in 0..15 {
        on_input_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
    }
    terminal.backend_mut().diff_log.clear();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    // Inspect what the LAST draw emitted for the composer area (rows 2-8):
    // any cell the diff skipped is a cell a real terminal keeps stale.
    let mut per_row: std::collections::BTreeMap<u16, Vec<(u16, String)>> =
        std::collections::BTreeMap::new();
    for (x, y, s) in terminal.backend().diff_log.iter() {
        if *y <= 8 {
            per_row.entry(*y).or_default().push((*x, s.clone()));
        }
    }
    for (y, mut cells) in per_row {
        cells.sort();
        eprintln!("emitted row {y}: {cells:?}");
    }

    // Ground truth: a fresh full render of the final state.
    let mut tb = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 24)).unwrap();
    tb.draw(|f| draw(f, &mut app)).unwrap();
    let tb_buf = tb.backend().buffer().clone();

    // The virtual terminal after the diff must equal the fresh render;
    // every mismatch is a cell a real terminal would leave as an
    // afterimage.
    let diffs: Vec<(usize, usize, String, String)> = (0..tb_buf.content.len())
        .filter(|&i| {
            let a = &terminal.backend().screen[i];
            let b = &tb_buf.content[i];
            a.symbol() != b.symbol()
                || a.style().bg != b.style().bg
                || a.style().fg != b.style().fg
        })
        .map(|i| {
            (
                i % w,
                i / w,
                format!(
                    "{:?}/{:?}",
                    terminal.backend().screen[i].symbol(),
                    terminal.backend().screen[i].style()
                ),
                format!("{:?}/{:?}", tb_buf.content[i].symbol(), tb_buf.content[i].style()),
            )
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "afterimage cells after wrap-crossing backspaces (x, y, stale, expected):\n{diffs:?}\nframe1:\n{}\n---\nfinal (fresh render):\n{}",
        frame1.iter().enumerate().map(|(i, l)| format!("{i:2}|{l}")).collect::<Vec<_>>().join("\n"),
        tb_buf
            .content
            .chunks(w)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .enumerate()
            .map(|(i, l)| format!("{i:2}|{l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn wide_char_residue_is_blanked_by_the_afterimage_pass() {
    // ratatui's diff blanks the LEFT half of a removed wide character but
    // skips its RIGHT half (that continuation cell is blank in its own
    // model both before and after — equal, so no update). A real terminal
    // renders the right half as a visible halved glyph that never gets
    // cleared: the backspace afterimage. akapen's repair pass must queue
    // an explicit space over exactly those cells.
    use ratatui::{buffer::Buffer, layout::Rect, style::Style};

    let mut prev = Buffer::empty(Rect::new(0, 0, 6, 3));
    prev.set_string(0, 0, " あx  ", Style::default()); // あ at col 1 (covers 1..2)
    prev.set_string(0, 2, "あb   ", Style::default()); // removed wide char at row 2
    let mut curr = Buffer::empty(Rect::new(0, 0, 6, 3));
    curr.set_string(0, 0, "    x ", Style::default()); // あ gone; col 2 blank
    curr.set_string(0, 2, "  b   ", Style::default()); // あ replaced by a narrow char
    // A third case that must NOT be touched: a wide char that is still
    // wide in the next frame (a new wide char owns its continuation).
    let mut prev3 = Buffer::empty(Rect::new(0, 0, 6, 1));
    prev3.set_string(0, 0, "あ    ", Style::default());
    let mut curr3 = Buffer::empty(Rect::new(0, 0, 6, 1));
    curr3.set_string(0, 0, "い    ", Style::default());

    let mut out: Vec<u8> = Vec::new();
    crate::clear_wide_char_residue_to(&prev, &curr, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    // crossterm MoveTo(x+1, y) prints CSI {y+1};{x+1}H — position (2,0)
    // for the あ at col 1, and (1,2) for the row-2 case — followed by a
    // space to clear the halved glyph.
    assert!(
        out.contains("\x1b[1;3H ") || out.contains("\x1b[0;2H ") || out.contains("\x1b[1;3H"),
        "the replaced wide char's right half (row 0, col 2) is blanked:\n{out:?}"
    );
    assert!(
        out.contains("\x1b[3;2H ") || out.contains("\x1b[2;1H ") || out.contains("\x1b[3;2H"),
        "the removed wide char's right half (row 2, col 1) is blanked:\n{out:?}"
    );
    // No cleanup for a wide char that stays wide.
    let mut out2: Vec<u8> = Vec::new();
    crate::clear_wide_char_residue_to(&prev3, &curr3, &mut out2).unwrap();
    assert!(
        out2.is_empty(),
        "a wide char replaced by a wide char needs no afterimage repair"
    );
}

#[test]
fn afterimage_repair_keeps_the_cells_background() {
    // Regression: the repair pass printed a bare space with whatever SGR
    // state the terminal was left in. When the repaired cell sits on a
    // highlight band (the cursor row's fill after a view/source toggle
    // moved the wide chars around), the bare space punched a default-
    // background hole into the band — visible as stripes. The space must
    // carry the current cell's own background.
    use ratatui::{buffer::Buffer, layout::Rect, style::{Color, Style}};

    let mut prev = Buffer::empty(Rect::new(0, 0, 6, 1));
    prev.set_string(0, 0, " あ   ", Style::default()); // あ covers cols 1..=2
    let mut curr = Buffer::empty(Rect::new(0, 0, 6, 1));
    curr.set_string(
        0,
        0,
        "      ",
        Style::default().bg(Color::Rgb(88, 91, 112)), // the highlight band
    );

    let mut out: Vec<u8> = Vec::new();
    crate::clear_wide_char_residue_to(&prev, &curr, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\x1b[48;2;88;91;112m"),
        "the repaired space carries the band's background:\n{out:?}"
    );
    assert!(
        out.ends_with("\x1b[0m"),
        "the pass leaves the terminal's SGR state clean:\n{out:?}"
    );
}


/// An inline comment card inserts rows into `view.rows`; `row_attrs`
/// must be inserted alongside, or every attribution BELOW the card shifts
/// up by the card's height and the range decoration lands on the wrong
/// line. Nothing else in the view reads `row_attrs`, so the drift is
/// silent — hence this test.
#[test]
fn a_comment_card_keeps_row_attrs_parallel_to_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.md");
    std::fs::write(&path, "一行目\n\n二行目\n\n前重要後\n").unwrap();
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(None, false);
    // A comment on the FIRST line: its card is inserted above the
    // paragraph the decoration targets.
    let comments = vec![crate::comment::Comment {
        file_path: "doc.md".into(),
        start: 1,
        end: 1,
        lines: String::new(),
        revision: None,
        text: "カード".into(),
    }];
    let view = render_view_with_cards(&source, 60, &highlight, &comments, Default::default());
    assert!(view.card_rows.iter().any(|&b| b), "the card was inserted");
    assert_eq!(
        view.row_attrs.len(),
        view.rows.len(),
        "row_attrs must grow with rows"
    );
    for (r, row) in view.rows.iter().enumerate() {
        assert_eq!(
            view.row_attrs[r].len(),
            row.len(),
            "row {r} ({:?}) lost its attributions",
            row.iter().map(|s| s.text.as_str()).collect::<String>()
        );
    }
}

/// The same drift, seen the way a user would: a decoration placed on a
/// paragraph BELOW a comment card must still paint that paragraph. With
/// the `row_attrs` insert missing, the mark lands on a row above.
#[test]
fn a_decoration_below_a_comment_card_still_lands_on_its_own_row() {
    use crate::decoration::{Decoration, DecorationKind, DecorationStyles};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.md");
    let text = "一行目\n\n二行目\n\n前重要後\n";
    std::fs::write(&path, text).unwrap();
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(None, false);
    let comments = vec![crate::comment::Comment {
        file_path: "doc.md".into(),
        start: 1,
        end: 1,
        lines: String::new(),
        revision: None,
        text: "カード".into(),
    }];
    let view = render_view_with_cards(&source, 60, &highlight, &comments, Default::default());
    let mark_bg = DecorationStyles::from_theme(&highlight, Default::default())
        .mark_style()
        .bg;
    let start = text.find("重要").unwrap();
    let decorations = vec![Decoration {
        range: start..start + "重要".len(),
        kind: DecorationKind::SemanticMark,
    }];
    let (painted, _) = view.visible_text_decorated(
        200,
        None,
        ratatui::style::Color::Rgb(88, 91, 112),
        &decorations,
    );
    // Exactly one row carries the mark, and it is the row that renders
    // the decorated paragraph.
    let marked: Vec<(usize, String)> = painted
        .lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let text: String = line
                .spans
                .iter()
                .filter(|s| s.style.bg == mark_bg)
                .map(|s| s.content.as_ref())
                .collect();
            (!text.is_empty()).then_some((i, text))
        })
        .collect();
    assert_eq!(marked.len(), 1, "exactly one row is marked: {marked:?}");
    let (row, text) = &marked[0];
    assert_eq!(text, "重要");
    let whole: String = painted.lines[*row]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(whole.contains("前重要後"), "marked row {row} is {whole:?}");
}

/// The whole stack, drawn into a real (test) terminal: `--decorations`
/// through `Config` → `App` → `draw` → cells. Three visibly different
/// regions on ONE rendered line — the milestone of this phase, seen the
/// way the terminal sees it.
#[test]
fn decorations_paint_three_regions_on_one_terminal_line() {
    use crate::decoration::{Decoration, DecorationKind, DecorationStyles};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.md");
    let text = "先頭の段落\n\n前 **重要** 後\n";
    std::fs::write(&path, text).unwrap();
    let mark_at = |needle: &str, kind| {
        let start = text.find(needle).unwrap();
        Decoration { range: start..start + needle.len(), kind }
    };
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: None,
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: false,
        semantic: None,
        semantic_cmd: None,
        decoration_blend: Default::default(),
        decorations: vec![
            mark_at("重要", DecorationKind::SemanticMark),
            mark_at(" 後", DecorationKind::Dim),
        ],
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, crate::view_render_width(60), &highlight, Default::default());
    let styles = DecorationStyles::from_theme(&highlight, Default::default());
    let mark_bg = styles.mark_style().bg;
    // The paragraph's plain text carries the theme's default foreground,
    // so that is what the dim resolves from.
    let dim_fg = styles.dim_fg(None);
    let mut app = App::new(config, source, highlight, view, false);
    app.mode = Mode::View;
    app.gutter_cols = 3;
    // The cursor stays on line 0, so the cursor band never touches the
    // decorated paragraph.
    assert_eq!(app.view.cursor, 0);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    let w = buf.area.width as usize;
    // The terminal row that shows the decorated paragraph.
    // Wide glyphs occupy two cells (the second is blank), so match on a
    // single character rather than the row's concatenated symbols.
    let row = (0..buf.area.height as usize)
        .find(|&y| (0..w).any(|x| buf.content[y * w + x].symbol() == "重"))
        .expect("the decorated paragraph is on screen");

    // Walk the cells and read off the three regions by their styles.
    let cell = |x: usize| &buf.content[row * w + x];
    let text_start = (0..w)
        .find(|&x| cell(x).symbol() == "前")
        .expect("the paragraph starts somewhere");
    let plain = cell(text_start).style();
    let marked = cell(text_start + 3).style(); // 重 (前 = 2 cols, space = 1)
    let dimmed = cell(text_start + 8).style(); // 後
    assert_eq!(cell(text_start + 3).symbol(), "重");
    assert_eq!(cell(text_start + 8).symbol(), "後");

    assert_eq!(marked.bg, mark_bg, "the marked region has the mark background");
    assert_ne!(plain.bg, mark_bg, "the plain region does not");
    // The dim is a real COLOR, not `Modifier::DIM` — SGR 2 is too widely
    // ignored for the layer to rely on (that is what made MARKED and DIM
    // indistinguishable on a real terminal).
    assert_eq!(dimmed.fg, Some(dim_fg), "the dimmed region has the dim color");
    assert_ne!(dimmed.fg, plain.fg);
    assert_eq!(dimmed.bg, plain.bg, "dim does not touch the background");
    for style in [plain, marked, dimmed] {
        assert!(!style.add_modifier.contains(ratatui::style::Modifier::DIM));
    }
    // Three distinct styles on one terminal line.
    assert_ne!(plain, marked);
    assert_ne!(plain, dimmed);
    assert_ne!(marked, dimmed);
    // Syntax highlighting survived: the strong span keeps BOLD and its
    // color under the mark, and the dimmed text keeps its hue (it is the
    // plain color moved toward the page, not a flat gray).
    assert!(marked.add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(marked.fg, plain.fg);
    assert_eq!(dimmed.add_modifier, plain.add_modifier);
}


/// **A dimmed list item takes its marker down with it** — the whole
/// chain, in cells: `atomize` → the Atom's range → a `Dim` decoration →
/// the renderer's attribution of the `*` marker → the terminal buffer.
///
/// Three components used to be individually right and off by one byte
/// together: the Atom stopped before the item's trailing newline, the
/// marker claimed the item's event range (newline included), and the
/// superset rule in `decoration.rs` decorates only what a decoration
/// COVERS. The marker therefore stayed bright under a dimmed item.
///
/// Splitting items into sentences made the mismatch structural rather
/// than off-by-one: the first sentence's Atom stops at the first `。`,
/// nowhere near the item's end. Anchoring the marker on its OWN bytes is
/// what makes every case work, so the item below is deliberately a
/// two-sentence one — the case that only the real fix can pass.
///
/// The ranges are not typed in by hand: they come from `atomize`, so a
/// change on either side of the seam breaks this test.
#[test]
fn a_dimmed_list_item_dims_its_marker_too() {
    use crate::decoration::{Decoration, DecorationKind, DecorationStyles};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.md");
    let text = "先頭の段落\n\n* 一文目です。二文目です。\n";
    std::fs::write(&path, text).unwrap();

    // 実際に akapen が使う Atom 列。手で書いた range では、ずれが戻っても
    // このテストは気づけない。
    let atoms = semantic_reading::atomize(text);
    let first_item = atoms
        .iter()
        .find(|a| a.kind == semantic_reading::AtomKind::ListItem)
        .expect("項目が Atom になっている");
    assert_eq!(
        &text[first_item.range.clone()],
        "* 一文目です。",
        "項目は文へ割れ、マーカーは最初の文に付く"
    );

    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: None,
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: false,
        semantic: None,
        semantic_cmd: None,
        decoration_blend: Default::default(),
        decorations: vec![Decoration {
            range: first_item.range.clone(),
            kind: DecorationKind::Dim,
        }],
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, crate::view_render_width(60), &highlight, Default::default());
    let styles = DecorationStyles::from_theme(&highlight, Default::default());
    let mut app = App::new(config, source, highlight, view, false);
    app.mode = Mode::View;
    app.gutter_cols = 3;
    // カーソルは 0 行目。帯は項目の行に触れない（帯の下では Dim を落とす）。
    assert_eq!(app.view.cursor, 0);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    let w = buf.area.width as usize;
    let row = (0..buf.area.height as usize)
        .find(|&y| (0..w).any(|x| buf.content[y * w + x].symbol() == "*"))
        .expect("箇条書きの行が画面に出ている");
    let cell = |x: usize| &buf.content[row * w + x];
    let marker_x = (0..w).find(|&x| cell(x).symbol() == "*").unwrap();
    let dimmed_x = (0..w).find(|&x| cell(x).symbol() == "一").unwrap();
    // 装飾の外。2 文目は別の Atom なので DIM ではない。
    let plain_x = (0..w).find(|&x| cell(x).symbol() == "二").unwrap();

    let marker = cell(marker_x).style();
    let dimmed = cell(dimmed_x).style();
    let plain = cell(plain_x).style();

    let dim_fg = styles.dim_fg(plain.fg);
    assert_eq!(dimmed.fg, Some(dim_fg), "本文は DIM に落ちる");
    assert_eq!(
        marker.fg,
        Some(dim_fg),
        "マーカーも同じ色まで沈む — 置き去りにされない"
    );
    assert_ne!(plain.fg, Some(dim_fg), "装飾していない 2 文目は明るいまま");
    assert_eq!(marker.bg, plain.bg, "Dim は背景に触らない");
}

/// The Semantic Reading Layer, drawn into a real (test) terminal:
/// `--semantic` through `Config` → `App` → `Provider` → `policy::decorate`
/// → `draw` → cells. The milestone of this phase — **one source line
/// splits mid-line into two different styles, and which two depends on
/// the READ budget** — seen the way the terminal sees it.
#[test]
fn the_reading_budget_splits_one_terminal_line_into_two_styles() {
    use crate::decoration::DecorationStyles;

    let path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.md"
    ));
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.json"
    ));
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: None,
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: false,
        semantic: Some(fixture.clone()),
        semantic_cmd: None,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, crate::view_render_width(80), &highlight, Default::default());
    let styles = DecorationStyles::from_theme(&highlight, Default::default());
    let mark_bg = styles.mark_style().bg;
    let mut app = App::new(config, source, highlight, view, false);
    // Config → provider の分岐 → App という実経路を通す。provider を
    // 増やすときに触るのはこの関数の match 1 つだけ、という約束の固定。
    app.set_semantic_source(crate::semantic::source_from_config(&app.config).unwrap());
    assert!(app.semantic_enabled(), "--semantic から provider が立つ");
    app.reanalyze_semantics();
    app.mode = Mode::View;
    app.gutter_cols = 3;
    assert!(app.semantic_doc.is_some(), "the fixture matches demo.md");
    // The cursor sits on the title, so the cursor band never touches the
    // line under test.
    assert_eq!(app.view.cursor, 0);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    // `(the style on 採, the style on 詳)` — the two halves of
    // 「採用する方式は差分配信である。詳細は付録にまとめた。」, which is ONE
    // source line holding two Atoms of two different Semantic Units.
    let halves = |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let cell = |y: usize, x: usize| &buf.content[y * w + x];
        // 「詳」は文書内で 1 度しか出ない。「採」は導入段落にも出るので、
        // 「詳」の行を先に決めてから同じ行の「採」を探す。
        let (row, right) = (0..buf.area.height as usize)
            .find_map(|y| (0..w).find(|&x| cell(y, x).symbol() == "詳").map(|x| (y, x)))
            .expect("the 結論 paragraph is on screen");
        let left = (0..right)
            .find(|&x| cell(row, x).symbol() == "採")
            .expect("both halves are on the SAME terminal row");
        (cell(row, left).style(), cell(row, right).style())
    };

    // READ 100 % — the design document's first demo: the whole document
    // is shown and the ESSENTIAL half is marked. Nothing is dim.
    assert_eq!(app.reading_budget, 100);
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_eq!(essential.bg, mark_bg, "ESSENTIAL は MARKED");
    assert_ne!(detail.bg, mark_bg, "DETAIL は NORMAL");
    assert!(!detail.add_modifier.contains(ratatui::style::Modifier::DIM));
    assert_ne!(essential, detail, "100 % でもう行の途中で切り替わっている");

    // READ 30 % — the same line, now MARKED against DIM. The dim is a
    // real foreground COLOR (SGR 2 is too widely ignored to rely on), so
    // the two halves differ in a way every terminal renders.
    let bright = detail.fg;
    assert!(app.nudge_reading_budget(-70));
    assert_eq!(app.reading_budget, 30);
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_eq!(essential.bg, mark_bg, "ESSENTIAL は Budget を下げても MARKED");
    assert_eq!(detail.fg, Some(styles.dim_fg(bright)), "DETAIL は DIM に落ちる");
    assert_ne!(detail.fg, bright);
    assert_eq!(essential.fg, bright, "MARKED 側の前景は動かない");
    for style in [essential, detail] {
        assert!(!style.add_modifier.contains(ratatui::style::Modifier::DIM));
    }
    assert_ne!(essential, detail);
}

/// **A MARKED line under the cursor shows the band, not the amber.** Seen
/// on a real document as "policy says MARKED for 17 Atoms and 16 of them
/// are amber": the seventeenth was the line the cursor had been moved to
/// in order to look at it. The cursor band paints the whole row's
/// background and wins over the mark by design (`view.rs`, `span_hl`),
/// so under the band a MARKED phrase and a NORMAL one are the same
/// color. The amber is not lost — it is back the moment the cursor
/// leaves — and nothing in `decorate_row` or the projection is involved.
///
/// Same demo line as the test above: 「採用する方式は差分配信である。」
/// (MARKED) and 「詳細は付録にまとめた。」 (NORMAL) on ONE source line.
#[test]
fn a_marked_line_under_the_cursor_shows_the_band_not_the_amber() {
    use crate::decoration::DecorationStyles;

    let path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.md"
    ));
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.json"
    ));
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: None,
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: false,
        semantic: Some(fixture),
        semantic_cmd: None,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    // `ViewState::cursor` is a SOURCE line, not a display row.
    let marked_line = source
        .content
        .lines()
        .position(|l| l.starts_with("採用する方式は差分配信である。"))
        .expect("the 結論 line is in demo.md");
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, crate::view_render_width(80), &highlight, Default::default());
    let styles = DecorationStyles::from_theme(&highlight, Default::default());
    let mark_bg = styles.mark_style().bg;
    let mut app = App::new(config, source, highlight, view, false);
    app.set_semantic_source(crate::semantic::source_from_config(&app.config).unwrap());
    app.reanalyze_semantics();
    app.mode = Mode::View;
    app.gutter_cols = 3;
    assert!(app.semantic_doc.is_some(), "the fixture matches demo.md");

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let halves = |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let cell = |y: usize, x: usize| &buf.content[y * w + x];
        let (row, right) = (0..buf.area.height as usize)
            .find_map(|y| (0..w).find(|&x| cell(y, x).symbol() == "詳").map(|x| (y, x)))
            .expect("the 結論 paragraph is on screen");
        let left = (0..right)
            .find(|&x| cell(row, x).symbol() == "採")
            .expect("both halves are on the SAME terminal row");
        (cell(row, left).style(), cell(row, right).style())
    };

    // Cursor on the title: the MARKED half is amber, the NORMAL half is not.
    assert_eq!(app.view.cursor, 0);
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_eq!(essential.bg, mark_bg, "カーソルが他の行にあれば MARKED は琥珀");
    assert_ne!(detail.bg, mark_bg);

    // Cursor on the marked line: the band paints BOTH halves the same
    // background, and that background is not the amber. This is the
    // "one MARKED line is not amber" sighting, reproduced.
    app.view.cursor = marked_line;
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_ne!(essential.bg, mark_bg, "カーソル行では帯が琥珀を覆う");
    assert!(essential.bg.is_some(), "覆っているのは帯の背景であって、無色ではない");
    assert_eq!(essential.bg, detail.bg, "帯の下では MARKED と NORMAL が同じ背景になる");

    // And it is the band, not a lost mark: leaving the line brings it back.
    app.view.cursor = 0;
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_eq!(essential.bg, mark_bg, "カーソルが離れれば琥珀は戻る");
    assert_ne!(detail.bg, mark_bg);
}

/// 設計書「Budget 変更では Jev を呼ばない」を、呼び出し回数と
/// レンダー済み行の同一性で固定する。
///
/// Phase 2 は「decoration が `render::render` に到達しない」ことを構造で
/// 示した。ここはその 1 段上 — Budget キーが `Provider::analyze` にも
/// `ViewState::render` にも到達しないこと。
#[test]
fn moving_the_budget_calls_neither_the_provider_nor_the_renderer() {
    use semantic_reading::{Provider, SemanticDocument};
    use std::cell::Cell;
    use std::rc::Rc;

    /// `analyze` が呼ばれた回数を数える provider。
    struct CountingProvider {
        document: SemanticDocument,
        calls: Rc<Cell<usize>>,
    }
    impl Provider for CountingProvider {
        fn analyze(&self, _source: &str) -> semantic_reading::Result<SemanticDocument> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.document.clone())
        }
    }

    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/semantic/demo.json");
    let document: SemanticDocument =
        serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();

    let mut app = make_app(3, Mode::View);
    let calls = Rc::new(Cell::new(0usize));
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Inline(Box::new(
        CountingProvider {
            document,
            calls: Rc::clone(&calls),
        },
    ))));
    // 文書が用意された時点で 1 回。以降は文書が変わるまで呼ばれない。
    app.reanalyze_semantics();
    assert_eq!(calls.get(), 1);
    assert!(app.semantic_doc.is_some());
    let at_100 = app.semantic_decorations.clone();
    assert!(!at_100.is_empty());

    // 「この行は再レンダーされていない」の目印。`ViewState::render` が
    // 走れば rows は作り直されて消える。
    app.view.rows[0] = vec![crate::highlight::Span {
        text: "SENTINEL".to_string(),
        style: ratatui::style::Style::default(),
    }];

    for _ in 0..7 {
        on_view_key(&mut app, KeyCode::Char('<'), KeyModifiers::NONE, None);
    }
    on_view_key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE, None);

    assert_eq!(app.reading_budget, 29, "7 回の <（-10）と 1 回の -（-1）");
    assert_eq!(calls.get(), 1, "Budget 操作で analyze は呼ばれない");
    assert_eq!(
        app.view.rows[0][0].text, "SENTINEL",
        "Budget 操作で markdown は再レンダーされない"
    );
    // それでいて表示状態はちゃんと変わっている（no-op ではない）。
    assert_ne!(app.semantic_decorations, at_100);

    // 上限・下限で止まり、そこでも provider には触れない。下限は 1 % では
    // なく文書の下限（`policy::floor` — 一段目の大きさ）である。
    for _ in 0..40 {
        on_view_key(&mut app, KeyCode::Char('<'), KeyModifiers::NONE, None);
    }
    assert_eq!(app.reading_budget, app.reading_floor().unwrap());
    for _ in 0..40 {
        on_view_key(&mut app, KeyCode::Char('>'), KeyModifiers::NONE, None);
    }
    assert_eq!(app.reading_budget, crate::semantic::MAX_BUDGET);
    assert_eq!(app.semantic_decorations, at_100, "100 % に戻れば元どおり");
    assert_eq!(calls.get(), 1);

    // `+` と `=` は同じ 1 段。`-` と対になる。
    on_view_key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE, None);
    on_view_key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE, None);
    assert_eq!(app.reading_budget, 98);
    on_view_key(&mut app, KeyCode::Char('+'), KeyModifiers::NONE, None);
    assert_eq!(app.reading_budget, 99);
    on_view_key(&mut app, KeyCode::Char('='), KeyModifiers::NONE, None);
    assert_eq!(app.reading_budget, 100);
    assert_eq!(calls.get(), 1);
}

/// **The budget cannot be turned below the document's floor.** Below the
/// first tier of the ledger (cores and their lineage, kept regardless of
/// the budget) the screen does not move, so `-`/`<` stop there instead
/// of spinning a number that lies. The footer says so — `(floor)` rides
/// on the percentage — and no BEL is rung (nothing invalid was asked).
#[test]
fn the_budget_stops_at_the_floor_and_the_footer_says_so() {
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.json"
    ));
    let path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.md"
    ));
    let mut app = make_app(3, Mode::View);
    app.source = Source::load(path).unwrap();
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Inline(
        crate::semantic::load_fixture(&fixture).unwrap(),
    )));
    app.reanalyze_semantics();
    let floor = app.reading_floor().expect("annotation in hand → a floor");
    assert!(
        floor > crate::semantic::MIN_BUDGET,
        "demo.json has cores, so the floor is above 1 % ({floor})"
    );

    // Above the floor: no mark.
    assert!(!app.at_reading_floor());
    let hints = crate::chrome::footer_hints(&app);
    assert!(hints.contains("READ 100%"), "{hints}");
    assert!(!hints.contains("floor"), "{hints}");

    // Hold `<`: the readout lands on the floor and stays there.
    for _ in 0..20 {
        on_view_key(&mut app, KeyCode::Char('<'), KeyModifiers::NONE, None);
    }
    assert_eq!(app.reading_budget, floor);
    assert!(app.at_reading_floor());
    let hints = crate::chrome::footer_hints(&app);
    assert!(hints.contains(&format!("READ {floor}% (floor)")), "{hints}");
    assert!(app.status.is_none(), "止まるだけで、何も鳴らさない: {:?}", app.status);

    // `-` at the floor is a no-op too, and the decorations are exactly
    // those of the floor budget — the number matches the screen.
    let at_floor = app.semantic_decorations.clone();
    assert!(!app.nudge_reading_budget(-1));
    assert_eq!(app.reading_budget, floor);
    assert_eq!(app.semantic_decorations, at_floor);
    assert_eq!(
        at_floor,
        crate::semantic::decorations_for(app.semantic_doc.as_ref().unwrap(), floor)
    );

    // What the floor promises: the bytes on screen fit in `floor` %, and
    // one step below they would not (the number would lie).
    let document = app.semantic_doc.as_ref().unwrap();
    let shown = |budget: u8| -> usize {
        semantic_reading::policy::decorate(document, budget)
            .iter()
            .filter(|(_, s)| *s != semantic_reading::DisplayState::Dim)
            .map(|(r, _)| r.len())
            .sum()
    };
    let total: usize = document.atoms.iter().map(|a| a.len()).sum();
    assert!(shown(floor) * 100 <= floor as usize * total);
    assert!(shown(floor - 1) * 100 > (floor as usize - 1) * total);

    // Stepping up leaves the floor; the mark goes away.
    on_view_key(&mut app, KeyCode::Char('+'), KeyModifiers::NONE, None);
    assert_eq!(app.reading_budget, floor + 1);
    assert!(!crate::chrome::footer_hints(&app).contains("floor"));
}

/// **An answer that arrives under the budget lifts it onto the floor.**
/// The budget rides along across documents, so it can be sitting at 5 %
/// when an annotation with a 40 % floor lands; a re-analysis can move
/// the floor the same way. Never lowered — a budget above the floor is
/// the user's choice. Without an annotation there is no floor at all.
#[test]
fn an_arriving_annotation_lifts_the_budget_onto_its_floor() {
    use semantic_reading::{Atom, AtomIndex, AtomKind, ReadingTier, SemanticDocument, SemanticUnit};

    let mut app = make_app(3, Mode::View);
    install_semantic_command(&mut app, "true");
    let tx = app.semantic_results.as_ref().unwrap().tx.clone();
    let content = app.source.content.clone();
    assert!(content.len() >= 10, "make_app の文書は 10 バイト以上: {}", content.len());

    // 核が文書の 4 割 → 下限 40 %。
    let with_floor = |core_len: usize| {
        let mut doc = SemanticDocument::new(
            vec![
                Atom::new(0..core_len, AtomKind::Sentence),
                Atom::new(core_len..10, AtomKind::Sentence),
            ],
            vec![
                SemanticUnit::new("core", [AtomIndex(0)], ReadingTier::Essential),
                SemanticUnit::new("rest", [AtomIndex(1)], ReadingTier::Detail),
            ],
        );
        doc.source_sha256 = Some(crate::semantic::source_digest(&content));
        doc
    };

    // No annotation yet: no floor, and the budget goes wherever it is put.
    assert_eq!(app.reading_floor(), None);
    assert!(!app.at_reading_floor());
    app.reading_budget = 5;

    app.semantic_generation = 1;
    app.semantic_inflight = Some(1);
    tx.send(crate::app::AnalysisMessage {
        generation: 1,
        result: Ok(with_floor(4)),
    })
    .unwrap();
    app.poll_semantic_analysis();
    assert_eq!(app.reading_floor(), Some(40));
    assert_eq!(app.reading_budget, 40, "5 % は下限を割っていたので引き上げる");
    assert!(app.at_reading_floor());
    assert_eq!(
        app.semantic_decorations,
        crate::semantic::decorations_for(app.semantic_doc.as_ref().unwrap(), 40),
        "引き上げた予算で投影されている"
    );

    // Re-analysis moves the floor up (core grew to 7 / 10): lifted again.
    app.semantic_generation = 2;
    app.semantic_inflight = Some(2);
    tx.send(crate::app::AnalysisMessage {
        generation: 2,
        result: Ok(with_floor(7)),
    })
    .unwrap();
    app.poll_semantic_analysis();
    assert_eq!(app.reading_floor(), Some(70));
    assert_eq!(app.reading_budget, 70);

    // Re-analysis moves the floor down (core shrank to 2 / 10): the
    // budget stays where the user left it, and `<` can now go lower.
    app.semantic_generation = 3;
    app.semantic_inflight = Some(3);
    tx.send(crate::app::AnalysisMessage {
        generation: 3,
        result: Ok(with_floor(2)),
    })
    .unwrap();
    app.poll_semantic_analysis();
    assert_eq!(app.reading_floor(), Some(20));
    assert_eq!(app.reading_budget, 70, "下限が下がっても予算は下げない");
    assert!(!app.at_reading_floor());
    for _ in 0..10 {
        on_view_key(&mut app, KeyCode::Char('<'), KeyModifiers::NONE, None);
    }
    assert_eq!(app.reading_budget, 20);
    assert!(crate::chrome::footer_hints(&app).contains("READ 20% (floor)"));

    // A failed re-analysis keeps the annotation, hence the floor.
    app.semantic_generation = 4;
    app.semantic_inflight = Some(4);
    tx.send(crate::app::AnalysisMessage {
        generation: 4,
        result: Err("boom".to_string()),
    })
    .unwrap();
    app.poll_semantic_analysis();
    assert_eq!(app.reading_floor(), Some(20));
    assert_eq!(app.reading_budget, 20);
}

/// 継ぎ目の反対側: **文書が入れ替わったら provider は呼ばれる。**
///
/// Budget では呼ばれないことを固定したので、呼ばれるべきときに呼ばれる
/// ことも固定しておく（両方無いと「そもそも繋がっていない」でも緑になる）。
/// これは設計書「編集時」の位置でもある — idle して文書が落ち着いたら
/// 再解析する。
#[test]
fn the_provider_is_re_asked_when_the_document_itself_changes() {
    use semantic_reading::{Provider, SemanticDocument};
    use std::cell::Cell;
    use std::rc::Rc;

    struct CountingProvider {
        calls: Rc<Cell<usize>>,
    }
    impl Provider for CountingProvider {
        fn analyze(&self, source: &str) -> semantic_reading::Result<SemanticDocument> {
            self.calls.set(self.calls.get() + 1);
            // 渡された source を実際に見る provider（Jev 側の形）。
            Ok(SemanticDocument::new(
                vec![semantic_reading::Atom::new(
                    0..source.len(),
                    semantic_reading::AtomKind::Sentence,
                )],
                vec![semantic_reading::SemanticUnit::new(
                    "u1",
                    [semantic_reading::AtomIndex(0)],
                    semantic_reading::ReadingTier::Essential,
                )],
            ))
        }
    }

    let (mut app, dir) = make_app_keep(4, Mode::View);
    let calls = Rc::new(Cell::new(0usize));
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Inline(Box::new(
        CountingProvider { calls: Rc::clone(&calls) },
    ))));
    app.reanalyze_semantics();
    assert_eq!(calls.get(), 1);
    let before = app.semantic_decorations.clone();

    // ファイルが外から書き換わった → 再解析。
    std::fs::write(dir.path().join("doc.md"), "line1
line2
line3
line4
line5
").unwrap();
    reload_source(&mut app, false).unwrap();
    assert_eq!(calls.get(), 2, "reload では provider を呼び直す");
    assert_ne!(
        app.semantic_decorations, before,
        "新しい文書に対する range になっている"
    );
}

/// 手順3の落とし穴を App の高さで: fixture が別の文書のものなら、
/// 装飾は 1 つも出ないし、READ の表示も出ない。
#[test]
fn a_fixture_for_another_document_is_refused_and_leaves_no_decorations() {
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.json"
    ));
    // make_app の文書は "line1..line3" — demo.md ではない。
    let mut app = make_app(3, Mode::View);
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Inline(
        crate::semantic::load_fixture(&fixture).unwrap(),
    )));
    app.reanalyze_semantics();

    assert!(app.semantic_doc.is_none(), "別文書の fixture は拒否される");
    assert!(app.semantic_decorations.is_empty());
    let (message, _, is_error) = app.status.clone().expect("警告が出ていること");
    assert!(is_error, "黙って何もしないのではなく loud に断る");
    assert!(message.contains("a different document"), "{message}");
    // READ の表示は出ない（Budget は値としては存在するが意味を持たない）。
    assert!(!crate::chrome::footer_hints(&app).contains("READ"));
    // キーも断る。
    on_view_key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE, None);
    assert_eq!(app.reading_budget, 100);
    assert!(
        app.status.as_ref().unwrap().0.contains("unavailable"),
        "{:?}",
        app.status
    );
    // キーそのものは生きている（`--semantic` は渡されているので）。
    assert!(app.semantic_enabled());
}

/// 追加要件: **`--semantic` が無いセッションは、この改修の前と完全に同一**。
///
/// API キー（将来の Jev）も fixture も持たない人に、使えない機能の気配を
/// 見せない。キーは束縛せず、読み出しも出さず、ヘルプにも載せず、断りの
/// メッセージすら出さない。
#[test]
fn without_semantic_the_budget_keys_are_not_bound_at_all() {
    let mut app = make_app(6, Mode::View);
    assert!(app.config.semantic.is_none());
    assert!(
        crate::semantic::source_from_config(&app.config)
            .unwrap()
            .is_none(),
        "--semantic が無ければ供給源も立たない"
    );
    assert!(!app.semantic_enabled(), "provider が無い = 層が無い");

    let before_status = app.status.clone();
    let before_cursor = app.view.cursor;
    for key in ['-', '+', '=', '<', '>'] {
        on_view_key(&mut app, KeyCode::Char(key), KeyModifiers::NONE, None);
        // 無い層は動かない。
        assert_eq!(app.reading_budget, crate::semantic::DEFAULT_BUDGET);
        assert!(app.semantic_decorations.is_empty());
        // そして「使えません」も言わない — 未束縛のキーとまったく同じ、
        // 何も起きないという振る舞い。
        assert_eq!(
            app.status.as_ref().map(|(m, _, e)| (m.clone(), *e)),
            before_status.as_ref().map(|(m, _, e)| (m.clone(), *e)),
            "{key} が toast を出している"
        );
        // 他の状態にも触らない（未束縛のキーは素通りするだけ）。
        assert_eq!(app.view.cursor, before_cursor);
        assert!(app.selection.is_none());
        assert!(app.overlay.is_none());
        assert!(app.running);
    }

    // ステータス行と `?` ヘルプにも痕跡が無い。
    let hints = crate::chrome::footer_hints(&app);
    assert!(!hints.contains("READ"), "{hints}");
    app.mode = Mode::Source;
    assert!(!crate::chrome::footer_hints(&app).contains("READ"));
    assert!(
        !crate::overlay::help_rows(false, false, false, app.semantic_enabled())
            .iter()
            .any(|(label, keys)| *label == "read" || keys.contains("budget")),
        "? ヘルプに READ の行が出ている"
    );
}

/// `READ 73%` はステータス行に出るが、semantic doc が読めているときだけ。
#[test]
fn the_read_readout_appears_only_with_a_semantic_document() {
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.json"
    ));
    let mut app = make_app(3, Mode::View);
    // Budget だけを動かしても、annotation が無ければ何も出ない。
    app.reading_budget = 73;
    assert!(!crate::chrome::footer_hints(&app).contains("READ"));
    assert!(
        !crate::overlay::help_rows(false, false, false, app.semantic_enabled())
            .iter()
            .any(|(label, _)| *label == "read"),
        "? ヘルプも、使えないキーを宣伝しない"
    );

    // demo.md の annotation を、demo.md に対して当てる。
    let path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.md"
    ));
    app.source = Source::load(path).unwrap();
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Inline(
        crate::semantic::load_fixture(&fixture).unwrap(),
    )));
    app.reanalyze_semantics();
    assert!(app.semantic_doc.is_some());

    let hints = crate::chrome::footer_hints(&app);
    assert!(hints.contains("READ 73%"), "{hints}");
    // source モードでも同じ読み出しが出る（文書の性質であってモードの
    // 性質ではない）。キー操作は view だけ。
    app.mode = Mode::Source;
    assert!(crate::chrome::footer_hints(&app).contains("READ 73%"));
    assert!(
        crate::overlay::help_rows(false, false, false, true)
            .iter()
            .any(|(label, keys)| *label == "read" && keys.contains("-/+")),
        "? ヘルプに READ の行が出る"
    );
}

// ---------------------------------------------------------------------------
// `--semantic-cmd` — 外部コマンドへの委譲（非同期・世代カウンタ）
// ---------------------------------------------------------------------------

/// 参照実装を起動するコマンド行。
fn reference_semantic_command() -> String {
    format!(
        "python3 '{}'",
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/semantic/annotate-doc.py")
    )
}

/// `--semantic-cmd` を App に挿す（供給源と結果チャネルを同時に用意する
/// のは `set_semantic_source` の仕事）。
fn install_semantic_command(app: &mut App, cmd: &str) {
    app.set_semantic_source(Some(crate::semantic::SemanticSource::Command(
        crate::semantic::CommandProvider::new(cmd),
    )));
}

/// 見出しを持つ Markdown を開いた App。参照実装は見出しを ESSENTIAL に
/// するので、これがあって初めて装飾が出る（見出しの無い文書は全部
/// DETAIL で、Budget 100 % では誰も装飾されない — それが正しい）。
fn semantic_markdown_app() -> (App, tempfile::TempDir) {
    let (mut app, dir) = make_app_keep(3, Mode::View);
    std::fs::write(
        dir.path().join("doc.md"),
        "# 見出し\n\n本文です。二文目です。\n\n## 次の節\n\n詳しい話。\n",
    )
    .unwrap();
    // まだ供給源が無いので、この reload は再解析を起こさない。
    reload_source(&mut app, false).unwrap();
    assert!(!app.semantic_enabled());
    (app, dir)
}

/// 答えが来るまでポンプを回す。来なければ panic（固まったのと同じなので
/// テストとしては失敗させたい）。
fn pump_until_idle(app: &mut App, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.semantic_inflight.is_some() {
        assert!(Instant::now() < deadline, "{what}: 答えが来ない");
        std::thread::sleep(Duration::from_millis(10));
        app.poll_semantic_analysis();
    }
}

/// 外部コマンド経路が App の高さで一周すること。**UI スレッドは待たない。**
#[test]
fn an_external_command_annotates_the_document_without_blocking_the_loop() {
    let (mut app, _dir) = semantic_markdown_app();
    install_semantic_command(&mut app, &reference_semantic_command());
    assert!(app.semantic_enabled(), "--semantic-cmd で層が立つ");

    app.reanalyze_semantics();
    // 戻ってきた時点ではまだ答えは無い — これが「固まらない」の中身。
    assert!(app.semantic_doc.is_none(), "解析はまだ走っているだけ");
    assert_eq!(app.semantic_inflight, Some(1));
    // ステータス行は黙らずに「解析中」と言う。
    let hints = crate::chrome::footer_hints(&app);
    assert!(hints.contains("analyzing"), "{hints}");
    assert!(hints.contains("READ 100%"), "{hints}");

    pump_until_idle(&mut app, "最初の解析");
    let document = app.semantic_doc.as_ref().expect("注釈が入ること");
    assert_eq!(
        document.atoms,
        semantic_reading::atomize(&app.source.content),
        "range は akapen 自身の atomize のもの"
    );
    assert!(!app.semantic_decorations.is_empty());
    // 答えが来たら「解析中」は消え、READ の読み出しだけが残る。
    let hints = crate::chrome::footer_hints(&app);
    assert!(!hints.contains("analyzing"), "{hints}");
    assert!(hints.contains("READ 100%"), "{hints}");
}

/// **世代カウンタ**: 解析中に文書が変わったら、古い方の答えは捨てる。
///
/// 捨てないと、変わった後の文書に変わる前の判定を当てて違う場所を装飾する。
/// しかも静かに壊れる — 古い range も文字境界には載るので panic もせず、
/// 見た目も「ただの誤判定」に見える。だからここで番号で固定する。
///
/// スレッドを使わない決定論的な形にしてある: ワーカーが送るのと同じ
/// メッセージを直接流し込み、ポンプに通す。
#[test]
fn an_answer_from_an_older_generation_is_thrown_away() {
    use semantic_reading::{Atom, AtomIndex, AtomKind, ReadingTier, SemanticDocument, SemanticUnit};

    let mut app = make_app(3, Mode::View);
    // 走らせないコマンド（このテストで子プロセスは 1 つも起動しない）。
    install_semantic_command(&mut app, "true");
    let tx = app.semantic_results.as_ref().unwrap().tx.clone();

    // 文書 A の注釈（世代 1）。
    let document_a = |text: &str| {
        let mut doc = SemanticDocument::new(
            vec![Atom::new(0..text.len().min(5), AtomKind::Sentence)],
            vec![SemanticUnit::new("old", [AtomIndex(0)], ReadingTier::Essential)],
        );
        doc.source_sha256 = Some(crate::semantic::source_digest(text));
        doc
    };

    app.semantic_generation = 1;
    app.semantic_inflight = Some(1);
    // 文書が入れ替わった（reload.rs がファイル変更を検知した相当）。
    app.semantic_generation = 2;
    app.semantic_inflight = Some(2);

    // 遅れて届いた世代 1 の答え。
    tx.send(crate::app::AnalysisMessage {
        generation: 1,
        result: Ok(document_a(&app.source.content)),
    })
    .unwrap();
    app.poll_semantic_analysis();

    assert!(
        app.semantic_doc.is_none(),
        "古い世代の答えは、たとえ中身が正しくても当てない"
    );
    assert!(app.semantic_decorations.is_empty());
    assert_eq!(
        app.semantic_inflight,
        Some(2),
        "古い答えが、走っている新しい解析まで終わったことにしない"
    );

    // 現行世代の答えは受け取る。
    tx.send(crate::app::AnalysisMessage {
        generation: 2,
        result: Ok(document_a(&app.source.content)),
    })
    .unwrap();
    app.poll_semantic_analysis();
    assert!(app.semantic_doc.is_some(), "現行世代は当てる");
    assert_eq!(app.semantic_inflight, None, "走っているものは無くなった");
}

/// 世代カウンタを本物のスレッドで。遅い解析と速い解析を同時に走らせ、
/// **後から始まった方が勝つ**こと。
///
/// 参照実装ではなく、入力に "SLOW" を含むときだけ眠るシェル 1 行を使う。
/// どちらの答えも必ず届くので、順序の問題だけが残る形になっている。
#[test]
fn a_slow_analysis_started_first_never_overwrites_a_newer_one() {
    let (mut app, dir) = make_app_keep(3, Mode::View);
    // 応答は「Atom 0 番だけの Unit 1 つ」。id で、どちらの答えかが分かる。
    let cmd = "input=$(cat); \
        case \"$input\" in *SLOWDOWN*) sleep 1; id=slow ;; *) id=fast ;; esac; \
        printf '{\"version\":1,\"units\":[{\"id\":\"%s\",\"atoms\":[0],\"reading_tier\":\"essential\"}]}' \"$id\"";
    install_semantic_command(&mut app, cmd);

    // 文書 A（遅い方）。
    std::fs::write(dir.path().join("doc.md"), "SLOWDOWN\n\nold text\n").unwrap();
    reload_source(&mut app, false).unwrap();
    assert_eq!(app.semantic_inflight, Some(1));

    // まだ答えが来ないうちに文書 B（速い方）へ入れ替わる。
    std::fs::write(dir.path().join("doc.md"), "new text here\n").unwrap();
    reload_source(&mut app, false).unwrap();
    assert_eq!(app.semantic_inflight, Some(2));

    // 速い方（世代 2）が先に返る。
    pump_until_idle(&mut app, "速い方の解析");
    let winner = |app: &App| {
        app.semantic_doc
            .as_ref()
            .map(|doc| doc.units[0].id.to_string())
    };
    assert_eq!(winner(&app).as_deref(), Some("fast"));

    // 遅い方（世代 1）は 1 秒後に届く。届いても**何も変えない**こと —
    // 2.5 秒ぶん回し続けて、その間ずっと注釈が動かないことを見る。
    let until = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
        app.poll_semantic_analysis();
        assert_eq!(
            winner(&app).as_deref(),
            Some("fast"),
            "遅れて届いた古い世代の答えが、新しい注釈を上書きした"
        );
        assert_eq!(app.semantic_inflight, None);
    }
    assert_eq!(
        app.semantic_doc.as_ref().unwrap().source_digest(),
        Some(crate::semantic::source_digest(&app.source.content).as_str()),
        "注釈は画面に出ている文書のもの"
    );
}

/// 失敗しても落ちず、**同じ文書に対する直前の注釈は保持する**。
#[test]
fn a_failing_command_keeps_the_annotation_it_already_has() {
    let (mut app, _dir) = semantic_markdown_app();
    // まず成功させて注釈を入れる。
    install_semantic_command(&mut app, &reference_semantic_command());
    app.reanalyze_semantics();
    pump_until_idle(&mut app, "最初の解析");
    let good = app.semantic_doc.clone().expect("注釈が入ること");
    let decorations = app.semantic_decorations.clone();
    assert!(!decorations.is_empty());

    // 以降は失敗するコマンドに差し替え、同じ文書で再解析する。
    // （供給源を差し替えてもチャネルは作り直される。）
    install_semantic_command(&mut app, "cat >/dev/null; echo 'rate limited' >&2; exit 1");
    app.semantic_doc = Some(good.clone());
    app.refresh_semantic_decorations();
    app.reanalyze_semantics();
    pump_until_idle(&mut app, "失敗する解析");

    assert_eq!(
        app.semantic_doc.as_ref(),
        Some(&good),
        "同じ文書の注釈は失敗で消えない"
    );
    assert_eq!(app.semantic_decorations, decorations);
    let (message, _, is_error) = app.status.clone().expect("理由が出ること");
    assert!(is_error, "黙って握りつぶさない");
    assert!(message.contains("rate limited"), "{message}");
    assert!(app.running, "落ちない");
}

/// 一方で、**文書が変わったら古い注釈は即座に落とす**。
///
/// 解析が終わるまでのあいだ古い range を新しい文書に当て続けるのは、
/// まさに世代カウンタが防いでいるのと同じ事故である。
#[test]
fn a_new_document_drops_the_old_annotation_before_the_answer_arrives() {
    let (mut app, dir) = semantic_markdown_app();
    install_semantic_command(&mut app, &reference_semantic_command());
    app.reanalyze_semantics();
    pump_until_idle(&mut app, "最初の解析");
    assert!(app.semantic_doc.is_some());
    assert!(!app.semantic_decorations.is_empty());

    // 文書が外から書き換わる → 再解析が始まる。
    std::fs::write(
        dir.path().join("doc.md"),
        "# まったく別の文書\n\nこれは前の文書ではない。\n",
    )
    .unwrap();
    reload_source(&mut app, false).unwrap();

    assert!(
        app.semantic_doc.is_none(),
        "前の文書の注釈は、答えを待つあいだも当てない"
    );
    assert!(app.semantic_decorations.is_empty());

    // 新しい答えが来れば、新しい文書に対する注釈になる。
    pump_until_idle(&mut app, "2 回目の解析");
    let document = app.semantic_doc.as_ref().unwrap();
    assert_eq!(document.atoms, semantic_reading::atomize(&app.source.content));
}

/// 解析中に Budget キーを押しても「使えません」ではなく「解析中」と言う
/// （数秒後には使えるので、断り方が違う）。
#[test]
fn the_budget_keys_say_analyzing_while_an_answer_is_on_its_way() {
    let mut app = make_app(3, Mode::View);
    install_semantic_command(&mut app, "sleep 5");
    app.reanalyze_semantics();
    assert!(app.semantic_doc.is_none());

    on_view_key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE, None);
    let (message, _, is_error) = app.status.clone().expect("何か言うこと");
    assert!(message.contains("analyzing"), "{message}");
    assert!(!is_error, "エラーではない — 待てば使える");
    assert_eq!(app.reading_budget, 100, "まだ動かない");
}
/// The Phase 2 milestone, now on the SOURCE screen: `--semantic` through
/// `Config` → `App` → `Provider` → `policy::decorate` → `build_rows` →
/// cells, with **one source line splitting mid-line into two styles** and
/// the split moving with the READ budget — driven by the real key
/// handler, because the budget keys are bound in source mode now.
///
/// The rendered-view twin is
/// `the_reading_budget_splits_one_terminal_line_into_two_styles`; both
/// read the same line of `examples/semantic/demo.md`, which is the point:
/// the two modes decorate the same source bytes.
#[test]
fn the_reading_budget_splits_one_source_line_into_two_styles() {
    use crate::decoration::DecorationStyles;

    let path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.md"
    ));
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/semantic/demo.json"
    ));
    let config = Config {
        files: vec![path.clone()],
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme: None,
        ime: ImeMode::Off,
        light: None,
        callback: None,
        esc_quit: EscQuit::Auto,
        cursor_anchor: true,
        fx: false,
        semantic: Some(fixture),
        semantic_cmd: None,
        decoration_blend: Default::default(),
        decorations: Vec::new(),
    };
    let source = Source::load(path).unwrap();
    let highlight = Highlighter::new(config.theme.as_deref(), false);
    let view = ViewState::render(&source, crate::view_render_width(80), &highlight, Default::default());
    let styles = DecorationStyles::from_theme(&highlight, Default::default());
    let mark_bg = styles.mark_style().bg;
    let mut app = App::new(config, source, highlight, view, false);
    app.set_semantic_source(crate::semantic::source_from_config(&app.config).unwrap());
    app.reanalyze_semantics();
    assert!(app.semantic_doc.is_some(), "the fixture matches demo.md");
    // run() tokenizes every file up front; source mode reads the result.
    app.spans = app
        .highlight
        .highlight_with(&app.source.content, syntax_for(&app.files[0]));
    app.mode = Mode::Source;
    app.gutter_cols = 1 + app.source.gutter_width as u16 + 1;
    // The cursor stays on line 1, so the cursor band never reaches the
    // line under test (a band covers the decoration by design).
    assert_eq!(app.cursor, 0);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    // 「採用する方式は差分配信である。詳細は付録にまとめた。」は 1 本の
    // source 行で、前半 u3（ESSENTIAL）と後半 u4（DETAIL）に分かれている。
    // source view は生の Markdown をそのまま出すので、この行は rendered
    // view と同じ文字列で画面に出る。
    let halves = |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let cell = |y: usize, x: usize| &buf.content[y * w + x];
        let (row, right) = (0..buf.area.height as usize)
            .find_map(|y| (0..w).find(|&x| cell(y, x).symbol() == "詳").map(|x| (y, x)))
            .expect("the 結論 paragraph is on screen");
        let left = (0..right)
            .find(|&x| cell(row, x).symbol() == "採")
            .expect("both halves are on the SAME terminal row");
        (cell(row, left).style(), cell(row, right).style())
    };

    // READ 100 %: the ESSENTIAL half is MARKED, the DETAIL half NORMAL.
    assert_eq!(app.reading_budget, 100);
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_eq!(essential.bg, mark_bg, "ESSENTIAL は source view でも MARKED");
    assert_ne!(detail.bg, mark_bg, "DETAIL は NORMAL");
    assert_ne!(essential, detail, "source view でも行の途中で切り替わる");
    let bright = detail.fg;

    // `<` を 7 回 — **source モードのキーハンドラを通す**。この層が view
    // 専用だったあいだ、これらのキーは source には束ねられていなかった。
    for _ in 0..7 {
        crate::on_source_key(&mut app, KeyCode::Char('<'), KeyModifiers::NONE, None);
    }
    assert_eq!(app.reading_budget, 30, "source モードで budget キーが効く");
    let (essential, detail) = halves(&mut app, &mut terminal);
    assert_eq!(essential.bg, mark_bg, "MARKED のまま");
    assert_eq!(detail.fg, Some(styles.dim_fg(bright)), "DETAIL は DIM に落ちる");
    assert_ne!(detail.fg, bright);
    assert_eq!(essential.fg, bright, "MARKED 側の前景は動かない");
    for style in [essential, detail] {
        assert!(!style.add_modifier.contains(ratatui::style::Modifier::DIM));
    }

    // `>` で戻せば元どおり（キーは両方向に効く）。
    for _ in 0..7 {
        crate::on_source_key(&mut app, KeyCode::Char('>'), KeyModifiers::NONE, None);
    }
    assert_eq!(app.reading_budget, 100);
    let (_, detail) = halves(&mut app, &mut terminal);
    assert_eq!(detail.fg, bright, "100 % に戻れば DIM も戻る");
}

/// `--semantic` のないセッションでは、source モードの budget キーは
/// 「その層が無かったとき」と同じ動きをする（腕が `semantic_enabled()`
/// ガードで落ち、`_ => {}` に吸われる）。view 側と同じ約束。
#[test]
fn budget_keys_do_nothing_in_source_mode_without_semantic() {
    let mut app = make_app(5, Mode::Source);
    assert!(!app.semantic_enabled());
    for key in ['-', '+', '=', '<', '>'] {
        crate::on_source_key(&mut app, KeyCode::Char(key), KeyModifiers::NONE, None);
        assert!(app.status.is_none(), "{key} は toast すら出さない");
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.cursor, 0);
    }
}
