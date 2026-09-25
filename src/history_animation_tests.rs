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
        let mut view = ViewState::render(&source, 60, &highlight, Default::default());
        let original_rows = view.rows.len();
        let original_start = view.source_starts[0];
        let rects = insert_history_ghosts(
            &mut view,
            &[history::DeletedBlock {
                anchor: 0,
                content: "## Gone\n\nold text".into(),
                ..Default::default()
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
        // Ghost rows are synthesized, but they still have to carry an
        // attribution entry: `row_attrs` runs parallel to `rows`, and the
        // range decoration would otherwise land a row too high below the
        // ghost (same failure mode as a comment card).
        assert_eq!(view.row_attrs.len(), view.rows.len());
        for (r, row) in view.rows.iter().enumerate() {
            assert_eq!(view.row_attrs[r].len(), row.len(), "row {r}");
        }
    }

    #[test]
    fn changed_blocks_become_staggered_scatter_effects() {
        // Contiguous changed lines merge into one scatter block; isolated
        // lines stand alone. The rects come from the view's source-line
        // mapping, so wrapped/merged rows are covered as one block.
        let highlight = Highlighter::new(None, false);
        let source = Source::from_content("doc.md".into(), "a\n\nb\n\nc\n\nd\n\ne\n\nf\n".into());
        let view = ViewState::render(&source, 60, &highlight, Default::default());
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
            Default::default(),
        );
        let new_view = ViewState::render(
            &Source::from_content("doc.md".into(), format!("{new}\n")),
            20,
            &highlight,
            Default::default(),
        );
        assert_eq!(new_view.rows.len(), 2, "the new block wraps to two rows");
        let row0 = new_view.source_starts[0];
        let mask = appear_mask(&new_view, row0, new_view.rows.len(), &old_view.rows);
        assert_eq!(mask[0], vec![(6, 12)], "only the inserted run masks");
        assert!(mask[1].is_empty(), "wrapped-around text stays put");
    }
