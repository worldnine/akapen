//! Range decoration: styling an arbitrary SOURCE byte range on top of a
//! rendered document (Phase 2 of `docs/design/range-attribution-plan.md`).
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
//! The superset rule makes an attributed range's WIDTH load-bearing: a
//! span is decorated only by a decoration that reaches its range's last
//! byte, so one byte too many in the attribution makes the span
//! undecoratable forever. A list item's `- ` marker used to be
//! attributed to the whole item — `pulldown_cmark`'s `Start(Item)`
//! range, trailing newline included — while `atomize` stopped the
//! Atom before that newline, and the marker stayed bright under a
//! dimmed item. Nothing here was wrong: the renderer now anchors the
//! marker on its OWN bytes, so a decoration over the item reaches it.
//! When a new synthesized span is attributed, attribute it to the
//! narrowest source it really stands for, not to its event's range.
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
//! # The mark's SHAPE is being chosen on the real terminal (temporary)
//!
//! `AKAPEN_MARK_STYLE` picks one of [`MarkVariant`]'s looks (see
//! [`MarkVariant::from_env`]). It exists so six candidates can be put side
//! by side in herdr panes and chosen by eye, and **it is not a setting**:
//! the one that wins gets written in as the only look and this switch (and
//! the losing variants) are deleted. Unset means [`MarkVariant::Amber`],
//! which is the shipped look, byte for byte.
//!
//! [`Rendered`]: crate::render::Rendered

use std::ops::Range;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
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
    /// **`AKAPEN_MARK_STYLE=f` の 2 段目** — 光った Unit 全体に敷く、核より
    /// 薄い琥珀の地。
    ///
    /// [`DecorationKind::SemanticMark`] と同じ背景のチャンネルを使うが、
    /// **別の kind にしてある**のは重ねたときの勝ち負けを順序で決めるため
    /// である — `decorate_row` は slice 順に patch するので、薄い地を先に、
    /// 核を後に置けば濃い側が勝つ。**既定では 1 つも出ない。**
    SemanticMarkFaint,
    /// "This can be skimmed": the foreground moved toward the page.
    Dim,
    /// **「ここを直せ」** — Review の候補（`R`）。細い下線だけを引く。
    ///
    /// marks の琥珀（[`DecorationKind::SemanticMark`]）と**別の機構**で
    /// ある。地色でも前景でもなく**下線**なのは、Review と marks が同時に
    /// 出るからで、候補の上に琥珀が乗っても両方読める必要がある
    /// （地色を 2 つ重ねると後勝ちで片方が消える）。
    ///
    /// 色は [`REVIEW_TINT`]。
    ReviewCandidate,
}

/// Where the mark background travels TO: a saturated amber, the color a
/// yellow highlighter leaves on paper.
///
/// The mark used to be lifted toward the theme's own FOREGROUND, which
/// produced a hueless gray (`rgb(68,70,89)` on Catppuccin Mocha). With the
/// whole document shown that gray was reported as "nothing to catch on": a marked
/// passage and an unmarked one differed only in lightness, and lightness
/// alone is what the selection band and the history glow already use.
/// Hue is the channel nothing else in the UI spends, so the mark spends
/// it — see `docs/design/reading-research.md` on Scim, where the colors
/// are the device the reader builds a correspondence with.
///
/// ONE destination for every theme, not a light/dark pair: the theme's
/// own paper is the origin, so a dark theme lands on a dark amber and a
/// light theme on a pale yellow, from the same formula. That is the
/// property the previous version had and the reason to keep blending
/// rather than writing a color in — see [`mark_background`].
const MARK_TINT: Color = Color::Rgb(0xff, 0xb0, 0x00);

/// **Review の下線が向かう先** — 冷たい青緑。
///
/// [`MARK_TINT`] の琥珀と**色相が最も遠い側**である。marks が「読め」で
/// Review が「直せ」という別の意味を持つので、読み手が作る対応づけも
/// 別でなければならない（`docs/design/reading-research.md` の Scim —
/// 色は読み手が対応づけを作る道具である）。
///
/// **新しい色を作ってはいない。** 青緑は akapen が既に選択と composer に
/// 使っている色相で（[`ratatui::style::Color::Cyan`]）、フッタの案内・
/// 一覧のカーソル・選択の帯がずっとこの系統である。ここが足しているのは
/// 「この色相を本文の下線にも使う」という 1 点だけである。
///
/// **紙から作る**のは琥珀とまったく同じ理由で、テーマ側が持っている色を
/// 読まない（[`mark_background`]「ONE path, and it always runs」）。
/// 暗いテーマでは暗い紙から、明るいテーマでは白い紙から、同じ式で寄せる
/// ので、`--light` でも `--theme` を変えても下線は読める。
const REVIEW_TINT: Color = Color::Rgb(0x00, 0xb4, 0xc8);

/// 下線が紙から [`REVIEW_TINT`] へどれだけ寄るか。
///
/// 高いのは、**下線は 1 ピクセルの線だから**である。地色（
/// [`MARK_BG_BLEND`] = 0.27）は面積が広いので薄くてよいが、線を同じ
/// 薄さで引くと明るいテーマではほとんど見えない。[`TICK_BLEND`] と
/// 同じ側の値で、あちらも 1 桁の点を打つための値である。
const REVIEW_BLEND: f32 = 0.80;

/// 目盛りと `FOCUS` バッジの琥珀が、紙から [`MARK_TINT`] へどれだけ寄るか。
///
/// マークの**背景**（[`DecorationStyles::mark_bg`] = [`MARK_BG_BLEND`]）は
/// 地色として設計された値で、`rgb(90,69,33)` を**前景**として溝に 1 桁
/// 置くとほとんど見えない。目盛りには別の明るさが要る — ただし色相は
/// 同じ琥珀なので、マーカーと目盛りが同じものを指していることは読める。
const TICK_BLEND: f32 = 0.75;

/// The background of [`DecorationKind::SemanticMark`] for a theme that
/// carries no background of its own. Split light/dark by the theme
/// foreground's brightness.
///
/// These are exactly what [`MARK_BG_BLEND`] toward [`MARK_TINT`] produces
/// on the two default papers ([`DIM_TARGET_DARK`]/[`DIM_TARGET_LIGHT`]),
/// so a theme without a background gets the same amber as everything
/// else instead of falling back to the old gray.
const MARK_BG_DARK: Color = Color::Rgb(0x5a, 0x45, 0x21);
const MARK_BG_LIGHT: Color = Color::Rgb(0xfd, 0xe3, 0xa5);

/// The theme background a [`DecorationKind::Dim`] foreground is blended
/// toward when the theme declares none of its own. Same split as
/// [`MARK_BG_DARK`]/[`MARK_BG_LIGHT`], picked by the foreground's
/// brightness.
const DIM_TARGET_DARK: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const DIM_TARGET_LIGHT: Color = Color::Rgb(0xfd, 0xf6, 0xe3);

/// How far the mark background is lifted from the theme background
/// toward [`MARK_TINT`].
///
/// 0.14 toward the foreground was tried first and read as "nothing
/// happened"; 0.22 shipped, and with the whole document shown still read
/// as "not enough to catch on".
///
/// What the user picked on a real terminal, out of four hues, is the
/// COLOR `#5a4520`. 0.27 is the blend that reproduces it from
/// Catppuccin Mocha's paper: it lands on `rgb(90,69,33)` (`#5a4521`,
/// one off — `lerp_color` truncates). On Solarized (light) the same
/// 0.27 gives `rgb(253,227,165)`; that side was delegated and picked by
/// eye against 0.20 / 0.34 / 0.40 on the real terminal.
pub const MARK_BG_BLEND: f32 = 0.27;

/// The highest default [`MARK_BG_BLEND`] may take before the text ON the
/// mark stops being comfortable to read.
///
/// **The old reason no longer holds and was replaced.** Until the mark
/// had a hue, the ceiling existed because a darker gray became
/// mistakable for the gray selection band (`rgb(88,91,112)`). That
/// collision is gone: `rgb(90,69,33)` and `rgb(88,91,112)` sit at almost
/// the same lightness and still do not mix, because one is warm and the
/// other is cool. Lightness was doing the separating; now hue does, and
/// hue does not run out as the blend climbs.
///
/// What DOES run out is contrast. The amber gets brighter as the blend
/// climbs while the text on top does not move, so on a dark theme
/// `#cdd6f4` over the mark falls from ~6:1 at the default to ~3:1 around
/// 0.55 — the point where code spans and syntax colors start to be
/// swallowed. 0.40 keeps a comfortable margin below that and still
/// leaves the flag somewhere to go.
///
/// A ceiling on the SHIPPED default, not on `--mark-blend`: the flag is a
/// knob for looking at alternatives, and silently clamping what the user
/// typed would make it a useless one. A test holds the default under it.
pub const MARK_BG_BLEND_CEILING: f32 = 0.40;

/// The ceiling is not advice: raising [`MARK_BG_BLEND`] past it stops the
/// build, not a test run.
const _: () = assert!(MARK_BG_BLEND <= MARK_BG_BLEND_CEILING);

/// **`AKAPEN_MARK_STYLE=c`（字を太く）**の地の濃さ。
///
/// 既定の 0.27 のままだと太さの違いが地に紛れるので、少しだけ濃くする。
/// 天井 0.40 の内側なので、上の字はまだ読める（約 5:1）。
const MARK_BG_BLEND_BOLD: f32 = 0.32;

/// **`AKAPEN_MARK_STYLE=f`（2 段）**の、Unit 全体に敷く薄い地。
///
/// 核（0.27）の下に敷くものなので、地の文と見分けが付くぎりぎりまで薄く
/// してある。読み手が「核」として拾うのは濃いほうだけである。
const MARK_BG_BLEND_FAINT: f32 = 0.10;

/// How far a [`DecorationKind::Dim`] foreground is moved from its own
/// color toward the theme background. 0.60 was picked by eye on a real
/// terminal: far enough to recede, near enough to stay readable and keep
/// its hue.
pub const DIM_BLEND: f32 = 0.60;

/// 候補の見た目を選ぶ環境変数。**暫定** — 選ばれたら消える。
const MARK_STYLE_ENV: &str = "AKAPEN_MARK_STYLE";

/// **実機で並べて選ぶための、暫定の見た目**（環境変数 `AKAPEN_MARK_STYLE`）。
///
/// これは**本番の設定項目ではない**。候補を 1 つのバイナリで切り替えて実機に
/// 並べ、選ばれた 1 案だけを正式に焼き直すための、使い捨ての切り替えである
/// （`marks-style` の作業）。
///
/// **未設定なら [`MarkVariant::Amber`]** で、以前と 1 バイトも変わらない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkVariant {
    /// `a` — いまのまま。amber `#ffb000` へ 0.27、地色だけ。
    #[default]
    Amber,
    /// `b` — 天井まで濃く（0.40）。
    Deep,
    /// `c` — 少し濃く（0.32）+ 字を太く。
    Bold,
    /// `d` — 地色はそのまま、ガターの行頭に琥珀の `▌` を足す。
    Bar,
    /// `e` — 地色をやめて、字の下に琥珀の下線。
    Rule,
    /// `f` — Unit 全体に薄い地（0.10）+ 核に濃い地（0.27）の 2 段。
    TwoTone,
}

impl MarkVariant {
    /// 記号（1 文字）と長い名前の両方を受ける。**知らない値は `Amber`**
    /// に落ちる — 打ち間違いで見た目が変わるより、既定に戻るほうが安全で、
    /// この切り替えは残らないものだから。
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "b" | "deep" => Self::Deep,
            "c" | "bold" => Self::Bold,
            "d" | "bar" => Self::Bar,
            "e" | "rule" | "underline" => Self::Rule,
            "f" | "two" | "twotone" => Self::TwoTone,
            _ => Self::Amber,
        }
    }

    /// 環境変数 [`MARK_STYLE_ENV`]。**プロセスで 1 度だけ読む** —
    /// 1 回の起動の中で見た目が変わることは無いし、レンダのたびに getenv を
    /// 叩く理由も無い。
    pub fn from_env() -> Self {
        static CACHED: OnceLock<MarkVariant> = OnceLock::new();
        *CACHED.get_or_init(|| {
            std::env::var(MARK_STYLE_ENV)
                .map(|value| Self::parse(&value))
                .unwrap_or_default()
        })
    }
}

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

/// The resolved per-kind styles for one theme. Built once per render (the
/// theme cannot change without a re-render) and applied by patching, so a
/// decorated span keeps its syntax color and its font style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationStyles {
    /// The mark's patch: a background and nothing else.
    mark: Style,
    /// [`DecorationKind::SemanticMarkFaint`] の地。既定では出さない。
    faint_mark: Style,
    /// **演出が最初に出す、一番濃い琥珀**（[`Self::mark_flash_bg`]）。
    flash: Color,
    /// いま選ばれている見た目（[`MarkVariant`]）。
    variant: MarkVariant,
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
            faint_mark: Style::default().bg(MARK_BG_DARK),
            // ダークの紙 rgb(30,30,46) から天井（0.40）の色。
            flash: Color::Rgb(0x78, 0x58, 0x1b),
            variant: MarkVariant::Amber,
            dim_target: DIM_TARGET_DARK,
            default_fg: Color::Rgb(0xcd, 0xd6, 0xf4),
            dim_blend: DIM_BLEND,
        }
    }
}

impl DecorationStyles {
    /// Resolve both kinds against `highlighter`'s theme at `blend`, with the
    /// look picked by [`MarkVariant::from_env`].
    pub fn from_theme(highlighter: &Highlighter, blend: DecorationBlend) -> Self {
        Self::from_theme_with_variant(highlighter, blend, MarkVariant::from_env())
    }

    /// [`Self::from_theme`] の見た目を明示する版。**テストと実機の並べ比べ
    /// だけが使う** — 本番は環境変数から 1 本に決まる
    /// （[`Self::from_theme`]）。
    pub fn from_theme_with_variant(
        highlighter: &Highlighter,
        blend: DecorationBlend,
        variant: MarkVariant,
    ) -> Self {
        let default_fg = highlighter.default_fg();
        let dim_target = match highlighter.theme().settings.background {
            Some(bg) => Color::Rgb(bg.r, bg.g, bg.b),
            // No theme background at all: pick the side from the text
            // color, exactly as `mark_background` does.
            None => match default_fg {
                Color::Rgb(r, g, b) if r as u32 + g as u32 + b as u32 >= 3 * 128 => {
                    DIM_TARGET_DARK
                }
                _ => DIM_TARGET_LIGHT,
            },
        };
        // 案ごとに、同じ紙・同じ amber から別の 1 点だけを動かす。
        let amber = mark_background(highlighter, blend.mark);
        let mark = match variant {
            MarkVariant::Amber => Style::default().bg(amber),
            MarkVariant::Deep => {
                Style::default().bg(mark_background(highlighter, MARK_BG_BLEND_CEILING))
            }
            MarkVariant::Bold => Style::default()
                .bg(mark_background(highlighter, MARK_BG_BLEND_BOLD))
                .add_modifier(Modifier::BOLD),
            MarkVariant::Bar => Style::default().bg(amber),
            // 地色をやめる。下線は 1 桁の線なので、[`TICK_BLEND`] と同じ
            // 濃さで紙から amber へ寄せる（薄いと明るいテーマで消える）。
            MarkVariant::Rule => Style::default()
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(crate::view::lerp_color(dim_target, MARK_TINT, TICK_BLEND)),
            MarkVariant::TwoTone => Style::default().bg(amber),
        };
        Self {
            mark,
            faint_mark: Style::default().bg(mark_background(highlighter, MARK_BG_BLEND_FAINT)),
            // **演出の出発点。** 天井（[`MARK_BG_BLEND_CEILING`]）の色で、
            // 確定色より濃い。`MarkVariant::Deep` では確定色と一致するので
            // 演出は「落ちない」— 見た目が既に天井だからである。
            flash: mark_background(highlighter, MARK_BG_BLEND_CEILING),
            variant,
            dim_target,
            default_fg,
            dim_blend: blend.dim,
        }
    }

    /// The mark's patch: a background, no foreground, no modifier.
    pub fn mark_style(&self) -> Style {
        self.mark
    }

    /// いま選ばれている見た目（[`MarkVariant`]）。**案 d だけが読む** —
    /// ガターに琥珀の `▌` を足すのは描画側の仕事なので、見た目の選択が
    /// ここから `view.rs` へ渡る。
    pub fn mark_variant(&self) -> MarkVariant {
        self.variant
    }

    /// **演出の線の色。**
    ///
    /// [`MARK_BG_BLEND_CEILING`]（0.40）の色で、確定色（[`Self::mark_bg`]）
    /// より濃い。`marks_draw_effect` は**この色で線を引き、通った後ろを
    /// 確定色へ戻す**ので、**「目立たないが読みやすい」確定色を変えずに、
    /// 引かれる線の瞬間だけ強くする**ことができる（読み手の注文、
    /// 2026-09-23）。
    ///
    /// **焼き込まない** — `--light` でも `--theme` でも同じ式で出る。
    pub fn mark_flash_bg(&self) -> Color {
        self.flash
    }

    /// The mark's background color on its own — the amber the paint
    /// actually writes, and the color the reveal settles to.
    ///
    /// The marks reveal animation filters the frame by exactly this
    /// color (`crate::effects::marks_draw_effect` /
    /// `crate::effects::marks_reveal_effect`), which is why it is read
    /// from here instead of being written into the effect: a theme,
    /// `--light` and `--mark-blend` all move it, and there must be one
    /// place that decides. The bright color the drawn LINE carries is
    /// [`Self::mark_flash_bg`].
    pub fn mark_bg(&self) -> Color {
        self.mark.bg.unwrap_or(MARK_BG_DARK)
    }

    /// **目盛りとバッジの琥珀** — スクロールバーの溝に打つ点と、フッタの
    /// `FOCUS` バッジの地色。
    ///
    /// [`Self::mark_bg`] と同じ紙・同じ [`MARK_TINT`] から作るので、
    /// テーマを変えても（`--light` / `--theme` / `--mark-blend`）
    /// マーカーと同じ琥珀の系統で動く。**焼き込んだ色ではない。**
    pub fn mark_tick(&self) -> Color {
        crate::view::lerp_color(self.dim_target, MARK_TINT, TICK_BLEND)
    }

    /// **Review の候補の下線の色**（`R`）。
    ///
    /// [`Self::mark_bg`] / [`Self::mark_tick`] と同じ紙から、別の
    /// 色相（[`REVIEW_TINT`]）へ寄せたもの。**焼き込んだ色ではない**ので、
    /// テーマを変えても marks の琥珀との距離が保たれる。
    pub fn review_underline(&self) -> Color {
        crate::view::lerp_color(self.dim_target, REVIEW_TINT, REVIEW_BLEND)
    }

    /// The paper the mark was lifted FROM — where the reveal fades in
    /// from.
    pub fn page_bg(&self) -> Color {
        self.dim_target
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
            // 案 f の薄い地。核（`SemanticMark`）と重なる場合は、呼び出し側が
            // 薄いほうを先に置くので濃いほうが勝つ。
            DecorationKind::SemanticMarkFaint => base.patch(self.faint_mark),
            DecorationKind::Dim => base.fg(self.dim_fg(base.fg)),
            // **下線だけ。** 前景も地色も触らないので、琥珀の上でも、
            // 選択の帯の上でも、syntax highlight の上でも重なって読める。
            DecorationKind::ReviewCandidate => base
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(self.review_underline()),
        }
    }
}

/// The mark background: the theme's own background lifted
/// [`MARK_BG_BLEND`] toward [`MARK_TINT`] (so it reads as "a highlighter
/// was drawn over this paper" in a dark AND a light theme), else a fixed
/// pair picked by the foreground's brightness.
///
/// # ONE path, and it always runs
///
/// There used to be a lookup in front of this: a `MARK_SCOPES` list
/// (`markup.highlight`, `markup.mark`, `markup.quote.highlight`,
/// `region.yellowish`) that let a theme with its own highlight
/// background keep it. **It was removed after being measured.** Out of
/// two-face's 32 embedded themes exactly one reached it, by accident,
/// and produced text at 1.14:1 — see
/// `docs/gotchas/rendering.md`「テーマの highlight scope 尊重は一度
/// やって落とした」for the numbers. Deferring to a theme that is not
/// actually saying anything about marks is not respect.
///
/// So the mark is always theme-DERIVED and never theme-SUPPLIED: the
/// theme's paper is the origin, [`MARK_TINT`] is the destination. A dark
/// theme lands on a dark amber, a light theme on a pale yellow, from the
/// same formula — and because there is no branch, the formula is the
/// only thing that can ever be wrong.
fn mark_background(highlighter: &Highlighter, blend: f32) -> Color {
    let settings = &highlighter.theme().settings;
    let fg = highlighter.default_fg();
    match settings.background {
        Some(bg) => crate::view::lerp_color(Color::Rgb(bg.r, bg.g, bg.b), MARK_TINT, blend),
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

    /// The mark background is a real color that differs from the page.
    /// There is only one path now, so this IS the path that ships, for
    /// every theme.
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

    /// **候補の見た目（`AKAPEN_MARK_STYLE`）は、amber の式のまま 1 点だけ
    /// 動かす。** 既定（`Amber`）は 1 バイトも変わらない。
    ///
    /// これは使い捨ての切り替えなので、案ごとの値は実機で並べたときの
    /// 記録としてここに固定する。選ばれたら負けた腕ごと消す。
    #[test]
    fn each_variant_moves_exactly_one_thing() {
        let hl = Highlighter::new(None, false);
        let blend = Default::default();
        let amber = mark_background(&hl, MARK_BG_BLEND);
        let base = DecorationStyles::from_theme_with_variant(&hl, blend, MarkVariant::Amber);
        assert_eq!(base.mark_style(), Style::default().bg(amber), "a は今のまま");

        let deep = DecorationStyles::from_theme_with_variant(&hl, blend, MarkVariant::Deep);
        assert_eq!(
            deep.mark_style(),
            Style::default().bg(mark_background(&hl, MARK_BG_BLEND_CEILING)),
            "b は天井まで濃く"
        );

        let bold = DecorationStyles::from_theme_with_variant(&hl, blend, MarkVariant::Bold);
        assert_eq!(bold.mark_style().bg, Some(mark_background(&hl, MARK_BG_BLEND_BOLD)));
        assert!(bold.mark_style().add_modifier.contains(Modifier::BOLD), "c は字も太く");

        let bar = DecorationStyles::from_theme_with_variant(&hl, blend, MarkVariant::Bar);
        assert_eq!(bar.mark_style(), base.mark_style(), "d は地色を変えない（線はガター）");
        assert_eq!(bar.mark_variant(), MarkVariant::Bar, "描画側が読む");

        let rule = DecorationStyles::from_theme_with_variant(&hl, blend, MarkVariant::Rule);
        assert_eq!(rule.mark_style().bg, None, "e は地色をやめる");
        assert!(rule.mark_style().add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(
            rule.mark_style().underline_color,
            Some(crate::view::lerp_color(DIM_TARGET_DARK, MARK_TINT, TICK_BLEND)),
            "e の下線は amber の濃いほう"
        );

        let two = DecorationStyles::from_theme_with_variant(&hl, blend, MarkVariant::TwoTone);
        assert_eq!(two.mark_style(), base.mark_style(), "f の核は a と同じ");
        assert_eq!(
            two.faint_mark,
            Style::default().bg(mark_background(&hl, MARK_BG_BLEND_FAINT)),
            "f は Unit 全体に薄い地を足す"
        );
    }

    /// **案 e の下線と Review の下線は別物であること。** 同じ「下線」という
    /// 形なので、分けるのは色相だけである（amber は暖色、Review は青緑）。
    /// 両者は同時に乗りうる — marks は地色、Review は下線しか書かないので
    /// 重なっても打ち消し合わない（`App::active_decorations`）。
    #[test]
    fn the_amber_rule_is_not_the_review_underline() {
        let hl = Highlighter::new(None, false);
        let styles = DecorationStyles::from_theme_with_variant(
            &hl,
            Default::default(),
            MarkVariant::Rule,
        );
        let rule = styles.mark_style().underline_color.expect("a rule");
        let review = styles.review_underline();
        assert_ne!(rule, review);
        let (Color::Rgb(rr, rg, rb), Color::Rgb(pr, pg, pb)) = (rule, review) else {
            panic!("RGB")
        };
        assert!(rr > rg && rg > rb, "案 e は暖色 rgb({rr},{rg},{rb})");
        assert!(pb > pr && pg > pr, "Review は青緑 rgb({pr},{pg},{pb})");
        assert!((rr > rb) != (pr > pb), "r と b の大小が逆 = 色相が遠い");
    }

    /// 案 f の 2 段は、薄い地を**先に**敷いて核を後に置く。`decorate_row`
    /// は slice 順に patch するので、順序が仕様である。逆にすると核が
    /// 薄いほうに負ける（黙って a に見える）。
    #[test]
    fn the_faint_unit_wash_loses_to_the_core() {
        let hl = Highlighter::new(None, false);
        let styles = DecorationStyles::from_theme_with_variant(
            &hl,
            Default::default(),
            MarkVariant::TwoTone,
        );
        let faint = styles.patch(Style::default(), DecorationKind::SemanticMarkFaint);
        assert!(faint.bg.is_some());
        assert_ne!(faint.bg, styles.mark_style().bg, "薄い地と核は別の色");
        // 薄い地の上に核を重ねると核の色。
        assert_eq!(
            styles.patch(faint, DecorationKind::SemanticMark).bg,
            styles.mark_style().bg
        );
        // 逆順は薄いほうが勝つ — だから順序が仕様である。
        assert_eq!(
            styles
                .patch(
                    styles.patch(Style::default(), DecorationKind::SemanticMark),
                    DecorationKind::SemanticMarkFaint,
                )
                .bg,
            faint.bg
        );
    }

    /// 案の切り替えは記号でも名前でも読み、**知らない値は既定**に落ちる。
    #[test]
    fn the_temporary_mark_variants_parse_by_letter_and_by_name() {
        assert_eq!(MarkVariant::parse("a"), MarkVariant::Amber);
        assert_eq!(MarkVariant::parse("b"), MarkVariant::Deep);
        assert_eq!(MarkVariant::parse("D"), MarkVariant::Bar);
        assert_eq!(MarkVariant::parse(" rule "), MarkVariant::Rule);
        assert_eq!(MarkVariant::parse("two"), MarkVariant::TwoTone);
        assert_eq!(MarkVariant::parse("f"), MarkVariant::TwoTone);
        assert_eq!(MarkVariant::parse(""), MarkVariant::Amber);
        assert_eq!(MarkVariant::parse("nonsense"), MarkVariant::Amber);
    }

    /// 既定のブレンド率で実際に出る色。ダークの値はユーザーが実機で
    /// 4 候補（violet / plum / teal / amber）を並べて選んだものなので、
    /// 黙って動かないように値そのものを固定する。ライトは同じ式が同じ
    /// amber へ向かって出す色で、こちらは実装側が実機で確かめて選んだ。
    #[test]
    fn the_default_blends_produce_the_colors_that_were_chosen() {
        // Catppuccin Mocha: 背景 rgb(30,30,46) / 前景 rgb(205,214,244)。
        // 見せて選ばれたのは #5a4520、式が出すのは #5a4521（lerp の
        // 切り捨てぶん 1 だけ違う）。
        let dark = DecorationStyles::from_theme(&Highlighter::new(None, false), Default::default());
        assert_eq!(dark.mark_style().bg, Some(Color::Rgb(0x5a, 0x45, 0x21)));
        assert_eq!(dark.dim_fg(None), Color::Rgb(99, 103, 125));
        // Solarized (light): 背景 rgb(253,246,227) / 前景 rgb(101,123,131)。
        let light = DecorationStyles::from_theme(&Highlighter::new(None, true), Default::default());
        assert_eq!(light.mark_style().bg, Some(Color::Rgb(0xfd, 0xe3, 0xa5)));
        assert_eq!(light.dim_fg(None), Color::Rgb(192, 196, 188));

        // 背景を持たないテーマ用の固定値は、この 2 つと同じ色であること
        // — amber の経路から取り残された灰色が残らないように。
        assert_eq!(dark.mark_style().bg, Some(MARK_BG_DARK));
        assert_eq!(light.mark_style().bg, Some(MARK_BG_LIGHT));
    }

    /// MARK の背景は selection 帯とはっきり別物であること。
    ///
    /// **分けているものが変わりました。** 灰色だった頃は「明るさ」が
    /// 唯一の違いで、濃くすると帯 `rgb(88,91,112)` に近づいて
    /// 「選択されている」と読み違えた。いまは色相で分かれます —
    /// mark `rgb(90,69,33)` は暖色、帯は寒色で、**明るさがほぼ同じでも
    /// 混ざりません**。だからここで見るのは距離ではなく色相です。
    /// 距離のほうも一応見ますが、天井（0.40）まで上げると距離は
    /// むしろ縮む方向にも動くので、距離だけを条件にはできません。
    #[test]
    fn the_mark_background_stays_clear_of_the_selection_band() {
        let band = crate::view::selected_bg(false);
        let Color::Rgb(sr, sg, sb) = band else { panic!("RGB") };
        assert_eq!((sr, sg, sb), (88, 91, 112), "selection 帯の色が変わった");
        assert!(sb > sr, "帯は寒色（青みの灰）という前提が崩れた");

        // 既定でも天井でも、mark は暖色側にいる。
        for blend in [MARK_BG_BLEND, MARK_BG_BLEND_CEILING] {
            let styles = DecorationStyles::from_theme(
                &Highlighter::new(None, false),
                DecorationBlend { mark: blend, ..Default::default() },
            );
            let Some(Color::Rgb(r, g, b)) = styles.mark_style().bg else { panic!("RGB") };
            assert_ne!((r, g, b), (sr, sg, sb));
            assert!(r > g && g > b, "blend {blend}: mark rgb({r},{g},{b}) が暖色でない");
            // 帯とは r と b の大小が逆 — これが「混ざらない」の中身。
            assert!(
                (r > b) != (sr > sb),
                "blend {blend}: mark rgb({r},{g},{b}) が帯と同じ寒暖に回った"
            );
        }

        // ライトの帯とも同じこと。
        let light_band = crate::view::selected_bg(true);
        let Color::Rgb(lr, _, lb) = light_band else { panic!("RGB") };
        let light = DecorationStyles::from_theme(&Highlighter::new(None, true), Default::default());
        let Some(Color::Rgb(r, _, b)) = light.mark_style().bg else { panic!("RGB") };
        assert!(r > b && lb > lr, "ライトでも mark は暖色、帯は寒色");
    }

    /// 天井の新しい理由 —— 帯との混同ではなく、**マークの上の字が
    /// 読めるか**。ブレンドを上げるほど amber は明るくなり、上に乗る
    /// 文字は動かないので、コントラストだけが減っていく。
    ///
    /// 数字は WCAG の相対輝度比。既定は余裕があり、天井でもまだ
    /// 本文が読める側に残っていること（4.5:1 は AA の本文基準）を
    /// 固定する。0.55 まで上げると 3:1 台に落ちる、というのが
    /// 天井を 0.40 に置いた理由です。
    #[test]
    fn the_ceiling_is_where_the_text_on_the_mark_stays_readable() {
        fn luminance(c: Color) -> f32 {
            let Color::Rgb(r, g, b) = c else { panic!("RGB") };
            let ch = |x: u8| {
                let x = x as f32 / 255.0;
                if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
        }
        fn contrast(a: Color, b: Color) -> f32 {
            let (x, y) = (luminance(a), luminance(b));
            let (hi, lo) = if x > y { (x, y) } else { (y, x) };
            (hi + 0.05) / (lo + 0.05)
        }

        let highlighter = Highlighter::new(None, false);
        let fg = highlighter.default_fg();
        let at = |blend: f32| {
            DecorationStyles::from_theme(
                &highlighter,
                DecorationBlend { mark: blend, ..Default::default() },
            )
            .mark_style()
            .bg
            .expect("a background")
        };

        let default = contrast(fg, at(MARK_BG_BLEND));
        assert!(default >= 6.0, "既定のコントラストが {default:.1}:1 まで落ちた");
        // 天井ちょうどが境界です（実測 4.52:1）。`lerp_color` の丸めを
        // いじるとここが最初に反転するので、落ちたら天井の位置を
        // 疑うこと — テストの閾値を下げて済ませないように。
        let ceiling = contrast(fg, at(MARK_BG_BLEND_CEILING));
        assert!(ceiling >= 4.5, "天井のコントラストが {ceiling:.2}:1 — AA を割った");
        // 天井の向こうは実際に危ない、というのが天井を置く根拠。
        let beyond = contrast(fg, at(0.55));
        assert!(beyond < 4.5, "0.55 でも {beyond:.1}:1 — 天井の理由が消えた");
    }

    /// **テーマの highlight scope 背景は、もう見ていません。**
    /// 前に `MARK_SCOPES`（`markup.highlight` / `markup.mark` /
    /// `markup.quote.highlight` / `region.yellowish`）のループが
    /// `mark_background` の先頭にあり、背景を持つテーマにはそれを
    /// 使わせていました。**実測して落としました** — 理由は
    /// `docs/gotchas/rendering.md`「テーマの highlight scope 尊重は
    /// 一度やって落とした」。
    ///
    /// これは**再発防止のテスト**です。「テーマを尊重しよう」は自然に
    /// 出てくる案なので、分岐を戻すとここが落ちます。落としてよいのは
    /// 上の gotcha を読んで、それでもなお戻すと決めたときだけです。
    #[test]
    fn a_theme_that_styles_a_highlight_scope_gets_the_amber_anyway() {
        const HIGHLIGHT_TM_THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key>
  <string>Highlighted</string>
  <key>settings</key>
  <array>
    <dict>
      <key>settings</key>
      <dict>
        <key>background</key>
        <string>#1e1e2e</string>
        <key>foreground</key>
        <string>#cdd6f4</string>
      </dict>
    </dict>
    <dict>
      <key>scope</key>
      <string>markup.highlight</string>
      <key>settings</key>
      <dict>
        <key>background</key>
        <string>#264f78</string>
      </dict>
    </dict>
  </array>
</dict>
</plist>
"#;
        let dir = std::env::temp_dir().join(format!("akapen-mark-scope-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("highlighted.tmTheme");
        std::fs::write(&path, HIGHLIGHT_TM_THEME).unwrap();
        let highlighter = Highlighter::new(Some(path.to_str().unwrap()), false);
        // scope_style からは今でも見える —— 見えるのに使っていない、
        // というのがこのテストの主張です（「実装し忘れ」ではない）。
        assert_eq!(
            highlighter.scope_style("markup.highlight").and_then(|s| s.bg),
            Some(Color::Rgb(0x26, 0x4f, 0x78)),
            "テーマ側の前提が変わった — この .tmTheme が効いていない"
        );

        let styles = DecorationStyles::from_theme(&highlighter, Default::default());
        assert_ne!(styles.mark_style().bg, Some(Color::Rgb(0x26, 0x4f, 0x78)));
        // 紙が Catppuccin Mocha と同じなので、出る色も同じ amber。
        assert_eq!(styles.mark_style().bg, Some(MARK_BG_DARK));

        // 経路が 1 本になったので、`--mark-blend` は**どのテーマでも**効く。
        // 以前はテーマ次第でつまみが死んでいました。
        for blend in [0.0, 1.0] {
            let forced = DecorationStyles::from_theme(
                &highlighter,
                DecorationBlend { mark: blend, ..Default::default() },
            );
            assert_ne!(
                forced.mark_style().bg,
                Some(Color::Rgb(0x26, 0x4f, 0x78)),
                "blend={blend}"
            );
        }
        assert_eq!(
            DecorationStyles::from_theme(
                &highlighter,
                DecorationBlend { mark: 1.0, ..Default::default() },
            )
            .mark_style()
            .bg,
            Some(MARK_TINT)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 経路が 1 本になったことを、埋め込みテーマ 32 本で確かめる。
    /// **どのテーマでも**マークはそのテーマの紙から amber へ向かう
    /// 計算で出ること —— 以前は DarkNeon 1 本だけがここを外れて
    /// `rgb(254,224,156)` を出し、その上の字が読めませんでした。
    #[test]
    fn every_embedded_theme_gets_the_amber_formula() {
        use two_face::theme::EmbeddedLazyThemeSet;
        for theme in EmbeddedLazyThemeSet::theme_names() {
            let name = theme.as_name();
            let highlighter = Highlighter::new(Some(name), false);
            let bg = DecorationStyles::from_theme(&highlighter, Default::default())
                .mark_style()
                .bg
                .expect("a background");
            let expected = match highlighter.theme().settings.background {
                Some(p) => crate::view::lerp_color(
                    Color::Rgb(p.r, p.g, p.b),
                    MARK_TINT,
                    MARK_BG_BLEND,
                ),
                None => match highlighter.default_fg() {
                    Color::Rgb(r, g, b) if r as u32 + g as u32 + b as u32 >= 3 * 128 => {
                        MARK_BG_DARK
                    }
                    _ => MARK_BG_LIGHT,
                },
            };
            assert_eq!(bg, expected, "theme={name:?} が式から外れた");
            // 暖色であること —— 紙がどんな色でも amber へ向かうので、
            // 赤み > 青み に落ちる。DarkNeon の `rgb(254,224,156)` が
            // これを満たしてしまう（r>b）ので、色相だけでは不十分。
            // 式そのものの一致を上で見ているのはそのためです。
            let Color::Rgb(r, _, b) = bg else { panic!("RGB") };
            assert!(r >= b, "theme={name:?}: rgb({r},_,{b}) が寒色側に落ちた");
        }
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
        // 1 は振り切り — amber そのもの / ページ背景そのもの。
        assert_eq!(strong.mark_style().bg, Some(MARK_TINT));
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
