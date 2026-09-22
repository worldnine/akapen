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
            semantic: None,
            semantic_cmd: None,
                marks_questions: None,
                review_rules: None,
                review_json: false,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
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
            semantic: None,
            semantic_cmd: None,
                marks_questions: None,
                review_rules: None,
                review_json: false,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 57, &highlight, Default::default());
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
            semantic: None,
            semantic_cmd: None,
                marks_questions: None,
                review_rules: None,
                review_json: false,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path.clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 57, &highlight, Default::default());
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
            semantic: None,
            semantic_cmd: None,
                marks_questions: None,
                review_rules: None,
                review_json: false,
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
            semantic: None,
            semantic_cmd: None,
                marks_questions: None,
                review_rules: None,
                review_json: false,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
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
