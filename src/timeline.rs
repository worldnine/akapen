//! The browsing timeline bar: a horizontal revision scrubber drawn over
//! the bottom rows while the reader travels through history.
//!
//! Everything here is pure layout math — the bar's drawing lives in
//! `main.rs` (next to the other drawers) and consumes this module's
//! [`TimelineLayout`]. The bar is bottom-anchored and always two rows:
//! the state words (`HERE`/`NOW`) replace the footer hints, the axis
//! replaces the view frame's bottom border (so view mode loses no
//! content rows at all).
//! The current revision is `◆`, the review baseline `▮`, LOCAL
//! snapshots `●` and COMMITs `◼`, with NOW always at the right edge.
//! The axis is dim left of the review baseline — reviewed history — and
//! normal from the baseline to NOW — the unreviewed stretch.

use std::collections::HashMap;
use std::time::Instant;

use crate::app::{App, Mode};
use crate::history::{DocumentHistory, RevisionSource};

/// The timeline bar is always two rows: the state words own the footer
/// row, the axis owns the row above it (the view frame's bottom border
/// in view mode, one content row in source mode).
pub(crate) const TIMELINE_BAR_ROWS: u16 = 2;

/// The marker kind of a timeline point: provenance plus the NOW anchor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PointKind {
    Local,
    Commit,
    Now,
}

/// One rendered point on the axis: its column, marker kind, and the two
/// state flags (current revision, review baseline).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TimelinePoint {
    pub(crate) col: usize,
    pub(crate) kind: PointKind,
    pub(crate) current: bool,
    pub(crate) baseline: bool,
}

/// The full layout of the browsing timeline bar, ready to draw.
#[derive(Debug)]
pub(crate) struct TimelineLayout {
    /// Axis points, sorted by column.
    pub(crate) points: Vec<TimelinePoint>,
    /// (start column, text) of the state words: `HERE` at the current
    /// point and `NOW` at the right edge.
    pub(crate) words: Vec<(usize, String)>,
    /// The column of the review baseline: the axis left of it is drawn
    /// dim — the reviewed history behind you — and the stretch from
    /// the baseline to NOW (the unreviewed accumulation) stands out.
    /// `None` before the first acknowledgement: everything is
    /// unreviewed, the whole axis reads normally.
    pub(crate) baseline_col: Option<usize>,
}

/// Whether the timeline bar is on screen right now: browsing history
/// with more than one point, no overlay open (overlays own the screen),
/// the composer closed, and a terminal wide enough for the axis row.
pub(crate) fn timeline_visible(app: &App) -> bool {
    app.overlay.is_none()
        && app.mode != Mode::Input
        && app
            .history()
            .is_some_and(|history| history.position > 0 && history.revisions.len() > 1)
        && terminal_width() >= 60
}

/// The bar is on screen, or lingers while its slide-out plays (the
/// position has already returned to NOW). Narrow terminals never get a
/// bar at all, so the linger is skipped there too.
pub(crate) fn timeline_active(app: &App) -> bool {
    if app.overlay.is_some() || app.mode == Mode::Input || terminal_width() < 60 {
        return false;
    }
    if timeline_visible(app) {
        return true;
    }
    app.timeline_exit_until
        .is_some_and(|until| Instant::now() < until)
}

fn terminal_width() -> u16 {
    ratatui::crossterm::terminal::size()
        .map(|s| s.0)
        .unwrap_or(80)
}

/// Lay out the whole bar: map revisions onto axis columns, resolve
/// column collisions when revisions crowd the width, and place the
/// state words. `None` when there is nothing to draw (fewer than two
/// revisions or a degenerate width).
pub(crate) fn layout_timeline(
    history: &DocumentHistory,
    width: usize,
) -> Option<TimelineLayout> {
    let n = history.revisions.len();
    if n < 2 || width < 2 {
        return None;
    }
    // Chronological position j: 0 = oldest, n-1 = NOW (revision index i
    // maps to j = n-1-i). Equal spacing across the width, NOW pinned to
    // the right edge.
    let col_of = |j: usize| -> usize {
        ((j as f64 * (width - 1) as f64 / (n - 1) as f64).round()) as usize
    };
    let baseline = history.baseline_position();
    // When revisions crowd the width several map to one column. The
    // current revision, then the baseline, then NOW, then the newest
    // revision win their column.
    let priority = |i: usize| -> u8 {
        if history.position == i {
            3
        } else if baseline == Some(i) {
            2
        } else if i == 0 {
            1 // NOW
        } else {
            0
        }
    };
    let mut best: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let col = col_of(n - 1 - i);
        let wins = best
            .get(&col)
            .is_none_or(|&existing| priority(i) > priority(existing));
        if wins {
            best.insert(col, i);
        }
    }
    let mut points: Vec<TimelinePoint> = best
        .into_iter()
        .map(|(col, i)| {
            let rev = &history.revisions[i];
            TimelinePoint {
                col,
                kind: match rev.source {
                    RevisionSource::Now => PointKind::Now,
                    RevisionSource::Local => PointKind::Local,
                    RevisionSource::Git => PointKind::Commit,
                },
                current: history.position == i,
                baseline: baseline == Some(i),
            }
        })
        .collect();
    points.sort_by_key(|p| p.col);

    // State words: `HERE` at the current point (shifted left when it
    // would hit the NOW anchor) and `NOW` at the right edge — the "you
    // are here" pair. The baseline keeps no word of its own: the yellow
    // `▎` marker plus the axis dimming to its left already tell that
    // story. Words are ASCII, so char width == byte length.
    let mut words: Vec<(usize, String)> = Vec::new();
    if let Some(p) = points.iter().find(|p| p.current)
        && p.kind != PointKind::Now
    {
        let start = p
            .col
            .saturating_sub(2)
            .min(width.saturating_sub(8)); // "HERE" ends before NOW
        words.push((start, "HERE".to_string()));
    }
    words.push((width.saturating_sub(3), "NOW".to_string()));
    words.sort_by_key(|(start, _)| *start);

    Some(TimelineLayout {
        baseline_col: baseline.map(|i| col_of(n - 1 - i)),
        points,
        words,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{DocumentHistory, Revision};

    fn revision(name: &str, source: RevisionSource) -> Revision {
        Revision {
            id: None,
            short_id: name.into(),
            summary: name.into(),
            content: name.into(),
            source,
            timestamp_ms: None,
        }
    }

    fn history(revs: Vec<Revision>) -> DocumentHistory {
        DocumentHistory {
            revisions: revs,
            position: 0,
            rendered_position: 0,
            reviewed_id: None,
            reviewed_content: None,
        }
    }

    #[test]
    fn oldest_is_left_now_is_right() {
        let h = history(vec![
            revision("now", RevisionSource::Now),
            revision("mid", RevisionSource::Local),
            revision("old", RevisionSource::Git),
        ]);
        let layout = layout_timeline(&h, 20).unwrap();
        assert_eq!(layout.points.len(), 3);
        assert_eq!(layout.points[0].col, 0);
        assert_eq!(layout.points[0].kind, PointKind::Commit);
        assert_eq!(layout.points[2].col, 19);
        assert_eq!(layout.points[2].kind, PointKind::Now);
        // No acknowledged baseline: the whole axis reads normally.
        assert_eq!(layout.baseline_col, None);
    }

    #[test]
    fn single_revision_has_no_timeline() {
        let h = history(vec![revision("now", RevisionSource::Now)]);
        assert!(layout_timeline(&h, 40).is_none());
    }

    #[test]
    fn crowding_keeps_current_baseline_and_now() {
        let mut h = history(vec![
            revision("now", RevisionSource::Now),
            revision("r1", RevisionSource::Local),
            revision("r2", RevisionSource::Local),
            revision("r3", RevisionSource::Git),
            revision("r4", RevisionSource::Git),
        ]);
        h.position = 2;
        h.reviewed_id = Some("r2".into());
        h.reviewed_content = Some("r2".into());
        // 5 revisions into 10 columns already collide (round(4j/9) hits
        // duplicates for j=2..4? no — 5 points, 10 cols, distinct);
        // use a genuinely crowded width instead.
        let layout = layout_timeline(&h, 4).unwrap();
        assert!(layout.points.len() < 5, "columns dedupe");
        assert!(layout.points.iter().any(|p| p.current));
        assert!(layout.points.iter().any(|p| p.baseline));
        assert!(layout.points.iter().any(|p| p.kind == PointKind::Now));
        // The axis dims off at the baseline column (revision r2, chron
        // j = 2): reviewed history left, the unreviewed stretch right.
        let baseline = layout.points.iter().find(|p| p.baseline).unwrap();
        assert_eq!(layout.baseline_col, Some(baseline.col));
    }

    #[test]
    fn baseline_at_now_dims_the_whole_axis() {
        let mut h = history(vec![
            revision("now", RevisionSource::Now),
            revision("r1", RevisionSource::Local),
            revision("r2", RevisionSource::Git),
        ]);
        // Acknowledging NOW: everything is reviewed, the axis has no
        // bright stretch.
        h.reviewed_id = Some("now".into());
        h.reviewed_content = Some("now".into());
        let layout = layout_timeline(&h, 40).unwrap();
        assert_eq!(layout.baseline_col, Some(39));
    }

    #[test]
    fn words_mark_here_and_now() {
        let mut h = history(vec![
            revision("now", RevisionSource::Now),
            revision("r1", RevisionSource::Local),
            revision("r2", RevisionSource::Git),
            revision("r3", RevisionSource::Local),
        ]);
        h.position = 1;
        h.reviewed_id = Some("r2".into());
        h.reviewed_content = Some("r2".into());
        let layout = layout_timeline(&h, 120).unwrap();
        let texts: Vec<&str> = layout.words.iter().map(|(_, t)| t.as_str()).collect();
        // The baseline keeps no word: the ▮ marker and the axis dimming
        // carry it. Only the "you are here" pair remains.
        assert_eq!(texts, ["HERE", "NOW"]);
        assert_eq!(layout.words.last().unwrap().0, 117, "NOW ends at the edge");
    }

    #[test]
    fn here_word_never_overlaps_the_now_anchor() {
        // Position 1 in a crowded history puts `HERE` right next to
        // NOW; the word shifts left instead of colliding.
        let mut h = history(vec![
            revision("now", RevisionSource::Now),
            revision("r1", RevisionSource::Local),
        ]);
        h.position = 1;
        let layout = layout_timeline(&h, 60).unwrap();
        let here = layout.words.iter().find(|(_, t)| t == "HERE").unwrap();
        let now = layout.words.iter().find(|(_, t)| t == "NOW").unwrap();
        assert!(here.0 + here.1.len() <= now.0);
    }
}
