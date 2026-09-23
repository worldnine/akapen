//! **波線（undercurl）** — Review の下線を、対応する端末でだけ波線にする。
//!
//! nvim の LSP の指摘と同じ `CSI 4:3 m`（kitty が始めた拡張。下線の形を
//! `4:<n>` のコロンの副引数で選ぶ）。ratatui 0.30 の `Modifier` には
//! undercurl が無いので、装飾の層は印（[`CURL_CARRIER`]）を立てるだけにして、
//! **描画の出口**（[`draw`]、`NoBlinkBackend::draw` から呼ぶ）で読み替える。
//!
//! # なぜ出口で読み替えるか
//!
//! - ratatui のセル（`Cell`）には下線の形を書く場所が無い。書けるのは
//!   `Modifier` と `underline_color` だけで、`Modifier` のビットは固定である
//! - crossterm には `Attribute::Undercurled` があるが、ratatui の
//!   `CrosstermBackend::draw` は `Modifier` から属性を作るので、そこへ届く
//!   道が無い
//! - 端末の判定を装飾の層（`decorate_row`、純粋）に持ち込まない。判定は
//!   端末の出口の 1 か所に閉じ、テストの `TestBackend` はセルの印を見る
//!
//! そのため [`draw`] は `CrosstermBackend::draw`（ratatui-crossterm 0.1.2、
//! MIT）の写しで、違いは**印を落とすこと**と**波線の副引数を足すこと**
//! だけである。
//!
//! # 対応の判定（[`resolve`]）
//!
//! `--undercurl <auto|on|off>`（既定 auto）、無ければ環境変数
//! [`UNDERCURL_ENV`]。auto の材料は上から:
//!
//! 1. `TERM` が空・`dumb`・`linux` → 普通の下線
//! 2. `TERM` の terminfo に拡張の `Smulx`（下線の形を選ぶ能力）がある → 波線
//! 3. tmux の中（`TMUX`）→ 波線。tmux は `4:3` を自分で解釈して持ち、
//!    外側の端末に `Smulx` が無ければ**普通の下線に落として**出す
//!    （外側に届くのは常に外側が読める形）
//! 4. `TERM_PROGRAM` / `TERM` が波線を描くと分かっている端末（Ghostty・
//!    kitty・WezTerm・iTerm2・VS Code・Alacritty・foot・Rio・Contour）→ 波線
//! 5. それ以外（Terminal.app を含む）→ 普通の下線
//!
//! **herdr の中**（`HERDR_ENV`）には専用の枝を持たない。herdr 0.9.1 は
//! `4:3` を**そのまま**外側へ出す（外側の `TERM` を `tmux-256color` にしても
//! 同じ）ので、判定は外側の端末についての 4. に任せる — herdr の中の
//! `TERM_PROGRAM` は herdr を立てた端末のものが残っている。ただし herdr は
//! **下線の色（`CSI 58`）を落とす**ので、herdr の中では重さの色は下線に
//! 出ず、ガターの白抜きの印（地と字の色）にだけ出る（`docs/gotchas/rendering.md`）。

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ratatui::backend::IntoCrossterm;
use ratatui::buffer::Cell;
use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::queue;
use ratatui::crossterm::style::{
    Attribute as CAttribute, Color as CColor, Colors, Print, SetAttribute, SetBackgroundColor,
    SetColors, SetForegroundColor, SetUnderlineColor,
};
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier};

pub(crate) use crate::decoration::CURL_CARRIER;

/// `--undercurl` を省いたときの既定を持つ環境変数（値は `auto|on|off`）。
///
/// 端末ごとの設定なので、`AKAPEN_LINT_CMD` と同じく環境変数に置ける。
pub const UNDERCURL_ENV: &str = "AKAPEN_UNDERCURL";

/// `--undercurl <auto|on|off>`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UndercurlMode {
    /// 端末を見て決める（[`resolve`]）。
    #[default]
    Auto,
    /// 常に波線。
    On,
    /// 常に普通の下線。
    Off,
}

impl UndercurlMode {
    /// 知らない値は `Auto`（`--esc-quit` と同じ作法）。
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "on" => Self::On,
            "off" => Self::Off,
            _ => Self::Auto,
        }
    }
}

/// 波線を描くと分かっている端末の `TERM_PROGRAM`。
const CURL_TERM_PROGRAMS: &[&str] = &["ghostty", "WezTerm", "iTerm.app", "vscode", "rio"];

/// 波線を描くと分かっている端末の `TERM` に含まれる語。
const CURL_TERM_NAMES: &[&str] =
    &["kitty", "ghostty", "wezterm", "alacritty", "foot", "rio", "contour"];

/// 波線にするかを決める。`env` は環境変数を 1 つ引く関数、`smulx` は
/// `TERM` の terminfo に `Smulx` があるか（テストでは差し替える）。
pub fn resolve(
    mode: UndercurlMode,
    env: impl Fn(&str) -> Option<String>,
    smulx: impl Fn(&str) -> bool,
) -> bool {
    match mode {
        UndercurlMode::On => return true,
        UndercurlMode::Off => return false,
        UndercurlMode::Auto => {}
    }
    let term = env("TERM").unwrap_or_default();
    if term.is_empty() || term == "dumb" || term == "linux" {
        return false;
    }
    if smulx(&term) {
        return true;
    }
    if env("TMUX").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    if env("TERM_PROGRAM").is_some_and(|p| CURL_TERM_PROGRAMS.contains(&p.as_str())) {
        return true;
    }
    CURL_TERM_NAMES.iter().any(|name| term.contains(name))
}

/// 実際の環境で [`resolve`] する。
pub fn resolve_from_env(mode: UndercurlMode) -> bool {
    let env = |name: &str| std::env::var(name).ok();
    resolve(mode, env, |term| terminfo_has_smulx(term, &terminfo_dirs(env)))
}

/// terminfo を探すディレクトリ（ncurses の順）。
fn terminfo_dirs(env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    const SYSTEM: &[&str] = &[
        "/etc/terminfo",
        "/lib/terminfo",
        "/usr/share/terminfo",
        "/usr/local/share/terminfo",
        "/opt/homebrew/share/terminfo",
    ];
    let mut dirs = Vec::new();
    if let Some(dir) = env("TERMINFO").filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(home) = env("HOME").filter(|d| !d.is_empty()) {
        dirs.push(Path::new(&home).join(".terminfo"));
    }
    if let Some(list) = env("TERMINFO_DIRS") {
        for dir in list.split(':') {
            if dir.is_empty() {
                dirs.extend(SYSTEM.iter().map(PathBuf::from));
            } else {
                dirs.push(PathBuf::from(dir));
            }
        }
    }
    dirs.extend(SYSTEM.iter().map(PathBuf::from));
    dirs
}

/// `term` の terminfo（最初に見つかった 1 つ）が拡張の `Smulx` を持つか。
///
/// **解析はしない。** 拡張能力の名前は terminfo の本体に NUL 区切りの
/// 文字列として入っているので、`Smulx\0` を探すだけで足りる（値の中に
/// 同じ並びが現れることは無い — 値は制御列である）。置き場所は頭文字の
/// ディレクトリ（Linux）か、その 16 進（macOS）。
pub(crate) fn terminfo_has_smulx(term: &str, dirs: &[PathBuf]) -> bool {
    let Some(first) = term.chars().next() else {
        return false;
    };
    if term.contains('/') {
        return false;
    }
    for dir in dirs {
        for sub in [first.to_string(), format!("{:x}", first as u32)] {
            if let Ok(bytes) = std::fs::read(dir.join(sub).join(term)) {
                return bytes.windows(6).any(|w| w == b"Smulx\0");
            }
        }
    }
    false
}

/// **描画の出口。** `CrosstermBackend::draw` と同じ手順でセルを書き、
/// [`CURL_CARRIER`] の立ったセルの下線を `undercurl` なら波線（`CSI 4:3 m`）、
/// そうでなければ普通の下線にする。**印そのもの（点滅）は決して出さない。**
pub(crate) fn draw<'a, W, I>(w: &mut W, content: I, undercurl: bool) -> io::Result<()>
where
    W: Write,
    I: Iterator<Item = (u16, u16, &'a Cell)>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut underline_color = Color::Reset;
    let mut modifier = Modifier::empty();
    let mut curled = false;
    let mut last_pos: Option<Position> = None;
    for (x, y, cell) in content {
        if !matches!(last_pos, Some(p) if x == p.x + 1 && y == p.y) {
            queue!(w, MoveTo(x, y))?;
        }
        last_pos = Some(Position { x, y });
        let to = cell.modifier - CURL_CARRIER;
        let underlined = to.contains(Modifier::UNDERLINED);
        let curl = undercurl && underlined && cell.modifier.contains(CURL_CARRIER);
        let newly_underlined = underlined && !modifier.contains(Modifier::UNDERLINED);
        if to != modifier {
            queue_modifier_diff(w, modifier, to)?;
            modifier = to;
        }
        // 下線の形。`CSI 4 m`（差分が出す）は形を「1 本線」に戻すので、
        // 波線は毎回その後ろに足す。下線のまま波線から 1 本線へ戻るときは
        // `4` を打ち直す（差分は下線が続いているので何も出さない）。
        if curl && (newly_underlined || !curled) {
            w.write_all(b"\x1b[4:3m")?;
        } else if !curl && curled && underlined && !newly_underlined {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        curled = curl;
        if cell.fg != fg || cell.bg != bg {
            queue!(
                w,
                SetColors(Colors::new(cell.fg.into_crossterm(), cell.bg.into_crossterm()))
            )?;
            fg = cell.fg;
            bg = cell.bg;
        }
        if cell.underline_color != underline_color {
            queue!(w, SetUnderlineColor(cell.underline_color.into_crossterm()))?;
            underline_color = cell.underline_color;
        }
        queue!(w, Print(cell.symbol()))?;
    }
    queue!(
        w,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetUnderlineColor(CColor::Reset),
        SetAttribute(CAttribute::Reset),
    )
}

/// `Modifier` の差分を SGR にする（ratatui-crossterm の `ModifierDiff` の写し）。
/// [`CURL_CARRIER`] は呼ぶ前に落としてあるので、点滅はここを通らない。
fn queue_modifier_diff<W: Write>(w: &mut W, from: Modifier, to: Modifier) -> io::Result<()> {
    let removed = from - to;
    if removed.contains(Modifier::REVERSED) {
        queue!(w, SetAttribute(CAttribute::NoReverse))?;
    }
    let reset_intensity = removed.contains(Modifier::BOLD) || removed.contains(Modifier::DIM);
    if reset_intensity {
        queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        if to.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if to.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
    }
    if removed.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(CAttribute::NoItalic))?;
    }
    if removed.contains(Modifier::UNDERLINED) {
        queue!(w, SetAttribute(CAttribute::NoUnderline))?;
    }
    if removed.contains(Modifier::CROSSED_OUT) {
        queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
    }
    if removed.contains(Modifier::HIDDEN) {
        queue!(w, SetAttribute(CAttribute::NoHidden))?;
    }
    if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
        queue!(w, SetAttribute(CAttribute::NoBlink))?;
    }
    let added = to - from;
    if added.contains(Modifier::REVERSED) {
        queue!(w, SetAttribute(CAttribute::Reverse))?;
    }
    if added.contains(Modifier::BOLD) && !reset_intensity {
        queue!(w, SetAttribute(CAttribute::Bold))?;
    }
    if added.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(CAttribute::Italic))?;
    }
    if added.contains(Modifier::UNDERLINED) {
        queue!(w, SetAttribute(CAttribute::Underlined))?;
    }
    if added.contains(Modifier::DIM) && !reset_intensity {
        queue!(w, SetAttribute(CAttribute::Dim))?;
    }
    if added.contains(Modifier::CROSSED_OUT) {
        queue!(w, SetAttribute(CAttribute::CrossedOut))?;
    }
    if added.contains(Modifier::HIDDEN) {
        queue!(w, SetAttribute(CAttribute::Hidden))?;
    }
    if added.contains(Modifier::SLOW_BLINK) {
        queue!(w, SetAttribute(CAttribute::SlowBlink))?;
    }
    if added.contains(Modifier::RAPID_BLINK) {
        queue!(w, SetAttribute(CAttribute::RapidBlink))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn cell(symbol: &'static str, style: Style) -> Cell {
        let mut c = Cell::new(symbol);
        c.set_style(style);
        c
    }

    /// 1 行ぶんのセルを描いた出力（エスケープ列ごと）。
    fn paint(cells: &[Cell], undercurl: bool) -> String {
        let mut out = Vec::new();
        draw(
            &mut out,
            cells.iter().enumerate().map(|(x, c)| (x as u16, 0, c)),
            undercurl,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn review() -> Style {
        Style::default()
            .add_modifier(Modifier::UNDERLINED | CURL_CARRIER)
            .underline_color(Color::Red)
    }

    #[test]
    fn a_carried_underline_curls_only_when_the_terminal_can() {
        let cells = [cell("a", Style::default()), cell("か", review()), cell("b", Style::default())];
        let curly = paint(&cells, true);
        assert!(curly.contains("\x1b[4m\x1b[4:3m"), "波線は 1 本線の後ろに足す: {curly:?}");
        // 重さの色はパレットの名前 — 下線の色もパレット番号で出る（赤 = 1）。
        assert!(curly.contains("\x1b[58;5;1m"), "色も出る: {curly:?}");
        let plain = paint(&cells, false);
        assert!(!plain.contains("4:3"), "{plain:?}");
        assert!(plain.contains("\x1b[4m"), "{plain:?}");
        for out in [curly, plain] {
            assert!(!out.contains("\x1b[6m") && !out.contains("\x1b[5m"), "印は点滅として出ない: {out:?}");
        }
    }

    /// 印の無い下線（見出し・リンク）は、波線の端末でも 1 本線のまま。
    #[test]
    fn an_underline_without_the_carrier_stays_straight() {
        let heading = Style::default().add_modifier(Modifier::UNDERLINED);
        let out = paint(&[cell("h", heading)], true);
        assert!(!out.contains("4:3"), "{out:?}");
        assert!(out.contains("\x1b[4m"), "{out:?}");
    }

    /// 波線 → 1 本線 → 波線と隣り合うとき、形を毎回打ち直す。
    #[test]
    fn the_shape_is_restated_where_curl_and_straight_meet() {
        let straight = Style::default().add_modifier(Modifier::UNDERLINED);
        let out = paint(&[cell("a", review()), cell("b", straight), cell("c", review())], true);
        let a = out.find('a').unwrap();
        let b = out.find('b').unwrap();
        let c = out.find('c').unwrap();
        assert!(out[..a].contains("4:3"), "{out:?}");
        assert!(out[a..b].contains("\x1b[4m") && !out[a..b].contains("4:3"), "{out:?}");
        assert!(out[b..c].contains("4:3"), "{out:?}");
    }

    /// 下線が切れたら `24`（波線もここで終わる）。
    #[test]
    fn leaving_the_underline_ends_the_curl() {
        let out = paint(&[cell("a", review()), cell("b", Style::default())], true);
        let a = out.find('a').unwrap();
        assert!(out[a..].contains("\x1b[24m"), "{out:?}");
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    #[test]
    fn the_flag_wins_over_the_terminal() {
        let ghostty = [("TERM", "xterm-ghostty"), ("TERM_PROGRAM", "ghostty")];
        assert!(!resolve(UndercurlMode::Off, env(&ghostty), |_| true));
        assert!(resolve(UndercurlMode::On, env(&[("TERM", "dumb")]), |_| false));
    }

    #[test]
    fn auto_reads_terminfo_tmux_and_the_terminal_name() {
        let auto = |pairs: &[(&str, &str)], smulx: bool| resolve(UndercurlMode::Auto, env(pairs), |_| smulx);
        assert!(!auto(&[], true), "TERM が無い");
        assert!(!auto(&[("TERM", "dumb")], true));
        assert!(auto(&[("TERM", "xterm-256color")], true), "Smulx がある");
        assert!(auto(&[("TERM", "tmux-256color"), ("TMUX", "/tmp/tmux-501/default,1,0")], false));
        assert!(auto(&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "ghostty")], false));
        assert!(auto(&[("TERM", "xterm-kitty")], false));
        assert!(!auto(&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "Apple_Terminal")], false));
        assert!(!auto(&[("TERM", "xterm-256color")], false), "分からなければ 1 本線");
        // herdr の中: 専用の枝は無く、外側の端末（TERM_PROGRAM）で決まる。
        assert!(auto(&[("TERM", "xterm-256color"), ("HERDR_ENV", "1"), ("TERM_PROGRAM", "ghostty")], false));
        assert!(!auto(
            &[("TERM", "xterm-256color"), ("HERDR_ENV", "1"), ("TERM_PROGRAM", "Apple_Terminal")],
            false
        ));
    }

    #[test]
    fn smulx_is_found_by_its_extended_name() {
        let dir = std::env::temp_dir().join(format!("akapen-terminfo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("78")).unwrap();
        std::fs::create_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("78").join("xterm-curly"), b"\x1a\x01junk\0Smulx\0Setulc\0").unwrap();
        std::fs::write(dir.join("d").join("dumbish"), b"\x1a\x01junk\0Smul\0").unwrap();
        let dirs = vec![dir.clone()];
        assert!(terminfo_has_smulx("xterm-curly", &dirs), "16 進の頭文字（macOS）");
        assert!(!terminfo_has_smulx("dumbish", &dirs));
        assert!(!terminfo_has_smulx("missing", &dirs));
        assert!(!terminfo_has_smulx("../etc", &dirs));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_values_fall_back_to_auto() {
        assert_eq!(UndercurlMode::parse("on"), UndercurlMode::On);
        assert_eq!(UndercurlMode::parse("off"), UndercurlMode::Off);
        assert_eq!(UndercurlMode::parse("auto"), UndercurlMode::Auto);
        assert_eq!(UndercurlMode::parse("bogus"), UndercurlMode::Auto);
    }
}
