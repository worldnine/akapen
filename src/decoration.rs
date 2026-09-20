//! Range decoration: styling an arbitrary SOURCE byte range on top of a
//! rendered document (Phase 2 of `docs/range-attribution-plan.md`).
//!
//! Phase 1 gave every rendered span an [`Attr`] — the source byte range it
//! came from, plus whether that range is *exact* (`span.text ==
//! source[attr.range]`, so it may be sub-sliced) or a correct *superset*.
//! This layer is the first consumer: it intersects a list of
//! [`Decoration`]s with those attributions and patches the spans' styles,
//! splitting a span where a decoration ends inside it.
//!
//! # The intersection rules
//!
//! - **exact** attribution → cut the span at the decoration's byte
//!   offsets and style only the intersecting piece. Both cut points fall
//!   on UTF-8 boundaries by construction (an exact range IS the span's
//!   text), which is asserted.
//! - **superset** attribution → all or nothing: the span is decorated
//!   only when the decoration COVERS the whole attributed range. A
//!   partial overlap decorates nothing. The span's text is not a verbatim
//!   slice of its range (a table cell was re-wrapped, `&amp;` became `&`,
//!   syntect re-split a code line), so any byte offset into it would be a
//!   guess — and a wrong guess bleeds decoration onto the neighbouring
//!   text. This rule is what keeps table cells and entity references from
//!   smearing.
//! - **no** attribution (`None`) → a synthesized span: a quote prefix, a
//!   table border, padding. It has no source, so it is never decorated.
//!
//! A known MVP consequence of the superset rule: a list item's `- `
//! marker is attributed to the WHOLE item (`- 項目\n`), so dimming the
//! item's TEXT leaves the marker at full brightness. Dimming the item's
//! whole source range dims the marker too. Accepted for now — a marker
//! that dims with its text needs an exact attribution in the renderer,
//! which is Phase 3's territory.
//!
//! # Style is patched, never replaced
//!
//! Syntax highlighting must survive decoration, so each kind touches the
//! narrowest part of the style it can afford to:
//! [`DecorationKind::SemanticMark`] sets a **background** only, and
//! [`DecorationKind::Dim`] sets a **foreground** only.
//!
//! # Why Dim writes the foreground (Phase 2 said it never would)
//!
//! Phase 2 shipped `Dim` as [`Modifier::DIM`] with the foreground
//! untouched, on the reasoning that a theme-derived gray would flatten
//! every color in the dimmed range into one. That was right about the
//! gray and wrong about the modifier: **SGR `2` is widely ignored.** On
//! the terminal this was tried on it did nothing at all, so MARKED and
//! DIM were indistinguishable — the layer's whole point.
//!
//! So `Dim` now blends the span's EFFECTIVE foreground
//! ([`DecorationStyles::dim_fg`]) toward the theme background by
//! [`DIM_BLEND`]. It is a real color, so every terminal renders it. The
//! objection about flattening does not apply: each span keeps its own
//! hue, merely moved toward the page — a dimmed heading is still a
//! dimmed HEADING. `Modifier::DIM` is deliberately NOT set as well;
//! a terminal that honours it would darken twice.
//!
//! "Effective" foreground means the span's own `fg`, or the theme's
//! default foreground when the span has none. Leaving it `None` would
//! dim nothing, which is how the bug looked in the first place.
//!
//! The view's own layers (selection, cursor band, history glow) are
//! applied AFTER this one in `view.rs`, so they win by construction —
//! except that they can no longer win over `Dim`, because a foreground
//! is not a background. A band laid over a dimmed foreground would read
//! as "I selected it and the text went pale". `view.rs` therefore
//! **drops `Dim` decorations for rows under a band** before decorating,
//! which is the same intent Phase 2's `remove_modifier(Modifier::DIM)`
//! carried, expressed in the only way the new mechanism allows.
//!
//! # Purity
//!
//! [`decorate_row`] is a pure transform over one already-rendered row.
//! [`Rendered`] stays the decoration-free cache it was in Phase 1, and the
//! decoration list never reaches `render::render` — changing which ranges
//! are decorated cannot re-parse the markdown.
//!
//! The view applies it per VISIBLE row while painting, which is how its
//! selection layer already works; a whole-document
//! `Rendered -> Vec<Vec<Span>>` is the same call folded over
//! `rows.iter().zip(&row_attrs)`.
//!
//! [`Rendered`]: crate::render::Rendered

use std::ops::Range;

use ratatui::style::{Color, Style};
use tui_markdown::Attr;

use crate::highlight::{Highlighter, Span};

/// A styled source byte range. `range` is an offset range into the
/// document's source text — the same coordinate space as [`Attr::range`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoration {
    pub range: Range<usize>,
    pub kind: DecorationKind,
}

/// What a decoration means. Deliberately a small, presentation-free
/// vocabulary: akapen owns these, and the mapping from a higher layer's
/// state (the Semantic Reading Layer's `DisplayState`, a search hit, a
/// diff range) onto them belongs to that layer, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DecorationKind {
    /// "This is worth reading": a subdued background, theme-derived.
    SemanticMark,
    /// "This can be skimmed": the foreground moved toward the page.
    Dim,
}

/// The theme-derived background of [`DecorationKind::SemanticMark`], for a
/// theme that carries no highlight scope AND no background of its own.
/// Split light/dark by the theme foreground's brightness.
const MARK_BG_DARK: Color = Color::Rgb(0x36, 0x37, 0x49);
const MARK_BG_LIGHT: Color = Color::Rgb(0xe7, 0xe5, 0xd5);

/// The theme background a [`DecorationKind::Dim`] foreground is blended
/// toward when the theme declares none of its own. Same split as
/// [`MARK_BG_DARK`]/[`MARK_BG_LIGHT`], picked by the foreground's
/// brightness.
const DIM_TARGET_DARK: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const DIM_TARGET_LIGHT: Color = Color::Rgb(0xfd, 0xf6, 0xe3);

/// How far the mark background is lifted from the theme background toward
/// the theme foreground when no scope supplies one.
///
/// The mark sits BELOW the selection band in the priority order, so it
/// must stay clearly quieter than it — see [`MARK_BG_BLEND_CEILING`].
/// 0.14 was tried first and read as "nothing happened" on a real
/// terminal; 0.22 was picked by eye against four alternatives.
pub const MARK_BG_BLEND: f32 = 0.22;

/// The highest default [`MARK_BG_BLEND`] may take before the mark starts
/// to be mistakable for the selection band (dark: the band is
/// `rgb(88,91,112)`, and 0.30 already lands on `rgb(82,85,105)`).
///
/// A ceiling on the SHIPPED default, not on `--mark-blend`: the flag is a
/// knob for looking at alternatives, and silently clamping what the user
/// typed would make it a useless one. A test holds the default under it.
pub const MARK_BG_BLEND_CEILING: f32 = 0.25;

/// The ceiling is not advice: raising [`MARK_BG_BLEND`] past it stops the
/// build, not a test run.
const _: () = assert!(MARK_BG_BLEND <= MARK_BG_BLEND_CEILING);

/// How far a [`DecorationKind::Dim`] foreground is moved from its own
/// color toward the theme background. 0.60 was picked by eye on a real
/// terminal: far enough to recede, near enough to stay readable and keep
/// its hue.
pub const DIM_BLEND: f32 = 0.60;

/// The two blend factors, together. **The defaults live here and nowhere
/// else**; the command line overrides them (`--mark-blend` /
/// `--dim-blend`) and any future configuration file would slot in
/// between, as the middle layer of `CLI > config file > default`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationBlend {
    /// [`MARK_BG_BLEND`].
    pub mark: f32,
    /// [`DIM_BLEND`].
    pub dim: f32,
}

impl Default for DecorationBlend {
    fn default() -> Self {
        Self { mark: MARK_BG_BLEND, dim: DIM_BLEND }
    }
}

/// The scopes a theme may use for a "marked passage" background, best
/// first. Resolved through the very same [`Highlighter::scope_style`] path
/// the markdown style sheet uses (`render::MdcommentStyleSheet::from_theme`),
/// so a theme that styles any of them keeps its own look. Neither akapen
/// default theme (Catppuccin Mocha / Solarized (light)) defines one, so
/// both fall through to the blend below — an `invalid`-style red is
/// deliberately NOT in this list, its meaning is "error", not "marked".
const MARK_SCOPES: &[&str] = &[
    "markup.highlight",
    "markup.mark",
    "markup.quote.highlight",
    "region.yellowish",
];

/// The resolved per-kind styles for one theme. Built once per render (the
/// theme cannot change without a re-render) and applied by patching, so a
/// decorated span keeps its syntax color and its font style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationStyles {
    /// The mark's patch: a background and nothing else.
    mark: Style,
    /// Where a dimmed foreground travels toward — the theme's background.
    dim_target: Color,
    /// The foreground a span that declares none actually renders with.
    default_fg: Color,
    /// How far along `fg -> dim_target` a dimmed span lands.
    dim_blend: f32,
}

impl Default for DecorationStyles {
    /// The dark-theme styles at the default blends. Only reachable
    /// through a `ViewState` built without a render (`std::mem::take`),
    /// which is never painted.
    fn default() -> Self {
        Self {
            mark: Style::default().bg(MARK_BG_DARK),
            dim_target: DIM_TARGET_DARK,
            default_fg: Color::Rgb(0xcd, 0xd6, 0xf4),
            dim_blend: DIM_BLEND,
        }
    }
}

impl DecorationStyles {
    /// Resolve both kinds against `highlighter`'s theme at `blend`.
    pub fn from_theme(highlighter: &Highlighter, blend: DecorationBlend) -> Self {
        let default_fg = highlighter.default_fg();
        Self {
            mark: Style::default().bg(mark_background(highlighter, blend.mark)),
            dim_target: match highlighter.theme().settings.background {
                Some(bg) => Color::Rgb(bg.r, bg.g, bg.b),
                // No theme background at all: pick the side from the text
                // color, exactly as `mark_background` does.
                None => match default_fg {
                    Color::Rgb(r, g, b) if r as u32 + g as u32 + b as u32 >= 3 * 128 => {
                        DIM_TARGET_DARK
                    }
                    _ => DIM_TARGET_LIGHT,
                },
            },
            default_fg,
            dim_blend: blend.dim,
        }
    }

    /// The mark's patch: a background, no foreground, no modifier.
    pub fn mark_style(&self) -> Style {
        self.mark
    }

    /// The foreground a span whose own foreground is `base` renders with
    /// once dimmed.
    ///
    /// `None` — a span that inherits the theme's text color — resolves to
    /// that color first, so it dims like everything else instead of
    /// staying bright. A NAMED color (a terminal-palette entry) has no
    /// RGB to interpolate, so it falls back to the theme default too: the
    /// hue is lost, but "this text recedes" is the point of the kind, and
    /// content spans in the rendered view carry RGB from the theme.
    pub fn dim_fg(&self, base: Option<Color>) -> Color {
        let fg = match base {
            Some(rgb @ Color::Rgb(..)) => rgb,
            Some(_) | None => self.default_fg,
        };
        crate::view::lerp_color(fg, self.dim_target, self.dim_blend)
    }

    /// `base` with `kind` applied. Patching, not replacing: everything
    /// the kind does not own — the other of fg/bg, and every modifier
    /// already on `base` — survives.
    ///
    /// Applying both kinds to one span composes in either order: the mark
    /// only writes a background, and the dim reads `base.fg` before
    /// writing it. Applying the SAME `Dim` twice would darken twice; the
    /// producers here emit one decoration per Atom, and `--decorations`
    /// is a development flag.
    pub fn patch(&self, base: Style, kind: DecorationKind) -> Style {
        match kind {
            DecorationKind::SemanticMark => base.patch(self.mark_style()),
            DecorationKind::Dim => base.fg(self.dim_fg(base.fg)),
        }
    }
}

/// The mark background: the first [`MARK_SCOPES`] entry the theme gives a
/// background, else the theme's own background lifted [`MARK_BG_BLEND`]
/// toward its foreground (so it reads as "slightly raised paper" in a dark
/// AND a light theme), else a fixed pair picked by the foreground's
/// brightness.
fn mark_background(highlighter: &Highlighter, blend: f32) -> Color {
    for scope in MARK_SCOPES {
        if let Some(bg) = highlighter.scope_style(scope).and_then(|s| s.bg) {
            return bg;
        }
    }
    let settings = &highlighter.theme().settings;
    let fg = highlighter.default_fg();
    match settings.background {
        Some(bg) => crate::view::lerp_color(Color::Rgb(bg.r, bg.g, bg.b), fg, blend),
        // No theme background at all: pick the side from the text color.
        None => match fg {
            Color::Rgb(r, g, b) if r as u32 + g as u32 + b as u32 >= 3 * 128 => MARK_BG_DARK,
            _ => MARK_BG_LIGHT,
        },
    }
}

/// Drop the decorations that cannot be applied to `source`: a range that
/// runs past the end of the document, or whose ends do not land on UTF-8
/// character boundaries.
///
/// Decorations produced INSIDE akapen are well-formed by construction —
/// they come from the same byte space the attribution does, which is why
/// [`decorate_row`] asserts it. Decorations that arrive from OUTSIDE
/// (the `--decorations` flag) are not, and a mid-character offset would
/// otherwise trip that assertion and take the TUI down. Filter them here,
/// at the boundary, so the invariant below stays an invariant.
pub fn sanitize(decorations: &[Decoration], source: &str) -> Vec<Decoration> {
    decorations
        .iter()
        .filter(|d| {
            d.range.end <= source.len()
                && source.is_char_boundary(d.range.start)
                && source.is_char_boundary(d.range.end)
        })
        .cloned()
        .collect()
}

/// Apply `decorations` to one rendered row, returning the row's spans and
/// their attributions (both still parallel — a span split into three
/// pieces yields three attributions).
///
/// Pure: `row` and `attrs` are untouched. With an empty `decorations` the
/// output is a byte-for-byte copy of the input, which is the identity the
/// tests pin.
///
/// Decorations are applied in slice order, each patched onto the result of
/// the previous one, so a later decoration wins on a field two of them
/// both set.
pub fn decorate_row(
    row: &[Span],
    attrs: &[Option<Attr>],
    decorations: &[Decoration],
    styles: &DecorationStyles,
) -> (Vec<Span>, Vec<Option<Attr>>) {
    debug_assert_eq!(
        row.len(),
        attrs.len(),
        "row_attrs runs parallel to the row's spans"
    );
    if decorations.is_empty() {
        return (row.to_vec(), attrs.to_vec());
    }
    let mut out_spans = Vec::with_capacity(row.len());
    let mut out_attrs = Vec::with_capacity(row.len());
    for (i, span) in row.iter().enumerate() {
        match attrs.get(i).and_then(|a| a.as_ref()) {
            // Synthesized span (quote/list prefix, table frame, padding):
            // no source, no decoration.
            None => {
                out_spans.push(span.clone());
                out_attrs.push(None);
            }
            // A superset range: all or nothing (see the module docs).
            Some(attr) if !attr.exact => {
                let mut style = span.style;
                for d in decorations {
                    if d.range.is_empty() {
                        continue;
                    }
                    if d.range.start <= attr.range.start && d.range.end >= attr.range.end {
                        style = styles.patch(style, d.kind);
                    }
                }
                out_spans.push(Span {
                    text: span.text.clone(),
                    style,
                });
                out_attrs.push(Some(attr.clone()));
            }
            // An exact range: cut at the decoration boundaries.
            Some(attr) => {
                push_exact(span, attr, decorations, styles, &mut out_spans, &mut out_attrs);
            }
        }
    }
    (out_spans, out_attrs)
}

/// Split one EXACT span at every decoration edge that falls strictly
/// inside it, and style each piece by the decorations that cover it whole.
/// Because every edge became a cut, each piece is either fully inside a
/// decoration or fully outside it — there is no partial piece left to
/// decide about.
fn push_exact(
    span: &Span,
    attr: &Attr,
    decorations: &[Decoration],
    styles: &DecorationStyles,
    out_spans: &mut Vec<Span>,
    out_attrs: &mut Vec<Option<Attr>>,
) {
    let base = attr.range.start;
    let len = span.text.len();
    debug_assert_eq!(
        len,
        attr.range.len(),
        "an exact attribution IS the span's text, so the lengths agree"
    );
    // An empty span (a blank row renders as one) cannot be cut and cannot
    // show a style; keep it as it is so rows stay countable.
    if len == 0 {
        out_spans.push(span.clone());
        out_attrs.push(Some(attr.clone()));
        return;
    }
    let mut cuts: Vec<usize> = Vec::with_capacity(2 + 2 * decorations.len());
    cuts.push(0);
    cuts.push(len);
    for d in decorations {
        // An empty or backwards range decorates nothing. (`--decorations`
        // hands whatever the caller wrote straight through, and a
        // backwards range would otherwise reach the cut logic below with
        // its ends swapped.)
        if d.range.is_empty() || d.range.start >= attr.range.end || d.range.end <= attr.range.start
        {
            continue;
        }
        for edge in [d.range.start, d.range.end] {
            if edge <= base || edge >= base + len {
                continue;
            }
            let off = edge - base;
            // The decoration's ends land on UTF-8 boundaries: an exact
            // range is a verbatim slice of the source, so a boundary of
            // the source inside it is a boundary of the span's text.
            debug_assert!(
                span.text.is_char_boundary(off),
                "a decoration edge must fall on a UTF-8 boundary"
            );
            // Belt and braces for a decoration that came from outside
            // (the `--decorations` flag): cutting mid-character would
            // panic in release too, so an unusable edge is dropped rather
            // than taken.
            if span.text.is_char_boundary(off) {
                cuts.push(off);
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut style = span.style;
        for d in decorations {
            if d.range.is_empty() {
                continue;
            }
            if d.range.start <= base + a && d.range.end >= base + b {
                style = styles.patch(style, d.kind);
            }
        }
        out_spans.push(Span {
            text: span.text[a..b].to_string(),
            style,
        });
        out_attrs.push(Some(attr.slice(a, b, true)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use crate::render::{Rendered, render};
    use crate::source::Source;

    /// Render `text` and resolve the decoration styles from the same
    /// (default dark) theme.
    fn doc(text: &str, width: usize) -> (Source, Rendered, DecorationStyles) {
        let source = Source::from_content("doc.md".into(), text.to_string());
        let highlighter = Highlighter::new(None, false);
        let rendered = render(&source, width, &highlighter);
        let styles = DecorationStyles::from_theme(&highlighter, Default::default());
        (source, rendered, styles)
    }

    /// The byte range of `needle`'s first occurrence in the source.
    fn at(source: &Source, needle: &str) -> Range<usize> {
        let start = source
            .content
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in the source"));
        start..start + needle.len()
    }

    fn mark(range: Range<usize>) -> Decoration {
        Decoration {
            range,
            kind: DecorationKind::SemanticMark,
        }
    }

    fn dim(range: Range<usize>) -> Decoration {
        Decoration {
            range,
            kind: DecorationKind::Dim,
        }
    }

    /// Row `i`, decorated: `(text, style)` per span.
    fn row(
        r: &Rendered,
        styles: &DecorationStyles,
        decorations: &[Decoration],
        i: usize,
    ) -> Vec<(String, Style)> {
        decorate_row(&r.rows[i], &r.row_attrs[i], decorations, styles)
            .0
            .into_iter()
            .map(|s| (s.text, s.style))
            .collect()
    }

    fn texts(row: &[(String, Style)]) -> Vec<&str> {
        row.iter().map(|(t, _)| t.as_str()).collect()
    }

    fn joined(row: &[(String, Style)]) -> String {
        row.iter().map(|(t, _)| t.as_str()).collect()
    }

    /// The row a decoration's text landed on, by looking for `needle` in
    /// the rows' concatenated text.
    fn row_with(r: &Rendered, needle: &str) -> usize {
        r.rows
            .iter()
            .position(|row| {
                row.iter().map(|s| s.text.as_str()).collect::<String>().contains(needle)
            })
            .unwrap_or_else(|| panic!("no row renders {needle:?}"))
    }

    /// THE milestone of this phase: three different styles on one
    /// rendered line, driven purely by source byte ranges.
    ///
    /// The baseline (no decorations) is the comparison: the untouched
    /// phrase must come out bit-identical, the marked phrase must gain
    /// ONLY a background, and the dimmed phrase ONLY the DIM modifier.
    /// Anything else means the decoration replaced syntax highlighting
    /// instead of patching it.
    #[test]
    fn marked_normal_dim_on_one_rendered_line() {
        let (source, r, styles) = doc("前 **重要** 後\n", 80);
        let decorations = vec![mark(at(&source, "重要")), dim(at(&source, " 後"))];
        let before = row(&r, &styles, &[], 0);
        let after = row(&r, &styles, &decorations, 0);

        assert_eq!(texts(&before), ["前 ", "重要", " 後"]);
        assert_eq!(joined(&after), joined(&before), "the text is untouched");
        assert_eq!(texts(&after), ["前 ", "重要", " 後"]);

        let (normal, marked, dimmed) = (after[0].1, after[1].1, after[2].1);
        assert_ne!(normal, marked);
        assert_ne!(normal, dimmed);
        assert_ne!(marked, dimmed);

        // NORMAL: byte-identical to the undecorated render.
        assert_eq!(normal, before[0].1);
        // MARKED: the strong span keeps its color AND its BOLD; only the
        // background is new.
        assert!(before[1].1.add_modifier.contains(Modifier::BOLD));
        assert_eq!(marked.fg, before[1].1.fg);
        assert_eq!(marked.add_modifier, before[1].1.add_modifier);
        assert_eq!(marked.bg, styles.mark_style().bg);
        assert!(marked.bg.is_some());
        // DIM: only the foreground is new — the background and every
        // modifier are the undecorated ones, and the new color is the
        // old one moved toward the page (not a flat gray).
        assert_eq!(dimmed.bg, before[2].1.bg);
        assert_eq!(dimmed.add_modifier, before[2].1.add_modifier);
        assert!(!dimmed.add_modifier.contains(Modifier::DIM), "SGR 2 は使わない");
        assert_ne!(dimmed.fg, before[2].1.fg);
        assert_eq!(dimmed.fg, Some(styles.dim_fg(before[2].1.fg)));
    }

    /// The intersection rule for an EXACT span: a decoration ending
    /// inside it cuts it. `前重要後` is one Text event — one span — so
    /// this case cannot be satisfied by the markdown structure the way
    /// `**重要**` can.
    #[test]
    fn an_exact_span_is_cut_at_the_decoration_edges() {
        let (source, r, styles) = doc("前重要後\n", 80);
        let before = row(&r, &styles, &[], 0);
        assert_eq!(texts(&before), ["前重要後"], "one span before decoration");

        let after = row(&r, &styles, &[mark(at(&source, "重要"))], 0);
        assert_eq!(texts(&after), ["前", "重要", "後"]);
        assert_eq!(joined(&after), "前重要後");
        assert_eq!(after[0].1, before[0].1);
        assert_eq!(after[2].1, before[0].1);
        assert_eq!(after[1].1.bg, styles.mark_style().bg);
        assert_eq!(after[1].1.fg, before[0].1.fg);
    }

    /// The pieces keep a correct (and still exact) attribution, so a
    /// second decoration pass over the output would land identically.
    #[test]
    fn split_pieces_keep_their_own_source_ranges() {
        let (source, r, styles) = doc("前重要後\n", 80);
        let (spans, attrs) =
            decorate_row(&r.rows[0], &r.row_attrs[0], &[mark(at(&source, "重要"))], &styles);
        assert_eq!(spans.len(), attrs.len());
        for (span, attr) in spans.iter().zip(&attrs) {
            let attr = attr.as_ref().expect("text spans stay attributed");
            assert!(attr.exact, "a slice of an exact range is exact");
            assert_eq!(&source.content[attr.range.clone()], span.text);
        }
        assert_eq!(attrs[1].as_ref().unwrap().range, at(&source, "重要"));
    }

    /// A decoration that crosses a soft-wrap boundary decorates its part
    /// of each row — the wrap split the span, the decoration splits it
    /// again.
    #[test]
    fn a_decoration_crosses_a_soft_wrap() {
        let (source, r, styles) = doc("あいうえおかきくけこさしすせそたちつてと\n", 12);
        let decorations = vec![mark(at(&source, "かきくけこ"))];
        let mark_bg = styles.mark_style().bg;
        let first = row(&r, &styles, &decorations, 0);
        let second = row(&r, &styles, &decorations, 1);
        assert_eq!(texts(&first), ["あいうえお", "か"]);
        assert_eq!(texts(&second), ["きくけこ", "さし"]);
        assert_eq!(first[0].1.bg, None);
        assert_eq!(first[1].1.bg, mark_bg);
        assert_eq!(second[0].1.bg, mark_bg);
        assert_eq!(second[1].1.bg, None);
    }

    /// A link: the label is exact, the rendered ` (`/URL/`)` are
    /// supersets of the WHOLE link. Decorating the label must therefore
    /// stop at the label — this is the bleed the exactness contract
    /// exists to prevent.
    #[test]
    fn a_decoration_on_a_link_label_does_not_bleed_into_the_url() {
        let (source, r, styles) = doc("前 [ラベル](https://example.com) 後\n", 80);
        let mark_bg = styles.mark_style().bg;
        let after = row(&r, &styles, &[mark(at(&source, "ラベル"))], 0);
        assert_eq!(
            texts(&after),
            ["前 ", "ラベル", " (", "https://example.com", ")", " 後"]
        );
        let decorated: Vec<&str> = after
            .iter()
            .filter(|(_, s)| s.bg == mark_bg)
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(decorated, ["ラベル"], "only the label is marked");
    }

    /// The converse: a decoration that COVERS the whole link markup does
    /// reach the URL spans — they are supersets of exactly that range.
    #[test]
    fn a_decoration_covering_the_whole_link_reaches_its_url() {
        let (source, r, styles) = doc("前 [ラベル](https://example.com) 後\n", 80);
        let mark_bg = styles.mark_style().bg;
        let link = at(&source, "[ラベル](https://example.com)");
        let after = row(&r, &styles, &[mark(link)], 0);
        let decorated: Vec<&str> = after
            .iter()
            .filter(|(_, s)| s.bg == mark_bg)
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(decorated, ["ラベル", " (", "https://example.com", ")"]);
    }

    /// A partial overlap with a SUPERSET span decorates nothing. A table
    /// cell's text was re-wrapped out of its source range, so there is no
    /// honest byte offset to cut it at — decorating it anyway would smear
    /// the mark over text the caller never asked for.
    #[test]
    fn a_partial_overlap_of_a_superset_span_is_not_decorated() {
        let table = "| あ | い |\n|---|---|\n| cell one | cell two |\n";
        let (source, r, styles) = doc(table, 40);
        let mark_bg = styles.mark_style().bg;
        let i = row_with(&r, "cell one");
        // The cell's attribution is a superset by construction.
        let cell = r.rows[i]
            .iter()
            .zip(&r.row_attrs[i])
            .find(|(s, _)| s.text == "cell one")
            .and_then(|(_, a)| a.as_ref())
            .expect("the cell is attributed");
        assert!(!cell.exact, "a table cell is attributed as a superset");

        // Half of it: nothing is decorated.
        let partial = row(&r, &styles, &[mark(at(&source, "cell"))], i);
        assert!(
            partial.iter().all(|(_, s)| s.bg != mark_bg),
            "a partial overlap must not decorate: {partial:?}"
        );
        // All of it: the whole span is decorated, as one piece.
        let whole = row(&r, &styles, &[mark(at(&source, "cell one"))], i);
        let decorated: Vec<&str> = whole
            .iter()
            .filter(|(_, s)| s.bg == mark_bg)
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(decorated, ["cell one"]);
    }

    /// Synthesized spans (a table's borders and padding) have no source
    /// and are never decorated, not even by a decoration covering the
    /// whole document.
    #[test]
    fn synthesized_spans_are_never_decorated() {
        let table = "| あ | い |\n|---|---|\n| cell one | cell two |\n";
        let (source, r, styles) = doc(table, 40);
        let mark_bg = styles.mark_style().bg;
        let everything = vec![mark(0..source.content.len())];
        let mut frame_spans = 0usize;
        for i in 0..r.rows.len() {
            let (spans, attrs) =
                decorate_row(&r.rows[i], &r.row_attrs[i], &everything, &styles);
            for (span, attr) in spans.iter().zip(&attrs) {
                if attr.is_none() {
                    frame_spans += 1;
                    assert_ne!(span.style.bg, mark_bg, "{:?} is synthesized", span.text);
                }
            }
        }
        assert!(frame_spans > 0, "the fixture has synthesized spans");
    }

    /// The accepted MVP rough edge, pinned so it cannot change silently:
    /// a list marker is attributed to the whole item, so dimming the
    /// item's TEXT leaves `- ` bright — and dimming the item's whole
    /// source range does dim it.
    #[test]
    fn a_list_marker_follows_the_whole_item_not_its_text() {
        let (source, r, styles) = doc("- 項目ひとつ\n", 40);
        let plain = row(&r, &styles, &[], 0);
        let text_only = row(&r, &styles, &[dim(at(&source, "項目ひとつ"))], 0);
        assert_eq!(texts(&text_only), ["- ", "項目ひとつ"]);
        assert_eq!(text_only[0].1.fg, plain[0].1.fg, "marker stays bright");
        assert_ne!(text_only[1].1.fg, plain[1].1.fg, "the body dims");

        let whole_item = row(&r, &styles, &[dim(0..source.content.len())], 0);
        assert_ne!(whole_item[0].1.fg, plain[0].1.fg, "the marker dims too");
    }

    /// Multi-byte text: the cut points are byte offsets, and they land on
    /// character boundaries for Japanese, emoji and fullwidth letters
    /// alike.
    #[test]
    fn decorations_land_on_multibyte_characters() {
        let (source, r, styles) = doc("あい🎉うえＡＢ\n", 80);
        let mark_bg = styles.mark_style().bg;
        for needle in ["🎉", "あい", "Ａ", "うえＡ"] {
            let after = row(&r, &styles, &[mark(at(&source, needle))], 0);
            assert_eq!(joined(&after), "あい🎉うえＡＢ", "{needle}");
            let decorated: String = after
                .iter()
                .filter(|(_, s)| s.bg == mark_bg)
                .map(|(t, _)| t.as_str())
                .collect();
            assert_eq!(decorated, needle, "exactly {needle:?} is decorated");
        }
    }

    /// Two kinds over the same range compose instead of replacing each
    /// other: the background from one, the foreground from the other.
    #[test]
    fn two_kinds_over_the_same_range_compose() {
        let (source, r, styles) = doc("前重要後\n", 80);
        let range = at(&source, "重要");
        let plain = row(&r, &styles, &[], 0);
        let after = row(&r, &styles, &[mark(range.clone()), dim(range)], 0);
        assert_eq!(texts(&after), ["前", "重要", "後"]);
        assert_eq!(after[1].1.bg, styles.mark_style().bg);
        assert_eq!(after[1].1.fg, Some(styles.dim_fg(plain[0].1.fg)));
    }

    /// Junk ranges (past the end of the document, backwards, empty) are
    /// inert rather than a panic — `--decorations` hands them straight in.
    #[test]
    fn out_of_range_decorations_are_inert() {
        let (source, r, styles) = doc("前重要後\n", 80);
        let before = row(&r, &styles, &[], 0);
        #[allow(clippy::reversed_empty_ranges)]
        let junk = vec![
            mark(9_000..9_100),
            mark(8..4),
            mark(3..3),
            mark(source.content.len()..source.content.len() + 50),
        ];
        assert_eq!(row(&r, &styles, &junk, 0), before);
    }

    /// A decoration edge inside a multi-byte character cannot come from
    /// the attribution layer, but CAN come from `--decorations`. It must
    /// be dropped, not panic (release builds have no `debug_assert`).
    #[test]
    #[cfg(not(debug_assertions))]
    fn a_mid_character_edge_is_dropped_instead_of_panicking() {
        let (_, r, styles) = doc("前重要後\n", 80);
        // 4 is inside 重 (3..6).
        let after = row(&r, &styles, &[mark(0..4)], 0);
        assert_eq!(joined(&after), "前重要後");
    }

    /// The identity this phase must not break: with no decorations the
    /// rendered rows come out unchanged, for every fixture at both
    /// widths the view actually uses.
    #[test]
    fn no_decorations_is_the_identity() {
        let root = env!("CARGO_MANIFEST_DIR");
        let highlighter = Highlighter::new(None, false);
        let styles = DecorationStyles::from_theme(&highlighter, Default::default());
        for name in ["full.md", "a-readme.md", "b-design.md", "c-impl.rs"] {
            let source =
                Source::load(std::path::Path::new(root).join("testdata").join(name)).unwrap();
            for width in [40usize, 80] {
                let r = render(&source, width, &highlighter);
                let out: Vec<Vec<Span>> = r
                    .rows
                    .iter()
                    .zip(&r.row_attrs)
                    .map(|(row, attrs)| decorate_row(row, attrs, &[], &styles).0)
                    .collect();
                assert_eq!(out, r.rows, "{name} at width {width}");
            }
        }
    }

    /// The same identity on the row level, including the attributions:
    /// nothing is split, nothing is re-styled, nothing is dropped.
    #[test]
    fn no_decorations_preserves_the_attributions_too() {
        let (_, r, styles) = doc("# 見出し\n\n本文 **強調** です。\n\n- 項目\n", 40);
        for i in 0..r.rows.len() {
            let (spans, attrs) = decorate_row(&r.rows[i], &r.row_attrs[i], &[], &styles);
            assert_eq!(spans, r.rows[i]);
            assert_eq!(attrs, r.row_attrs[i]);
        }
    }

    /// The mark background is a real color that differs from the page —
    /// both default themes fall through the scope lookup to the blend, so
    /// this is the path that actually ships.
    #[test]
    fn the_mark_background_is_theme_derived_in_both_themes() {
        for light in [false, true] {
            let highlighter = Highlighter::new(None, light);
            let styles = DecorationStyles::from_theme(&highlighter, Default::default());
            let bg = styles.mark_style().bg.expect("a background");
            let page = highlighter.theme().settings.background.expect("a theme background");
            assert_ne!(bg, Color::Rgb(page.r, page.g, page.b), "light={light}");
            // Subdued: closer to the page than to the text.
            let Color::Rgb(r, g, b) = bg else {
                panic!("an RGB background")
            };
            let distance = |x: u8, y: u8| (x as i32 - y as i32).abs();
            let from_page =
                distance(r, page.r) + distance(g, page.g) + distance(b, page.b);
            let Color::Rgb(fr, fg_, fb) = highlighter.default_fg() else {
                panic!("an RGB foreground")
            };
            let from_text = distance(r, fr) + distance(g, fg_) + distance(b, fb);
            assert!(from_page < from_text, "light={light}: the mark must stay subdued");
        }
    }

    /// Each kind owns exactly one channel: the mark a background, the dim
    /// a foreground. Neither touches the other, and neither sets a
    /// modifier — that is what keeps BOLD/ITALIC and the syntax color's
    /// HUE alive under a decoration.
    #[test]
    fn each_kind_owns_one_channel_and_no_modifier() {
        let highlighter = Highlighter::new(None, false);
        let styles = DecorationStyles::from_theme(&highlighter, Default::default());
        let base = Style::default()
            .fg(Color::Rgb(200, 100, 50))
            .bg(Color::Rgb(9, 9, 9))
            .add_modifier(Modifier::BOLD | Modifier::ITALIC);

        // MARK: background only.
        let marked = styles.patch(base, DecorationKind::SemanticMark);
        assert_eq!(marked.fg, base.fg, "mark leaves the foreground alone");
        assert_ne!(marked.bg, base.bg);
        assert_eq!(marked.add_modifier, base.add_modifier);

        // DIM: foreground only, and NOT `Modifier::DIM` as well (a
        // terminal that honours it would darken twice).
        let dimmed = styles.patch(base, DecorationKind::Dim);
        assert_eq!(dimmed.bg, base.bg, "dim leaves the background alone");
        assert_eq!(dimmed.add_modifier, base.add_modifier);
        assert!(!dimmed.add_modifier.contains(Modifier::DIM));
        assert_eq!(dimmed.fg, Some(styles.dim_fg(base.fg)));
        assert_ne!(dimmed.fg, base.fg);

        // The hue survives: the dimmed color is the base moved toward the
        // page, so the channel ORDER (r > g > b here) is preserved and a
        // differently-colored span stays differently colored.
        let Some(Color::Rgb(r, g, b)) = dimmed.fg else {
            panic!("an RGB foreground")
        };
        assert!(r > g && g > b, "dimmed to rgb({r},{g},{b})");
        let other = styles.patch(Style::default().fg(Color::Rgb(50, 100, 200)), DecorationKind::Dim);
        assert_ne!(other.fg, dimmed.fg, "two colors do not flatten into one");

        // 両方を当てても、順序によらず同じところに落ち着く。
        let both = styles.patch(
            styles.patch(base, DecorationKind::SemanticMark),
            DecorationKind::Dim,
        );
        let reversed = styles.patch(
            styles.patch(base, DecorationKind::Dim),
            DecorationKind::SemanticMark,
        );
        assert_eq!(both, reversed);
        assert_eq!(both.fg, dimmed.fg);
        assert_eq!(both.bg, marked.bg);
    }

    /// 前景色を持たない span（テーマ既定前景を継ぐもの）も暗くなる。
    /// `None` のまま放置すると、その範囲だけ明るいままになる — DIM が
    /// 効いていないように見えた元の症状と同じ形のバグ。
    #[test]
    fn a_span_without_a_foreground_still_dims() {
        for light in [false, true] {
            let highlighter = Highlighter::new(None, light);
            let styles = DecorationStyles::from_theme(&highlighter, Default::default());
            let bare = styles.patch(Style::default(), DecorationKind::Dim);
            assert_eq!(
                bare.fg,
                Some(styles.dim_fg(None)),
                "light={light}: 既定前景を解決してからブレンドする"
            );
            // テーマの既定前景そのものではない（ちゃんと動いている）。
            assert_ne!(bare.fg, Some(highlighter.default_fg()), "light={light}");
            // ページ背景に向かって動いている。
            let page = highlighter.theme().settings.background.expect("a background");
            let (Some(Color::Rgb(r, g, b)), Color::Rgb(fr, fg_, fb)) =
                (bare.fg, highlighter.default_fg())
            else {
                panic!("RGB")
            };
            let near = |x: u8, a: u8, p: u8| {
                (x as i32 - p as i32).abs() < (a as i32 - p as i32).abs()
            };
            assert!(near(r, fr, page.r) && near(g, fg_, page.g) && near(b, fb, page.b));
        }
    }

    /// 既定のブレンド率で実際に出る色。ユーザーが実機で選んだ値なので、
    /// 黙って動かないように値そのものを固定する。
    #[test]
    fn the_default_blends_produce_the_colors_that_were_chosen() {
        // Catppuccin Mocha: 背景 rgb(30,30,46) / 前景 rgb(205,214,244)。
        let dark = DecorationStyles::from_theme(&Highlighter::new(None, false), Default::default());
        assert_eq!(dark.mark_style().bg, Some(Color::Rgb(68, 70, 89)));
        assert_eq!(dark.dim_fg(None), Color::Rgb(99, 103, 125));
        // Solarized (light): 背景 rgb(253,246,227) / 前景 rgb(101,123,131)。
        let light = DecorationStyles::from_theme(&Highlighter::new(None, true), Default::default());
        assert_eq!(light.mark_style().bg, Some(Color::Rgb(219, 218, 205)));
        assert_eq!(light.dim_fg(None), Color::Rgb(192, 196, 188));
    }

    /// MARK の背景は selection 帯とはっきり別物であること。
    ///
    /// 帯（dark `rgb(88,91,112)`）に寄りすぎると「選択されている」と
    /// 読み違える。0.22 は `rgb(68,70,90)` で十分離れているが、0.30 は
    /// `rgb(82,85,105)` で危険域なので、既定値には天井を置いてある。
    #[test]
    fn the_mark_background_stays_clear_of_the_selection_band() {
        // 天井そのものは `const _: () = assert!(..)` がコンパイル時に
        // 押さえている。ここで見るのは、その天井が実際の色として意味を
        // 持っているか。
        let band = crate::view::selected_bg(false);
        let Color::Rgb(sr, sg, sb) = band else { panic!("RGB") };
        assert_eq!((sr, sg, sb), (88, 91, 112), "selection 帯の色が変わった");

        let styles = DecorationStyles::from_theme(&Highlighter::new(None, false), Default::default());
        let Some(Color::Rgb(r, g, b)) = styles.mark_style().bg else { panic!("RGB") };
        assert_ne!((r, g, b), (sr, sg, sb));
        let distance = (r as i32 - sr as i32).abs()
            + (g as i32 - sg as i32).abs()
            + (b as i32 - sb as i32).abs();
        assert!(distance >= 60, "mark rgb({r},{g},{b}) は帯に近すぎる（{distance}）");
        // 天井ちょうどでもまだ離れている。
        let ceiling = DecorationStyles::from_theme(
            &Highlighter::new(None, false),
            DecorationBlend { mark: MARK_BG_BLEND_CEILING, ..Default::default() },
        );
        let Some(Color::Rgb(cr, cg, cb)) = ceiling.mark_style().bg else { panic!("RGB") };
        let ceiling_distance = (cr as i32 - sr as i32).abs()
            + (cg as i32 - sg as i32).abs()
            + (cb as i32 - sb as i32).abs();
        assert!(ceiling_distance >= 40, "天井 rgb({cr},{cg},{cb}) が帯に近すぎる");
    }

    /// ブレンド率はつまみとして効く。
    #[test]
    fn the_blend_factors_move_the_colors() {
        let highlighter = Highlighter::new(None, false);
        let weak = DecorationStyles::from_theme(
            &highlighter,
            DecorationBlend { mark: 0.0, dim: 0.0 },
        );
        let strong = DecorationStyles::from_theme(
            &highlighter,
            DecorationBlend { mark: 1.0, dim: 1.0 },
        );
        let page = highlighter.theme().settings.background.unwrap();
        // 0 は「何もしない」— ページ背景そのもの / 前景そのもの。
        assert_eq!(weak.mark_style().bg, Some(Color::Rgb(page.r, page.g, page.b)));
        assert_eq!(weak.dim_fg(Some(Color::Rgb(1, 2, 3))), Color::Rgb(1, 2, 3));
        // 1 は振り切り — 前景そのもの / ページ背景そのもの。
        assert_eq!(strong.mark_style().bg, Some(highlighter.default_fg()));
        assert_eq!(
            strong.dim_fg(Some(Color::Rgb(1, 2, 3))),
            Color::Rgb(page.r, page.g, page.b)
        );
    }
}

/// `sanitize` is the boundary between the well-formed ranges akapen
/// produces and whatever `--decorations` was handed on the command line.
#[cfg(test)]
mod sanitize_tests {
    use super::*;

    #[test]
    fn sanitize_drops_only_the_unusable_ranges() {
        let source = "前重要後\n"; // 前 = 0..3, 重要 = 3..9, 後 = 9..12
        let keep = Decoration {
            range: 3..9,
            kind: DecorationKind::SemanticMark,
        };
        let decorations = vec![
            keep.clone(),
            // Mid-character on the left, on the right, past the end.
            Decoration { range: 4..9, kind: DecorationKind::Dim },
            Decoration { range: 3..7, kind: DecorationKind::Dim },
            Decoration { range: 3..9_000, kind: DecorationKind::Dim },
        ];
        assert_eq!(sanitize(&decorations, source), vec![keep]);
        assert!(sanitize(&[], source).is_empty());
    }

    /// The whole point: a sanitized list can no longer trip the
    /// UTF-8-boundary assertion inside the splitter, even in a debug
    /// build (where it would otherwise take the TUI down).
    #[test]
    fn a_sanitized_list_never_cuts_mid_character() {
        use crate::render::render;
        use crate::source::Source;

        let text = "前重要後\n";
        let source = Source::from_content("doc.md".into(), text.to_string());
        let highlighter = Highlighter::new(None, false);
        let r = render(&source, 40, &highlighter);
        let styles = DecorationStyles::from_theme(&highlighter, Default::default());
        // Every offset, boundary or not — only the usable ones survive.
        let all: Vec<Decoration> = (0..text.len())
            .flat_map(|a| {
                (a..=text.len()).map(move |b| Decoration {
                    range: a..b,
                    kind: DecorationKind::SemanticMark,
                })
            })
            .collect();
        let safe = sanitize(&all, &source.content);
        assert!(safe.len() < all.len(), "some ranges are unusable");
        // No panic, and the row's text survives unchanged.
        for i in 0..r.rows.len() {
            let (spans, _) = decorate_row(&r.rows[i], &r.row_attrs[i], &safe, &styles);
            let before: String = r.rows[i].iter().map(|s| s.text.as_str()).collect();
            let after: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(before, after);
        }
    }
}
