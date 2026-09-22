//! Command-line configuration.
//!
//! `akapen <file...> [--send-cmd <cmd>] [--theme <syntect-theme>]
//!              [--ime <off|ascii|jp>] [--light|--dark] [--esc-quit <auto|always|never>]
//!              [--semantic <fixture.json> | --semantic-cmd <cmd>]
//!              [--mark-blend <f>] [--dim-blend <f>]`
//! Positional arguments are the files to open (one or more). Unknown flags
//! are ignored (reviewr-style). `--help`/`--version` short-circuit before parsing.

use std::path::PathBuf;

use anyhow::{Result, bail};

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

/// Which projection the Semantic Reading Layer uses
/// (`--semantic-mode <marks|budget>`).
///
/// **両方とも残る。** DIM 版（`Budget`）は測定が続いているので既定のままで、
/// marks は切り替えて使う。設計書 `docs/design/marks-only-and-review-mode.md`
/// の「いまの実装はそのまま置く。Jev 的なものが安くなったときに戻せるように」
/// がこの enum である。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SemanticMode {
    /// Reading Budget と Tier で全 Unit に判決を下す（MARKED / NORMAL / DIM）。
    /// `docs/design/semantic-reading-layer.md` の正典。
    #[default]
    Budget,
    /// 問いに答えている箇所だけを光らせる。DIM は出さない。
    /// `docs/design/marks-only-and-review-mode.md` 0 節。
    Marks,
}

impl SemanticMode {
    /// **未知の値は既定に落とさずエラーにする。** `--esc-quit` と違って、
    /// ここで黙って `budget` に落ちると「marks のつもりで DIM を見ている」
    /// ことになり、画面を見ても気づけない（DIM 版も光りはする）。
    pub fn parse(s: &str) -> Result<SemanticMode> {
        match s {
            "marks" => Ok(SemanticMode::Marks),
            "budget" => Ok(SemanticMode::Budget),
            other => bail!("--semantic-mode {other}: expected `marks` or `budget`"),
        }
    }

    /// marks モードか。
    pub fn is_marks(self) -> bool {
        matches!(self, SemanticMode::Marks)
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
    /// `--theme <name>`: syntect theme name for source-mode highlighting,
    /// or a path to a `.tmTheme` file (e.g. tokyo-night.tmTheme).
    pub theme: Option<String>,
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
    /// The command is NOT run at startup — it runs once a document is
    /// on screen, on its own thread, because a process launch plus a
    /// network round trip does not return within a frame (see
    /// [`crate::app::App::reanalyze_semantics`]).
    pub semantic_cmd: Option<String>,
    /// `--semantic-mode <marks|budget>`: which projection the Semantic
    /// Reading Layer uses. Default [`SemanticMode::Budget`] — the DIM
    /// version, whose measurements are still running.
    ///
    /// **文書の種類は推定しない。** 推定は「読み手のモデルが無い」という
    /// 穴を「書き手のモデル」に置き換えるだけで、同じ穴に落ちる
    /// （`docs/design/marks-only-and-review-mode.md` 2 節）。だからフラグ。
    pub semantic_mode: SemanticMode,
    /// `--marks-questions <path>`: read the marks-mode questions from
    /// this file instead of the built-in set (and instead of
    /// `$XDG_CONFIG_HOME/akapen/marks-questions.json`).
    ///
    /// Measurement and tests point this at a file of their own so they
    /// never read the real user's questions — the same care
    /// [`crate::semantic_cache`] takes with `~/.cache`.
    pub marks_questions: Option<PathBuf>,
    /// `--mark-blend <0.0..1.0>` / `--dim-blend <0.0..1.0>`: how strong
    /// the two range-decoration kinds are. `mark` lifts the mark
    /// background off the page toward the text color; `dim` moves a
    /// dimmed foreground toward the page.
    ///
    /// The defaults live on [`DecorationBlend`], not here: a
    /// configuration file, if akapen ever grows one, belongs BETWEEN the
    /// default and this field (`CLI > config file > default`), and that
    /// only works if the default is a value the layers overwrite rather
    /// than an `Option` each layer re-invents.
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

impl Config {
    /// Parse the process arguments (after argv[0]).
    ///
    /// All non-flag tokens are files; `--send-cmd`/`--theme`/`--ime` take a
    /// value; `-h`/`--help` and `-V`/`--version` short-circuit. At least
    /// one file is required.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Action> {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut send_cmd: Option<String> = None;
        let mut send_agent = false;
        let mut reply = false;
        let mut theme: Option<String> = None;
        let mut ime = ImeMode::Ascii;
        let mut light: Option<bool> = None;
        let mut callback: Option<String> = None;
        let mut esc_quit = EscQuit::Auto;
        let mut fx = true;
        let mut cursor_anchor = true;
        let mut decorations: Vec<Decoration> = Vec::new();
        let mut semantic: Option<PathBuf> = None;
        let mut semantic_cmd: Option<String> = None;
        let mut semantic_mode = SemanticMode::Budget;
        let mut marks_questions: Option<PathBuf> = None;
        let mut decoration_blend = DecorationBlend::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Action::Help),
                "--semantic-cache-clear" => return Ok(Action::ClearSemanticCache),
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
                "--decorations" => {
                    if let Some(v) = it.next() {
                        decorations = parse_decorations(&v)?;
                    }
                }
                "--semantic" => semantic = it.next().map(PathBuf::from),
                "--semantic-cmd" => semantic_cmd = it.next(),
                "--semantic-mode" => {
                    if let Some(v) = it.next() {
                        semantic_mode = SemanticMode::parse(&v)?;
                    }
                }
                "--marks-questions" => marks_questions = it.next().map(PathBuf::from),
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
                "usage: akapen <file...> [--send-cmd <cmd> | --send-agent] [--reply] [--theme <name>] [--ime <off|ascii|jp>] [--light|--dark] [--semantic <fixture.json> | --semantic-cmd <cmd>] [--semantic-mode <marks|budget>]"
            );
        }
        if send_cmd.is_some() && send_agent {
            bail!("--send-cmd and --send-agent are mutually exclusive");
        }
        // Two annotations for one document is not a configuration: the
        // fixture and the command would each claim the same Atom list,
        // and whichever lost would still be what the user asked for.
        if semantic.is_some() && semantic_cmd.is_some() {
            bail!("--semantic and --semantic-cmd are mutually exclusive");
        }
        // A mode without a layer is a flag that does nothing. Say so
        // rather than starting a session where the keys refuse.
        if semantic_mode != SemanticMode::Budget && semantic.is_none() && semantic_cmd.is_none() {
            bail!("--semantic-mode needs --semantic or --semantic-cmd");
        }
        if marks_questions.is_some() && semantic_mode != SemanticMode::Marks {
            bail!("--marks-questions needs --semantic-mode marks");
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
            semantic_mode,
            marks_questions,
            decoration_blend,
            decorations,
        })))
    }

    /// Parse from the real process arguments.
    pub fn from_env() -> Result<Action> {
        Self::parse(std::env::args().skip(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Config, EscQuit};
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
        assert_eq!(c.theme.as_deref(), Some("base16-ocean.dark"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(parse(&["--help"]), Action::Help));
        assert!(matches!(parse(&["-h", "x.md"]), Action::Help));
        assert!(matches!(parse(&["--version"]), Action::Version));
        assert!(matches!(parse(&["-V"]), Action::Version));
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
        let action = parse(&["x.md", "--semantic", "examples/semantic/demo.json"]);
        assert_eq!(
            cfg(&action).semantic.as_deref(),
            Some(std::path::Path::new("examples/semantic/demo.json"))
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
            vec!["x.md", "--semantic", "demo.json", "--semantic-cmd", "annotate-doc"],
            vec!["x.md", "--semantic-cmd", "annotate-doc", "--semantic", "demo.json"],
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
}
