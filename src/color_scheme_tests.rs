//! 開いたまま端末の配色（ライト／ダーク）が切り替わったときの作り直し
//! （[`App::set_light`] / [`App::follow_color_scheme`]）。
//!
//! 知らせ（モード 2031）そのものは termtheme の読み手が読み、イベントループは
//! `Input::light()` を [`App::follow_color_scheme`] に渡すだけなので、ここでは
//! 端末なしで「切り替えた後の App」を見る。基準は**その配色で起動した App**:
//! ダークで起動してライトに切り替えた絵と、ライトで起動した絵が同じであること。

use crate::*;

use crate::config::{Config, EscQuit, ThemePair};
use crate::decoration::DecorationStyles;
use crate::highlight::Highlighter;
use crate::ime::ImeMode;
use crate::view::{border_color, scrollbar_thumb, selected_bg};

use std::path::PathBuf;

const DARK: &str = "Catppuccin Mocha";
const LIGHT: &str = "Catppuccin Latte";

/// 見出し・強調・コードフェンスを持つ Markdown（テーマで色が変わるもの）。
const DOC: &str = "# 見出し\n\n本文と **強調** と `コード`。\n\n```rust\nfn main() {}\n```\n";

/// `run()` と同じ組み立ての App（全ファイルの行と view を起動時の配色で塗り、
/// 1 つ目を表に出す）。`fixed` は `--light` / `--dark`、`light` は起動時の判定。
fn session(theme: ThemePair, fixed: Option<bool>, light: bool) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<PathBuf> = ["a.md", "b.md", "c.rs"]
        .iter()
        .map(|name| dir.path().join(name))
        .collect();
    std::fs::write(&files[0], DOC).unwrap();
    std::fs::write(&files[1], format!("## 二つ目\n\n{DOC}")).unwrap();
    std::fs::write(&files[2], "fn main() {\n    let x = 1;\n}\n").unwrap();
    let config = Config {
        files: files.clone(),
        send_cmd: None,
        send_agent: false,
        reply: false,
        theme,
        ime: ImeMode::Off,
        light: fixed,
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
    let highlight = Highlighter::new(config.theme.for_background(light), light);
    // `render_current_view` と同じ幅（テストが端末の中で走ると本物の幅になる）。
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let file_states = files
        .iter()
        .map(|f| {
            let source = Source::load(f.clone()).unwrap();
            let spans = highlight.highlight_with(&source.content, syntax_for(f));
            let view = if supports_view(f) {
                render_view_with_cards(
                    &source,
                    view_render_width(w),
                    &highlight,
                    &[],
                    config.decoration_blend,
                )
            } else {
                ViewState::default()
            };
            FileState {
                mode: if supports_view(f) { Mode::View } else { Mode::Source },
                source,
                spans,
                view,
                ..Default::default()
            }
        })
        .collect();
    let mut app = App::new(config, Source::default(), highlight, ViewState::default(), light);
    app.file_states = file_states;
    app.histories = app
        .file_states
        .iter()
        .zip(&files)
        .map(|(state, path)| DocumentHistory::load(path, &state.source.content, 0))
        .collect();
    activate_first_file(&mut app);
    (app, dir)
}

/// `light` から導いているもののうち、画面に出るものを並べる。2 つの App が
/// 同じ絵を描くかを比べるため。
#[derive(Debug, PartialEq)]
struct Painted {
    ui: Vec<ratatui::style::Color>,
    ui_light: bool,
    default_fg: ratatui::style::Color,
    page: Option<syntect::highlighting::Color>,
    decoration: DecorationStyles,
    spans: Vec<highlight::TaggedLine>,
    view_rows: Vec<Vec<highlight::Span>>,
    view_decoration: DecorationStyles,
    effects: (bool, bool),
}

fn painted(app: &App) -> Painted {
    Painted {
        ui: vec![
            app.ui_selected_bg,
            app.ui_changed_bg,
            app.ui_deleted_bg,
            app.ui_changed_emph_bg,
            app.ui_deleted_emph_bg,
            app.ui_history_glow_bg,
            app.ui_border,
            app.ui_history_border,
            app.ui_landing_pulse,
            app.ui_scrollbar,
        ],
        ui_light: app.ui_light,
        default_fg: app.highlight.default_fg(),
        page: app.highlight.theme().settings.background,
        decoration: app.decoration_styles,
        spans: app.spans.clone(),
        view_rows: app.view.rows.clone(),
        view_decoration: app.view.decoration_styles,
        effects: (app.time_machine_fx.is_some(), app.starfield_fx.is_some()),
    }
}

fn pair() -> ThemePair {
    ThemePair { dark: Some(DARK.into()), light: Some(LIGHT.into()) }
}

#[test]
fn a_switch_repaints_everything_derived_from_light_as_if_started_there() {
    for (from, to) in [(false, true), (true, false)] {
        let (mut switched, _a) = session(pair(), None, from);
        let (started, _b) = session(pair(), None, to);
        assert_ne!(painted(&switched), painted(&started), "{from}→{to}: 前提 — 配色で絵が違う");
        assert!(switched.follow_color_scheme(to), "{from}→{to}: 作り直した");
        assert_eq!(painted(&switched), painted(&started), "{from}→{to}");
    }
}

#[test]
fn the_theme_is_the_side_of_the_pair_for_the_new_background() {
    let (mut app, _dir) = session(pair(), None, false);
    let latte = termtheme::theme::by_name(LIGHT).unwrap();
    assert_ne!(app.highlight.theme().settings.background, latte.settings.background);
    app.follow_color_scheme(true);
    assert_eq!(app.highlight.theme().settings.background, latte.settings.background);
    // ダークへ戻ると、元のテーマに戻る。
    app.follow_color_scheme(false);
    let mocha = termtheme::theme::by_name(DARK).unwrap();
    assert_eq!(app.highlight.theme().settings.background, mocha.settings.background);
}

#[test]
fn light_and_dark_flags_pin_the_scheme_and_ignore_notifications() {
    for fixed in [false, true] {
        let (mut app, _dir) = session(pair(), Some(fixed), fixed);
        let before = painted(&app);
        assert!(!app.follows_color_scheme());
        assert!(!app.follow_color_scheme(!fixed), "固定は固定");
        assert_eq!(painted(&app), before);
    }
}

#[test]
fn the_same_scheme_again_changes_nothing() {
    let (mut app, _dir) = session(pair(), None, false);
    app.marks_fx = Some(crate::effects::toast_effect());
    assert!(!app.follow_color_scheme(false));
    assert!(app.marks_fx.is_some(), "同じ配色の知らせでは演出も落とさない");
}

#[test]
fn one_theme_for_both_sides_still_lets_the_ui_colors_follow() {
    // `--theme Dracula`: 構文のテーマは両側で同じでも、UI の色は背景に合わせる。
    let (mut app, _dir) = session(ThemePair::both("Dracula"), None, false);
    let dracula = termtheme::theme::by_name("Dracula").unwrap();
    app.follow_color_scheme(true);
    assert_eq!(app.ui_selected_bg, selected_bg(true));
    assert_eq!(app.ui_border, border_color(true));
    assert_eq!(app.ui_scrollbar, scrollbar_thumb(true));
    assert!(app.ui_light);
    assert_eq!(app.highlight.theme().settings.background, dracula.settings.background);
}

#[test]
fn effects_painted_with_the_old_colors_are_dropped() {
    let (mut app, _dir) = session(pair(), None, false);
    app.warp_fx = Some(crate::effects::warp_effect(true, false));
    app.landing_pulse_fx = Some(crate::effects::landing_pulse_effect(app.ui_landing_pulse));
    app.readout_fx = Some(crate::effects::toast_effect());
    app.marks_fx = Some(crate::effects::toast_effect());
    app.follow_color_scheme(true);
    assert!(app.warp_fx.is_none());
    assert!(app.landing_pulse_fx.is_none());
    assert!(app.readout_fx.is_none());
    assert!(app.marks_fx.is_none());
    // 回り続ける枠と星空は、新しい配色で組み直してある（消えない）。
    assert!(app.time_machine_fx.is_some() && app.starfield_fx.is_some());
}

#[test]
fn a_background_file_is_repainted_when_it_is_opened() {
    let (mut app, _dir) = session(pair(), None, false);
    let (started_light, _b) = session(pair(), None, true);
    app.follow_color_scheme(true);
    // 裏のファイルは印だけ付けて、切り替えのたびに全部を塗り直さない。
    assert!(!app.file_states[0].theme_stale, "表のファイルの枠は空き（塗り直し済み）");
    assert!(app.file_states[1].theme_stale && app.file_states[2].theme_stale);
    for index in [1, 2] {
        app.switch_to_file(index);
        let expected = &started_light.file_states[index];
        assert_eq!(app.spans, expected.spans, "file {index}: source の行");
        assert_eq!(app.view.rows, expected.view.rows, "file {index}: view");
        assert!(!app.file_states[index].theme_stale);
    }
    // 表に出ていたファイルは、裏へ回っても塗り直し済みのまま。
    assert!(!app.file_states[0].theme_stale && !app.file_states[1].theme_stale);
    app.switch_to_file(0);
    assert_eq!(app.spans, started_light.spans);
    assert_eq!(app.view.rows, started_light.view.rows);
}

#[test]
fn the_cursor_stays_on_its_line_through_a_switch() {
    let (mut app, _dir) = session(pair(), None, false);
    app.view.goto_source_line(4);
    app.cursor = 4;
    app.follow_color_scheme(true);
    assert_eq!(app.view.cursor, 4, "view のカーソル行");
    assert_eq!(app.cursor, 4, "source のカーソル行");
}

#[test]
fn the_session_backend_never_asks_the_terminal_where_the_cursor_is() {
    // crossterm の `cursor::position()` は crossterm の入力の読み手を動かす
    // （知らせが届けば止まる）。置いた位置を覚えて返す。
    let mut backend = NoBlinkBackend {
        inner: ratatui::backend::CrosstermBackend::new(Vec::new()),
        out: Vec::new(),
        undercurl: false,
        cursor: ratatui::layout::Position::ORIGIN,
    };
    use ratatui::backend::Backend;
    backend.set_cursor_position((7, 3)).unwrap();
    assert_eq!(backend.get_cursor_position().unwrap(), ratatui::layout::Position::new(7, 3));
}

/// 購読が書いた列を後で見る書き先（`Subscription::with_writer` に渡す）。
#[derive(Clone, Default)]
struct Written(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Written {
    /// 前に取り出してから書かれた分。
    fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
    }
}

impl std::io::Write for Written {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn following_subscribes_once_and_hands_the_terminal_to_the_editor_and_back() {
    // `run()` → エディタ（`open_editor_at` の suspend / resume）→ 終わり
    // （`run()` の stop）で端末へ書く列。起動時は今の配色を聞かない。
    let (mut app, _dir) = session(pair(), None, false);
    let written = Written::default();
    start_following_color_scheme(
        &mut app,
        termtheme::scheme::Subscription::with_writer(written.clone()),
    );
    assert_eq!(written.take(), "\x1b[?2031h");
    app.scheme.suspend().unwrap();
    assert_eq!(written.take(), "\x1b[?2031l", "エディタの前に外す");
    app.scheme.resume().unwrap();
    assert_eq!(written.take(), "\x1b[?2031h\x1b[?996n", "戻ったら張り直して聞く");
    app.scheme.stop().unwrap();
    assert_eq!(written.take(), "\x1b[?2031l", "終わるときに外す");
    drop(app);
    assert_eq!(written.take(), "", "外した後の Drop は書かない");
}

#[test]
fn an_app_that_is_dropped_while_subscribed_unsubscribes() {
    // 早い戻りと panic の unwind: `run()` の stop を通らずに `App` が落ちる。
    let (mut app, _dir) = session(pair(), None, true);
    let written = Written::default();
    start_following_color_scheme(
        &mut app,
        termtheme::scheme::Subscription::with_writer(written.clone()),
    );
    written.take();
    drop(app);
    assert_eq!(written.take(), "\x1b[?2031l");
}

#[test]
fn light_and_dark_flags_never_subscribe() {
    // `--light` / `--dark`: 張らないので、エディタの前後も終わるときも書かない。
    for fixed in [false, true] {
        let (mut app, _dir) = session(pair(), Some(fixed), fixed);
        let written = Written::default();
        start_following_color_scheme(
            &mut app,
            termtheme::scheme::Subscription::with_writer(written.clone()),
        );
        app.scheme.suspend().unwrap();
        app.scheme.resume().unwrap();
        app.scheme.stop().unwrap();
        assert!(!app.scheme.is_subscribed());
        drop(app);
        assert_eq!(written.take(), "", "--{}", if fixed { "light" } else { "dark" });
    }
}

#[test]
fn an_app_built_outside_run_does_not_touch_the_terminal() {
    // テストや TUI を立てない道の `App` は、購読が何もしない形のまま。
    let (app, _dir) = session(pair(), None, false);
    assert!(!app.scheme.is_subscribed());
    assert!(format!("{:?}", app.scheme).contains("\"fixed\""));
}
