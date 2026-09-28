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
            theme: crate::config::ThemePair::both("base16-ocean.dark"),
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
                lint_cmd: None,
                undercurl: Default::default(),
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path.into()).unwrap();
        let highlight = Highlighter::new(config.theme.for_background(false), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
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
