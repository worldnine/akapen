//! Formatting comments for export and copying them to the clipboard.
//!
//! A comment becomes a block of `location`, the anchored snippet, then the
//! comment text — the same structure as herdr-reviewr's `export.rs`
//! (MIT, Dmitry Persiyanov), minus the diff side markers: akapen
//! anchors to plain source lines. Reply mode (`--reply`, scripts/akp)
//! drops the location and the line-number prefixes, quoting the snippet
//! GitHub-style (`> `) instead — see `format_comment_reply`.
//!
//! Export is non-destructive: comments stay in the list after a copy so the
//! user can re-output or send them again (delete is explicit, `d`).

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::process::{ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::comment::Comment;

/// One comment as its export block: optional revision, location, numbered
/// snippet, then text.
pub fn format_comment(comment: &Comment) -> String {
    format_comment_with(comment, true, None)
}

/// Reply-mode export: the blockquoted snippet then the comment, with a
/// blank line between them. Used by `--reply` (scripts/akp): the commented
/// document is the agent's own message, so a temp-file path and line
/// numbers (which reference nothing without a file) would only add noise.
///
/// The blank line is load-bearing: CommonMark lazy continuation pulls a
/// non-blank line after a `> ` line into the blockquote, so without it
/// the comment renders as part of the quote (pi and GitHub both do this).
///
/// `number` prefixes the QUOTE (`N. > ...`) when a batch of comments is
/// sent — the numbering tells the agent the message is a list of distinct
/// points to address in order, and the comment is indented so the whole
/// pair stays inside the numbered item. The number sits before the `> `
/// marker so quoted content (which can itself contain `1. ` list markers)
/// can never collide with it.
pub fn format_comment_reply(comment: &Comment, number: Option<usize>) -> String {
    format_comment_with(comment, false, number)
}

fn format_comment_with(
    comment: &Comment,
    include_location: bool,
    number: Option<usize>,
) -> String {
    if include_location {
        let revision = comment
            .revision
            .as_ref()
            .map(|revision| format!("Revision: {revision}\n"))
            .unwrap_or_default();
        format!(
            "{revision}{}\n{}\n{}",
            export_location(comment),
            numbered_snippet(comment),
            normalize_text(&comment.text)
        )
    } else {
        let quote = quoted_snippet(comment);
        let quote = match number {
            Some(n) => {
                // `N. > first` then 3-space-indented continuation quote
                // lines, so the whole quote stays one numbered item (the
                // list marker is at column 1-3, content at column 4).
                let mut lines = quote.lines();
                let mut out = format!("{n}. {}", lines.next().unwrap_or(""));
                for line in lines {
                    out.push_str("\n   ");
                    out.push_str(line);
                }
                out
            }
            None => quote,
        };
        let text = normalize_text(&comment.text);
        // In a batch, indent the comment 4 spaces so it lives inside the
        // numbered item next to its quote (unambiguous pairing, and the
        // comment's own text cannot collide with the item number).
        let text = match number {
            Some(_) => text
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
            None => text,
        };
        let revision = comment
            .revision
            .as_ref()
            .map(|revision| format!("Revision: {revision}\n\n"))
            .unwrap_or_default();
        format!("{revision}{quote}\n\n{text}")
    }
}

/// Export a resolvable path when the commented file still exists. UI
/// labels retain the concise command-line spelling, while the agent-facing
/// location does not depend on inheriting akapen's working directory.
fn export_location(comment: &Comment) -> String {
    let Ok(path) = std::fs::canonicalize(&comment.file_path) else {
        return comment.location();
    };
    if comment.start == comment.end {
        format!("{}:{}", path.display(), comment.start)
    } else {
        format!("{}:{}-{}", path.display(), comment.start, comment.end)
    }
}

/// The snippet's lines, pinned to the location's `start-end` range (short
/// `lines` are padded, extra parts dropped) — the range is the single
/// source of truth for how many lines a snippet shows.
fn snippet_parts(comment: &Comment) -> Vec<&str> {
    let parts: Vec<&str> = comment.lines.split('\n').collect();
    let count = (comment.end - comment.start + 1) as usize;
    (0..count)
        .map(|i| parts.get(i).copied().unwrap_or(""))
        .collect()
}

/// The anchored snippet with a `n: ` line-number prefix per line (design
/// 2026-08-01): each line shows its real file line number, so a blank line
/// inside the selection renders as `13: ` and can never be confused with the
/// blank-line block separator. The snippet's line count comes from the
/// location's `start-end` alone (short `lines` are padded, extra parts
/// dropped) — adapters like scripts/akapen2hunk rely on that to split
/// the snippet from the comment text.
fn numbered_snippet(comment: &Comment) -> String {
    snippet_parts(comment)
        .iter()
        .enumerate()
        .map(|(i, text)| format!("{}: {text}", comment.start + i as u32))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reply-mode snippet: GitHub-style blockquote, no line numbers — the
/// message is right there in the conversation, so `n: ` prefixes (which
/// reference nothing without a file) would be noise. Blank lines in the
/// selection render as `> ` so they still read as quoted.
fn quoted_snippet(comment: &Comment) -> String {
    snippet_parts(comment)
        .iter()
        .map(|text| format!("> {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Comment text for export: drop `\r`, trim trailing space per line, and
/// drop blank lines so a multi-line comment can never introduce the
/// blank-line block separator.
fn normalize_text(text: &str) -> String {
    text.replace('\r', "")
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Many comments, sorted by file then start line, one blank line between
/// blocks (reviewr's `format_all`).
pub fn format_all(comments: &[Comment]) -> String {
    format_all_with(comments, true)
}

/// Reply-mode export: `format_all` without the location lines.
pub fn format_all_reply(comments: &[Comment]) -> String {
    format_all_with(comments, false)
}

fn format_all_with(comments: &[Comment], include_location: bool) -> String {
    let mut sorted: Vec<&Comment> = comments.iter().collect();
    sorted.sort_by(|a, b| a.file_path.cmp(&b.file_path).then(a.start.cmp(&b.start)));
    // Reply-mode batches are numbered (`1. `, `2. `...) so the receiving
    // agent reads the message as a list of distinct points; a single
    // comment stays unnumbered (plain chat).
    let numbered = !include_location && sorted.len() > 1;
    sorted
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if include_location {
                format_comment(c)
            } else {
                format_comment_reply(c, numbered.then_some(i + 1))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A clipboard tool and the args that make it read stdin into the system
/// clipboard, tried in order — the first one on `PATH` wins. macOS ships
/// `pbcopy`; Linux needs `wl-copy` (Wayland) or `xclip`/`xsel` (X11).
const CLIPBOARD_TOOLS: &[(&str, &[&str])] = &[
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// Whether `name` resolves to an executable on `PATH` (dependency-free
/// which). The file must be executable, not merely present: a
/// non-executable file in PATH would pass the existence check and fail
/// at spawn with a confusing error.
fn which(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| is_executable(&dir.join(name)))
    })
}

/// Whether `path` is an executable file (Unix: any execute bit set).
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Copy `text` into the system clipboard. Errors mention installing one of
/// the Linux clipboard tools, since macOS ships pbcopy.
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let (cmd, args) = select_tool(CLIPBOARD_TOOLS, which)
        .context("no clipboard tool found (install wl-clipboard, xclip, or xsel)")?;
    copy_via(cmd, args, text)
}

/// How long a clipboard/send child may run before it is killed. A tool
/// that never reads its stdin (or waits forever) must not freeze the
/// TUI: the event loop is single-threaded, so a blocked `wait` would
/// leave every key dead, with no way to interrupt from inside the app.
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

/// When [`run_child`] gives up on a child.
///
/// Two policies, because the callers want different things from the same
/// four hazards. A clipboard tool or a `--send-cmd` delivery either works
/// in a moment or is wedged, so **wall time is the right question** for
/// them. `--semantic-cmd` is not like that: its wall time is set by how
/// big the document is, which akapen cannot know before it spawns the
/// child, and the answer is an expensive one to throw away.
///
/// # なぜ `--semantic-cmd` だけ別扱いなのか（2026-09-22 の実測）
///
/// 60 秒の固定値は**もう足りていない**。アダプタのプロセス壁時計は
/// `examples/semantic/measurements/speed-and-limits.md` の第 2 版で
///
/// ```text
/// 1,664 B     6.7〜7.0 秒（12 リクエスト）
/// 22,685 B   28.9〜29.4 秒（27〜28 リクエスト）
/// 35,021 B   46.9〜47.4 秒（40〜41 リクエスト）
/// ```
///
/// で、**同じ文書がその日のうちに 54,435 B へ育って推定 74 秒**（63
/// リクエスト）になっている。リクエスト数は文書の構造で決まり、実測でも
/// Atom あたり 0.095〜0.44 と 4.6 倍ぶれる — **spawn 前に見積もれる量では
/// ない。**
///
/// だから量を見積もるのをやめて、**進捗そのものを見る**。タイムアウトに
/// 当たると読み手は約 8 円払って何も得ない（子が殺されるので stdout は空、
/// キャッシュにも入らない）ので、誤って殺す側の害が大きい。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Deadline {
    /// Kill the child once it has run this long, whatever it is doing.
    /// The clipboard and `--send-cmd` paths; the historical behaviour.
    Absolute(Duration),
    /// **Kill the child only when it goes quiet.** `idle` is how long a
    /// silence may last before it counts as wedged; `backstop` stops it
    /// regardless, so a child that chatters forever still terminates.
    ///
    /// Progress means bytes on stdout OR stderr ([`Capture::drain`]).
    /// The child has to say something for this to help — an adapter that
    /// prints nothing until it exits is indistinguishable from a hung
    /// one, and gets `idle` as its whole budget.
    WhileProgressing {
        /// The longest silence tolerated. **Must exceed the child's own
        /// per-request timeout**, or akapen kills it during a request
        /// the child would itself have given up on and reported
        /// (`jev-annotate.py`'s `DEFAULT_TIMEOUT` is 20 s; that error
        /// message is worth far more than a kill).
        idle: Duration,
        /// The absolute ceiling, as [`Deadline::Absolute`].
        backstop: Duration,
    },
}

/// Pipe `text` into a child's stdin and wait for its exit — but never
/// longer than `timeout`. The write stays on the caller's thread and is
/// non-blocking: stdin is `O_NONBLOCK` and every attempt is gated by a
/// 10ms `poll`, so a child that never reads (filling the pipe buffer)
/// cannot freeze the TUI, whose event loop is this same thread.
/// `try_wait` polling enforces the deadline; past it the child is
/// killed and an error is returned, so the existing caller paths (the
/// toast in `export_all`) show it unchanged. The child's exit ends the
/// write: stdin is dropped without waiting for the pipe to drain,
/// since a grandchild inheriting the read end could hold it open
/// forever.
///
/// The child's stdout and stderr are captured too: inherited, they
/// would be written straight onto the TUI's alternate screen (a
/// `herdr agent prompt` JSON reply, a chatty wrapper script) and sit
/// there as garbage until the next full redraw. Both pipes are drained
/// non-blockingly on every loop turn — so a child that reads its stdin
/// and then prints more than a pipe buffer cannot deadlock against us
/// — and never read to EOF, for the same grandchild reason as stdin.
/// On success the output is discarded; on a non-zero exit its tail
/// (stderr, else stdout) rides along in the error so the toast can say
/// *why*, mirroring `send_to_agent`.
fn pipe_and_wait(
    label: &str,
    cmd: &str,
    args: &[&str],
    text: &str,
    timeout: Duration,
) -> Result<()> {
    run_child(
        label,
        cmd,
        args,
        text,
        Deadline::Absolute(timeout),
        CAPTURE_LIMIT,
    )
    .map(|_| ())
}

/// What a finished child left behind on stdout.
pub(crate) struct ChildOutput {
    /// The bytes the child wrote to stdout, capped at the caller's limit.
    pub(crate) stdout: Vec<u8>,
    /// Whether the cap dropped anything (the front of the stream). A
    /// caller that PARSES the output must refuse a truncated capture —
    /// a clipped JSON document would surface as a syntax error and be
    /// read as "the command is broken" rather than "it said too much".
    pub(crate) truncated: bool,
}

/// The body of [`pipe_and_wait`], with the stdout capture handed back to
/// the caller.
///
/// `stdout_limit` bounds what is kept: the toast paths want only a tail
/// ([`CAPTURE_LIMIT`]), while `--semantic-cmd` wants the whole JSON
/// answer and says so with a much larger limit.
///
/// `deadline` picks *when* to give up ([`Deadline`]). Both policies kill
/// and reap the child the same way; they differ only in the clock they
/// read, and both say `timed out` so the toast reads the same.
fn run_child(
    label: &str,
    cmd: &str,
    args: &[&str],
    text: &str,
    deadline: Deadline,
    stdout_limit: usize,
) -> Result<ChildOutput> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {label}"))?;
    let stdin = child
        .stdin
        .take()
        .with_context(|| format!("{label} stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("{label} stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("{label} stderr unavailable"))?;
    // Every pipe fd must be non-blocking: a blocking write on a full
    // stdin pipe — or a blocking read on an empty stdout — would freeze
    // the TUI's only thread.
    set_nonblocking(stdin.as_raw_fd());
    set_nonblocking(stdout.as_raw_fd());
    set_nonblocking(stderr.as_raw_fd());
    let mut stdin = Some(stdin);
    let mut out = Capture::new(stdout, stdout_limit);
    let mut err = Capture::new(stderr, CAPTURE_LIMIT);
    let mut bytes = text.as_bytes();
    let started = Instant::now();
    // The absolute wall — a plain deadline, or the backstop under
    // `WhileProgressing`. Reached, it kills either way.
    let (wall, idle_limit) = match deadline {
        Deadline::Absolute(timeout) => (started + timeout, None),
        Deadline::WhileProgressing { idle, backstop } => (started + backstop, Some(idle)),
    };
    // The last moment the child said anything. Spawning counts as the
    // first sign of life, so a child that prints nothing at all still
    // gets a full `idle` window to produce its answer.
    let mut last_output = started;
    loop {
        match child
            .try_wait()
            .with_context(|| format!("waiting for {label}"))?
        {
            Some(status) => {
                // The child is gone: abandon the rest of the write and
                // close our write end without waiting for the pipe to
                // drain — a grandchild inheriting the read end could
                // hold it open indefinitely. There is no writer thread
                // to leak; dropping the handle is the whole cleanup.
                drop(stdin.take());
                // Pick up whatever the child left in the pipes before
                // exiting (without waiting for EOF — see above).
                out.drain();
                err.drain();
                if !status.success() {
                    let tail = err.tail().or_else(|| out.tail());
                    return Err(match tail {
                        // **stderr の末尾を先に置く。** ここは 3 段の接頭辞の
                        // 2 段目で、`{label} exited non-zero:` を先に置くと
                        // 子プロセスが言いたかった一文がステータス行の幅から
                        // 押し出される。切られるなら後ろの括弧側でよい。
                        Some(tail) => anyhow!("{tail} ({label} exited non-zero)"),
                        None => anyhow!("{label} exited non-zero"),
                    });
                }
                return Ok(ChildOutput {
                    truncated: out.truncated,
                    stdout: out.buf,
                });
            }
            // The child outlived its budget (e.g. a send command that
            // never reads stdin) — kill it instead of blocking the UI
            // forever, then reap it so it cannot linger. Under
            // `WhileProgressing` this arm is the backstop: a child that
            // keeps talking still stops here.
            None if Instant::now() >= wall => {
                drop(stdin.take());
                let _ = child.kill();
                let _ = child.wait();
                bail!(match idle_limit {
                    Some(_) => anyhow!(
                        "{label} timed out after {}s (backstop)",
                        started.elapsed().as_secs_f64().round()
                    ),
                    // **The historical message, unchanged.** The
                    // `--send-cmd` and clipboard toasts read this.
                    None => anyhow!(
                        "{label} timed out after {}s",
                        (wall - started).as_secs_f64()
                    ),
                });
            }
            // Gone quiet for too long. Distinct from the backstop in
            // the message: "with no output" is the part that tells the
            // reader their command is wedged rather than slow.
            None
                if idle_limit
                    .is_some_and(|idle| last_output.elapsed() >= idle) =>
            {
                drop(stdin.take());
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{label} timed out after {}s with no output",
                    last_output.elapsed().as_secs_f64().round()
                );
            }
            None => {}
        }
        // Keep the output pipes from filling up: a child blocked on a
        // full stdout would never exit, and we would never see it.
        // **The byte counts are also the progress signal** — see
        // `Deadline::WhileProgressing`.
        if out.drain() + err.drain() > 0 {
            last_output = Instant::now();
        }
        if bytes.is_empty() {
            // Everything is written: close our write end so the child
            // sees EOF and can exit, then just keep reaping it.
            drop(stdin.take());
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        match stdin.as_mut().map(|stdin| write_pipe(stdin, bytes)) {
            Some(WriteOutcome::Wrote(n)) => bytes = &bytes[n..],
            // The pipe's reader is gone: stop writing and let try_wait
            // surface the child's exit (or the deadline kill it).
            Some(WriteOutcome::Closed) => {
                drop(stdin.take());
                thread::sleep(Duration::from_millis(10));
            }
            // Not writable yet, or stdin is already closed: the poll's
            // own pause paces the loop until the next try_wait check.
            Some(WriteOutcome::Retry) | None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Put a pipe fd into `O_NONBLOCK` mode.
fn set_nonblocking(fd: RawFd) {
    // SAFETY: `fd` is a valid open pipe fd owned by the caller;
    // F_SETFL/O_NONBLOCK is the standard flag write.
    unsafe {
        libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
    }
}

/// How much of a child's stdout/stderr to keep — only the tail matters
/// (it becomes one toast line), so anything beyond this is dropped
/// from the front. A herdr JSON reply can run to many KiB.
const CAPTURE_LIMIT: usize = 64 * 1024;

/// The longest tail the toast gets; a full-width row is far shorter
/// than a JSON dump.
const TAIL_LIMIT: usize = 160;

/// A child's stdout or stderr, read non-blockingly as it arrives and
/// bounded to the last `CAPTURE_LIMIT` bytes.
struct Capture<R: Read> {
    pipe: Option<R>,
    buf: Vec<u8>,
    /// How much to keep; anything beyond is dropped from the front.
    limit: usize,
    /// Whether the limit ever dropped anything.
    truncated: bool,
}

impl<R: Read> Capture<R> {
    fn new(pipe: R, limit: usize) -> Self {
        Self {
            pipe: Some(pipe),
            buf: Vec::new(),
            limit,
            truncated: false,
        }
    }

    /// Read everything currently buffered in the pipe and stop at the
    /// first would-block — never wait for EOF, which a grandchild
    /// holding the write end could postpone forever. EOF or a hard
    /// error retires the pipe.
    ///
    /// **Returns how many bytes arrived this call.** That count is what
    /// [`Deadline::WhileProgressing`] means by progress: bytes on either
    /// pipe are the only evidence, from outside, that the child is still
    /// working. The count is the bytes READ, not the bytes kept — a
    /// capture sitting at its limit still reports progress, or a chatty
    /// child would be killed for being too chatty.
    fn drain(&mut self) -> usize {
        let Some(pipe) = self.pipe.as_mut() else {
            return 0;
        };
        let mut chunk = [0u8; 4096];
        let mut read = 0;
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => {
                    self.pipe = None;
                    return read;
                }
                Ok(n) => {
                    read += n;
                    self.buf.extend_from_slice(&chunk[..n]);
                    if self.buf.len() > self.limit {
                        let excess = self.buf.len() - self.limit;
                        self.buf.drain(..excess);
                        self.truncated = true;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return read,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    self.pipe = None;
                    return read;
                }
            }
        }
    }

    /// The last non-empty line, trimmed and clipped to `TAIL_LIMIT`
    /// characters — one toast-sized reason.
    fn tail(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.buf);
        let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
        let mut clipped: String = line.chars().take(TAIL_LIMIT).collect();
        if clipped.chars().count() < line.chars().count() {
            clipped.push('…');
        }
        Some(clipped)
    }
}

/// The outcome of one non-blocking write attempt into the child's stdin
/// pipe.
enum WriteOutcome {
    /// The pipe accepted `n` bytes.
    Wrote(usize),
    /// The pipe broke (its reader went away): stop writing and let
    /// `try_wait` report the child's exit.
    Closed,
    /// Nothing written this round (buffer still full, or a transient
    /// error) — try again after the next `try_wait` check.
    Retry,
}

/// Wait up to 10ms for the pipe to accept more (`poll` — the loop's
/// pacemaker), then write as much as fits. The fd is `O_NONBLOCK`, so
/// a full buffer surfaces as WouldBlock instead of freezing the TUI's
/// only thread; EPIPE / POLLERR / POLLHUP mean the reader is gone.
fn write_pipe(stdin: &mut ChildStdin, bytes: &[u8]) -> WriteOutcome {
    let mut pollfd = libc::pollfd {
        fd: stdin.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: pollfd points at a valid fd, revents is ours to fill.
    if unsafe { libc::poll(&mut pollfd, 1, 10) } <= 0 {
        // Timeout (still full) or a transient error such as EINTR.
        return WriteOutcome::Retry;
    }
    if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return WriteOutcome::Closed;
    }
    match stdin.write(bytes) {
        Ok(n) if n > 0 => WriteOutcome::Wrote(n),
        Ok(_) => WriteOutcome::Retry,
        Err(e) if e.kind() == ErrorKind::WouldBlock => WriteOutcome::Retry,
        // EPIPE and anything else: the reader is gone.
        Err(_) => WriteOutcome::Closed,
    }
}

/// Pipe `text` into `cmd`'s stdin and wait for success.
fn copy_via(cmd: &str, args: &[&str], text: &str) -> Result<()> {
    pipe_and_wait(cmd, cmd, args, text, CHILD_TIMEOUT)
}

/// Pipe `text` into a shell command's stdin via `sh -c`. The command
/// receives the formatted export on stdin.
pub fn send_command(cmd: &str, text: &str) -> Result<()> {
    pipe_and_wait("send command", "sh", &["-c", cmd], text, CHILD_TIMEOUT)
}

/// Run a shell command with `text` on its stdin and hand back what it
/// wrote to stdout — the `--send-cmd` machinery, used for an ANSWER
/// rather than a delivery.
///
/// `--semantic-cmd` runs here (from its own thread, see
/// [`crate::semantic::CommandProvider`]). The hazards this shares with
/// the export path are the reason it is not a second implementation: a
/// child that never reads its stdin, a grandchild holding a pipe open
/// past the child's exit, an output flood larger than a pipe buffer,
/// and a child that simply never returns — all four are already solved
/// in [`run_child`], and all four are things any script wrapped around a
/// child process runs into, whatever it is a wrapper FOR.
///
/// A non-zero exit carries the last non-empty line of stderr (else
/// stdout) in the error, so the caller's toast can say *why*.
///
/// **The caller chooses the deadline policy** ([`Deadline`]), and
/// `--semantic-cmd` is the one caller that does not want a wall-clock
/// limit — see [`crate::semantic::COMMAND_IDLE_TIMEOUT`].
pub(crate) fn run_capturing(
    label: &str,
    cmd: &str,
    text: &str,
    deadline: Deadline,
    stdout_limit: usize,
) -> Result<String> {
    let out = run_child(label, "sh", &["-c", cmd], text, deadline, stdout_limit)?;
    if out.truncated {
        bail!("{label} wrote more than {stdout_limit} bytes to stdout");
    }
    String::from_utf8(out.stdout).with_context(|| format!("{label} stdout is not UTF-8"))
}

// ---------------------------------------------------------------------------
// herdr auto-resolution (`--send-agent`): find the sole agent in the
// CURRENT TAB (else the sole workspace agent) and submit the export as a
// positional argument — no shell involved, so no quoting problems.
// ---------------------------------------------------------------------------

/// Resolve the agent pane to send to: the sole agent in this tab, else
/// the sole workspace agent (the same resolution herdr-reviewr ships —
/// see its `specs/herdr-host.md`, MIT, credited in the README). A
/// refusal is an error whose message the toast shows (`no agent here` /
/// `several agents here`); the clipboard copy already happened either
/// way. `--send-agent` opts in explicitly, so no env probing is needed.
pub fn resolve_agent_pane() -> Result<String> {
    let list = herdr_json(&["agent", "list"])?;
    let agents = parse_agents(&list)?;
    let tab = std::env::var("HERDR_TAB_ID").ok();
    let ws = std::env::var("HERDR_WORKSPACE_ID").ok();
    let me = std::env::var("HERDR_PANE_ID").ok();
    pick_agent(&agents, tab.as_deref(), ws.as_deref(), me.as_deref())
}

fn herdr_json(args: &[&str]) -> Result<String> {
    let out = Command::new("herdr")
        .args(args)
        .output()
        .with_context(|| format!("running herdr {args:?}"))?;
    if !out.status.success() {
        bail!(
            "herdr {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The agents array from `herdr agent list`: a bare array, `result.agents`,
/// or `agents` (the envelope is not pinned; reviewr accepts all three).
fn parse_agents(json: &str) -> Result<Vec<serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_str(json).context("parsing agent list")?;
    if let Some(array) = value.as_array() {
        return Ok(array.clone());
    }
    value
        .get("result")
        .and_then(|r| r.get("agents"))
        .or_else(|| value.get("agents"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .context("agent list has no agents array")
}

/// The sole agent in this tab, else the sole workspace agent. Anything
/// else refuses with a reason the toast can show.
fn pick_agent(
    agents: &[serde_json::Value],
    tab: Option<&str>,
    ws: Option<&str>,
    me: Option<&str>,
) -> Result<String> {
    let in_tab = candidates(agents, "tab_id", tab, me);
    if let [agent] = in_tab.as_slice() {
        return pane_id(agent).context("agent entry has no pane_id");
    }
    match candidates(agents, "workspace_id", ws, me).as_slice() {
        [agent] => pane_id(agent).context("agent entry has no pane_id"),
        [] if in_tab.is_empty() => bail!("no agent here"),
        _ => bail!("several agents here"),
    }
}

/// The real agents whose `key` equals `want`, ignoring our own pane: only
/// entries carrying an `agent` field count (`herdr agent list` returns
/// every pane; plugin sidebars and plain shells have no `agent` field).
fn candidates<'a>(
    agents: &'a [serde_json::Value],
    key: &str,
    want: Option<&str>,
    me: Option<&str>,
) -> Vec<&'a serde_json::Value> {
    let Some(want) = want else { return Vec::new() };
    agents
        .iter()
        .filter(|a| a.get("agent").and_then(serde_json::Value::as_str).is_some())
        .filter(|a| a.get(key).and_then(serde_json::Value::as_str) == Some(want))
        .filter(|a| pane_id(a).as_deref() != me)
        .collect()
}

/// The `pane_id` of an agent entry.
fn pane_id(agent: &serde_json::Value) -> Option<String> {
    agent
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

/// Submit `text` to a herdr agent via `herdr agent prompt <target> <text>`
/// (the text is a positional argument, so no shell quoting issues). A
/// missing `herdr` binary or a failed submission is an error the UI
/// surfaces as a red toast without losing the clipboard copy.
pub fn send_to_agent(target: &str, text: &str) -> Result<()> {
    let out = Command::new("herdr")
        .args(["agent", "prompt", target, text])
        .output()
        .context("spawning herdr")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("herdr agent prompt failed: {}", stderr.trim());
    }
    Ok(())
}

/// The first clipboard tool `present` accepts, preserving list order.
fn select_tool(
    tools: &'static [(&'static str, &'static [&'static str])],
    present: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'static [&'static str])> {
    tools.iter().copied().find(|(cmd, _)| present(cmd))
}

#[cfg(test)]
mod tests {
    use super::{CLIPBOARD_TOOLS, format_all, format_all_reply, format_comment, select_tool};
    use crate::comment::Comment;
    use std::time::{Duration, Instant};

    fn comment(file: &str, start: u32, end: u32, lines: &str, text: &str) -> Comment {
        Comment {
            file_path: std::path::PathBuf::from(file),
            start,
            end,
            lines: lines.into(),
            revision: None,
            text: text.into(),
        }
    }

    #[test]
    fn block_is_location_numbered_snippet_text() {
        let c = comment(
            "wiki/cases/aozora-plan.md",
            31,
            33,
            "一覧に出ない」は仕様 → 利用手引の更新を確認\n\nここ、手引の文言と実装のズレがまだ残ってる。",
            "このコメント、利用手引の該当箇所も直して。",
        );
        assert_eq!(
            format_comment(&c),
            "wiki/cases/aozora-plan.md:31-33\n31: 一覧に出ない」は仕様 → 利用手引の更新を確認\n32: \n33: ここ、手引の文言と実装のズレがまだ残ってる。\nこのコメント、利用手引の該当箇所も直して。"
        );
    }

    #[test]
    fn historical_comment_tells_the_agent_which_document_it_saw() {
        let mut c = comment("doc.md", 2, 2, "old paragraph", "この簡潔さを戻したい");
        c.revision = Some("abc123 (abc123full) — simplify intro".into());
        let out = format_comment(&c);
        assert!(out.starts_with("Revision: abc123 (abc123full) — simplify intro\ndoc.md:2"));
        assert!(out.contains("2: old paragraph"));
    }

    #[test]
    fn existing_files_are_exported_with_an_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "line\n").unwrap();
        let mut c = comment("unused", 1, 1, "line", "note");
        c.file_path = path.clone();

        let canonical = std::fs::canonicalize(path).unwrap();
        assert!(format_comment(&c).starts_with(&format!("{}:1\n", canonical.display())));
    }

    #[test]
    fn single_line_location() {
        let c = comment("doc.md", 12, 12, "line", "note");
        assert_eq!(format_comment(&c), "doc.md:12\n12: line\nnote");
    }

    #[test]
    fn blank_selection_line_still_gets_its_number() {
        // The design's core case: an empty selected line renders as `n: `
        // (with the trailing space), never as a bare blank line.
        let c = comment("doc.md", 7, 7, "", "note");
        assert_eq!(format_comment(&c), "doc.md:7\n7: \nnote");
        // A run of blank lines numbers every one of them.
        let c = comment("doc.md", 12, 14, "\n", "note");
        assert_eq!(format_comment(&c), "doc.md:12-14\n12: \n13: \n14: \nnote");
    }

    #[test]
    fn snippet_line_count_is_pinned_to_the_location_range() {
        // Adapters derive the snippet length from `start-end`; extra
        // `lines` parts (unreachable via Source::snippet) must not extend
        // the numbering past `end` and leak into the comment text.
        let c = comment("doc.md", 3, 4, "a\nb\nc", "note");
        assert_eq!(format_comment(&c), "doc.md:3-4\n3: a\n4: b\nnote");
    }

    #[test]
    fn multiline_text_keeps_breaks_but_drops_blank_lines() {
        let c = comment("a.md", 1, 1, "+x", "first line\n\n  \nsecond line\n");
        assert_eq!(format_comment(&c), "a.md:1\n1: +x\nfirst line\nsecond line");
    }

    #[test]
    fn all_sorts_by_file_then_start_with_blank_separator() {
        let b = comment("b.md", 5, 5, "x", "two");
        let a2 = comment("a.md", 20, 20, "x", "later");
        let a1 = comment("a.md", 3, 3, "x", "earlier");
        let out = format_all(&[b, a2, a1]);
        assert_eq!(
            out,
            "a.md:3\n3: x\nearlier\n\na.md:20\n20: x\nlater\n\nb.md:5\n5: x\ntwo"
        );
    }

    #[test]
    fn clipboard_tool_selection_prefers_list_order_and_can_be_empty() {
        assert!(select_tool(CLIPBOARD_TOOLS, |_| false).is_none());
        assert_eq!(
            select_tool(CLIPBOARD_TOOLS, |c| c == "xclip"),
            Some(("xclip", &["-selection", "clipboard"][..]))
        );
        assert_eq!(
            select_tool(CLIPBOARD_TOOLS, |c| c == "pbcopy" || c == "xclip").map(|(c, _)| c),
            Some("pbcopy")
        );
    }

    #[test]
    fn copy_via_fails_on_missing_tool() {
        // A non-existent command must fail cleanly, not panic.
        assert!(super::copy_via("definitely-not-a-real-tool-xyz", &[], "x").is_err());
    }

    #[test]
    fn pipe_and_wait_succeeds_when_the_child_reads_stdin() {
        // `cat` reads stdin to EOF then exits 0 — the happy path of both
        // copy_via (pbcopy) and send_command (`cat >> review.txt`).
        super::pipe_and_wait(
            "cat",
            "sh",
            &["-c", "cat >/dev/null"],
            "hello",
            Duration::from_secs(5),
        )
        .unwrap();
    }

    #[test]
    fn pipe_and_wait_reports_a_non_zero_exit() {
        let err = super::pipe_and_wait(
            "send command",
            "sh",
            &["-c", "exit 3"],
            "",
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.to_string().contains("exited non-zero"));
    }

    #[test]
    fn pipe_and_wait_succeeds_when_a_grandchild_keeps_stdin_open() {
        // `sh -c "sleep 2 &"` exits 0 right after forking the sleep,
        // which inherits the pipe's read end and never reads it: the
        // write stalls on a full buffer while the child is already
        // gone. The old writer-thread design misreported this as
        // "stdin writer did not finish" and leaked the blocked thread;
        // the non-blocking loop must instead notice the child's exit
        // and succeed right away. The sleep is short so the reparented
        // grandchild dies on its own instead of lingering past the
        // test.
        let start = Instant::now();
        super::pipe_and_wait(
            "send command",
            "sh",
            &["-c", "sleep 2 &"],
            &"x".repeat(1 << 20),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the child exits immediately, so the result must not wait on the write"
        );
    }

    #[test]
    fn pipe_and_wait_times_out_and_kills_the_child() {
        // `sh -c "sleep 30"` never reads stdin: with 1MiB of text the
        // pipe buffer fills and the write would block forever — the
        // exact freeze the timeout exists to break. The injectable
        // timeout keeps the test fast: the child is killed at 200ms,
        // never waited for.
        let start = Instant::now();
        let err = super::pipe_and_wait(
            "send command",
            "sh",
            &["-c", "sleep 30"],
            &"x".repeat(1 << 20),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the child must be killed, not waited for"
        );
    }

    /// **`--send-cmd` の見切り方は変わっていない。** `--semantic-cmd` が
    /// 無音で測るようになった（`Deadline::WhileProgressing`）あとも、
    /// 配送側は壁時計のままである — 送信は文書の大きさに比例しないし、
    /// 進捗を出しながら詰まっている子を待ち続ける理由が無い。
    ///
    /// 喋り続ける子を `pipe_and_wait` に渡し、**進捗では延命しない**ことと
    /// 文言が従来どおり（`(backstop)` も `with no output` も付かない）こと
    /// を確かめる。
    #[test]
    fn pipe_and_wait_still_uses_the_wall_clock_even_for_a_chatty_child() {
        let start = Instant::now();
        let err = super::pipe_and_wait(
            "send command",
            "sh",
            // 10ms ごとに stderr へ 1 行出しながら、決して終わらない。
            &["-c", "while :; do printf 'working\\n' >&2; sleep 0.01; done"],
            "x",
            Duration::from_millis(200),
        )
        .unwrap_err();
        let message = err.to_string();
        assert_eq!(message, "send command timed out after 0.2s", "{message}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "進捗があっても壁時計で殺すこと（{:?} かかった）",
            start.elapsed()
        );
    }

    /// 無音の上限と backstop が、それぞれ自分の文言で止める。
    ///
    /// `run_capturing` の層で見ているのは、`--semantic-cmd` が通るのが
    /// こちらだからである（[`super::Deadline::WhileProgressing`]）。
    #[test]
    fn run_capturing_distinguishes_a_silent_child_from_a_chatty_one() {
        // 黙って寝ている子 — 無音の上限で止まる。
        let start = Instant::now();
        let err = super::run_capturing(
            "--semantic-cmd",
            "sleep 30",
            "x",
            super::Deadline::WhileProgressing {
                idle: Duration::from_millis(200),
                backstop: Duration::from_secs(30),
            },
            1 << 20,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("with no output"),
            "無音であることを言うこと: {err}"
        );
        assert!(start.elapsed() < Duration::from_secs(5));

        // 喋り続ける子 — 無音では当たらないので backstop が止める。
        let start = Instant::now();
        let err = super::run_capturing(
            "--semantic-cmd",
            "while :; do printf 'working\\n' >&2; sleep 0.01; done",
            "x",
            super::Deadline::WhileProgressing {
                idle: Duration::from_secs(30),
                backstop: Duration::from_millis(300),
            },
            1 << 20,
        )
        .unwrap_err();
        assert!(err.to_string().contains("backstop"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    /// 進捗が続くかぎり、無音の上限を何倍越えても殺さない。
    ///
    /// **これが 60 秒の固定値をやめた中身である**（実測で 35 KB の文書が
    /// 47 秒、同日の版が推定 74 秒。`super::Deadline` のコメント）。
    ///
    /// **無音の上限は出力の間隔より桁で大きく取る。** 本番は 30 秒に対して
    /// 1 リクエスト 1.2 秒（25 倍）で、テストもその比を真似る —— 詰めると
    /// **並列に走る他のテストの負荷で `sleep` が伸びて偽陽性になる**
    /// （最初 100ms/30ms（3 倍）で書いて、`--workspace` の全 580 本と
    /// 一緒に走らせたときだけ落ちた）。
    #[test]
    fn run_capturing_does_not_kill_a_child_that_keeps_talking() {
        let start = Instant::now();
        let out = super::run_capturing(
            "--semantic-cmd",
            // 50ms 間隔で 50 行 ≒ 2.5 秒。無音の上限 1.5 秒に対して
            // 間隔は 30 倍の余裕があり、壁時計では上限を越えている。
            "cat >/dev/null; \
             i=0; while [ $i -lt 50 ]; do i=$((i+1)); \
               printf 'round %s\\n' \"$i\" >&2; sleep 0.05; done; \
             printf done",
            "x",
            super::Deadline::WhileProgressing {
                idle: Duration::from_millis(1500),
                backstop: Duration::from_secs(60),
            },
            1 << 20,
        )
        .expect("進捗があるかぎり殺さない");
        assert_eq!(out, "done");
        assert!(
            start.elapsed() > Duration::from_millis(1500),
            "無音の上限より長く生きたことを確かめる（{:?}）",
            start.elapsed()
        );
    }

    #[test]
    fn pipe_and_wait_swallows_a_successful_childs_output() {
        // A chatty child (`herdr agent prompt` echoes a JSON reply) must
        // neither leak onto the alternate screen — its stdout/stderr
        // are piped, never inherited — nor turn a success into an
        // error. Nothing is surfaced on success.
        super::pipe_and_wait(
            "send command",
            "sh",
            &["-c", "cat >/dev/null; echo noise; echo hiss >&2"],
            "hello",
            Duration::from_secs(5),
        )
        .unwrap();
    }

    #[test]
    fn pipe_and_wait_does_not_deadlock_on_a_flood_of_output() {
        // A child that prints far more than a pipe buffer (64 KiB)
        // after reading its stdin would block on write forever if we
        // only read its output after exit; the in-loop drain keeps it
        // flowing. Must finish well inside the timeout.
        let start = Instant::now();
        super::pipe_and_wait(
            "send command",
            "sh",
            &[
                "-c",
                "cat >/dev/null; head -c 300000 /dev/zero | tr '\\0' x; head -c 300000 /dev/zero | tr '\\0' y >&2",
            ],
            "hello",
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "output must be drained as it arrives"
        );
    }

    #[test]
    fn pipe_and_wait_puts_the_stderr_tail_in_the_error() {
        // On failure the toast needs a reason: the last non-empty line
        // of stderr rides along in the error message.
        let err = super::pipe_and_wait(
            "send command",
            "sh",
            &[
                "-c",
                "echo 'first line' >&2; echo 'no such agent: p9' >&2; echo ignored; exit 2",
            ],
            "",
            Duration::from_secs(5),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exited non-zero"), "{msg}");
        assert!(msg.contains("no such agent: p9"), "{msg}");
        assert!(!msg.contains("first line"), "only the tail is shown: {msg}");
        assert!(!msg.contains("ignored"), "stderr wins over stdout: {msg}");
    }

    #[test]
    fn pipe_and_wait_falls_back_to_the_stdout_tail_when_stderr_is_silent() {
        let err = super::pipe_and_wait(
            "send command",
            "sh",
            &["-c", "echo 'rejected by server'; exit 1"],
            "",
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.to_string().contains("rejected by server"), "{err}");
    }

    #[test]
    fn pipe_and_wait_clips_a_long_tail() {
        // A JSON dump on one line must not become a screen-wide toast.
        let err = super::pipe_and_wait(
            "send command",
            "sh",
            &["-c", "head -c 5000 /dev/zero | tr '\\0' j >&2; exit 1"],
            "",
            Duration::from_secs(5),
        )
        .unwrap_err();
        let msg = err.to_string();
        // 切り詰めは stderr の末尾のほうに起きる — `…` の後ろには
        // `(send command exited non-zero)` が続く（この括弧は後ろに置いて
        // あるので、ステータス行で切られてもいちばん損が小さい）。
        assert!(msg.contains('…'), "{msg}");
        assert!(msg.ends_with("(send command exited non-zero)"), "{msg}");
        assert!(msg.chars().count() < 250, "{}", msg.chars().count());
    }

    fn agent(pane: &str, tab: &str, ws: &str, is_agent: bool) -> serde_json::Value {
        let mut v = serde_json::json!({
            "pane_id": pane, "tab_id": tab, "workspace_id": ws,
        });
        if is_agent {
            v["agent"] = serde_json::json!("claude");
        }
        v
    }

    #[test]
    fn pick_agent_prefers_the_tab_then_the_workspace() {
        use super::pick_agent;
        let a = agent("p1", "t1", "w1", true);
        let b = agent("p2", "t2", "w1", true);
        let me = agent("p0", "t1", "w1", false); // 自分(非エージェント)
        let list = vec![a, b, me];
        // タブ内に唯一のエージェント → それ。
        assert_eq!(
            pick_agent(&list, Some("t1"), Some("w1"), Some("p0")).unwrap(),
            "p1"
        );
        // タブ内ゼロ・ワークスペースに唯一 → それ。
        assert_eq!(
            pick_agent(&list[1..2], Some("t9"), Some("w1"), Some("p0")).unwrap(),
            "p2"
        );
        // ワークスペースに複数 → 曖昧で拒否。
        let err = pick_agent(&list, Some("t9"), Some("w1"), Some("p0")).unwrap_err();
        assert!(err.to_string().contains("several"));
        // どこにも居ない → no agent。
        let err = pick_agent(&[], Some("t1"), Some("w1"), None).unwrap_err();
        assert!(err.to_string().contains("no agent"));
    }

    #[test]
    fn non_agent_panes_do_not_make_the_tab_ambiguous() {
        use super::pick_agent;
        // A tab full of plain shells / plugin sidebars (no `agent` field)
        // plus ONE real agent must resolve — pane count alone is not
        // ambiguity (reviewr parity).
        let list = vec![
            agent("p1", "t1", "w1", true),
            agent("p2", "t1", "w1", false),
            agent("p3", "t1", "w1", false),
            agent("p4", "t1", "w1", false),
        ];
        assert_eq!(
            pick_agent(&list, Some("t1"), Some("w1"), Some("p2")).unwrap(),
            "p1"
        );
    }

    #[test]
    fn missing_herdr_env_refuses_cleanly() {
        use super::pick_agent;
        // Outside herdr (no HERDR_* env) there is no tab/ws to match:
        // a clean "no agent here" instead of a wrong guess.
        let list = vec![agent("p1", "t1", "w1", true)];
        let err = pick_agent(&list, None, None, None).unwrap_err();
        assert!(err.to_string().contains("no agent"));
    }

    #[test]
    fn reply_format_blockquotes_without_location_or_numbers() {
        use super::format_comment_reply;
        let c = comment("doc.md", 12, 14, "a\nb\nc", "note");
        assert_eq!(
            format_comment_reply(&c, None),
            "> a\n> b\n> c\n\nnote",
            "blank line keeps the comment out of the lazy-continuation quote"
        );
        let all = format_all_reply(&[c]);
        assert!(!all.contains("doc.md"), "no file path leaks into a reply");
        assert!(!all.contains("12:"), "no line-number prefix leaks into a reply");
        assert_eq!(all, "> a\n> b\n> c\n\nnote");
    }

    #[test]
    fn reply_format_blank_selection_lines_stay_quoted() {
        use super::format_comment_reply;
        let c = comment("doc.md", 7, 9, "x\n\ny", "note");
        assert_eq!(
            format_comment_reply(&c, None),
            "> x\n> \n> y\n\nnote",
            "blank lines render as `> ` and cannot become block separators"
        );
    }

    #[test]
    fn reply_format_batch_numbers_each_quote() {
        // A batch of comments is a list of distinct points: the quote of
        // each pair gets the number (`N. > ...`) and the comment is
        // indented so the whole pair lives inside the numbered item.
        // The number sits before the `> ` marker — quoted content that
        // itself contains `1. ` list markers cannot collide.
        let c1 = comment("a.md", 3, 3, "x", "one");
        let c2 = comment("b.md", 1, 2, "p\nq", "two");
        assert_eq!(
            format_all_reply(&[c1.clone(), c2.clone()]),
            "1. > x\n\n    one\n\n2. > p\n   > q\n\n    two",
            "each pair's quote is numbered and its comment indented"
        );
        // Single comments stay plain (no number, no indent).
        assert_eq!(format_all_reply(&[c1]), "> x\n\none");
        assert_eq!(format_all_reply(&[c2]), "> p\n> q\n\ntwo");
    }

    #[test]
    fn reply_format_batch_indents_multiline_comment() {
        use super::format_comment_reply;
        // A multi-line comment stays inside its numbered item: every
        // comment line gets the 4-space indent.
        let c = comment("a.md", 1, 1, "x", "first\nsecond");
        assert_eq!(
            format_comment_reply(&c, Some(1)),
            "1. > x\n\n    first\n    second"
        );
    }

    #[test]
    fn reply_format_numbered_quote_indents_continuation() {
        use super::format_comment_reply;
        // Multi-line quote: `3. > first` then 3-space-indented `> rest`
        // lines, so the whole quote stays inside the numbered item.
        let c = comment("doc.md", 1, 2, "first\nsecond", "note");
        assert_eq!(
            format_comment_reply(&c, Some(3)),
            "3. > first\n   > second\n\n    note"
        );
    }

    #[test]
    fn reply_format_quoted_list_markers_do_not_collide() {
        use super::format_comment_reply;
        // Quoted content that is itself a numbered list: the inner `1. `
        // stays inside the blockquote and cannot be mistaken for the
        // batch number of this pair.
        let c = comment("doc.md", 1, 2, "1. first\n2. second", "note");
        assert_eq!(
            format_comment_reply(&c, Some(2)),
            "2. > 1. first\n   > 2. second\n\n    note",
            "item number (2.) precedes the quote marker, inner list stays inside"
        );
    }

    #[test]
    fn parse_agents_accepts_all_envelopes() {
        use super::parse_agents;
        assert_eq!(parse_agents(r#"[{"pane_id":"p"}]"#).unwrap().len(), 1);
        assert_eq!(
            parse_agents(r#"{"agents":[{"pane_id":"p"}]}"#).unwrap().len(),
            1
        );
        assert_eq!(
            parse_agents(r#"{"result":{"agents":[{"pane_id":"p"}]}}"#)
                .unwrap()
                .len(),
            1
        );
        assert!(parse_agents(r#"{"nope":1}"#).is_err());
    }
}
