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
