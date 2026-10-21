//! Git-style progress lines on stderr for transfers, checkout and maintenance.
//!
//! [`Progress`] turns the library's [`TransferCallbacks`] events into the lines `git` prints, such
//! as `Receiving objects:  50% (2/4)`, ending each phase with `, done.`. [`Activity`] covers
//! phases that report no counts. Both draw only when stderr is a terminal and never write to
//! stdout, so piped and scripted output stays clean. Drawing uses [`indicatif`] with a plain
//! `{msg}` template: this module formats the text, and indicatif redraws the line and rate-limits
//! terminal writes.
//!
//! Output failure never fails repository work: nothing here returns an error, and indicatif
//! ignores write failures on its draw target.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use girt::fetch::{LocalFetchProgress, ValidationProgress};
use girt::workflow::TransferCallbacks;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Minimum time between redraws, and the spinner tick for [`Activity`].
///
/// [`ProgressDrawTarget::stderr_with_hz`] already limits terminal writes to ten per second. This
/// check also skips formatting a line nobody will see, which matters for phases that report every
/// object or file.
const INTERVAL: Duration = Duration::from_millis(100);

/// Longest buffered line of server or transport text; longer lines are split.
///
/// The remote controls this text, so the bound keeps a line without terminators from growing
/// memory without limit.
const MAX_REMOTE_LINE: usize = 4096;

/// Whether a command may draw progress.
///
/// Even [`Terminal`](Self::Terminal) progress is drawn only when stderr is a terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    /// Draw progress when stderr is a terminal.
    Terminal,
    /// Never draw progress, as under `--quiet` or `--git`.
    Hidden,
}

impl Visibility {
    fn is_drawn(self) -> bool {
        self == Self::Terminal && std::io::stderr().is_terminal()
    }
}

/// How a buffered server line ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineEnd {
    /// `\r`: the next line replaces this one, as in a server's own counters.
    Replace,
    /// `\n`, or a line split at [`MAX_REMOTE_LINE`]: the line stays in the scrollback.
    Complete,
}

/// Progress for one transfer, checkout or remote listing, shown the way `git` shows it.
///
/// Pass it as the [`TransferCallbacks`] of a [`girt::workflow`] call, then call
/// [`finish`](Self::finish) after the call succeeds to complete the last line. Each phase gets its
/// own line: when the phase name changes, the previous line is finished with `, done.` and stays
/// in the scrollback. Server `remote:` messages are shown as their own lines between phases.
///
/// Dropping it without `finish`, as an early `?` return does, leaves the current line marked
/// `(stopped)` instead of a half-drawn bar.
///
/// Diagnostics from the transport process, such as `ssh` errors, are always written to stderr,
/// even when progress is disabled, because they usually explain why a transfer failed.

#[derive(Debug)]
pub struct Progress {
    enabled: bool,
    phase: &'static str,
    latest: String,
    count: u64,
    total: Option<u64>,
    last: Instant,
    remote: Vec<u8>,
    bar: Option<ProgressBar>,
    transport: Vec<u8>,
}

impl Progress {
    /// Creates progress output, drawn only for [`Visibility::Terminal`] when stderr is a terminal.
    ///
    /// The terminal check happens here as well as in indicatif's [`ProgressDrawTarget::term`],
    /// so a disabled `Progress` keeps no buffered text or counts.
    /// `Progress::new(Visibility::Hidden)` is a silent sink for internal fetches, such as lazy
    /// fetches of missing objects, that still reports transport diagnostics.
    pub fn new(visibility: Visibility) -> Self {
        Self {
            enabled: visibility.is_drawn(),
            phase: "",
            latest: String::new(),
            count: 0,
            total: None,
            last: Instant::now() - INTERVAL,
            remote: Vec::new(),
            bar: None,
            transport: Vec::new(),
        }
    }

    /// Records `count` of `total` items for `phase`, redrawing at most every [`INTERVAL`].
    ///
    /// A different `phase` than the current one finishes the current line first; phases are
    /// compared by name, so pass the same `&'static str` for every update of one phase. A `total`
    /// of `None` shows a bare count. A known total of zero draws nothing, as `git` does for empty
    /// phases.
    pub fn update(&mut self, phase: &'static str, count: u64, total: Option<u64>) {
        if !self.enabled || total == Some(0) {
            return;
        }
        if self.phase != phase {
            self.finish();
            self.phase = phase;
        }
        self.count = count;
        self.total = total;
        if self.last.elapsed() >= INTERVAL {
            self.show();
        }
    }

    /// Redraws the current line in place.
    fn show(&mut self) {
        self.refresh_latest();
        let line = self.latest.clone();
        self.bar.get_or_insert_with(message_bar).set_message(line);
        self.last = Instant::now();
    }

    /// Draws the current line a last time and leaves it in the scrollback.
    ///
    /// Count lines end with `, done.`, as `git` prints them; server lines are left as sent.
    fn complete(&mut self) {
        self.refresh_latest();
        let suffix = if self.phase == "remote" {
            ""
        } else {
            ", done."
        };
        let bar = self.bar.take().unwrap_or_else(message_bar);
        bar.finish_with_message(format!("{}{suffix}", self.latest));
        self.last = Instant::now();
    }

    /// Formats the latest counts; server lines are formatted as they arrive.
    fn refresh_latest(&mut self) {
        if self.phase != "remote" {
            self.latest = count_line(self.phase, self.count, self.total);
        }
    }

    /// Completes the current line, including a partial server message.
    ///
    /// Call this after a successful workflow call. It is also called internally at each phase
    /// change, and calling it with nothing in progress does nothing.
    pub fn finish(&mut self) {
        self.flush_transport();
        if self.enabled && !self.remote.is_empty() {
            if !self.phase.is_empty() && self.phase != "remote" {
                self.complete();
            }
            self.phase = "remote";
            self.latest = remote_line(&self.remote);
            self.remote.clear();
        }
        if self.enabled && !self.phase.is_empty() {
            self.complete();
            self.latest.clear();
        }
        self.phase = "";
        self.last = Instant::now() - INTERVAL;
    }

    /// Shows a local fetch event as `Reading objects` or `Packing objects`.
    pub fn local(&mut self, event: LocalFetchProgress) {
        match event {
            LocalFetchProgress::Reading { objects } => {
                self.update("Reading objects", objects, None)
            }
            LocalFetchProgress::Packing {
                objects: (done, total),
            } => {
                self.update("Packing objects", done, Some(total));
            }
            LocalFetchProgress::Complete => self.finish(),
            _ => {}
        }
    }

    /// Shows a validation snapshot as `Validating objects`, then `Resolving deltas` once the
    /// delta total is known.
    pub fn validation(&mut self, event: ValidationProgress) {
        let (phase, (done, total)) = match event.deltas {
            Some(counts) => ("Resolving deltas", counts),
            None => ("Validating objects", event.objects),
        };
        self.update(phase, done, Some(total));
        if event.complete {
            self.finish();
        }
    }

    /// Shows server text as `remote: …` lines, following the terminal conventions `git` uses.
    ///
    /// Messages arrive in arbitrary chunks, so partial lines are buffered across calls. A `\r`
    /// replaces the current line, as in a server's own progress counters, and a `\n` completes
    /// it. Control characters are dropped, so a server cannot send escape sequences to the
    /// terminal, and lines longer than [`MAX_REMOTE_LINE`] are split.
    pub fn remote(&mut self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        for &byte in bytes {
            if self.remote.len() == MAX_REMOTE_LINE {
                self.remote_line(LineEnd::Complete);
            }
            if byte == b'\r' || byte == b'\n' {
                self.remote_line(if byte == b'\n' {
                    LineEnd::Complete
                } else {
                    LineEnd::Replace
                });
            } else if byte >= b' ' && byte != 0x7f {
                self.remote.push(byte);
            }
        }
    }

    /// Shows the buffered server line, either in place or as a completed line.
    fn remote_line(&mut self, end: LineEnd) {
        let bytes = std::mem::take(&mut self.remote);
        if bytes.is_empty() {
            if end == LineEnd::Complete && self.phase == "remote" {
                self.complete();
                self.latest.clear();
                self.phase = "";
            }
            self.remote = bytes;
            return;
        }
        if self.phase != "remote" {
            self.finish();
            self.phase = "remote";
        }
        self.latest = remote_line(&bytes);
        if end == LineEnd::Complete {
            self.complete();
            self.latest.clear();
            self.phase = "";
        } else if self.last.elapsed() >= INTERVAL {
            self.show();
        }
        // Keep the buffer's allocation for the next line.
        self.remote = bytes;
        self.remote.clear();
    }

    /// Writes transport diagnostics to stderr a line at a time, above any progress line.
    ///
    /// `git` passes `ssh` errors straight through; this does the same, with control characters
    /// dropped. It ignores [`Visibility`].
    fn transport_text(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                self.flush_transport();
                continue;
            }
            if self.transport.len() == MAX_REMOTE_LINE {
                self.flush_transport();
            }
            if byte >= b' ' && byte != 0x7f {
                self.transport.push(byte);
            }
        }
    }

    /// Writes any buffered transport text as one line.
    fn flush_transport(&mut self) {
        if self.transport.is_empty() {
            return;
        }
        let line = sanitize(&self.transport);
        self.transport.clear();
        let write = || {
            // Progress output must not fail the transfer, so a failed write is ignored.
            let _ = writeln!(std::io::stderr(), "{line}");
        };
        // `suspend` clears the progress line, runs the write, then redraws.
        match &self.bar {
            Some(bar) => bar.suspend(write),
            None => write(),
        }
    }
}

impl TransferCallbacks for Progress {
    fn remote_message(&mut self, message: &[u8]) {
        self.remote(message);
    }

    fn fetch_progress(&mut self, progress: LocalFetchProgress) {
        self.local(progress);
    }

    fn push_progress(&mut self, progress: girt::push::PreparationProgress) {
        use girt::push::PreparationProgress;
        match progress {
            PreparationProgress::Reading { objects } => {
                self.update("Reading objects", objects, None)
            }
            PreparationProgress::Packing {
                objects: (done, total),
            } => self.update("Writing objects", done, Some(total)),
            PreparationProgress::Complete => self.finish(),
            _ => {}
        }
    }

    fn checkout_progress(&mut self, done: u64, total: u64) {
        self.update("Updating files", done, Some(total));
    }

    fn validation_progress(&mut self, progress: ValidationProgress) {
        self.validation(progress);
    }

    fn transport_message(&mut self, message: &[u8]) {
        self.transport_text(message);
    }
}

impl Drop for Progress {
    /// Marks an unfinished line `(stopped)`, using [`ProgressBar::abandon_with_message`] so the
    /// line stays visible instead of being cleared.
    fn drop(&mut self) {
        self.flush_transport();
        if let Some(bar) = self.bar.take() {
            bar.abandon_with_message(format!("{} (stopped)", self.latest));
        }
    }
}

/// A bar that shows only its message, drawn to stderr at most ten times a second.
///
/// [`ProgressDrawTarget::stderr_with_hz`] hides the bar when stderr is not a terminal.
fn message_bar() -> ProgressBar {
    ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10))
        .with_style(ProgressStyle::with_template("{msg}").expect("valid progress template"))
}

/// A phase with no library counters, kept visibly alive without inventing a percentage.
///
/// Shows `label: <spinner> (elapsed)`, ticking on its own thread through
/// [`ProgressBar::enable_steady_tick`] while the caller blocks on the work. End it with
/// [`finish`](Self::finish) to leave a `label: details, done.` line, or [`clear`](Self::clear) to
/// remove it. Dropping it otherwise leaves `label: stopped`.
pub struct Activity {
    bar: Option<ProgressBar>,
    label: &'static str,
}

impl Activity {
    /// Starts the spinner, drawn only for [`Visibility::Terminal`] when stderr is a terminal.
    pub fn new(visibility: Visibility, label: &'static str) -> Self {
        let bar = if visibility.is_drawn() {
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10))
                .with_style(
                    ProgressStyle::with_template("{msg}: {spinner} ({elapsed_precise})")
                        .expect("valid activity template"),
                );
            bar.set_message(label);
            bar.enable_steady_tick(INTERVAL);
            Some(bar)
        } else {
            None
        };
        Self { bar, label }
    }

    /// Removes the line, for phases whose result is reported elsewhere.
    pub fn clear(mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }

    /// Replaces the spinner with a completed `label: details, done.` line.
    pub fn finish(mut self, details: impl std::fmt::Display) {
        if let Some(bar) = self.bar.take() {
            bar.set_style(ProgressStyle::with_template("{msg}").expect("valid progress template"));
            bar.finish_with_message(format!("{}: {details}, done.", self.label));
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.set_style(ProgressStyle::with_template("{msg}").expect("valid progress template"));
            bar.abandon_with_message(format!("{}: stopped", self.label));
        }
    }
}

/// Formats a count the way `git` does: a padded percentage when the total is known.
fn count_line(phase: &str, count: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total != 0 => format!(
            "{phase}: {:3}% ({count}/{total})",
            count.saturating_mul(100) / total
        ),
        _ => format!("{phase}: {count}"),
    }
}

/// Formats server text as a `remote:` line.
fn remote_line(bytes: &[u8]) -> String {
    format!("remote: {}", sanitize(bytes))
}

/// Decodes untrusted text for the terminal, dropping control characters.
///
/// The byte filters in [`Progress::remote`] and [`Progress::transport_text`] drop C0 controls; this
/// also drops C1 controls such as U+009B, which some terminals treat as an escape sequence.
fn sanitize(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_phases_do_not_draw() {
        let mut progress = Progress::new(Visibility::Hidden);
        progress.enabled = true;
        progress.update("Updating files", 0, Some(0));
        assert!(progress.phase.is_empty());
        assert!(progress.bar.is_none());
    }

    #[test]
    fn counts_distinguish_unknown_and_known_totals() {
        assert_eq!(count_line("Reading", 4, None), "Reading: 4");
        assert_eq!(count_line("Packing", 2, Some(4)), "Packing:  50% (2/4)");
        assert_eq!(count_line("Packing", 0, Some(0)), "Packing: 0");
    }

    #[test]
    fn server_chunks_keep_partial_lines_and_replace_carriage_return_updates() {
        let mut progress = Progress::new(Visibility::Hidden);
        // The draw target remains hidden under the test runner's redirected stderr.
        progress.enabled = true;
        progress.remote(b"Counting obj");
        assert_eq!(progress.remote, b"Counting obj");
        progress.remote(b"ects: 1\rCounting objects: 2\npartial");
        assert_eq!(progress.remote, b"partial");
        assert!(progress.phase.is_empty());
        progress.finish();
        assert!(progress.remote.is_empty());
        assert!(progress.phase.is_empty());
    }

    #[test]
    fn oversized_server_lines_are_bounded() {
        let mut progress = Progress::new(Visibility::Hidden);
        progress.enabled = true;
        progress.remote(&vec![b'x'; MAX_REMOTE_LINE * 2 + 7]);
        assert_eq!(progress.remote.len(), 7);
        progress.finish();
    }

    #[test]
    fn server_text_cannot_inject_terminal_controls() {
        assert_eq!(
            remote_line(b"hello\x1b[31m\xc2\x9bworld\xff"),
            "remote: hello[31mworld�"
        );
    }

    #[test]
    fn transport_lines_are_buffered_and_bounded_even_when_disabled() {
        let mut progress = Progress::new(Visibility::Hidden);
        progress.transport_text(b"Permission den");
        assert_eq!(progress.transport, b"Permission den");
        progress.transport_text(b"ied (publickey).\r\npart\n");
        assert!(progress.transport.is_empty());
        progress.transport_text(&vec![b'x'; MAX_REMOTE_LINE + 3]);
        assert_eq!(progress.transport.len(), 3);
        progress.finish();
        assert!(progress.transport.is_empty());
    }

    #[test]
    fn disabled_progress_does_not_retain_server_bytes_or_counts() {
        let mut progress = Progress::new(Visibility::Hidden);
        progress.remote(&vec![b'x'; MAX_REMOTE_LINE * 2]);
        progress.update("Reading", 100, None);
        assert!(progress.remote.is_empty());
        assert!(progress.phase.is_empty());
        assert!(progress.latest.is_empty());
    }
}
