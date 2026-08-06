//! Terminal background auto-detection (OSC 11) for the light/dark UI
//! colors.
//!
//! akapen defaults to "auto": at startup it asks the terminal for its
//! background color (`ESC ] 11 ; ?`), and the answer's perceived
//! lightness picks the light/dark colors (selection/changed-line
//! backgrounds and the view border, resolved in `view.rs`). `--light` /
//! `--dark` override; terminals that don't answer (e.g. Terminal.app)
//! fall back to dark.
//!
//! NOTE: this file is a manual MIRROR of ashiato's `src/theme.rs`
//! (detection half). ashiato additionally carries the UI color constants
//! (`selected_bg` / `border_color`) in its theme.rs because it has no
//! view.rs; akapen's live in `view.rs`. Changes to the detection logic
//! should be applied to both files.

use std::io::{IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::time::Duration;

/// How long to wait for the terminal's OSC 11 answer before assuming
/// dark (unsupported terminals ignore the query; the timeout is the
/// only cost).
const OSC11_TIMEOUT: Duration = Duration::from_millis(150);
/// The query: "report the background color" (OSC 11), ST-terminated.
const OSC11_QUERY: &[u8] = b"\x1b]11;?\x1b\\";

/// Ask the terminal for its background color and decide light/dark.
/// `None` = unknown (no tty, no answer, unparseable) — the caller falls
/// back to dark.
///
/// Call after `enable_raw_mode` (the answer is plain bytes on stdin and
/// needs a non-canonical tty to be readable) and before the event loop
/// consumes input. The query goes to the terminal — stdout, or
/// `/dev/tty` when stdout is piped — and the answer comes back on
/// stdin (already rebound to the pty by `ensure_terminal_stdin`).
///
/// Known limitation: keys typed during the wait (type-ahead) land in the
/// same read and are dropped — they can't be pushed back onto stdin. The
/// parser tolerates them (the answer is located anywhere in the buffer),
/// so detection itself still works.
#[cfg(unix)]
pub fn detect_light() -> Option<bool> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let mut out: Box<dyn Write> = if std::io::stdout().is_terminal() {
        Box::new(std::io::stdout())
    } else {
        Box::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok()?,
        )
    };
    out.write_all(OSC11_QUERY).ok()?;
    out.flush().ok()?;

    let fd = std::io::stdin().as_raw_fd();
    let mut resp = Vec::new();
    let mut buf = [0u8; 64];
    let deadline = std::time::Instant::now() + OSC11_TIMEOUT;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe {
            libc::poll(
                &mut pfd,
                1,
                deadline.saturating_duration_since(now).as_millis() as i32,
            )
        };
        if n <= 0 {
            break; // timeout or error
        }
        // libc::read bypasses std's buffered Stdin so crossterm's event
        // reader never misses bytes we consumed here.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n as usize]);
        if response_complete(&resp) {
            break;
        }
    }
    Some(is_light_rgb(parse_osc11(&resp)?))
}

#[cfg(not(unix))]
pub fn detect_light() -> Option<bool> {
    None
}

/// The OSC 11 answer header. The buffer may hold type-ahead bytes before
/// it, so both the completeness check and the parser locate it anywhere.
const OSC11_HEADER: &[u8] = b"\x1b]11;";

/// OSC 11 answers end with ST (`ESC \`) or BEL (`0x07`) — but only a
/// terminator AFTER the header counts (a stray `^G` typed before the
/// answer arrives must not cut the wait short).
fn response_complete(resp: &[u8]) -> bool {
    let Some(pos) = find(resp, OSC11_HEADER) else {
        return false;
    };
    let rest = &resp[pos + OSC11_HEADER.len()..];
    rest.windows(2).any(|w| w == b"\x1b\\") || rest.contains(&0x07)
}

/// First occurrence of `needle` in `haystack` (byte-level `find`).
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse an OSC 11 answer into an RGB triple. Accepts xterm's
/// `rgb:RRRR/GGGG/BBBB` (and the 1–3 digit channel forms), `rgba:…`
/// (alpha dropped), and `#rrggbb`; `None` for anything else. Bytes
/// before the header (type-ahead) and after the terminator are ignored.
fn parse_osc11(resp: &[u8]) -> Option<(u8, u8, u8)> {
    let pos = find(resp, OSC11_HEADER)? + OSC11_HEADER.len();
    let rest = &resp[pos..];
    let end = rest
        .iter()
        .position(|&b| b == 0x1b || b == 0x07)
        .unwrap_or(rest.len());
    let s = std::str::from_utf8(&rest[..end]).ok()?;
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some((r, g, b));
    }
    let s = s
        .strip_prefix("rgba:")
        .or_else(|| s.strip_prefix("rgb:"))?;
    Some((
        parse_hex_channel(s.split('/').next()?)?,
        parse_hex_channel(s.split('/').nth(1)?)?,
        parse_hex_channel(s.split('/').nth(2)?)?,
    ))
}

/// Parse a 1–4 digit hex channel (XParseColor's `r` / `rr` / `rrr` /
/// `rrrr` forms; wider channels are scaled by dropping the low digits,
/// a 1-digit channel by repeating it).
fn parse_hex_channel(s: &str) -> Option<u8> {
    match s.len() {
        1 => u8::from_str_radix(s, 16).ok().map(|v| v * 0x11),
        2 => u8::from_str_radix(s, 16).ok(),
        3 | 4 => u8::from_str_radix(&s[..2], 16).ok(),
        _ => None,
    }
}

/// Perceived lightness (ITU-R BT.601 luma): backgrounds brighter than
/// mid-gray count as light.
fn is_light_rgb((r, g, b): (u8, u8, u8)) -> bool {
    (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) > 128.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_osc11_accepts_xterm_long_and_short_forms() {
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"),
            Some((0x1e, 0x1e, 0x1e))
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:ff/00/00\x07"),
            Some((0xff, 0x00, 0x00))
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;rgba:ffff/ffff/ffff/ffff\x1b\\"),
            Some((255, 255, 255))
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;#ffffff\x1b\\"),
            Some((255, 255, 255))
        );
        // XParseColor's 1- and 3-digit channel forms.
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:f/0/8\x1b\\"),
            Some((0xff, 0x00, 0x88))
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:fdf/246/227\x1b\\"),
            Some((0xfd, 0x24, 0x22))
        );
    }

    #[test]
    fn parse_osc11_tolerates_type_ahead_and_trailing_bytes() {
        // Keys typed while waiting land before the answer — the header is
        // located anywhere, not just at the start.
        assert_eq!(
            parse_osc11(b"jjq\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some((255, 255, 255))
        );
        // Bytes after the terminator (fast typing in the same read) are
        // ignored, even invalid UTF-8.
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:0000/0000/0000\x07\xff\xfe"),
            Some((0, 0, 0))
        );
    }

    #[test]
    fn parse_osc11_rejects_garbage() {
        assert_eq!(parse_osc11(b""), None);
        assert_eq!(parse_osc11(b"garbage"), None);
        // OSC 10 (foreground) answers are not background answers.
        assert_eq!(parse_osc11(b"\x1b]10;rgb:1e1e/1e1e/1e1e\x1b\\"), None);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:zz/zz/zz\x1b\\"), None);
    }

    #[test]
    fn response_complete_detects_terminators() {
        assert!(response_complete(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"));
        assert!(response_complete(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07"));
        assert!(!response_complete(b"\x1b]11;rgb:1e1e"));
        // A stray BEL typed before the answer must not end the wait early;
        // type-ahead before a complete answer is fine.
        assert!(!response_complete(b"\x07\x1b]11;rgb:1e1e"));
        assert!(response_complete(b"jj\x1b]11;rgb:1e1e/1e1e/1e1e\x07"));
    }

    #[test]
    fn lightness_threshold() {
        assert!(is_light_rgb((255, 255, 255)));
        assert!(is_light_rgb((253, 246, 227))); // Solarized light bg
        assert!(!is_light_rgb((0, 0, 0)));
        assert!(!is_light_rgb((40, 42, 54))); // dark editor bg
    }
}
