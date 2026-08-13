//! Command-line configuration.
//!
//! `akapen <file...> [--send-cmd <cmd>] [--theme <syntect-theme>]
//!              [--ime <off|ascii|jp>] [--light|--dark] [--esc-quit <auto|always|never>]`
//! Positional arguments are the files to open (one or more). Unknown flags
//! are ignored (reviewr-style). `--help`/`--version` short-circuit before parsing.

use std::path::PathBuf;

use anyhow::{Result, bail};

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
    Run(Config),
    /// Print usage and exit 0.
    Help,
    /// Print the version and exit 0.
    Version,
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
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Action::Help),
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
                "--send-cmd" => send_cmd = it.next(),
                "--send-agent" => send_agent = true,
                "--reply" => reply = true,
                "--theme" => theme = it.next(),
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
                "usage: akapen <file...> [--send-cmd <cmd> | --send-agent] [--reply] [--theme <name>] [--ime <off|ascii|jp>] [--light|--dark]"
            );
        }
        if send_cmd.is_some() && send_agent {
            bail!("--send-cmd and --send-agent are mutually exclusive");
        }
        Ok(Action::Run(Config {
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
        }))
    }

    /// Parse from the real process arguments.
    pub fn from_env() -> Result<Action> {
        Self::parse(std::env::args().skip(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Config, EscQuit};

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
}
