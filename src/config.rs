//! Command-line configuration.
//!
//! `akapen <file...> [--send-cmd <cmd>] [--theme <syntect-theme>]
//!              [--theme-dark <syntect-theme>] [--theme-light <syntect-theme>]
//!              [--ime <off|ascii|jp>] [--light|--dark] [--esc-quit <auto|always|never>]
//!              [--semantic <fixture.json> | --semantic-cmd <cmd>]
//!              [--mark-blend <f>] [--dim-blend <f>]`
//! Positional arguments are the files to open (one or more). Unknown flags
//! are ignored (reviewr-style). `--help`/`--version` short-circuit before parsing.
//!
//! 設定ファイル（`$XDG_CONFIG_HOME/akapen/config.toml`、[`crate::config_file`]）が
//! フラグと環境変数の下に入る。優先は**フラグ > 環境変数 > 設定ファイル > 既定**。
//! テーマは `--theme`（両側）> `--theme-dark` / `--theme-light` > `[theme]` > 既定。

use std::path::PathBuf;

use anyhow::{Result, bail};
use termtheme::config::ThemeFlags;
pub use termtheme::theme::ThemePair;

use crate::config_file::ConfigFile;
use crate::decoration::{Decoration, DecorationBlend, DecorationKind};
use crate::ime::ImeMode;

/// Whether `Esc` may quit the app (in normal mode, when nothing more
/// urgent is pending — overlays and the composer always cancel first).
///
/// `Auto` ties the ability to `--callback`: when the app is spawned as a
/// step in a loop (e.g. a file picker that re-runs akapen), quitting via
/// `Esc` is a "return to the loop", not a dead end, so the lightest key
/// becomes available. `Always` opts into `Esc`-to-quit without a callback
/// (e.g. a wrapper script that loops itself); `Never` keeps `Esc` as a
/// pure cancel even when a callback is set (e.g. callback used as an
/// exit hook, not a loop transition).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EscQuit {
    /// `Esc` quits only when `--callback` is set.
    #[default]
    Auto,
    /// `Esc` quits unconditionally.
    Always,
    /// `Esc` never quits; it only cancels (the pre-callback behavior).
    Never,
}

impl EscQuit {
    pub fn parse(s: &str) -> EscQuit {
        match s {
            "always" => EscQuit::Always,
            "never" => EscQuit::Never,
            _ => EscQuit::Auto, // "auto" and unknown values fall back
        }
    }
}

/// What the process should do, resolved from argv.
pub enum Action {
    /// Run the TUI with the parsed configuration.
    ///
    /// **Box に入れてある。** 他の枝はデータを持たないので、`Config` を
    /// 直接抱えると列挙そのものがその大きさになる（`clippy::large_enum_variant`）。
    Run(Box<Config>),
    /// Print usage and exit 0.
    Help,
    /// Print the version and exit 0.
    Version,
    /// `--semantic-cache-clear`: wipe the analysis cache and exit 0.
    ///
    /// **判定器のプロンプトを変えたときの逃げ道**である。キャッシュの
    /// キーは文書の sha とコマンド行の sha なので、同じコマンド行のまま
    /// プロンプトだけ変えると古い項目が当たる
    /// （`docs/gotchas/semantic-reading.md`）。ファイル引数を取らないので、
    /// `--help` と同じく短絡する。
    ///
    /// **marks モードの「問い」だけはこれを待たない** — 問いの文面が鍵に
    /// 入っているので、定型を直せば自動で外れる（`crate::semantic_cache`）。
    /// 判定器の中の文面はどちらのモードでもここが唯一の逃げ道である。
    ClearSemanticCache,
    /// `--review-dismissed-clear`: Review で捨てた記録と送った記録
    /// （`sent.jsonl`）を全部消して exit 0。
    ///
    /// `--semantic-cache-clear` と同じ作り（ファイル引数を取らず短絡する）。
    /// 捨てた候補は一覧に薄く残り 1 本ずつ `x` で戻せるが、まとめて
    /// 戻す道がここである。
    ClearReviewDismissed,
}

/// Resolved runtime configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// The files to open (one or more).
    pub files: Vec<PathBuf>,
    /// `--send-cmd <cmd>`: shell command to pipe the export to on `s`.
    pub send_cmd: Option<String>,
    /// `--send-agent`: resolve the sole herdr agent in the current tab
    /// (else workspace) and submit the export directly (no shell).
    pub send_agent: bool,
    /// `--reply`: omit the `file:lines` location line and the `n: `
    /// line-number prefixes from the export, quoting the snippet
    /// GitHub-style. External changes auto-reload (no ⚡/`r` dance) and
    /// the reload skips the diff — each refresh replaces the whole
    /// message, so a diff would mark everything as changed. For instant
    /// replies to an agent message (see scripts/akp), where the commented
    /// document is the agent's own output.
    pub reply: bool,
    /// シンタックスハイライトのテーマ。背景が dark のときと light のときの
    /// 2 本で、どちらを使うかは背景の判定（`--light` / `--dark`、無ければ起動時の
    /// OSC 11 と、開いているあいだのモード 2031 の知らせ）が決める
    /// （[`ThemePair::for_background`]）。`None` の側は既定
    /// （[`termtheme::theme::default_name`]）。
    ///
    /// 各側は `--theme-dark` / `--theme-light` > 設定ファイルの `[theme]` >
    /// 既定。`--theme <name>` は**両側を上書きする**（どちらでもそれを使う。
    /// 1 本だったころの意味のまま）。値は syntect のテーマ名か `.tmTheme` の
    /// パス（e.g. tokyo-night.tmTheme）。
    pub theme: ThemePair,
    /// `--ime <off|ascii|jp>`: input-source control around the composer.
    pub ime: ImeMode,
    /// `--light` / `--dark`: force the light/dark UI colors (selection/
    /// changed-line backgrounds, view border). `None` (the default) =
    /// auto-detect the terminal background via OSC 11, falling back to
    /// dark when the terminal doesn't answer.
    pub light: Option<bool>,
    /// `--callback <cmd>`: shell command to spawn on exit (e.g. to return
    /// to a file picker). Runs after the TUI is fully shut down.
    pub callback: Option<String>,
    /// `--esc-quit <auto|always|never>`: whether `Esc` may quit the app.
    pub esc_quit: EscQuit,
    /// `--no-fx`: disable the animated time-machine frame (the rotating
    /// purple→pink gradient around the page while browsing the past).
    /// The static history border color stays either way.
    pub fx: bool,
    /// `--no-cursor-anchor`: stop publishing the (hidden) hardware
    /// cursor position at the composer's `▏` while typing. The position
    /// exists so the macOS IME anchors its inline composition window
    /// there; terminals running cursor-following shaders (Ghostty's
    /// cursor_blaze etc.) blaze around that motion, so this lets shader
    /// users trade the IME anchor for a calm composer.
    pub cursor_anchor: bool,
    /// `--semantic <fixture.json>`: the Semantic Reading Layer's
    /// annotation for the document being opened — a
    /// `semantic-reading` [`SemanticDocument`] as JSON. With it the
    /// rendered view paints MARKED / NORMAL / DIM per Atom at the
    /// current READ budget (`-`/`+`, `<`/`>`); without it the layer is
    /// entirely absent (no status readout, no keys).
    ///
    /// The fixture is read and validated before the TUI starts, so a
    /// broken file is an ordinary command-line error rather than a
    /// document that silently paints nothing. Whether it belongs to the
    /// document actually open is a separate, per-document check (its
    /// optional `source_sha256`, see [`crate::semantic::DigestChecked`]).
    ///
    /// [`SemanticDocument`]: semantic_reading::SemanticDocument
    pub semantic: Option<PathBuf>,
    /// `--semantic-cmd <cmd>`: delegate the semantic judgement to an
    /// external command instead of reading a fixture. akapen writes
    /// `{"version":1,"source":…,"atoms":[…]}` to its stdin and reads
    /// `{"version":1,"units":[…]}` back from its stdout; the command
    /// returns Atom INDICES, never ranges, so the positions stay
    /// akapen's own (see [`crate::semantic::CommandProvider`]).
    ///
    /// This is where a Jev adapter plugs in — Jev is a System One model,
    /// NOT an LLM; see `docs/design/jev.md`. akapen itself gains no HTTP client
    /// and no async runtime for it: `--send-cmd`'s arrangement — a shell
    /// command on stdin/stdout — keeps API-key handling out of akapen and
    /// leaves the adapter (atoms in, typed answers back out as units) a
    /// script the user owns.
    ///
    /// Mutually exclusive with `--semantic`: two annotations for one
    /// document is not a configuration, it is a question about which
    /// one wins.
    ///
    /// フラグが無いときは環境変数 [`SEMANTIC_CMD_ENV`]、それも無ければ設定
    /// ファイルの `semantic_cmd` を既定にする（[`Config::parse_with_sources`]）。
    /// **排他はそのまま効く** —— 環境変数や設定ファイルで設定していることを
    /// 忘れて `--semantic <fixture>` を渡したときに黙ってどちらかが勝つのは、
    /// フラグ 2 つのときと同じで「どちらが勝つか」という問いであり、設定ではない。
    ///
    /// The command is NOT run at startup — it runs once a document is
    /// on screen, on its own thread, because a process launch plus a
    /// network round trip does not return within a frame (see
    /// [`crate::app::App::reanalyze_semantics`]).
    pub semantic_cmd: Option<String>,
    /// `--marks-questions <path>`: read the marks-mode questions from
    /// this file instead of the built-in set (and instead of
    /// `$XDG_CONFIG_HOME/akapen/marks-questions.json`).
    ///
    /// Measurement and tests point this at a file of their own so they
    /// never read the real user's questions — the same care
    /// [`crate::semantic_cache`] takes with `~/.cache`.
    pub marks_questions: Option<PathBuf>,
    /// `--review-rules <path>`: read the Review mode's rules from this
    /// file instead of the built-in three (and instead of
    /// `$XDG_CONFIG_HOME/akapen/review-rules.json`).
    ///
    /// **marks の問いとは別のファイルである** — Review は別機能で、
    /// 定型の環にも入らない（`docs/design/marks-only-and-review-mode.md`
    /// 4 節、[`crate::review_rules`]）。
    pub review_rules: Option<PathBuf>,
    /// `--review-json`: TUI を立てず、有効なルールの候補を JSON で
    /// stdout に出して終わる。
    ///
    /// **段階 2 と LSP の入口である。** 本文は 1 バイトも出ない
    /// （[`crate::review::to_json`]）。`--semantic-cmd` が要る
    /// （fixture は 1 つの問いにしか答えられない）。
    pub review_json: bool,
    /// `--lint-cmd <cmd>`: Review（`R`）の候補を linter に出させる。
    ///
    /// **判定は linter に任せ、akapen はその後ろの流れ（一覧・捨てる・
    /// accept・送る・`e` で直す・引き継ぎ）だけを受け持つ**
    /// （`docs/design/marks-only-and-review-mode.md` 4 節「判定の出どころと
    /// しての linter」、[`crate::lint`]）。`sh -c` で走らせ、作業ディレクトリは
    /// 文書のあるディレクトリ、文書の絶対パスを最後の引数に足す。
    ///
    /// フラグが無いときは環境変数 [`LINT_CMD_ENV`]、それも無ければ設定ファイルの
    /// `lint_cmd` を既定にする（空・空白は設定していないのと同じ）。
    /// **意味層（`--semantic-cmd`）は要らない。**
    pub lint_cmd: Option<String>,
    /// `--undercurl <auto|on|off>`: Review の下線を波線（`CSI 4:3 m`）に
    /// するか（[`crate::undercurl`]）。フラグが無ければ環境変数
    /// [`crate::undercurl::UNDERCURL_ENV`]、次に設定ファイルの `undercurl`、
    /// どれも無ければ `auto`。
    pub undercurl: crate::undercurl::UndercurlMode,
    /// `--mark-blend <0.0..1.0>` / `--dim-blend <0.0..1.0>`: how strong
    /// the two range-decoration kinds are. `mark` lifts the mark
    /// background off the page toward the text color; `dim` moves a
    /// dimmed foreground toward the page.
    ///
    /// The defaults live on [`DecorationBlend`], not here: the
    /// configuration file ([`crate::config_file`], which does not carry
    /// these yet) belongs BETWEEN the default and this field
    /// (`CLI > config file > default`), and that only works if the default
    /// is a value the layers overwrite rather than an `Option` each layer
    /// re-invents.
    pub decoration_blend: DecorationBlend,
    /// `--decorations <json>`: a hidden development flag that paints
    /// range decorations onto the rendered view, so the layer can be seen
    /// on real documents before a producer (the Semantic Reading Layer)
    /// exists. The value is a JSON array of
    /// `{"range": [start, end], "kind": "mark" | "dim"}`, with `start`
    /// and `end` byte offsets into the file. Empty (and inert) by
    /// default.
    pub decorations: Vec<Decoration>,
}

/// The `--decorations` JSON shape. The wire format lives here, not on
/// [`Decoration`]: the decoration layer is a plain in-process API and
/// stays free of serde.
#[derive(serde::Deserialize)]
struct DecorationSpec {
    range: (usize, usize),
    kind: String,
}

/// Parse a blend factor (`--mark-blend` / `--dim-blend`): a fraction in
/// `0.0..=1.0`.
///
/// Out of range is an ERROR, not a clamp — the same contract
/// `--decorations` has. A silently clamped 1.5 looks exactly like a
/// working 1.0, and the next thing the user does is conclude the flag
/// has no effect.
fn parse_blend(flag: &str, value: &str) -> Result<f32> {
    let n: f32 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag}: {value:?} is not a number (0.0..1.0)"))?;
    if !(0.0..=1.0).contains(&n) {
        // NaN fails this too: it compares false against everything.
        bail!("{flag}: {n} is out of range (0.0..1.0)");
    }
    Ok(n)
}

/// Parse the `--decorations` value. Byte offsets that do not land on a
/// UTF-8 boundary of the document are not rejected here (the file is not
/// open yet) — the decoration layer drops an unusable cut instead of
/// panicking.
fn parse_decorations(json: &str) -> Result<Vec<Decoration>> {
    let specs: Vec<DecorationSpec> = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("--decorations: invalid JSON: {e}"))?;
    specs
        .into_iter()
        .map(|spec| {
            let (start, end) = spec.range;
            if start > end {
                bail!("--decorations: range {start}..{end} runs backwards");
            }
            let kind = match spec.kind.as_str() {
                "mark" | "semantic-mark" => DecorationKind::SemanticMark,
                "dim" => DecorationKind::Dim,
                other => bail!("--decorations: unknown kind {other:?} (mark | dim)"),
            };
            Ok(Decoration { range: start..end, kind })
        })
        .collect()
}

/// `--semantic-cmd` を省いたときの既定を持つ環境変数。
///
/// `~/.zshrc` に 1 行書けば `akapen foo.md` が**どこからでも** marks で開く。
/// 意味層はフラグを書いたときだけ生えるようになっているが、**判定器を選ぶ
/// のは文書ごとの判断ではなく環境の設定**なので、環境変数の方が形に合う。
pub const SEMANTIC_CMD_ENV: &str = "AKAPEN_SEMANTIC_CMD";

/// `--lint-cmd` を省いたときの既定を持つ環境変数。
///
/// [`SEMANTIC_CMD_ENV`] と同じ理由で環境変数を置く — どの linter を使うかは
/// 文書ごとではなく環境の設定である（ルールは linter 自身の設定が決める）。
pub const LINT_CMD_ENV: &str = "AKAPEN_LINT_CMD";

impl Config {
    /// 引数だけで解釈する（**環境は読まない**）。テスト用。
    ///
    /// 環境変数の既定が要るのは [`Config::from_env`] の側だけである。
    /// ここが `std::env::var` を読むと、`AKAPEN_SEMANTIC_CMD` を export して
    /// いる開発者のシェルでだけテストが落ちる（`set_var` がテスト間に漏れる
    /// のと同じ穴の、入口が違うだけのもの）。
    #[cfg(test)]
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Action> {
        Self::parse_with_env(args, |_| None)
    }

    /// 引数と環境だけで解釈する（**設定ファイルは無いものとする**）。テスト用。
    #[cfg(test)]
    pub fn parse_with_env<I, F>(args: I, env: F) -> Result<Action>
    where
        I: IntoIterator<Item = String>,
        F: Fn(&str) -> Option<String>,
    {
        Self::parse_with_sources(args, env, || Ok(None))
    }

    /// Parse the process arguments (after argv[0]) with an environment and
    /// a configuration file.
    ///
    /// All non-flag tokens are files; `--send-cmd`/`--theme`/`--ime` take a
    /// value; `-h`/`--help` and `-V`/`--version` short-circuit. At least
    /// one file is required.
    ///
    /// `env` は環境変数 1 つを引く関数、`config_file` は設定ファイルを読む
    /// 関数である。**実環境と実ファイルを読むのは [`Config::from_env`] だけ**で、
    /// テストは好きな値を注入できる。
    ///
    /// `config_file` は**短絡の後で**呼ぶ。壊れた設定ファイルは起動時の
    /// エラーだが、それで `--help` まで読めなくなると直し方を調べる道が無い。
    pub fn parse_with_sources<I, F, C>(args: I, env: F, config_file: C) -> Result<Action>
    where
        I: IntoIterator<Item = String>,
        F: Fn(&str) -> Option<String>,
        C: FnOnce() -> Result<Option<ConfigFile>>,
    {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut send_cmd: Option<String> = None;
        let mut send_agent = false;
        let mut reply = false;
        let mut theme: Option<String> = None;
        let mut theme_dark: Option<String> = None;
        let mut theme_light: Option<String> = None;
        let mut ime = ImeMode::Ascii;
        let mut light: Option<bool> = None;
        let mut callback: Option<String> = None;
        let mut esc_quit = EscQuit::Auto;
        let mut fx = true;
        let mut cursor_anchor = true;
        let mut decorations: Vec<Decoration> = Vec::new();
        let mut semantic: Option<PathBuf> = None;
        let mut semantic_cmd: Option<String> = None;
        let mut marks_questions: Option<PathBuf> = None;
        let mut review_rules: Option<PathBuf> = None;
        let mut review_json = false;
        let mut lint_cmd: Option<String> = None;
        let mut undercurl: Option<crate::undercurl::UndercurlMode> = None;
        let mut decoration_blend = DecorationBlend::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Action::Help),
                "--semantic-cache-clear" => return Ok(Action::ClearSemanticCache),
                "--review-dismissed-clear" => return Ok(Action::ClearReviewDismissed),
                "-V" | "--version" => return Ok(Action::Version),
                "--light" => light = Some(true),
                "--dark" => light = Some(false),
                "--callback" => callback = it.next(),
                "--esc-quit" => {
                    if let Some(v) = it.next() {
                        esc_quit = EscQuit::parse(&v);
                    }
                }
                "--no-fx" => fx = false,
                "--no-cursor-anchor" => cursor_anchor = false,
                "--send-cmd" => send_cmd = it.next(),
                "--send-agent" => send_agent = true,
                "--reply" => reply = true,
                "--theme" => theme = it.next(),
                "--theme-dark" => theme_dark = it.next(),
                "--theme-light" => theme_light = it.next(),
                "--decorations" => {
                    if let Some(v) = it.next() {
                        decorations = parse_decorations(&v)?;
                    }
                }
                "--semantic" => semantic = it.next().map(PathBuf::from),
                "--semantic-cmd" => semantic_cmd = it.next(),
                "--marks-questions" => marks_questions = it.next().map(PathBuf::from),
                "--review-rules" => review_rules = it.next().map(PathBuf::from),
                "--review-json" => review_json = true,
                "--lint-cmd" => lint_cmd = it.next(),
                "--undercurl" => {
                    undercurl = it.next().map(|v| crate::undercurl::UndercurlMode::parse(&v));
                }
                "--mark-blend" => {
                    if let Some(v) = it.next() {
                        decoration_blend.mark = parse_blend("--mark-blend", &v)?;
                    }
                }
                "--dim-blend" => {
                    if let Some(v) = it.next() {
                        decoration_blend.dim = parse_blend("--dim-blend", &v)?;
                    }
                }
                "--ime" => {
                    if let Some(v) = it.next() {
                        ime = ImeMode::parse(&v);
                    }
                }
                other if !other.starts_with('-') => {
                    files.push(PathBuf::from(other));
                }
                _ => {} // unknown flags ignored, like reviewr
            }
        }
        if files.is_empty() {
            bail!(
                "usage: akapen <file...> [--send-cmd <cmd> | --send-agent] [--reply] [--theme <name>] [--theme-dark <name>] [--theme-light <name>] [--ime <off|ascii|jp>] [--light|--dark] [--semantic <fixture.json> | --semantic-cmd <cmd>]"
            );
        }
        // 設定ファイルは短絡と usage の後で読む（`--help` を壊さない）。
        let file = config_file()?;
        let file = file.as_ref();
        // テーマ。`--theme` は両側を上書きする（1 本だったころの意味のまま、
        // `--theme-dark` / `--theme-light` より強い）。各側はフラグ > 設定
        // ファイル > 既定（`None`、[`crate::highlight::Highlighter::new`]）。
        let theme = ThemeFlags { both: theme, dark: theme_dark, light: theme_light }
            .over(file.map(|f| &f.theme));
        // `--semantic-cmd` を書いていなければ環境変数を、それも無ければ
        // 設定ファイルを既定にする。**フラグが勝ち、環境変数が設定ファイルに
        // 勝つ。**
        //
        // 環境変数が**ある**なら、空・空白でも設定ファイルは見ない —— 空は
        // 「層を外す」である（`export AKAPEN_SEMANTIC_CMD=` で一時的に外せる。
        // 設定ファイルに書いてあっても外せないと、この道が消える）。
        //
        // `semantic_cmd_from` は出どころ。排他のエラーで言う。
        let mut semantic_cmd_from: Option<String> = None;
        if semantic_cmd.is_none() {
            match env(SEMANTIC_CMD_ENV) {
                Some(v) => {
                    if !v.trim().is_empty() {
                        semantic_cmd = Some(v);
                        semantic_cmd_from = Some(SEMANTIC_CMD_ENV.to_string());
                    }
                }
                None => {
                    if let Some(f) = file
                        && let Some(v) = &f.semantic_cmd
                    {
                        semantic_cmd = Some(v.clone());
                        semantic_cmd_from = Some(format!("semantic_cmd in {}", f.path.display()));
                    }
                }
            }
        }
        // `--lint-cmd` も同じ作法。フラグ > 環境変数 > 設定ファイル。環境変数が
        // あれば（空でも）設定ファイルは見ない。空・空白は「無い」と同じ。
        let lint_cmd = lint_cmd
            .or_else(|| env(LINT_CMD_ENV))
            .or_else(|| file.and_then(|f| f.lint_cmd.clone()))
            .filter(|cmd| !cmd.trim().is_empty());
        // `--undercurl` も同じ作法。環境変数の値の読み方は今までどおり
        // （知らない値は auto）で、あれば設定ファイルは見ない。
        let undercurl = undercurl
            .or_else(|| {
                env(crate::undercurl::UNDERCURL_ENV)
                    .map(|v| crate::undercurl::UndercurlMode::parse(&v))
            })
            .or_else(|| file.and_then(|f| f.undercurl))
            .unwrap_or_default();
        if send_cmd.is_some() && send_agent {
            bail!("--send-cmd and --send-agent are mutually exclusive");
        }
        // Two annotations for one document is not a configuration: the
        // fixture and the command would each claim the same Atom list,
        // and whichever lost would still be what the user asked for.
        if semantic.is_some() && semantic_cmd.is_some() {
            // 出どころを添える。環境変数・設定ファイル由来のときは、打った
            // 覚えのない `--semantic-cmd` を名指しされることになるので、どこで
            // 設定したのかが言えないとユーザーは自分の shell を疑うところから
            // 始める。
            if let Some(from) = semantic_cmd_from {
                bail!(
                    "--semantic and --semantic-cmd are mutually exclusive (--semantic-cmd from {from})"
                );
            }
            bail!("--semantic and --semantic-cmd are mutually exclusive");
        }
        // `--marks-questions` は層が無ければ読まれない。「書いたのに
        // 効かない」を黙って通さず、ここで言う。
        let no_layer = semantic.is_none() && semantic_cmd.is_none();
        if marks_questions.is_some() && no_layer {
            bail!("--marks-questions needs --semantic or --semantic-cmd");
        }
        // Review も同じ。層が無ければルールは 1 度も使われない。
        if review_rules.is_some() && no_layer {
            bail!("--review-rules needs --semantic or --semantic-cmd");
        }
        // `--review-json` は判定器を**必ず**呼ぶ（有効なルール 1 本ごとに
        // 1 往復）。fixture は 1 つの問いへの固定の答えなので、ルールの
        // 文面で聞き直す道が無い — 黙って 1 本ぶんだけ出すより、断る。
        if review_json {
            if semantic.is_some() {
                bail!("--review-json needs --semantic-cmd (a fixture answers only one question)");
            }
            if semantic_cmd.is_none() {
                bail!(
                    "--review-json needs --semantic-cmd (or {SEMANTIC_CMD_ENV}, or semantic_cmd in the config file)"
                );
            }
        }
        Ok(Action::Run(Box::new(Config {
            files,
            send_cmd,
            send_agent,
            reply,
            theme,
            ime,
            light,
            callback,
            esc_quit,
            fx,
            cursor_anchor,
            semantic,
            semantic_cmd,
            marks_questions,
            review_rules,
            review_json,
            lint_cmd,
            undercurl,
            decoration_blend,
            decorations,
        })))
    }

    /// Parse from the real process arguments, **the real environment and
    /// the real configuration file** ([`ConfigFile::discover`]).
    pub fn from_env() -> Result<Action> {
        Self::parse_with_sources(
            std::env::args().skip(1),
            |name| std::env::var(name).ok(),
            ConfigFile::discover,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Config, EscQuit, SEMANTIC_CMD_ENV, ThemePair};
    use crate::config_file::ConfigFile;
    use std::path::Path;
    use crate::decoration::{Decoration, DecorationBlend, DecorationKind};

    fn parse(args: &[&str]) -> Action {
        Config::parse(args.iter().map(|s| (*s).to_string())).unwrap()
    }

    fn cfg(action: &Action) -> &Config {
        match action {
            Action::Run(c) => c,
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn positional_files_with_flags() {
        let action = parse(&["--send-cmd", "herdr pane run p1", "docs/design.md"]);
        let c = cfg(&action);
        assert_eq!(c.files.len(), 1);
        assert_eq!(c.files[0].to_str(), Some("docs/design.md"));
        assert_eq!(c.send_cmd.as_deref(), Some("herdr pane run p1"));
        assert!(!c.send_agent);
    }

    #[test]
    fn send_agent_flag_parses() {
        let action = parse(&["--send-agent", "a.md"]);
        let c = cfg(&action);
        assert!(c.send_agent);
        assert!(c.send_cmd.is_none());
        assert!(!c.reply, "--reply defaults to off");
    }

    #[test]
    fn reply_flag_parses() {
        let action = parse(&["--reply", "a.md"]);
        let c = cfg(&action);
        assert!(c.reply);
        assert!(!c.send_agent, "--reply is independent of --send-agent");
    }

    #[test]
    fn send_cmd_and_send_agent_are_mutually_exclusive() {
        assert!(Config::parse(
            ["--send-agent", "--send-cmd", "cat", "a.md"]
                .iter()
                .map(|s| s.to_string())
        )
        .is_err());
    }

    #[test]
    fn multiple_files_collected() {
        let action = parse(&["a.md", "b.md", "c.md"]);
        let c = cfg(&action);
        assert_eq!(c.files.len(), 3);
        assert_eq!(c.files[0].to_str(), Some("a.md"));
        assert_eq!(c.files[2].to_str(), Some("c.md"));
    }

    #[test]
    fn files_surrounding_flags() {
        let action = parse(&["a.md", "--theme", "base16-ocean.dark", "b.md"]);
        let c = cfg(&action);
        assert_eq!(c.files.len(), 2);
        assert_eq!(c.files[0].to_str(), Some("a.md"));
        assert_eq!(c.files[1].to_str(), Some("b.md"));
        assert_eq!(c.theme, ThemePair::both("base16-ocean.dark"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(parse(&["--help"]), Action::Help));
        assert!(matches!(parse(&["-h", "x.md"]), Action::Help));
        assert!(matches!(parse(&["--version"]), Action::Version));
        assert!(matches!(parse(&["-V"]), Action::Version));
        assert!(matches!(parse(&["--semantic-cache-clear"]), Action::ClearSemanticCache));
        assert!(matches!(
            parse(&["--review-dismissed-clear", "x.md"]),
            Action::ClearReviewDismissed
        ));
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(Config::parse(std::iter::empty::<String>()).is_err());
    }

    #[test]
    fn unknown_flags_are_ignored() {
        let action = parse(&["--bogus", "x.md", "--wrap", "off"]);
        let c = cfg(&action);
        assert_eq!(c.files[0].to_str(), Some("x.md"));
    }

    #[test]
    fn light_dark_flags_parse() {
        use crate::ime::ImeMode;
        // Default: auto-detect (None).
        assert_eq!(cfg(&parse(&["x.md"])).light, None);
        assert_eq!(cfg(&parse(&["x.md", "--light"])).light, Some(true));
        assert_eq!(cfg(&parse(&["x.md", "--dark"])).light, Some(false));
        // The last of --light/--dark wins.
        assert_eq!(
            cfg(&parse(&["x.md", "--light", "--dark"])).light,
            Some(false)
        );
        assert_eq!(cfg(&parse(&["x.md"])).ime, ImeMode::Ascii);
    }

    #[test]
    fn fx_defaults_to_on_and_no_fx_disables_it() {
        assert!(cfg(&parse(&["x.md"])).fx, "--fx is the default");
        assert!(!cfg(&parse(&["x.md", "--no-fx"])).fx);
        assert!(
            cfg(&parse(&["x.md", "--no-fx", "--send-agent"])).send_agent,
            "--no-fx is independent of other flags"
        );
    }

    #[test]
    fn cursor_anchor_defaults_to_on_and_no_cursor_anchor_disables_it() {
        assert!(
            cfg(&parse(&["x.md"])).cursor_anchor,
            "publishing the cursor position is the default (IME anchor)"
        );
        assert!(!cfg(&parse(&["x.md", "--no-cursor-anchor"])).cursor_anchor);
        assert!(
            cfg(&parse(&["x.md", "--no-cursor-anchor", "--send-agent"])).send_agent,
            "--no-cursor-anchor is independent of other flags"
        );
    }

    #[test]
    fn esc_quit_defaults_to_auto() {
        assert_eq!(cfg(&parse(&["x.md"])).esc_quit, EscQuit::Auto);
        assert_eq!(
            cfg(&parse(&["x.md", "--callback", "fzf"])).esc_quit,
            EscQuit::Auto
        );
    }

    #[test]
    fn esc_quit_parses_all_three_values() {
        assert_eq!(
            cfg(&parse(&["x.md", "--esc-quit", "always"])).esc_quit,
            EscQuit::Always
        );
        assert_eq!(
            cfg(&parse(&["x.md", "--esc-quit", "never"])).esc_quit,
            EscQuit::Never
        );
        assert_eq!(
            cfg(&parse(&["x.md", "--esc-quit", "auto"])).esc_quit,
            EscQuit::Auto
        );
        // Unknown values fall back to the neutral default.
        assert_eq!(
            cfg(&parse(&["x.md", "--esc-quit", "bogus"])).esc_quit,
            EscQuit::Auto
        );
    }

    #[test]
    fn ime_mode_parses_and_defaults_to_ascii() {
        use crate::ime::ImeMode;
        assert_eq!(cfg(&parse(&["x.md"])).ime, ImeMode::Ascii);
        assert_eq!(cfg(&parse(&["x.md", "--ime", "off"])).ime, ImeMode::Off);
        assert_eq!(cfg(&parse(&["x.md", "--ime", "jp"])).ime, ImeMode::Jp);
        assert_eq!(
            cfg(&parse(&["x.md", "--ime", "ascii"])).ime,
            ImeMode::Ascii
        );
        // Unknown values fall back to the neutral default.
        assert_eq!(
            cfg(&parse(&["x.md", "--ime", "bogus"])).ime,
            ImeMode::Ascii
        );
    }

    #[test]
    fn semantic_defaults_to_off_and_takes_a_path() {
        assert!(
            cfg(&parse(&["x.md"])).semantic.is_none(),
            "the Semantic Reading Layer is opt-in"
        );
        let action = parse(&["x.md", "--semantic", "examples/semantic/demo-marks.json"]);
        assert_eq!(
            cfg(&action).semantic.as_deref(),
            Some(std::path::Path::new("examples/semantic/demo-marks.json"))
        );
        // The fixture itself is read at startup, not here — a path that
        // does not exist is not a PARSE error.
        assert!(
            cfg(&parse(&["x.md", "--semantic", "nope.json"])).semantic.is_some()
        );
    }

    #[test]
    fn semantic_cmd_parses_and_defaults_to_off() {
        assert!(cfg(&parse(&["x.md"])).semantic_cmd.is_none());
        let action = parse(&["x.md", "--semantic-cmd", "annotate-doc --model fast"]);
        assert_eq!(
            cfg(&action).semantic_cmd.as_deref(),
            Some("annotate-doc --model fast"),
            "the whole string is one shell command, spaces and all"
        );
        assert!(
            cfg(&action).semantic.is_none(),
            "--semantic-cmd does not imply a fixture"
        );
        // The command is not run here — a nonsense command still parses.
        assert!(
            cfg(&parse(&["x.md", "--semantic-cmd", "exit 1"]))
                .semantic_cmd
                .is_some()
        );
    }

    #[test]
    fn semantic_and_semantic_cmd_are_mutually_exclusive() {
        // Two annotations for one document is a question, not a
        // configuration — same shape as --send-cmd / --send-agent.
        for args in [
            vec!["x.md", "--semantic", "d.json", "--semantic-cmd", "annotate-doc"],
            vec!["x.md", "--semantic-cmd", "annotate-doc", "--semantic", "d.json"],
        ] {
            let err = match Config::parse(args.iter().map(|s| s.to_string())) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("併用はエラーであるべき: {args:?}"),
            };
            assert!(err.contains("--semantic"), "{err}");
            assert!(err.contains("--semantic-cmd"), "{err}");
        }
        // Either one alone is fine.
        assert!(Config::parse(
            ["x.md", "--semantic-cmd", "annotate-doc"]
                .iter()
                .map(|s| s.to_string())
        )
        .is_ok());
        assert!(
            Config::parse(["x.md", "--semantic", "d.json"].iter().map(|s| s.to_string())).is_ok()
        );
    }

    /// 環境変数 1 つだけを持つ `env` を作る。
    fn one_var(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |asked| (asked == name).then(|| value.to_string())
    }

    fn parse_with(args: &[&str], env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Action> {
        Config::parse_with_env(args.iter().map(|s| (*s).to_string()), env)
    }

    #[test]
    fn undercurl_defaults_to_auto_reads_its_env_and_the_flag_wins() {
        use crate::undercurl::{UNDERCURL_ENV, UndercurlMode};
        assert_eq!(cfg(&parse(&["x.md"])).undercurl, UndercurlMode::Auto);
        assert_eq!(cfg(&parse(&["x.md", "--undercurl", "off"])).undercurl, UndercurlMode::Off);
        let action = parse_with(&["x.md"], one_var(UNDERCURL_ENV, "on")).unwrap();
        assert_eq!(cfg(&action).undercurl, UndercurlMode::On);
        let action =
            parse_with(&["x.md", "--undercurl", "off"], one_var(UNDERCURL_ENV, "on")).unwrap();
        assert_eq!(cfg(&action).undercurl, UndercurlMode::Off, "フラグが勝つ");
    }

    #[test]
    fn the_lint_cmd_needs_no_layer_and_its_env_is_the_default() {
        use super::LINT_CMD_ENV;
        assert!(cfg(&parse(&["x.md"])).lint_cmd.is_none());
        // 意味層（`--semantic-cmd`）無しで立つ。
        let action = parse(&["x.md", "--lint-cmd", "textlint-diagnostics.py"]);
        assert_eq!(cfg(&action).lint_cmd.as_deref(), Some("textlint-diagnostics.py"));
        assert!(cfg(&action).semantic_cmd.is_none());
        let action = parse_with(&["x.md"], one_var(LINT_CMD_ENV, "from-env")).unwrap();
        assert_eq!(cfg(&action).lint_cmd.as_deref(), Some("from-env"));
        let action =
            parse_with(&["x.md", "--lint-cmd", "flag"], one_var(LINT_CMD_ENV, "env")).unwrap();
        assert_eq!(cfg(&action).lint_cmd.as_deref(), Some("flag"), "フラグが勝つ");
        for blank in ["", "  "] {
            let action = parse_with(&["x.md"], move |n| (n == LINT_CMD_ENV).then(|| blank.into()))
                .unwrap();
            assert!(cfg(&action).lint_cmd.is_none(), "{blank:?}");
        }
    }

    #[test]
    fn the_semantic_cmd_env_is_the_default_and_the_flag_wins() {
        use super::SEMANTIC_CMD_ENV;
        // 何も無ければ層は生えない —— `parse` は環境を読まないので、
        // このテストは `AKAPEN_SEMANTIC_CMD` を export している端末でも
        // 同じ答えを出す。
        assert!(cfg(&parse(&["x.md"])).semantic_cmd.is_none());

        // 環境変数が既定になる（`akapen foo.md` がそのまま marks で開く）。
        let action = parse_with(&["x.md"], one_var(SEMANTIC_CMD_ENV, "jev-annotate")).unwrap();
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("jev-annotate"));

        // 書いてあるフラグが勝つ。
        let action = parse_with(
            &["x.md", "--semantic-cmd", "from-the-flag"],
            one_var(SEMANTIC_CMD_ENV, "jev-annotate"),
        )
        .unwrap();
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("from-the-flag"));

        // 空・空白だけは「設定していない」と同じ（一時的に外せる）。
        for blank in ["", "   "] {
            let action =
                Config::parse_with_env(["x.md"].iter().map(|s| (*s).to_string()), |asked| {
                    (asked == SEMANTIC_CMD_ENV).then(|| blank.to_string())
                })
                .unwrap();
            assert!(cfg(&action).semantic_cmd.is_none(), "{blank:?}");
        }

        // 他の環境変数には反応しない。
        let action = parse_with(&["x.md"], one_var("AKAPEN_CACHE_DIR", "/tmp/x")).unwrap();
        assert!(cfg(&action).semantic_cmd.is_none());
    }

    #[test]
    fn the_env_default_still_collides_with_a_fixture_and_says_where_it_came_from() {
        use super::SEMANTIC_CMD_ENV;
        // 排他は環境変数由来でも効く。ただし打った覚えのない
        // `--semantic-cmd` を名指しされる側なので、出どころを添える。
        let err = match parse_with(
            &["x.md", "--semantic", "d.json"],
            one_var(SEMANTIC_CMD_ENV, "jev-annotate"),
        ) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("環境変数由来でも併用はエラーであるべき"),
        };
        assert!(err.contains("mutually exclusive"), "{err}");
        assert!(err.contains(SEMANTIC_CMD_ENV), "{err}");

        // フラグで書いたときは出どころを言わない（言うことが無い）。
        let err = match parse_with(
            &["x.md", "--semantic", "d.json", "--semantic-cmd", "c"],
            |_| None,
        ) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("併用はエラーであるべき"),
        };
        assert!(!err.contains(SEMANTIC_CMD_ENV), "{err}");
    }

    #[test]
    fn the_env_default_counts_as_a_layer_for_the_questions_check() {
        use super::SEMANTIC_CMD_ENV;
        // 「層が無いのに問いのファイルを書いた」の検査は、環境変数で層が
        // あるなら通さなければならない。通さないと `--marks-questions` が
        // `~/.zshrc` の 1 行のせいでだけ落ちる。
        assert!(
            parse_with(
                &["x.md", "--marks-questions", "q.json"],
                one_var(SEMANTIC_CMD_ENV, "jev-annotate"),
            )
            .is_ok()
        );
        assert!(
            parse_with(&["x.md", "--marks-questions", "q.json"], |_| None).is_err(),
            "層が無いままファイルだけ書いたら今までどおり止まる"
        );
    }

    #[test]
    fn a_layerless_start_passes_but_a_flag_that_needs_the_layer_does_not() {
        // 層を渡さない起動（`akapen foo.md`）は通る。
        assert!(
            Config::parse(["x.md"].iter().map(|s| s.to_string())).is_ok(),
            "層を渡さない起動は通る"
        );
        // 書いたのに効かない、は言う。
        let err = match Config::parse(
            ["x.md", "--marks-questions", "q.json"].iter().map(|s| s.to_string()),
        ) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("層の無いフラグはエラーであるべき"),
        };
        assert!(err.contains("--semantic"), "{err}");
        // 層があれば通る。
        assert!(
            Config::parse(
                ["x.md", "--semantic", "d.json", "--marks-questions", "q.json"]
                    .iter()
                    .map(|s| s.to_string())
            )
            .is_ok()
        );
    }

    #[test]
    fn blend_factors_default_to_the_shipped_constants() {
        use crate::decoration::{DIM_BLEND, MARK_BG_BLEND};
        // The defaults live on DecorationBlend, not in the parser: a
        // config file would slot in between, and that only works if
        // there is one value to overwrite.
        let action = parse(&["x.md"]);
        let c = cfg(&action);
        assert_eq!(c.decoration_blend, DecorationBlend::default());
        assert_eq!(c.decoration_blend.mark, MARK_BG_BLEND);
        assert_eq!(c.decoration_blend.dim, DIM_BLEND);
    }

    #[test]
    fn blend_factors_parse_and_are_independent() {
        let action = parse(&["x.md", "--mark-blend", "0.4"]);
        let c = cfg(&action);
        assert_eq!(c.decoration_blend.mark, 0.4);
        assert_eq!(
            c.decoration_blend.dim,
            DecorationBlend::default().dim,
            "--mark-blend leaves the other alone"
        );
        let action = parse(&["x.md", "--dim-blend", "0.85"]);
        let c = cfg(&action);
        assert_eq!(c.decoration_blend.dim, 0.85);
        assert_eq!(c.decoration_blend.mark, DecorationBlend::default().mark);
        let action = parse(&["x.md", "--mark-blend", "0", "--dim-blend", "1"]);
        let c = cfg(&action);
        assert_eq!((c.decoration_blend.mark, c.decoration_blend.dim), (0.0, 1.0));
    }

    #[test]
    fn an_out_of_range_blend_is_an_error_not_a_clamp() {
        // A silently clamped 1.5 looks exactly like a working 1.0, and
        // the next thing the user concludes is that the flag does
        // nothing. Same contract as --decorations.
        for flag in ["--mark-blend", "--dim-blend"] {
            for bad in ["1.5", "-0.1", "2", "NaN", "", "half", "0.5x"] {
                assert!(
                    Config::parse(["x.md", flag, bad].iter().map(|s| s.to_string())).is_err(),
                    "{flag} {bad:?} should be rejected"
                );
            }
        }
    }

    #[test]
    fn decorations_parse_from_json_and_default_to_none() {
        assert!(cfg(&parse(&["x.md"])).decorations.is_empty());
        let action = parse(&[
            "x.md",
            "--decorations",
            r#"[{"range":[3,9],"kind":"mark"},{"range":[9,12],"kind":"dim"}]"#,
        ]);
        assert_eq!(
            cfg(&action).decorations,
            vec![
                Decoration { range: 3..9, kind: DecorationKind::SemanticMark },
                Decoration { range: 9..12, kind: DecorationKind::Dim },
            ]
        );
    }

    #[test]
    fn a_malformed_decorations_value_is_an_error_not_a_silent_no_op() {
        // A hidden development flag still fails loudly: a typo that
        // silently painted nothing would be read as "the layer is broken".
        for bad in [
            "not json",
            r#"[{"range":[1,2],"kind":"glow"}]"#,
            r#"[{"range":[9,4],"kind":"mark"}]"#,
            r#"[{"kind":"mark"}]"#,
        ] {
            assert!(
                Config::parse(
                    ["x.md", "--decorations", bad].iter().map(|s| s.to_string())
                )
                .is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    // ---- Review（`R` / `--review-json`） -----------------------------

    /// 断りの文面。`Action` は `Debug` を持たないので `unwrap_err` は
    /// 使えない — 環境変数も空にして、フラグだけで断られることを見る。
    fn refusal(args: &[&str]) -> String {
        match Config::parse_with_env(args.iter().map(|s| (*s).to_string()), |_| None) {
            Ok(_) => panic!("{args:?} が通ってしまった"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn review_rules_needs_the_layer() {
        // 「書いたのに効かない」を黙って通さない（`--marks-questions` と
        // 同じ判断）。
        assert!(
            Config::parse(["x.md", "--review-rules", "r.json"].iter().map(|s| s.to_string()))
                .is_err()
        );
        let action = parse(&["x.md", "--semantic-cmd", "cat", "--review-rules", "r.json"]);
        assert_eq!(cfg(&action).review_rules.as_deref(), Some(Path::new("r.json")));
    }

    #[test]
    fn review_json_refuses_a_session_that_cannot_ask_the_analyser() {
        // ルール 1 本ごとに判定器を 1 往復するので、コマンドが要る。
        let err = refusal(&["x.md", "--review-json"]);
        assert!(err.contains("--review-json needs --semantic-cmd"), "{err}");

        // fixture は 1 つの問いへの固定の答えで、ルールの文面で聞き直す
        // 道が無い。黙って 1 本ぶんだけ出すより断る。
        let err = refusal(&["x.md", "--semantic", "f.json", "--review-json"]);
        assert!(err.contains("a fixture answers only one question"), "{err}");
    }

    #[test]
    fn review_json_rides_the_environment_variable_like_the_tui_does() {
        // `$AKAPEN_SEMANTIC_CMD` だけでも立つ（フラグを打っていない人が
        // `--review-json` だけで使える）。
        let action = Config::parse_with_env(
            ["x.md", "--review-json"].iter().map(|s| s.to_string()),
            |name| (name == SEMANTIC_CMD_ENV).then(|| "cat".to_string()),
        )
        .unwrap();
        assert!(cfg(&action).review_json);
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("cat"));
    }

    #[test]
    fn review_json_is_off_unless_asked_for() {
        let action = parse(&["x.md"]);
        assert!(!cfg(&action).review_json);
        assert!(cfg(&action).review_rules.is_none());
    }

    // ---- 設定ファイル（`config.toml`） -------------------------------

    const FILE_PATH: &str = "/cfg/akapen/config.toml";

    /// 設定ファイルの中身を注入して解釈する（実ファイルは読まない）。
    fn parse_with_file(
        args: &[&str],
        env: impl Fn(&str) -> Option<String>,
        toml: &str,
    ) -> anyhow::Result<Action> {
        let file = ConfigFile::parse(Path::new(FILE_PATH), toml, None).unwrap();
        Config::parse_with_sources(args.iter().map(|s| (*s).to_string()), env, move || {
            Ok(Some(file))
        })
    }

    fn with_file(args: &[&str], toml: &str) -> Action {
        parse_with_file(args, |_| None, toml).unwrap()
    }

    fn file_refusal(args: &[&str], env: impl Fn(&str) -> Option<String>, toml: &str) -> String {
        match parse_with_file(args, env, toml) {
            Ok(_) => panic!("{args:?} が通ってしまった"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn the_theme_sides_default_to_none_and_pick_by_background() {
        let action = parse(&["x.md"]);
        assert_eq!(cfg(&action).theme, ThemePair::default(), "既定は Highlighter が選ぶ");
        let themes = ThemePair { dark: Some("D".into()), light: Some("L".into()) };
        assert_eq!(themes.for_background(false), Some("D"));
        assert_eq!(themes.for_background(true), Some("L"));
    }

    #[test]
    fn the_theme_dark_and_light_flags_set_one_side_each() {
        let action = parse(&["x.md", "--theme-dark", "Dracula"]);
        assert_eq!(
            cfg(&action).theme,
            ThemePair { dark: Some("Dracula".into()), light: None },
            "もう片側は既定のまま"
        );
        let action = parse(&["x.md", "--theme-light", "Catppuccin Latte", "--theme-dark", "Nord"]);
        assert_eq!(
            cfg(&action).theme,
            ThemePair { dark: Some("Nord".into()), light: Some("Catppuccin Latte".into()) }
        );
    }

    #[test]
    fn the_theme_flag_covers_both_sides_and_beats_the_per_side_flags() {
        // 1 本だったころの意味を変えない。どちらの背景でもそれを使う。
        let action = parse(&["x.md", "--theme-dark", "Nord", "--theme", "Dracula"]);
        assert_eq!(cfg(&action).theme, ThemePair::both("Dracula"));
        // 順番に依らない（後ろの `--theme-light` にも勝つ）。
        let action = parse(&["x.md", "--theme", "Dracula", "--theme-light", "Catppuccin Latte"]);
        assert_eq!(cfg(&action).theme, ThemePair::both("Dracula"));
    }

    #[test]
    fn the_config_file_theme_sits_under_the_flags() {
        let toml = "[theme]\ndark = \"Catppuccin Mocha\"\nlight = \"Catppuccin Latte\"\n";
        // 設定ファイルだけ。
        let action = with_file(&["x.md"], toml);
        assert_eq!(
            cfg(&action).theme,
            ThemePair {
                dark: Some("Catppuccin Mocha".into()),
                light: Some("Catppuccin Latte".into())
            }
        );
        // 片側のフラグはその側だけを上書きする。
        let action = with_file(&["x.md", "--theme-light", "Solarized (light)"], toml);
        assert_eq!(
            cfg(&action).theme,
            ThemePair {
                dark: Some("Catppuccin Mocha".into()),
                light: Some("Solarized (light)".into())
            }
        );
        // `--theme` は両側を上書きする。
        let action = with_file(&["x.md", "--theme", "Dracula"], toml);
        assert_eq!(cfg(&action).theme, ThemePair::both("Dracula"));
        // 片側だけ書いた設定ファイルは、もう片側を既定に残す。
        let action = with_file(&["x.md"], "[theme]\nlight = \"Catppuccin Latte\"\n");
        assert_eq!(
            cfg(&action).theme,
            ThemePair { dark: None, light: Some("Catppuccin Latte".into()) }
        );
    }

    #[test]
    fn the_config_file_semantic_cmd_sits_under_the_flag_and_the_env() {
        let toml = "semantic_cmd = \"from-the-file\"\n";
        let action = with_file(&["x.md"], toml);
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("from-the-file"));
        let action =
            parse_with_file(&["x.md"], one_var(SEMANTIC_CMD_ENV, "from-env"), toml).unwrap();
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("from-env"), "環境変数が勝つ");
        let action = parse_with_file(
            &["x.md", "--semantic-cmd", "from-the-flag"],
            one_var(SEMANTIC_CMD_ENV, "from-env"),
            toml,
        )
        .unwrap();
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("from-the-flag"), "フラグが勝つ");
    }

    #[test]
    fn a_blank_env_still_takes_the_layer_off_over_the_config_file() {
        // `export AKAPEN_SEMANTIC_CMD=` で一時的に外せる道は、設定ファイルに
        // 書いてあっても残る。
        for blank in ["", "   "] {
            let action = parse_with_file(
                &["x.md"],
                move |n| (n == SEMANTIC_CMD_ENV).then(|| blank.to_string()),
                "semantic_cmd = \"from-the-file\"\n",
            )
            .unwrap();
            assert!(cfg(&action).semantic_cmd.is_none(), "{blank:?}");
        }
    }

    #[test]
    fn the_config_file_default_collides_with_a_fixture_and_names_the_file() {
        // 環境変数の既定と同じ扱い。打った覚えのない `--semantic-cmd` を
        // 名指しされる側なので、どのファイルのどのキーかを言う。
        let err = file_refusal(
            &["x.md", "--semantic", "d.json"],
            |_| None,
            "semantic_cmd = \"jev-annotate\"\n",
        );
        assert!(err.contains("mutually exclusive"), "{err}");
        assert!(err.contains("semantic_cmd in /cfg/akapen/config.toml"), "{err}");
        // 環境変数が勝っているときは、環境変数を言う。
        let err = file_refusal(
            &["x.md", "--semantic", "d.json"],
            one_var(SEMANTIC_CMD_ENV, "jev-annotate"),
            "semantic_cmd = \"jev-annotate\"\n",
        );
        assert!(err.contains(SEMANTIC_CMD_ENV), "{err}");
        assert!(!err.contains(FILE_PATH), "{err}");
    }

    #[test]
    fn the_config_file_default_counts_as_a_layer() {
        let toml = "semantic_cmd = \"cat\"\n";
        // 問いのファイル・ルールのファイルの検査は通る。
        assert!(parse_with_file(&["x.md", "--marks-questions", "q.json"], |_| None, toml).is_ok());
        assert!(parse_with_file(&["x.md", "--review-rules", "r.json"], |_| None, toml).is_ok());
        // `--review-json` も設定ファイルだけで立つ。
        let action = with_file(&["x.md", "--review-json"], toml);
        assert!(cfg(&action).review_json);
        assert_eq!(cfg(&action).semantic_cmd.as_deref(), Some("cat"));
        // 空の値は書いていないのと同じなので、層にならない。
        let err = file_refusal(&["x.md", "--review-json"], |_| None, "semantic_cmd = \"\"\n");
        assert!(err.contains("--review-json needs --semantic-cmd"), "{err}");
        assert!(err.contains("config file"), "{err}");
    }

    #[test]
    fn the_config_file_lint_cmd_sits_under_the_flag_and_the_env() {
        use super::LINT_CMD_ENV;
        let toml = "lint_cmd = \"from-the-file\"\n";
        let action = with_file(&["x.md"], toml);
        assert_eq!(cfg(&action).lint_cmd.as_deref(), Some("from-the-file"));
        let action = parse_with_file(&["x.md"], one_var(LINT_CMD_ENV, "from-env"), toml).unwrap();
        assert_eq!(cfg(&action).lint_cmd.as_deref(), Some("from-env"), "環境変数が勝つ");
        let action = with_file(&["x.md", "--lint-cmd", "flag"], toml);
        assert_eq!(cfg(&action).lint_cmd.as_deref(), Some("flag"), "フラグが勝つ");
        // 空の環境変数は、設定ファイルがあっても外す（`--semantic-cmd` と同じ）。
        let action = parse_with_file(&["x.md"], one_var(LINT_CMD_ENV, ""), toml).unwrap();
        assert!(cfg(&action).lint_cmd.is_none());
    }

    #[test]
    fn the_config_file_undercurl_sits_under_the_flag_and_the_env() {
        use crate::undercurl::{UNDERCURL_ENV, UndercurlMode};
        let toml = "undercurl = \"off\"\n";
        assert_eq!(cfg(&with_file(&["x.md"], toml)).undercurl, UndercurlMode::Off);
        let action = parse_with_file(&["x.md"], one_var(UNDERCURL_ENV, "on"), toml).unwrap();
        assert_eq!(cfg(&action).undercurl, UndercurlMode::On, "環境変数が勝つ");
        let action = with_file(&["x.md", "--undercurl", "on"], toml);
        assert_eq!(cfg(&action).undercurl, UndercurlMode::On, "フラグが勝つ");
    }

    #[test]
    fn no_config_file_changes_nothing() {
        // ファイルが無い（`Ok(None)`）のは、今までの `parse_with_env` と同じ。
        let with = Config::parse_with_sources(
            ["x.md"].iter().map(|s| s.to_string()),
            |_| None,
            || Ok(None),
        )
        .unwrap();
        let c = cfg(&with);
        assert_eq!(c.theme, ThemePair::default());
        assert!(c.semantic_cmd.is_none() && c.lint_cmd.is_none());
        assert_eq!(c.undercurl, crate::undercurl::UndercurlMode::Auto);
    }

    #[test]
    fn a_broken_config_file_is_a_startup_error_but_help_still_works() {
        // 読めなかった設定ファイルのエラーはそのまま起動のエラーになる。
        let broken =
            || ConfigFile::parse(Path::new(FILE_PATH), "[theme]\ndrak = \"x\"\n", None).map(Some);
        let args = ["x.md"].iter().map(|s| s.to_string());
        let err = match Config::parse_with_sources(args, |_| None, broken) {
            Ok(_) => panic!("壊れた設定ファイルで起動してしまった"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains(FILE_PATH) && err.contains("drak"), "{err}");
        // 短絡（`--help` など）は設定ファイルを読まない — 直し方を調べる道を塞がない。
        for args in [&["--help"][..], &["x.md", "--version"], &["--semantic-cache-clear"]] {
            let action = Config::parse_with_sources(
                args.iter().map(|s| s.to_string()),
                |_| None,
                || -> anyhow::Result<Option<ConfigFile>> {
                    panic!("{args:?} で設定ファイルを読んだ")
                },
            );
            assert!(!matches!(action, Ok(Action::Run(_))), "{args:?}");
        }
    }
}
