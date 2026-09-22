    use super::*;
    use crate::config::{Config, EscQuit};
    use crate::decoration::{Decoration, DecorationKind, DecorationStyles};
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::view::ViewState;

    /// An App in source mode over `content`, carrying `decorations`.
    /// Returns the app, the resolved styles, and the tempdir (the file
    /// must outlive the load on the reload paths).
    fn app_with(
        content: &str,
        decorations: Vec<Decoration>,
    ) -> (App, DecorationStyles, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, content).unwrap();
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
            fx: false,
            semantic: None,
            semantic_cmd: None,
                marks_questions: None,
            decoration_blend: Default::default(),
            decorations,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let styles = DecorationStyles::from_theme(&highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        (app, styles, dir)
    }

    /// The CONTENT spans of each emitted row: the gutter is always two
    /// spans (mark + number, or mark + indent on a continuation row), so
    /// what is left is the text the decoration layer works on.
    ///
    /// A row under the cursor/selection/changed band also ends with a
    /// fill span running to the pane's right edge, and the band's
    /// background covers every span on that row — so every test below
    /// parks the cursor on a line it is not inspecting. That is not a
    /// workaround but the documented priority order: the bands are
    /// applied AFTER the decoration and win, exactly as in the rendered
    /// view.
    fn content_spans(app: &App, width: u16) -> Vec<Vec<(String, Style)>> {
        app_rows(app, width)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .skip(2)
                    .map(|s| (s.content.to_string(), s.style))
                    .collect()
            })
            .collect()
    }

    fn app_rows(app: &App, width: u16) -> Vec<Line<'static>> {
        build_rows(app, 40, width).0.lines
    }

    /// The row holding `needle`, as (text, style) pairs.
    fn row_with(app: &App, width: u16, needle: &str) -> Vec<(String, Style)> {
        content_spans(app, width)
            .into_iter()
            .find(|row| {
                row.iter().map(|(t, _)| t.as_str()).collect::<String>().contains(needle)
            })
            .unwrap_or_else(|| panic!("no row contains {needle:?}"))
    }

    fn prepare(app: &mut App, width: u16) {
        app.ensure_row_cache(width);
        app.rebuild_base_rows();
        app.refresh_line_rows();
    }

    /// **The milestone, on the source screen**: one source line shows
    /// MARKED, NORMAL and DIM at three different byte offsets inside it —
    /// the same thing `decoration::marked_normal_dim_on_one_rendered_line`
    /// pins for the rendered view. The cursor is parked on another line
    /// so no band interferes.
    #[test]
    fn marked_normal_dim_on_one_source_row() {
        let content = "first line\nMARKnormalDIMx\nthird\n";
        let start = "first line\n".len();
        let (mut app, styles, _d) = app_with(
            content,
            vec![
                Decoration {
                    range: start..start + 4, // MARK
                    kind: DecorationKind::SemanticMark,
                },
                Decoration {
                    range: start + 10..start + 13, // DIM
                    kind: DecorationKind::Dim,
                },
            ],
        );
        app.cursor = 0;
        prepare(&mut app, 40);
        let row = row_with(&app, 40, "normal");
        let texts: Vec<&str> = row.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec!["MARK", "normal", "DIM", "x"],
            "the span was cut at both decoration edges"
        );
        let mark_bg = styles.mark_style().bg;
        let normal_fg = row[1].1.fg;
        assert_eq!(row[0].1.bg, mark_bg, "MARKED: a background");
        assert_ne!(row[1].1.bg, mark_bg, "NORMAL: untouched");
        assert_ne!(row[2].1.bg, mark_bg, "DIM writes no background");
        assert_eq!(row[2].1.fg, Some(styles.dim_fg(normal_fg)), "DIM: a foreground");
        assert_ne!(row[2].1.fg, normal_fg);
        // Three DIFFERENT styles on one row, which is the whole point.
        assert_ne!(row[0].1, row[1].1);
        assert_ne!(row[1].1, row[2].1);
        assert_ne!(row[0].1, row[2].1);
        // The trailing "x" is outside every decoration: it reads exactly
        // like NORMAL, so the layer stopped where it was told to.
        assert_eq!(row[3].1, row[1].1);
    }

    /// Syntax highlighting survives the decoration: MARKED only adds a
    /// background, so every span of the heading keeps the exact color
    /// syntect gave it. Checked span by span against the SAME document
    /// drawn without decorations, so no assumption about how the grammar
    /// splits `# 見出し` is baked in.
    #[test]
    fn syntax_highlighting_survives_the_decoration() {
        let content = "# 見出し\n\nplain text\n";
        let (mut bare, styles, _d) = app_with(content, Vec::new());
        bare.cursor = 2;
        prepare(&mut bare, 40);
        let before = row_with(&bare, 40, "見出し");
        // The assertion below is only worth making if syntect actually
        // colored the heading differently from body text.
        let plain_fg = row_with(&bare, 40, "plain").last().unwrap().1.fg;
        assert_ne!(before.last().unwrap().1.fg, plain_fg, "syntect colors the heading");

        let (mut marked, _s, _d2) = app_with(
            content,
            vec![Decoration {
                range: 0.."# 見出し".len(),
                kind: DecorationKind::SemanticMark,
            }],
        );
        marked.cursor = 2;
        prepare(&mut marked, 40);
        let after = row_with(&marked, 40, "見出し");
        assert_eq!(
            after.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            before.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            "a decoration covering whole spans splits nothing"
        );
        for ((text, got), (_, want)) in after.iter().zip(&before) {
            assert_eq!(got.fg, want.fg, "{text:?} keeps its syntax color");
            assert_eq!(
                got.add_modifier, want.add_modifier,
                "{text:?} keeps its modifiers"
            );
            assert_eq!(got.bg, styles.mark_style().bg, "{text:?} gained only the mark");
        }
    }

    /// A decoration that starts on one display row and ends on another:
    /// the wrap carried the attribution across the boundaries, so each
    /// row is decorated for its own bytes and no others.
    #[test]
    fn a_decoration_crosses_a_wrap_boundary() {
        // Line 0 is the spare the cursor parks on (a banded row's
        // background covers the mark — see `content_spans`).
        let content = "spare\naaaaabbbbbccccc\n";
        let base = "spare\n".len();
        let (mut app, styles, _d) = app_with(
            content,
            vec![Decoration {
                range: base + 3..base + 12,
                kind: DecorationKind::SemanticMark,
            }],
        );
        app.cursor = 0;
        prepare(&mut app, 5);
        let rows = content_spans(&app, 5);
        // Row 0 is the spare line; the wrapped line follows.
        let wrapped: Vec<_> = rows[1..].to_vec();
        assert_eq!(wrapped.len(), 3, "15 columns at width 5");
        let marked: String = wrapped
            .iter()
            .flatten()
            .filter(|(_, st)| st.bg == styles.mark_style().bg)
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(
            marked,
            &content[base + 3..base + 12],
            "exactly the decorated bytes, spread over three rows"
        );
        let all: String = wrapped.iter().flatten().map(|(t, _)| t.as_str()).collect();
        assert_eq!(all, "aaaaabbbbbccccc", "the wrap itself is unchanged");
    }

    /// Japanese, emoji and full-width characters: a decoration is cut at
    /// BYTE offsets while the wrap measures COLUMNS, and confusing the
    /// two is the trap `docs/design/range-attribution-plan.md` names by name.
    /// The marked text is compared against the source slice, so an
    /// off-by-one in either space fails here.
    #[test]
    fn a_decoration_lands_on_multibyte_characters() {
        let content = "spare\n日本語と🎉とＡＢＣ\n";
        let base = "spare\n".len();
        let start = base + "日本語".len(); // 9 bytes, 6 columns
        let end = base + "日本語と🎉".len();
        let (mut app, styles, _d) = app_with(
            content,
            vec![Decoration {
                range: start..end,
                kind: DecorationKind::SemanticMark,
            }],
        );
        app.cursor = 0;
        for width in [4u16, 7, 40] {
            prepare(&mut app, width);
            let marked: String = content_spans(&app, width)[1..]
                .iter()
                .flatten()
                .filter(|(_, st)| st.bg == styles.mark_style().bg)
                .map(|(t, _)| t.as_str())
                .collect();
            assert_eq!(marked, &content[start..end], "at width {width}");
        }
    }

    /// A decoration whose edge falls INSIDE a tab-expanded fragment
    /// decorates nothing there: the expansion rewrote the text, so the
    /// fragment is a superset and `decorate_row`'s all-or-nothing rule
    /// applies. A decoration that COVERS the fragment still lands.
    /// (`highlight::a_tab_expanded_fragment_keeps_its_range_and_loses_exactness`
    /// pins the demotion itself; this is what it looks like on screen.)
    #[test]
    fn a_tab_fragment_is_all_or_nothing() {
        let content = "spare\n\tfoo bar\n";
        let base = "spare\n".len();
        let covering = Decoration {
            range: base..base + "\tfoo bar".len(),
            kind: DecorationKind::SemanticMark,
        };
        let partial = Decoration {
            // Ends inside the fragment, three bytes in ("\tfo").
            range: base..base + 3,
            kind: DecorationKind::SemanticMark,
        };
        for (deco, expect_marked) in [(covering, true), (partial, false)] {
            let (mut app, styles, _d) = app_with(content, vec![deco]);
            app.cursor = 0;
            prepare(&mut app, 40);
            let row = row_with(&app, 40, "foo bar");
            let any_marked = row.iter().any(|(_, st)| st.bg == styles.mark_style().bg);
            assert_eq!(any_marked, expect_marked, "row: {row:?}");
        }
    }

    /// With no decorations the source screen is what it always was: the
    /// rows, their spans and their styles are unchanged. (The mechanical
    /// version of this — every testdata file × three widths × cursors ×
    /// offsets, dumped and `cmp`-ed against the pre-change HEAD — is in
    /// docs/handoff-archive.md.)
    #[test]
    fn no_decorations_leaves_the_source_rows_alone() {
        let content = "# 見出し\n\n本文と `code` と **強調**\n\n- 項目1\n- 項目2\n\tタブ\n";
        let (mut plain, _s, _d) = app_with(content, Vec::new());
        // The same document, with a decoration list that sanitize drops
        // whole (past EOF): the layer runs and must still change nothing.
        let (mut dropped, _s2, _d2) = app_with(
            content,
            vec![Decoration {
                range: content.len() + 10..content.len() + 20,
                kind: DecorationKind::Dim,
            }],
        );
        for width in [7u16, 20, 40] {
            for cursor in [0usize, 2, 4] {
                plain.cursor = cursor;
                dropped.cursor = cursor;
                prepare(&mut plain, width);
                prepare(&mut dropped, width);
                assert_eq!(
                    app_rows(&plain, width),
                    app_rows(&dropped, width),
                    "width {width}, cursor {cursor}"
                );
            }
        }
    }

    /// The cursor band drops `Dim` for its row, exactly as the rendered
    /// view does: `Dim` writes a foreground and a background band cannot
    /// undo it, so a line the cursor is on would otherwise read as
    /// "I moved onto it and the text went pale". The MARK (a background)
    /// is NOT dropped — the band simply paints over it.
    #[test]
    fn the_cursor_band_undims_its_row() {
        let content = "alpha\nbravo\n";
        let start = "alpha\n".len();
        let (mut app, styles, _d) = app_with(
            content,
            vec![Decoration {
                range: start..start + 5,
                kind: DecorationKind::Dim,
            }],
        );
        // Cursor away: the row is dim.
        app.cursor = 0;
        prepare(&mut app, 40);
        let away = row_with(&app, 40, "bravo");
        let dim_fg = away.last().unwrap().1.fg;
        // Cursor on it: the dim is gone and the band's background is on.
        app.cursor = 1;
        prepare(&mut app, 40);
        let under = row_with(&app, 40, "bravo");
        let lit_fg = under.last().unwrap().1.fg;
        assert_ne!(dim_fg, lit_fg, "the band un-dims the text");
        assert_eq!(under.last().unwrap().1.bg, Some(app.ui_selected_bg));
        assert_eq!(
            dim_fg,
            Some(styles.dim_fg(lit_fg)),
            "and the un-dimmed color is the one the dim was derived from"
        );
    }

    /// The same band rule for a SELECTED row, and the mark's background
    /// survives both bands (it is painted under them, not instead).
    #[test]
    fn a_selected_row_undims_but_keeps_being_selectable() {
        let content = "alpha\nbravo\n";
        let start = "alpha\n".len();
        let (mut app, _s, _d) = app_with(
            content,
            vec![Decoration {
                range: start..start + 5,
                kind: DecorationKind::Dim,
            }],
        );
        app.cursor = 0;
        prepare(&mut app, 40);
        let away_fg = row_with(&app, 40, "bravo").last().unwrap().1.fg;
        app.selection = Some(crate::comment::Selection { anchor: 1, cursor: 1 });
        prepare(&mut app, 40);
        let selected = row_with(&app, 40, "bravo");
        assert_ne!(selected.last().unwrap().1.fg, away_fg, "selection un-dims too");
        assert_eq!(selected.last().unwrap().1.bg, Some(app.ui_selected_bg));
    }
