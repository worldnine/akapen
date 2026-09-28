//! Syntax highlighting via `syntect`, ported from herdr-reviewr's
//! `highlight.rs` (MIT, Dmitry Persiyanov) and simplified: akapen always
//! highlights markdown, uses syntect's bundled defaults instead of two-face,
//! and renders directly to ratatui `Style`s.
//!
//! The whole file is tokenized once (cross-line context like fenced code
//! blocks needs the full content); the UI then wraps and styles per line.

use std::str::FromStr;
use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use tui_markdown::Attr;
use syntect::easy::HighlightLines;
use syntect::highlighting::{ScopeSelectors, Theme, ThemeItem, ThemeSet};
use syntect::parsing::{Scope, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedLazyThemeSet;

/// Default syntax theme when the dark side has no theme (`--theme` /
/// `--theme-dark` / the config file's `[theme] dark`, see
/// [`crate::config::SyntaxThemes`]) or it is unknown (dark mode).
pub const DEFAULT_THEME: &str = "Catppuccin Mocha";
/// The light-mode counterpart (`--theme-light` / `[theme] light`) — a
/// light background must never fall back to a dark theme's pale
/// foreground colors.
pub const DEFAULT_THEME_LIGHT: &str = "Solarized (light)";

/// Markdown scope aliases: syntect's/two-face's markdown grammars emit
/// scopes like `markup.raw.code-fence.rust.markdown-gfm` and
/// `markup.heading.1.markdown`, while many third-party `.tmTheme` files
/// (tokyo-night, etc.) define their markdown colors under older Sublime
/// scope names (`markup.fenced_code.block.markdown`, `heading.1.markdown`).
/// Theme selectors match by *prefix* (`is_prefix_of`), so when a theme has
/// no rule for the emitted prefix, the legacy rule's style is duplicated
/// onto that prefix — any theme colors markdown as its author intended,
/// with no per-theme patching. The language component of a code-fence
/// scope (`.rust`) is skipped by using the prefix before it.
const MARKDOWN_SCOPE_ALIASES: &[(&str, &str)] = &[
    // (prefix the grammar emits, legacy scope themes commonly define)
    ("markup.raw.code-fence", "markup.fenced_code.block.markdown"),
    ("markup.raw.code-fence", "markup.raw.block.markdown"),
    ("markup.raw.inline", "markup.inline.raw.string.markdown"),
    (
        "meta.code-fence.definition",
        "markup.fenced_code.block.markdown",
    ),
    ("markup.heading.1.markdown", "heading.1.markdown"),
    ("markup.heading.2.markdown", "heading.2.markdown"),
    ("markup.heading.3.markdown", "heading.3.markdown"),
    ("markup.heading.4.markdown", "heading.4.markdown"),
    ("markup.heading.5.markdown", "heading.5.markdown"),
    ("markup.heading.6.markdown", "heading.6.markdown"),
];

/// Does any selector in `sel` mention a scope sharing a prefix with `target`?
fn scope_selector_mentions(sel: &ScopeSelectors, target: &Scope) -> bool {
    sel.selectors.iter().any(|s| {
        s.path
            .scopes
            .iter()
            .any(|sc| target.is_prefix_of(*sc) || sc.is_prefix_of(*target))
    })
}

/// Fold legacy markdown scope names into the theme (see
/// [`MARKDOWN_SCOPE_ALIASES`]) so themes that predate syntect's GFM
/// grammar still color fenced code and inline code.
fn apply_markdown_scope_aliases(theme: &mut Theme) {
    for (new_name, legacy_name) in MARKDOWN_SCOPE_ALIASES {
        let Ok(new_scope) = Scope::new(new_name) else {
            continue;
        };
        let Ok(legacy_scope) = Scope::new(legacy_name) else {
            continue;
        };
        // The theme already styles the new scope: leave it alone.
        if theme
            .scopes
            .iter()
            .any(|item| scope_selector_mentions(&item.scope, &new_scope))
        {
            continue;
        }
        // Duplicate the legacy rules under the new scope name.
        let copies: Vec<ThemeItem> = theme
            .scopes
            .iter()
            .filter(|item| scope_selector_mentions(&item.scope, &legacy_scope))
            .map(|item| ThemeItem {
                scope: ScopeSelectors::from_str(new_name).expect("alias selector"),
                style: item.style,
            })
            .collect();
        theme.scopes.extend(copies);
    }
}

/// The default text color when the theme carries no foreground.
const DEFAULT_FG_DARK: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const DEFAULT_FG_LIGHT: Color = Color::Rgb(0x30, 0x30, 0x40);

fn default_fg_fallback(light: bool) -> Color {
    if light { DEFAULT_FG_LIGHT } else { DEFAULT_FG_DARK }
}

/// The broad two-face syntax set, deserialized once and shared (it is
/// expensive to build). two-face carries newer grammar definitions than
/// syntect's bundled defaults, so third-party themes match better.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// The two-face embedded theme set, deserialized once and shared.
fn embedded_themes() -> &'static EmbeddedLazyThemeSet {
    static THEMES: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    THEMES.get_or_init(two_face::theme::extra)
}

/// Resolve a `--theme <name>` to an embedded two-face theme by its canonical
/// name (e.g. `Catppuccin Mocha`, `Solarized (dark)`); `None` when unknown.
fn theme_by_name(name: &str) -> Option<Theme> {
    EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| t.as_name() == name)
        .map(|t| embedded_themes().get(t).clone())
}

/// One styled text fragment: syntect's per-token color plus the display
/// styles (selection background, cursor) applied by the UI later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// One tokenized source line: the line's spans plus the source byte range
/// each of them came from, kept **parallel** (`attrs[i]` belongs to
/// `spans[i]`) — the same shape [`crate::render::Rendered`] carries for
/// the rendered view, so both modes feed [`crate::decoration::decorate_row`]
/// the same way.
///
/// # Every attribution here is exact
///
/// Source mode displays the source itself, so a span's text is by
/// construction a verbatim slice of it: syntect hands back subslices of
/// the line it was given, in order and without rewriting them. There is
/// no rendered-vs-source gap to bridge and therefore no superset range
/// — [`Highlighter::highlight_with`] only ever produces
/// `Some(Attr { exact: true })`, which
/// `source_attribution_is_exact_and_verbatim` pins.
///
/// The `Option` is kept anyway because it is the type
/// [`wrap_spans_tagged`] and `decorate_row` speak; a synthesized span
/// (the wrap's hanging pad) still has to be expressible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaggedLine {
    pub spans: Vec<Span>,
    pub attrs: Vec<Option<Attr>>,
}

/// Highlights content into per-line span vectors. The grammar is picked
/// per file ([`syntax_for`]).
pub struct Highlighter {
    theme: Theme,
    default_fg_fallback: Color,
}

/// Pick the grammar for `path` from syntect's bundled set (100+ languages):
/// by extension and filename (Makefile, Dockerfile, …), falling back to
/// plain text for unknown files.
pub fn syntax_for(path: &std::path::Path) -> &'static SyntaxReference {
    syntaxes()
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .unwrap_or_else(|| syntaxes().find_syntax_plain_text())
}

impl Highlighter {
    /// Build from a theme: an embedded two-face theme name, or a path to a
    /// `.tmTheme` file (e.g. a tokyo-night.tmTheme downloaded from a theme
    /// repo). Unknown names and unreadable files fall back to
    /// [`DEFAULT_THEME`] / [`DEFAULT_THEME_LIGHT`] matching `light`. The
    /// default grammar is markdown ([`highlight`]); source mode passes
    /// per-file grammars via [`Self::highlight_with`].
    pub fn new(theme_name: Option<&str>, light: bool) -> Self {
        let mut theme = theme_name
            .and_then(|name| {
                if name.to_ascii_lowercase().ends_with(".tmtheme") {
                    // A file path: load the theme directly from disk.
                    ThemeSet::get_theme(name).ok()
                } else {
                    theme_by_name(name)
                }
            })
            .unwrap_or_else(|| {
                let name = if light { DEFAULT_THEME_LIGHT } else { DEFAULT_THEME };
                theme_by_name(name).expect("default themes are embedded")
            });
        // Fold legacy markdown scope names in so third-party themes still
        // color fenced/inline code (the newer grammars emit newer names).
        apply_markdown_scope_aliases(&mut theme);
        Self {
            theme,
            default_fg_fallback: default_fg_fallback(light),
        }
    }

    /// The theme's default foreground (what plain text renders as).
    pub fn default_fg(&self) -> Color {
        self.theme
            .settings
            .foreground
            .map_or(self.default_fg_fallback, |c| Color::Rgb(c.r, c.g, c.b))
    }

    /// The parsed syntect theme (for serializing the code-highlighting
    /// theme and resolving scope styles).
    pub fn theme(&self) -> &syntect::highlighting::Theme {
        &self.theme
    }

    /// Resolve a single scope (e.g. `markup.heading.2.markdown`) against
    /// the theme exactly as syntect would: the best-matching rule wins, and
    /// rules apply in ascending specificity order. `None` when the theme
    /// has no rule touching `scope`.
    pub fn scope_style(&self, scope: &str) -> Option<Style> {
        use ratatui::style::{Color as TuiColor, Modifier};
        use syntect::highlighting::{FontStyle, Highlighter as SynHighlighter};
        use syntect::parsing::Scope;
        let scope = Scope::new(scope).ok()?;
        let highlighter = SynHighlighter::new(&self.theme);
        let m = highlighter.style_mod_for_stack(&[scope]);
        if m.foreground.is_none() && m.background.is_none() && m.font_style.is_none() {
            return None;
        }
        let mut style = Style::default();
        if let Some(fg) = m.foreground {
            style = style.fg(TuiColor::Rgb(fg.r, fg.g, fg.b));
        }
        if let Some(bg) = m.background {
            style = style.bg(TuiColor::Rgb(bg.r, bg.g, bg.b));
        }
        if let Some(fs) = m.font_style {
            if fs.contains(FontStyle::BOLD) {
                style = style.add_modifier(Modifier::BOLD);
            }
            if fs.contains(FontStyle::ITALIC) {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if fs.contains(FontStyle::UNDERLINE) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
        }
        Some(style)
    }

    /// Tokenize `content` with the given grammar into per-line spans
    /// (cross-line context like fenced code blocks needs the full
    /// content), each carrying the source byte range it came from. A
    /// grammar error degrades that line to a single plain span — still
    /// attributed, since the whole line is its own source.
    ///
    /// # syntect region → document byte range
    ///
    /// syntect returns each line's regions as subslices of the line, in
    /// order and covering it exactly, so a running offset is all the
    /// mapping needs: the line's start in the document plus the bytes of
    /// that line already emitted. The trailing `\n` is trimmed off the
    /// span's text, so it is trimmed off its range too — a span's range
    /// is its own text, never the newline after it.
    ///
    /// Every attribution is [`Attr::exact`]: the text IS `content[range]`
    /// (see [`TaggedLine`]).
    pub fn highlight_with(
        &self,
        content: &str,
        syntax: &'static SyntaxReference,
    ) -> Vec<TaggedLine> {
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        // Byte offset of the current line's start in `content`.
        let mut line_start = 0usize;
        for line in LinesWithEndings::from(content) {
            let mut spans = Vec::new();
            let mut attrs = Vec::new();
            // Bytes of THIS line already turned into spans — the region's
            // offset into the line, and so into the document.
            let mut consumed = 0usize;
            match h.highlight_line(line, syntaxes()) {
                Ok(regions) => {
                    for (style, text) in regions {
                        let body = text.trim_end_matches('\n');
                        let at = line_start + consumed;
                        spans.push(Span {
                            text: body.to_string(),
                            style: Style::default().fg(Color::Rgb(
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                            )),
                        });
                        attrs.push(Some(Attr::exact(at..at + body.len())));
                        consumed += text.len();
                    }
                }
                Err(_) => {
                    let body = line.trim_end_matches('\n');
                    spans.push(Span {
                        text: body.to_string(),
                        style: Style::default().fg(self.default_fg()),
                    });
                    attrs.push(Some(Attr::exact(line_start..line_start + body.len())));
                }
            }
            out.push(TaggedLine { spans, attrs });
            line_start += line.len();
        }
        out
    }
}

/// The display width of a tab: terminals expand tabs to the next 8-column
/// stop, so a tab at column 0 is 8 columns wide, at column 3 it is 5, etc.
const TAB_STOP: usize = 8;

/// Wrap `spans` into display rows no wider than `width` columns, without
/// attribution — the caller has no source ranges to track (a deleted
/// block's baseline text, a row-count cache).
///
/// A thin wrapper over [`wrap_spans_tagged`] with no attribution and no
/// hanging indent, which is exactly this function's old body: the wrap
/// algorithm (unicode widths, tab expansion at 8-column stops, the
/// narrow-pane retry) lives there and only there, so the untagged and
/// tagged paths can never drift apart.
pub fn wrap_spans(spans: &[Span], width: usize) -> Vec<Vec<Span>> {
    let attrs = vec![None; spans.len()];
    wrap_spans_tagged(spans, &attrs, width, 0)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}


/// The narrowest continuation row a hanging indent may leave: below this
/// many columns the indent is dropped and the line wraps from column 0
/// (a sliver of one-or-two-column rows would be unreadable, and the wrap
/// loop needs room to always make progress).
const MIN_HANGING_BODY: usize = 8;

/// Width-aware wrapping that keeps a parallel source attribution.
/// Takes one [`Attr`] per input span and returns, per display row, the
/// row's spans plus one attribution per span.
///
/// Widths are measured with `unicode-width`, so CJK full-width
/// characters never misalign. Tabs are expanded to the spaces a terminal
/// would show (8-column stops from the row's current column): ratatui's
/// cell grid measures `\t` as width 0 and its renderer drops control
/// characters, so a raw tab would misalign every following character and
/// lose the selection/cursor background on the expansion. An empty input
/// yields one empty row (a blank source line stays a row).
///
/// **The only wrap implementation.** Both the rendered view and source
/// mode wrap through here — source mode passes the attribution
/// [`Highlighter::highlight_with`] produced and `hang = 0`, the rendered
/// view its own plus a hanging indent — and [`wrap_spans`] is this
/// function with no attribution and no indent. There is no second copy
/// of the wrap math to keep in step.
///
/// # How a split fragment inherits its range
///
/// - An **exact** span (`span.text == source[range]`, see [`Attr`]) is
///   sub-sliced by byte offset: fragment `k` bytes in, `n` bytes long,
///   gets `range.start + k .. range.start + k + n`. It stays exact unless
///   tab expansion rewrote the fragment's text, which keeps the range and
///   drops the verbatim claim.
/// - A **superset** span cannot be cut (its text is not a copy of its
///   range), so every fragment keeps the span's whole range.
///
/// Either way the invariant a range decoration depends on holds: **a
/// fragment's range is always a subset of its span's range**, asserted
/// below.
///
/// `hang` is the hanging indent: continuation rows (the second and later
/// display rows of a wrapped line) start with `hang` columns of spaces,
/// so a list item's continuation aligns under its text instead of under
/// the marker. The pad span inherits the attribution of the text that
/// follows it — demoted to a superset, since spaces are not source text —
/// so selection highlighting and mouse mapping treat the pad as part of
/// the line. A `hang` that would leave the continuation body narrower
/// than [`MIN_HANGING_BODY`] falls back to 0 (plain wrapping).
pub fn wrap_spans_tagged(
    spans: &[Span],
    attrs: &[Option<Attr>],
    width: usize,
    hang: usize,
) -> Vec<(Vec<Span>, Vec<Option<Attr>>)> {
    debug_assert_eq!(spans.len(), attrs.len(), "attribution parallels spans");
    let width = width.max(1);
    let hang = if width.saturating_sub(hang) < MIN_HANGING_BODY {
        0
    } else {
        hang
    };
    let mut rows: Vec<(Vec<Span>, Vec<Option<Attr>>)> = Vec::new();
    let mut row: Vec<Span> = Vec::new();
    let mut row_attrs: Vec<Option<Attr>> = Vec::new();
    let mut col = 0usize; // display column where the next character lands
    // A continuation row owes its hanging pad; materialized lazily when
    // the first content lands (so a line ending exactly at a row boundary
    // never leaves a trailing pad-only row).
    let mut pad_due = false;
    for (span, attr) in spans.iter().zip(attrs) {
        let mut rest = span.text.as_str();
        // Bytes of THIS span already emitted, i.e. the fragment's offset
        // into `span.text` — and, for an exact attribution, into its
        // source range.
        let mut consumed = 0usize;
        while !rest.is_empty() {
            // The current row is full: flush it and start the next.
            if col >= width {
                rows.push((std::mem::take(&mut row), std::mem::take(&mut row_attrs)));
                col = hang;
                pad_due = hang > 0;
            }
            let (take, take_w) = take_fit(rest, col, width - col);
            if take.is_empty() {
                // The next character cannot fit in the rest of this row
                // (a wide char at the row's last column, or a tab): flush
                // and retry from the continuation column, where it fits
                // (MIN_HANGING_BODY guarantees at least 8 free columns).
                rows.push((std::mem::take(&mut row), std::mem::take(&mut row_attrs)));
                col = hang;
                pad_due = hang > 0;
                continue;
            }
            let text = expand_tabs(take, col);
            let frag = attr.as_ref().map(|a| {
                if a.exact {
                    a.slice(consumed, consumed + take.len(), text == take)
                } else {
                    a.clone()
                }
            });
            if pad_due {
                row.push(Span {
                    text: " ".repeat(hang),
                    style: Style::default(),
                });
                // The pad is synthesized whitespace, never source text:
                // it takes the span's position but not its exactness.
                row_attrs.push(attr.as_ref().map(Attr::demoted));
                pad_due = false;
            }
            row.push(Span {
                text,
                style: span.style,
            });
            debug_assert!(
                match (&frag, attr) {
                    (Some(f), Some(a)) => f.range.start >= a.range.start && f.range.end <= a.range.end,
                    (None, _) => true,
                    _ => false,
                },
                "a fragment's range must stay inside its span's range"
            );
            row_attrs.push(frag);
            col += take_w;
            consumed += take.len();
            rest = &rest[take.len()..];
            if col >= width {
                rows.push((std::mem::take(&mut row), std::mem::take(&mut row_attrs)));
                col = hang;
                pad_due = hang > 0;
            }
        }
    }
    if !row.is_empty() {
        rows.push((row, row_attrs));
    }
    if rows.is_empty() {
        rows.push((Vec::new(), Vec::new()));
    }
    rows
}

/// The longest prefix of `s` that fits in `avail` columns, given that the
/// row already holds `col` columns. Returns the prefix and the display
/// width it occupies (tabs measured at 8-column stops).
///
/// A first character wider than `avail` is still taken when the row is
/// EMPTY (narrow-pane safety, so the loop always makes progress); in a
/// non-empty row it returns an empty prefix instead — the caller flushes
/// the row and retries from column 0, where a wide char or a line-leading
/// tab fits without overflowing the row.
fn take_fit(s: &str, col: usize, avail: usize) -> (&str, usize) {
    let mut w = 0usize;
    let mut last = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = char_width(ch, col + w);
        if w + cw > avail {
            if last != 0 {
                // The row is full: leave the rest (and this character) for
                // the next row.
                break;
            }
            if col > 0 {
                // The next character cannot fit in the rest of a NON-EMPTY
                // row (a wide char at the row's last column, or a tab):
                // flush the row and retry from column 0. Taking it here
                // would overflow the row by one cell — the old code took
                // it whenever the current call had taken nothing yet, so a
                // wide char at the exact boundary pushed the row past the
                // pane width and the text collided with the scrollbar.
                return ("", 0);
            }
            // A first character wider than an EMPTY row (narrow-pane
            // safety): take it anyway, so the loop always makes progress.
        }
        w += cw;
        last = i + ch.len_utf8();
    }
    if last == 0 {
        // Unreachable with the loop above (the first character is always
        // taken), kept as a safety net: take the first character.
        let ch = s.chars().next().expect("non-empty input");
        w = char_width(ch, col);
        last = ch.len_utf8();
    }
    (&s[..last], w)
}

/// The display width of `ch` at column `col` of a row: unicode-width for
/// regular characters, the next 8-column stop for tabs.
fn char_width(ch: char, col: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    if ch == '\t' {
        TAB_STOP - col % TAB_STOP
    } else {
        ch.width().unwrap_or(0)
    }
}

/// Replace tabs in `s` with the spaces a terminal would display (8-column
/// stops measured from `col`), so the returned text occupies exactly the
/// cells the wrap math counted. Text without tabs is returned as-is.
fn expand_tabs(s: &str, col: usize) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    use unicode_width::UnicodeWidthChar;
    let mut out = String::with_capacity(s.len());
    let mut c = col;
    for ch in s.chars() {
        if ch == '\t' {
            let pad = TAB_STOP - c % TAB_STOP;
            out.push_str(&" ".repeat(pad));
            c += pad;
        } else {
            out.push(ch);
            c += ch.width().unwrap_or(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_THEME, Highlighter, Span, syntax_for, wrap_spans, wrap_spans_tagged};
    use std::path::Path;
    use ratatui::style::{Color, Style};
    use unicode_width::UnicodeWidthStr;

    /// Display width of a string, in terminal columns (test helper).
    fn width(s: &str) -> usize {
        s.width()
    }

    #[test]
    fn highlights_markdown_into_colored_spans() {
        let h = Highlighter::new(Some(DEFAULT_THEME), false);
        let lines = h.highlight_with("# Heading\n\n**bold**\n", syntax_for(Path::new("x.md")));
        assert_eq!(lines.len(), 3);
        // Heading line tokenizes (markdown header), not a single plain span.
        assert!(!lines[0].spans.is_empty());
        let joined: String = lines[2].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "**bold**");
    }

    #[test]
    fn plain_lines_carry_the_default_foreground() {
        let h = Highlighter::new(None, false);
        let lines = h.highlight_with("plain\n", syntax_for(Path::new("x.md")));
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].text, "plain");
    }

    #[test]
    fn unknown_theme_falls_back_matching_light_dark() {
        use super::{DEFAULT_THEME_LIGHT, theme_by_name};
        // An unresolvable --theme on a light background must fall back to
        // the light default, not to dark pale-on-light unreadability.
        let dark = Highlighter::new(Some("no-such-theme"), false);
        let light = Highlighter::new(Some("no-such-theme"), true);
        assert_eq!(
            dark.theme().settings.background,
            theme_by_name(DEFAULT_THEME).unwrap().settings.background
        );
        assert_eq!(
            light.theme().settings.background,
            theme_by_name(DEFAULT_THEME_LIGHT).unwrap().settings.background
        );
    }

    /// A minimal .tmTheme (plist XML) whose foreground is bright red.
    const MINIMAL_TM_THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key>
  <string>Minimal</string>
  <key>settings</key>
  <array>
    <dict>
      <key>settings</key>
      <dict>
        <key>foreground</key>
        <string>#ff0000</string>
      </dict>
    </dict>
  </array>
</dict>
</plist>
"#;

    #[test]
    fn theme_loads_from_tmtheme_file_path() {
        // A `--theme /path/to/theme.tmTheme` must load the file directly.
        let dir = std::env::temp_dir().join(format!("akapen-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("minimal.tmTheme");
        std::fs::write(&path, MINIMAL_TM_THEME).unwrap();
        let h = Highlighter::new(Some(path.to_str().unwrap()), false);
        // The file's foreground (#ff0000) wins over the defaults.
        assert_eq!(h.default_fg(), Color::Rgb(0xff, 0x00, 0x00));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A theme that only knows the legacy `markup.fenced_code.block.markdown`
    /// scope name (what syntect's GFM grammar no longer emits).
    const LEGACY_TM_THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key>
  <string>Legacy</string>
  <key>settings</key>
  <array>
    <dict>
      <key>settings</key>
      <dict>
        <key>foreground</key>
        <string>#a9b1d6</string>
      </dict>
    </dict>
    <dict>
      <key>scope</key>
      <string>markup.fenced_code.block.markdown</string>
      <key>settings</key>
      <dict>
        <key>foreground</key>
        <string>#ff0000</string>
      </dict>
    </dict>
  </array>
</dict>
</plist>
"#;

    #[test]
    fn legacy_markdown_code_scope_is_aliased() {
        // syntect's GFM grammar emits `markup.raw.code-fence.markdown`;
        // a theme defining only the legacy name must still color the
        // fenced code (the alias duplicates the rule onto the new scope).
        let dir = std::env::temp_dir().join(format!("akapen-alias-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.tmTheme");
        std::fs::write(&path, LEGACY_TM_THEME).unwrap();
        let h = Highlighter::new(Some(path.to_str().unwrap()), false);
        let lines = h.highlight_with("```\ncode\n```\n", syntax_for(Path::new("x.md")));
        // The opening fence carries the aliased code color.
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn syntax_for_picks_the_language_by_file() {
        use std::path::Path;
        // Known extensions resolve to their grammars.
        assert_eq!(syntax_for(Path::new("x.rs")).name, "Rust");
        assert_eq!(syntax_for(Path::new("x.toml")).name, "TOML");
        assert_eq!(syntax_for(Path::new("Makefile")).name, "Makefile");
        // Markdown stays markdown.
        assert!(syntax_for(Path::new("x.md")).name.contains("Markdown"));
        // Unknown files fall back to plain text (never panics).
        assert_eq!(syntax_for(Path::new("x.unknown-ext")).name, "Plain Text");
        assert_eq!(syntax_for(Path::new("noext")).name, "Plain Text");
    }

    #[test]
    fn unknown_theme_name_falls_back_to_default() {
        // An unknown name must render exactly like the bundled default.
        let h = Highlighter::new(Some("tokyo-night"), false);
        let fallback = Highlighter::new(None, false);
        assert_eq!(h.default_fg(), fallback.default_fg());
    }

    #[test]
    fn missing_tmtheme_path_falls_back_to_default() {
        let h = Highlighter::new(Some("/nonexistent/theme.tmTheme"), false);
        let fallback = Highlighter::new(None, false);
        assert_eq!(h.default_fg(), fallback.default_fg());
    }

    #[test]
    fn unknown_theme_falls_back_to_default() {
        let h = Highlighter::new(Some("no-such-theme"), false);
        assert_eq!(h.default_fg(), Highlighter::new(None, false).default_fg());
    }

    fn span(text: &str) -> Span {
        Span {
            text: text.to_string(),
            style: Style::default().fg(Color::White),
        }
    }

    #[test]
    fn wrap_keeps_rows_within_width_and_preserves_text() {
        let rows = wrap_spans(&[span("abcde"), span("fgh")], 4);
        // "abcd" | "efgh" — the second row exactly fills: e joins fgh.
        assert_eq!(rows.len(), 2);
        let joined: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "abcdefgh");
        assert!(
            rows.iter()
                .all(|r| width(&r.iter().map(|s| s.text.as_str()).collect::<String>()) <= 4)
        );
    }

    #[test]
    fn wrap_splits_wide_chars_never_in_half() {
        let rows = wrap_spans(&[span("あいうえお")], 4);
        // あいう(6 cols) fits 4? No: 2+2+2 > 4 → あ(2) い(2) → next row う(2) え(2) → お(2)
        let joined: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "あいうえお");
        assert!(
            rows.iter()
                .all(|r| width(&r.iter().map(|s| s.text.as_str()).collect::<String>()) <= 4)
        );
        // Each row's text is whole characters only.
        for r in &rows {
            let t: String = r.iter().map(|s| s.text.as_str()).collect();
            assert!(t.chars().all(|c| "あいうえお".contains(c)));
        }
    }

    #[test]
    fn wrap_wide_char_at_the_row_boundary_never_overflows() {
        // Regression: a wide char landing exactly at the last column of a
        // row (here: あい = 4 cells at width 5, う cannot fit in the
        // remaining 1) used to be taken anyway, producing a 6-cell row
        // that collided with the scrollbar column. Rows must never exceed
        // the width.
        for (text, pane) in [
            ("あいうえお", 5),
            ("これは日本語の文章です", 21),
            ("ab日本語cd", 5),
            ("日本語abc日本語", 7),
        ] {
            let rows = wrap_spans(&[span(text)], pane);
            let joined: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, text, "text is preserved at width {pane}");
            for r in &rows {
                let t: String = r.iter().map(|s| s.text.as_str()).collect();
                assert!(
                    width(&t) <= pane,
                    "row {t:?} exceeds width {pane} at {text:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_mixed_ascii_and_cjk() {
        let rows = wrap_spans(&[span("ab日本語cd")], 6);
        let joined: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "ab日本語cd");
        assert!(
            rows.iter()
                .all(|r| width(&r.iter().map(|s| s.text.as_str()).collect::<String>()) <= 6)
        );
    }

    #[test]
    fn wrap_expands_tabs_to_terminal_columns() {
        // A line-leading tab renders as 8 spaces; a tab after two characters
        // pads to the next 8-column stop — exactly what a terminal shows.
        let row: String = wrap_spans(&[span("\tfoo")], 20)[0]
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(row, "        foo");
        assert!(!row.contains('\t'), "rows never carry raw tabs");

        let row: String = wrap_spans(&[span("ab\tc")], 20)[0]
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(row, "ab      c", "tab after 2 cols pads to col 8");
        assert_eq!(width(&row), 9);
    }

    #[test]
    fn wrap_overflowing_tab_starts_the_next_row() {
        // "a\tb" in a 3-col pane: the tab at col 1 needs 7 columns and
        // cannot fit, so the row ends before it and the tab starts the next
        // row from column 0 (8 spaces even in a narrow pane — documented
        // narrow-pane safety, same as a wide char).
        let rows = wrap_spans(&[span("a\tb")], 3);
        let joined: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(joined, vec!["a", "        ", "b"]);
    }

    #[test]
    fn wrap_measures_rows_at_the_expanded_tab_width() {
        // "abc\tde" is 3 + 5 (tab at col 3 -> col 8) + 2 = 10 columns and
        // fills a 10-col row exactly, so the trailing "fgh" wraps. The
        // wrap boundary is decided by the EXPANDED width, not the raw text.
        let rows = wrap_spans(&[span("abc\tdefgh")], 10);
        let joined: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(joined, vec!["abc     de", "fgh"]);
    }

    #[test]
    fn wrap_mixed_cjk_and_tabs() {
        // あ (2 cols, col 0-1), a tab at col 2 -> 6 spaces, then い (2).
        let rows = wrap_spans(&[span("あ\tい")], 12);
        let joined: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "あ      い");
        assert!(
            rows.iter()
                .all(|r| width(&r.iter().map(|s| s.text.as_str()).collect::<String>()) <= 12)
        );
    }

    #[test]
    fn wrap_empty_and_narrow_pane() {
        assert_eq!(
            wrap_spans(&[], 10),
            vec![vec![]],
            "empty line is one empty row"
        );
        let rows = wrap_spans(&[span("あ")], 1);
        assert_eq!(rows.len(), 1, "a wide char still fits a width-1 pane");
    }

    use tui_markdown::Attr;

    /// Text of one tagged row, pad included.
    fn row_text(row: &(Vec<Span>, Vec<Option<Attr>>)) -> String {
        row.0.iter().map(|s| s.text.as_str()).collect()
    }

    /// An exact attribution over `range`, for wrap tests whose spans are
    /// stand-ins rather than real renderer output.
    fn at(range: std::ops::Range<usize>) -> Option<Attr> {
        Some(Attr::exact(range))
    }

    #[test]
    fn hanging_wrap_indents_continuation_rows() {
        let rows = wrap_spans_tagged(&[span("- abcdefghijklmn")], &[at(100..116)], 10, 2);
        assert_eq!(
            rows.iter().map(row_text).collect::<Vec<_>>(),
            vec!["- abcdefgh", "  ijklmn"]
        );
        // The pad inherits the span's position: selection highlighting
        // and mouse mapping treat it as part of the item. It is not
        // source text, so it keeps the whole range as a superset; the
        // fragment beside it is sliced exactly.
        assert_eq!(
            rows[1].1,
            vec![Some(Attr::inexact(100..116)), at(110..116)]
        );
        assert_eq!(rows[0].1, vec![at(100..110)]);
    }

    #[test]
    fn hanging_wrap_measures_cjk_by_display_width() {
        let rows = wrap_spans_tagged(&[span("- あいうえおかきくけこ")], &[at(0..32)], 10, 2);
        let texts: Vec<String> = rows.iter().map(row_text).collect();
        assert_eq!(texts, vec!["- あいうえ", "  おかきく", "  けこ"]);
        assert!(texts.iter().all(|t| width(t) <= 10));
        // Stripping the pads reassembles the original text.
        let joined: String = std::iter::once(texts[0].as_str())
            .chain(texts[1..].iter().map(|t| &t[2..]))
            .collect();
        assert_eq!(joined, "- あいうえおかきくけこ");
    }

    #[test]
    fn hanging_wrap_falls_back_when_the_body_would_be_too_narrow() {
        // width 12, hang 6 leaves 6 < MIN_HANGING_BODY columns: plain wrap.
        let rows = wrap_spans_tagged(&[span("- [x] abcdefghijkl")], &[at(0..18)], 12, 6);
        assert_eq!(
            rows.iter().map(row_text).collect::<Vec<_>>(),
            vec!["- [x] abcdef", "ghijkl"]
        );
    }

    #[test]
    fn hanging_wrap_leaves_no_trailing_pad_only_row() {
        // The text ends exactly at the row boundary: the owed pad must
        // never materialize as a spurious empty continuation row.
        let rows = wrap_spans_tagged(&[span("- abcdefgh")], &[at(0..10)], 10, 2);
        assert_eq!(rows.iter().map(row_text).collect::<Vec<_>>(), vec!["- abcdefgh"]);
    }

    #[test]
    fn hanging_wrap_with_zero_hang_matches_plain_wrap() {
        let spans = [span("abcde"), span("fgh")];
        let tagged = wrap_spans_tagged(&spans, &[at(0..5), at(5..8)], 4, 0);
        let plain = wrap_spans(&spans, 4);
        assert_eq!(
            tagged.iter().map(row_text).collect::<Vec<_>>(),
            plain
                .iter()
                .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
        );
    }
    // ---- source-mode attribution (source view の range) ----

    /// Every source line's spans are attributed, every attribution is
    /// EXACT, and `content[range]` IS the span's text — the invariant the
    /// whole source-mode decoration path rests on. Checked over markdown
    /// AND Rust so the assertion does not depend on one grammar's
    /// tokenization.
    #[test]
    fn source_attribution_is_exact_and_verbatim() {
        let h = Highlighter::new(Some(DEFAULT_THEME), false);
        for (content, file) in [
            ("# 見出し\n\n本文 **強調** と `code`\n\n- 項目\n", "x.md"),
            ("fn main() {\n\tlet x = 1; // コメント\n}\n", "x.rs"),
            ("```rust\nlet y = 2;\n```\n", "x.md"),
        ] {
            let lines = h.highlight_with(content, syntax_for(Path::new(file)));
            for line in &lines {
                assert_eq!(
                    line.spans.len(),
                    line.attrs.len(),
                    "attribution runs parallel to the spans"
                );
                for (sp, attr) in line.spans.iter().zip(&line.attrs) {
                    let attr = attr.as_ref().expect("source mode attributes every span");
                    assert!(attr.exact, "source spans are verbatim source, so exact");
                    assert_eq!(
                        &content[attr.range.clone()],
                        sp.text,
                        "content[range] IS the span's text"
                    );
                }
            }
        }
    }

    /// The ranges tile the document: line by line, span by span, they run
    /// forward and leave only the line terminators uncovered. A drifting
    /// offset (a `\n` counted into a span, a line's start taken from the
    /// wrong place) would show up here even where the text still matched.
    #[test]
    fn source_ranges_advance_through_the_document() {
        let content = "alpha\n\n日本語の行\nlast";
        let h = Highlighter::new(Some(DEFAULT_THEME), false);
        let lines = h.highlight_with(content, syntax_for(Path::new("x.md")));
        assert_eq!(lines.len(), 4, "one entry per source line");
        let mut prev_end = 0usize;
        for line in &lines {
            for attr in line.attrs.iter().flatten() {
                assert!(
                    attr.range.start >= prev_end,
                    "ranges never go backwards: {:?} after {prev_end}",
                    attr.range
                );
                prev_end = attr.range.end;
            }
        }
        // The last line has no terminator, so its range ends at EOF.
        assert_eq!(prev_end, content.len());
        // The third line starts after "alpha\n" + "\n".
        let third = lines[2].attrs[0].as_ref().unwrap();
        assert_eq!(third.range.start, "alpha\n\n".len());
        assert_eq!(&content[third.range.clone()], "日本語の行");
    }

    /// A blank line is one empty, exact span at the line's own offset —
    /// not a missing entry and not the newline. `decorate_row` keeps such
    /// a span as it is, so a blank row stays countable.
    #[test]
    fn a_blank_source_line_is_an_empty_exact_span() {
        let content = "a\n\nb\n";
        let h = Highlighter::new(Some(DEFAULT_THEME), false);
        let lines = h.highlight_with(content, syntax_for(Path::new("x.md")));
        let attrs: Vec<_> = lines[1].attrs.iter().flatten().collect();
        assert!(!attrs.is_empty(), "the blank line is still attributed");
        for a in attrs {
            assert!(a.exact && a.range.is_empty());
            assert_eq!(a.range.start, 2, "at the blank line's own offset");
        }
    }

    /// Wrapping a source line keeps the fragments verbatim: every
    /// fragment that is still exact slices the source to its own text,
    /// at any width, in Japanese / emoji / full-width text. This is the
    /// byte-offset-vs-terminal-column trap the design doc names.
    #[test]
    fn wrapped_source_fragments_stay_verbatim() {
        let content = "日本語の見出し\n絵文字 🎉🎉 と ＡＢＣ 全角\nplain ascii line\n";
        let h = Highlighter::new(Some(DEFAULT_THEME), false);
        let lines = h.highlight_with(content, syntax_for(Path::new("x.md")));
        for w in [3usize, 4, 5, 7, 12, 40] {
            for line in &lines {
                for (row, attrs) in wrap_spans_tagged(&line.spans, &line.attrs, w, 0) {
                    for (frag, attr) in row.iter().zip(&attrs) {
                        let attr = attr.as_ref().expect("a wrapped source fragment keeps its range");
                        if attr.exact {
                            assert_eq!(
                                &content[attr.range.clone()],
                                frag.text,
                                "exact fragment at width {w}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The one place a source fragment is NOT verbatim: a tab is expanded
    /// to the spaces the terminal shows, so the fragment's text stops
    /// being `source[range]`. `wrap_spans_tagged` keeps the range and
    /// drops the verbatim claim (`Attr::slice`'s `still_verbatim`), which
    /// moves that fragment onto `decorate_row`'s SUPERSET branch: a
    /// decoration covering it whole still lands, one ending INSIDE it
    /// decorates nothing. That is the accepted cost of expanding tabs —
    /// the alternative is a fragment claiming to be source text it has
    /// rewritten, which would bleed a decoration onto its neighbours.
    ///
    /// The invariant pinned here is the general one: **a fragment is
    /// exact exactly when its text is still `source[range]`.**
    #[test]
    fn a_tab_expanded_fragment_keeps_its_range_and_loses_exactness() {
        let h = Highlighter::new(None, false);
        let syntax = syntax_for(Path::new("x.txt"));
        for (content, expect_demotion) in [("\tfoo bar\n", true), ("ab\tc\n", true), ("foo bar\n", false)] {
            let lines = h.highlight_with(content, syntax);
            // Before the wrap every span is verbatim, tab and all.
            for (sp, attr) in lines[0].spans.iter().zip(&lines[0].attrs) {
                let attr = attr.as_ref().unwrap();
                assert!(attr.exact);
                assert_eq!(&content[attr.range.clone()], sp.text);
            }
            let rows = wrap_spans_tagged(&lines[0].spans, &lines[0].attrs, 40, 0);
            let mut demoted = false;
            for (row, attrs) in &rows {
                for (frag, attr) in row.iter().zip(attrs) {
                    let attr = attr.as_ref().expect("a wrapped fragment keeps its range");
                    let verbatim = content[attr.range.clone()] == frag.text;
                    assert_eq!(
                        attr.exact, verbatim,
                        "exactness tracks verbatim-ness for {frag:?} in {content:?}"
                    );
                    // The POSITION survives either way: only the claim
                    // about the text was dropped.
                    assert!(attr.range.end <= content.len());
                    demoted |= !attr.exact;
                }
            }
            assert_eq!(
                demoted, expect_demotion,
                "only a tab line demotes a fragment: {content:?}"
            );
            let joined: String = rows[0].0.iter().map(|s| s.text.as_str()).collect();
            assert!(!joined.contains('\t'), "the tab was expanded: {joined:?}");
        }
    }

    /// The superset branch of `decorate_row`'s intersection rule is
    /// essentially DEAD CODE for source mode — which is the claim
    /// `docs/design/range-attribution-plan.md` makes when it calls source view
    /// "比較的単純". Swept over the real fixtures at several widths:
    /// every wrapped fragment is exact, with the single documented
    /// exception of a line carrying a tab (see
    /// `a_tab_expanded_fragment_keeps_its_range_and_loses_exactness`).
    ///
    /// The rendered view has the opposite balance — emphasis, entities,
    /// softbreaks and table cells all produce supersets — so this is the
    /// real difference between the two modes' attribution, pinned rather
    /// than assumed.
    #[test]
    fn source_wrapping_reaches_the_superset_branch_only_through_tabs() {
        let h = Highlighter::new(Some(DEFAULT_THEME), false);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0usize;
        let mut tab_lines = 0usize;
        for name in [
            "testdata/a-readme.md",
            "testdata/b-design.md",
            "testdata/c-impl.rs",
            "testdata/full.md",
        ] {
            let path = root.join(name);
            let content = std::fs::read_to_string(&path).unwrap();
            let lines = h.highlight_with(&content, syntax_for(&path));
            for (line, src) in lines.iter().zip(content.split_inclusive('\n')) {
                let has_tab = src.contains('\t');
                tab_lines += usize::from(has_tab);
                for w in [8usize, 17, 40, 80] {
                    for (row, attrs) in wrap_spans_tagged(&line.spans, &line.attrs, w, 0) {
                        for (frag, attr) in row.iter().zip(&attrs) {
                            let attr =
                                attr.as_ref().expect("every source fragment is attributed");
                            if !has_tab {
                                assert!(
                                    attr.exact,
                                    "a tab-free source fragment is never a superset: \
                                     {frag:?} in {name} at width {w}"
                                );
                            }
                            // Whatever the exactness, the POSITION is
                            // always a real slice of the document.
                            assert!(attr.range.end <= content.len());
                            if attr.exact {
                                assert_eq!(content[attr.range.clone()], frag.text);
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 10_000, "the sweep covered the corpus: {checked}");
        assert!(tab_lines > 0, "the corpus contains tab lines to exempt");
    }

    /// `wrap_spans` IS `wrap_spans_tagged` with no attribution and no
    /// hanging indent — the delegation that keeps one wrap
    /// implementation. Pinned over the cases the wrap loop branches on:
    /// wide characters at the row edge, tabs, and an empty input.
    #[test]
    fn plain_wrap_equals_the_tagged_wrap_it_delegates_to() {
        let cases: Vec<Vec<Span>> = vec![
            vec![span("abcdefghij")],
            vec![span("日本語のテキスト")],
            vec![span("ab"), span("日本語"), span("cd")],
            vec![span("\tfoo"), span("ab\tc")],
            vec![span("🎉🎉🎉"), span("ＡＢＣ")],
            vec![span("")],
            vec![],
        ];
        for spans in &cases {
            let attrs: Vec<Option<Attr>> = vec![None; spans.len()];
            for w in [1usize, 2, 3, 5, 8, 80] {
                let plain = wrap_spans(spans, w);
                let tagged = wrap_spans_tagged(spans, &attrs, w, 0);
                assert_eq!(
                    plain.len(),
                    tagged.len(),
                    "same row count at width {w} for {spans:?}"
                );
                for (p, t) in plain.iter().zip(&tagged) {
                    assert_eq!(p, &t.0, "same spans at width {w} for {spans:?}");
                }
            }
        }
    }
}
