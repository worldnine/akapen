//! The big end-to-end state tests: key handling across every mode,
//! overlays, prompts/toasts, title/footer chrome, reloads, and the
//! composer (moved wholesale from main.rs).

/// State-machine tests: mode transitions, selection, composer, deletion,
/// and quit confirmation. These run the real key handlers against a built
/// App (no TTY), so the event-loop logic is exercised the same way a user
/// would — only the rendering is bypassed.
use crate::*;

use std::path::{Path, PathBuf};

use crate::comment::Selection;
    use crate::config::{Config, EscQuit};
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
        let rows = help_rows(false, false, false);
        assert!(
            rows.iter().any(|(l, k)| *l == "quit" && *k == "q quit · Esc cancel"),
            "default help advertises Esc as cancel"
        );
        let rows = help_rows(true, false, false);
        assert!(
            rows.iter().any(|(l, k)| *l == "quit" && *k == "Esc/q quit"),
            "esc-quit help advertises Esc/q as quit"
        );
    }

    #[test]
    fn help_advertises_review_navigation() {
        let rows = help_rows(false, false, true);
        assert!(rows
            .iter()
            .any(|(l, k)| *l == "compare" && k.contains("a acknowledge")));
        let rows = help_rows(false, true, true);
        assert!(!rows.iter().any(|(l, _)| *l == "compare"));
    }

    #[test]
    fn reply_mode_help_hides_file_navigation_and_edit() {
        // Reply mode: messages replace files — no file switching, no
        // edit, and reloads are automatic.
        let rows = help_rows(false, true, false);
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
        let rows = help_rows(false, false, false);
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
            content.contains("最終行のコメント▏"),
            "the typed comment text and cursor glyph are visible"
        );
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
            fx: true,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
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
            fx: true,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
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
            fx: true,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
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
        assert!(content.contains('▏'), "the cursor glyph is in the bar");
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
            content.contains("最終行▏"),
            "the typed comment text and cursor glyph are visible"
        );
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
            fx: true,
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
        let view = render_view_with_cards(&source, width, &highlight, &comments);
        let card_h = view.card_rows.iter().filter(|&&b| b).count();
        assert!(card_h >= 3, "title + body + rule rows");
        let base = ViewState::render(&source, width, &highlight);
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
            fx: true,
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
        let mut view = render_view_with_cards(&source, width, &highlight, &comments);
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
        let base = ViewState::render(&source, 60, &highlight);
        let view = render_view_with_cards(&source, 60, &highlight, &comments);
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
        on_view_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE, None);
        assert!(app.status.is_some(), "y flashes 'no comments yet'");
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
        let viewport = app.source_viewport_rows() as usize;
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
        assert_cursor_visible(&app, app.source_viewport_rows() as usize);
        // Cursor far below the viewport: one k lands it inside — this
        // direction is the one that used to fail (height-dependent branch).
        app.cursor = 90;
        app.offset = 10;
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 89);
        assert_cursor_visible(&app, app.source_viewport_rows() as usize);
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
            fx: true,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(None, false);
        let view = ViewState::render(&source, 75, &highlight);
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
    fn the_generation_label_lives_in_the_title_not_the_footer() {
        // Browsing history must not duplicate the generation label top
        // and bottom: the title's state slot carries it (clipped to a
        // budget), the footer keeps only the short navigation
        // affordance — the old footer label was the same text twice and
        // truncated the footer on narrow screens.
        let mut app = make_app(3, Mode::Source);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Local,
        });
        app.histories[0].position = 1;

        // The footer names the direction, never the generation.
        let hints = footer_hints(&app);
        assert!(hints.contains("← older · newer →"), "nav affordance stays: {hints}");
        assert!(
            !hints.contains("LOCAL") && !hints.contains("local snapshot"),
            "no generation label in the footer: {hints}"
        );
        // The title's state slot carries the label, within its budget.
        let m = title_metrics(&app, 80);
        assert!(m.change.contains("LOCAL · 1/2"), "title shows the label: {}", m.change);
        assert!(
            m.change.contains("base 2/2"),
            "the baseline context survives the clip: {}",
            m.change
        );
        assert!(
            m.path.contains("doc"),
            "the path survives beside the clipped label: {}",
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
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "oldest".into(),
            content: "oldest first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Git,
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
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "oldest".into(),
            content: "oldest first line\nline2\nline3\n".into(),
            source: history::RevisionSource::Git,
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
        // edge, the viewing point is marked.
        let mut app = make_app(5, Mode::View);
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:old".into()),
            short_id: "old".into(),
            summary: "local snapshot".into(),
            content: "old first line\nline2\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Local,
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("cafe".into()),
            short_id: "cafe".into(),
            summary: "oldest".into(),
            content: "oldest first line\nline2\nline3\nline4\nline5\n".into(),
            source: history::RevisionSource::Git,
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
        assert!(axis.contains('◆'), "viewing marker on the axis: {axis}");
        assert!(axis.contains('◼'), "commit marker on the axis: {axis}");
        assert!(
            axis.find('◆').unwrap_or(0) < axis.rfind('●').unwrap_or(0),
            "viewing sits left of NOW: {axis}"
        );
        let words = &frame[23];
        assert!(words.contains("viewing"), "words row: {words}");
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
        });
        app.histories[0].revisions.push(history::Revision {
            id: Some("local:add".into()),
            short_id: "add".into(),
            summary: "added a line".into(),
            content: "line1\nline3\nline4\nline5\nline6\n".into(),
            source: history::RevisionSource::Local,
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
    fn prompt_priority_quit_over_edit_over_reload_over_change() {
        let mut app = make_app(10, Mode::Source);
        app.confirm_quit = true;
        app.confirm_edit = true;
        app.confirm_reload = true;
        app.file_changed = true;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — q to quit, Esc to cancel")
        );
        app.confirm_quit = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — e again to edit & clear, Esc to cancel")
        );
        app.confirm_edit = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("unsent comments — r again to reload & clear, Esc to cancel")
        );
        app.confirm_reload = false;
        assert_eq!(
            prompt_message(&app).as_deref(),
            Some("file changed — r reload · i ignore")
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
    fn y_copies_without_sending() {
        // `y` is copy-only: even with --send-cmd configured it must not
        // deliver, and the comments stay.
        let mut app = make_app(5, Mode::Source);
        app.config.send_cmd = Some("cat > /dev/null".to_string());
        add_comment(&mut app, 1, 1, "c");
        on_source_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE, None);
        assert_eq!(app.comments.len(), 1, "y keeps the comments");
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
                },
                crate::history::Revision {
                    id: Some("abc".into()),
                    short_id: "abc".into(),
                    summary: "old".into(),
                    content: "# Old\n".into(),
                    source: crate::history::RevisionSource::Git,
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
        select_history(&mut app, 1);
        assert!(
            matches!(app.status.as_ref(), Some((message, _, false)) if message.starts_with("COMMIT")),
            "the edge label stays informational until that revision is rendered"
        );
        render_history_when_settled(&mut app);
        assert_eq!(app.source.content, "line1\n", "renderer waits for the debounce");
        app.history_render_due = Some(std::time::Instant::now());
        render_history_when_settled(&mut app);
        assert_eq!(app.source.content, "# Old\n");
        assert!(app.history_render_due.is_none());
        assert!(app.history_frame_flash_pending);
        assert!(
            app.history_frame_flash_until.is_none(),
            "the pulse does not overlap the first paint of the rendered document"
        );
        begin_history_frame_flash_after_draw(&mut app);
        assert!(!app.history_frame_flash_pending);
        assert!(app.history_frame_flash_until.is_some());
        select_history(&mut app, 1);
        assert!(
            matches!(app.status.as_ref(), Some((message, _, true)) if message == "oldest document version"),
            "the edge becomes an error only after its document is visible"
        );
        select_history(&mut app, -1);
        select_history(&mut app, -1);
        assert!(
            matches!(app.status.as_ref(), Some((message, _, false)) if message.contains("NOW")),
            "the present label also stays informational until it is rendered"
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
                },
                crate::history::Revision {
                    id: Some("abc".into()),
                    short_id: "abc".into(),
                    summary: "old".into(),
                    content: "# Old\n\nsource history\n".into(),
                    source: crate::history::RevisionSource::Git,
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
        app.history_frame_flash_until = Some(std::time::Instant::now() + Duration::from_secs(1));
        app.gutter_cols = 4;
        app.ensure_row_cache(60);
        app.refresh_line_rows();

        let (text, _) = build_rows(&app, 10, 60);
        assert_eq!(
            text.lines[2].spans[1].style.fg,
            Some(app.ui_history_frame_flash),
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
        // No `▀` position mark remains in source mode.
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
        // …while the net-deletion position set (view mode's `▀`, the
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
