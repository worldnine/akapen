//! macOS input-source switching for the comment composer.
//!
//! While the input mode is open we force the Japanese input source so the
//! comment text can be typed without reaching for the IME toggle; when the
//! composer closes we switch back to ASCII so j/k navigation is never
//! swallowed by the IME afterwards. Backed by a tiny Swift helper (Carbon
//! TIS API — no accessibility permission needed), compiled once into
//! `~/.cache/akapen/ime`. On non-macOS (or when swiftc is missing) every
//! call degrades to a no-op, so the TUI stays usable.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

const IME_SWIFT: &str = include_str!("../scripts/ime.swift");

fn ime_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("akapen")
}

/// The compiled helper's path, `Some` only once the build has produced it.
fn bin_path() -> Option<PathBuf> {
    let bin = ime_dir().join("ime");
    bin.is_file().then_some(bin)
}

/// One-shot guard so at most one build runs per process.
static BUILD_STARTED: AtomicBool = AtomicBool::new(false);

/// Kick off a background build of the IME helper. No-op when the helper
/// already exists or a build is already running. Called once at startup so
/// the first composer close never blocks on `swiftc` (≈3.5 s) — the build
/// runs concurrently with the session, and the helper appears the moment
/// it is done.
pub fn start_background_build() {
    if bin_path().is_some() || BUILD_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let _ = ensure_binary();
    });
}

/// Build (once) and return the compiled IME helper. `None` when swiftc is
/// missing or the build fails — callers just no-op in that case. Runs on
/// the background thread from [`start_background_build`]; nothing else
/// calls it, so it never blocks the TUI.
fn ensure_binary() -> Option<PathBuf> {
    let dir = ime_dir();
    let bin = dir.join("ime");
    if bin.exists() {
        return Some(bin);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("ime.swift");
    std::fs::write(&src, IME_SWIFT).ok()?;
    let status = Command::new("swiftc")
        .args(["-O", src.to_str()?, "-o", bin.to_str()?])
        .status()
        .ok()?;
    if status.success() && bin.exists() {
        Some(bin)
    } else {
        None
    }
}

/// Run the helper with `args`; `Some(stdout)` on success. While the
/// background build is still running (or when swiftc is unavailable) the
/// helper does not exist yet, so this no-ops with `None` — the IME simply
/// stays as the user left it for the few seconds the build takes.
fn ime(args: &[&str]) -> Option<String> {
    let bin = bin_path()?;
    let out = Command::new(&bin).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Switch the input source to ASCII (the ABC layout).
fn set_ascii() {
    let _ = ime(&["abc"]);
}

/// Switch the input source to the first enabled Japanese source.
fn set_japanese() {
    let _ = ime(&["jp"]);
}

/// Input-source control policy, from `--ime <mode>`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImeMode {
    /// No input-source control at all.
    Off,
    /// Switch to ASCII when the composer closes (default). Neutral for
    /// non-Japanese users: typing state is untouched while the composer is
    /// open, and j/k navigation is protected afterwards.
    Ascii,
    /// Also switch to Japanese when the composer opens (JP commenters).
    Jp,
}

impl ImeMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "off" => ImeMode::Off,
            "jp" => ImeMode::Jp,
            _ => ImeMode::Ascii, // "ascii" and anything unknown: neutral default
        }
    }
}

/// While the comment composer is open: optionally force Japanese for typing,
/// and always switch back to ASCII on drop (unless `Off`) so j/k navigation
/// is never captured by the IME afterwards.
pub struct ImeGuard {
    mode: ImeMode,
}

impl ImeGuard {
    pub fn enter(mode: ImeMode) -> Self {
        if mode == ImeMode::Jp {
            set_japanese();
        }
        ImeGuard { mode }
    }
}

impl Drop for ImeGuard {
    fn drop(&mut self) {
        if self.mode != ImeMode::Off {
            set_ascii();
        }
    }
}

/// Session-level input-source control: command mode always runs in ASCII
/// (so j/k and friends are never swallowed by the IME), and the input
/// source the user had before launching is restored on drop.
///
/// `--ime off` disables this entirely (no subprocess calls). Otherwise
/// [`SessionIme::force_ascii`] saves the current source and switches to
/// ASCII; it returns false until the helper exists (it is still being
/// compiled in the background on first run), so the event loop retries it
/// each tick until it succeeds.
pub struct SessionIme {
    mode: ImeMode,
    saved: Option<String>,
}

impl SessionIme {
    pub fn new(mode: ImeMode) -> Self {
        Self { mode, saved: None }
    }

    /// Save the current input source and switch to ASCII. Returns true when
    /// the session is protected (or `--ime off`). Call repeatedly until it
    /// returns true; on first run the helper may still be building.
    pub fn force_ascii(&mut self) -> bool {
        if self.mode == ImeMode::Off {
            return true;
        }
        if self.saved.is_none() {
            self.saved = ime(&["get"]);
        }
        ime(&["abc"]).is_some()
    }
}

impl Drop for SessionIme {
    fn drop(&mut self) {
        if self.mode == ImeMode::Off {
            return;
        }
        if let Some(id) = &self.saved {
            let _ = ime(&["set", id]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_source_supports_the_commands() {
        // include_str! fails at compile time if the script is missing; this
        // guards the CLI contract the Rust side relies on.
        assert!(IME_SWIFT.contains("case \"abc\""));
        assert!(IME_SWIFT.contains("case \"jp\""));
        assert!(IME_SWIFT.contains("TISSelectInputSource"));
    }
}
