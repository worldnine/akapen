//! macOS input-source switching for the comment composer.
//!
//! While the input mode is open we force the Japanese input source so the
//! comment text can be typed without reaching for the IME toggle; when the
//! composer closes we switch back to ASCII so j/k navigation is never
//! swallowed by the IME afterwards. Backed by a tiny Swift helper (Carbon
//! TIS API — no accessibility permission needed), compiled once into
//! `~/.cache/akapen/ime-<hash>`, where `<hash>` derives from the Swift
//! source so a helper from an older release is never silently reused.
//! On non-macOS (or when swiftc is missing) every call degrades to a
//! no-op, so the TUI stays usable.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

const IME_SWIFT: &str = include_str!("../scripts/ime.swift");

/// FNV-1a 64-bit hash. Hand-rolled so the build needs no extra
/// dependency; not cryptographic, which is fine — the hash only picks a
/// cache file name.
fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `ime-<hash8>` file name for a helper built from `src`. Only the low 32
/// bits of the hash (8 hex digits) are used: a collision between two
/// sources merely rebuilds the helper under the same name, harmless for a
/// local cache — and with the source checked in, the chance of a real
/// mismatch is ≈2^-32, negligible here.
fn bin_name_for(src: &str) -> String {
    format!("ime-{:08x}", (fnv1a(src.as_bytes()) & 0xffff_ffff) as u32)
}

/// File name for the helper built from the embedded [`IME_SWIFT`].
fn helper_bin_name() -> String {
    bin_name_for(IME_SWIFT)
}

fn ime_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("akapen")
}

/// The compiled helper's path, `Some` only once the build has produced it.
///
/// The name embeds the source hash, so a stale binary can never outlive
/// its contract: when a future release changes `scripts/ime.swift`, the
/// hash changes and the next launch builds a new helper under a fresh
/// name instead of reusing the old one with a mismatched CLI. Old
/// binaries stay behind in `~/.cache` (throwaway by nature; the name
/// never collides, so there is nothing to clean up).
fn bin_path() -> Option<PathBuf> {
    let bin = ime_dir().join(helper_bin_name());
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
    let bin = dir.join(helper_bin_name());
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

    #[test]
    fn source_hash_is_deterministic() {
        // Same input → same output, different input → different output:
        // the helper's file name derives from the hash, so determinism is
        // what keeps `bin_path` stable across launches. The known vectors
        // also pin the FNV-1a implementation itself.
        assert_eq!(fnv1a(b"get\nabc\njp\nset"), fnv1a(b"get\nabc\njp\nset"));
        assert_ne!(fnv1a(b"get\nabc\njp\nset"), fnv1a(b"get\nabc\njp\nset <id>"));
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn helper_bin_name_embeds_the_source_hash() {
        // The name must be derived from the embedded source (never fixed),
        // so editing scripts/ime.swift changes the binary name and the
        // next launch rebuilds instead of reusing a stale helper.
        // `bin_name_for` keeps this testable without depending on HOME,
        // which `ime_dir` needs.
        assert_eq!(helper_bin_name(), bin_name_for(IME_SWIFT));
        assert!(helper_bin_name().starts_with("ime-"));
        assert_eq!(helper_bin_name().len(), "ime-".len() + 8);
        assert!(helper_bin_name().chars().skip(4).all(|c| c.is_ascii_hexdigit()));
        // A changed source must map to a different name.
        assert_ne!(bin_name_for("abc"), bin_name_for("abd"));
    }
}
