//! Terminal-only, throttled progress on stderr. Output failure never fails repository work.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use girt::fetch::{LocalFetchProgress, ValidationProgress};
use girt::workflow::TransferCallbacks;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

const INTERVAL: Duration = Duration::from_millis(100);
const MAX_REMOTE_LINE: usize = 4096;

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
}

impl Progress {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: enabled && std::io::stderr().is_terminal(),
            phase: "",
            latest: String::new(),
            count: 0,
            total: None,
            last: Instant::now() - INTERVAL,
            remote: Vec::new(),
            bar: None,
        }
    }

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
            self.draw(false);
        }
    }

    fn draw(&mut self, final_line: bool) {
        if self.phase != "remote" {
            self.latest = count_line(self.phase, self.count, self.total);
        }
        let bar = self.bar.get_or_insert_with(message_bar);
        if final_line {
            let suffix = if self.phase == "remote" {
                ""
            } else {
                ", done."
            };
            bar.finish_with_message(format!("{}{suffix}", self.latest));
            self.bar = None;
        } else {
            bar.set_message(self.latest.clone());
        }
        self.last = Instant::now();
    }

    pub fn finish(&mut self) {
        if self.enabled && !self.remote.is_empty() {
            if !self.phase.is_empty() && self.phase != "remote" {
                self.draw(true);
            }
            self.phase = "remote";
            self.latest = remote_line(&self.remote);
            self.remote.clear();
        }
        if self.enabled && !self.phase.is_empty() {
            self.draw(true);
            self.latest.clear();
        }
        self.phase = "";
        self.last = Instant::now() - INTERVAL;
    }

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

    pub fn remote(&mut self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        for &byte in bytes {
            if self.remote.len() == MAX_REMOTE_LINE {
                self.remote_line(true);
            }
            if byte == b'\r' || byte == b'\n' {
                self.remote_line(byte == b'\n');
            } else if byte >= b' ' && byte != 0x7f {
                self.remote.push(byte);
            }
        }
    }

    fn remote_line(&mut self, final_line: bool) {
        let bytes = std::mem::take(&mut self.remote);
        if bytes.is_empty() {
            if final_line && self.phase == "remote" {
                self.draw(true);
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
        if final_line {
            self.draw(true);
            self.latest.clear();
            self.phase = "";
        } else if self.last.elapsed() >= INTERVAL {
            self.draw(false);
        }
        self.remote = bytes;
        self.remote.clear();
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

    fn validation_progress(&mut self, progress: ValidationProgress) {
        self.validation(progress);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.abandon_with_message(format!("{} (stopped)", self.latest));
        }
    }
}

fn message_bar() -> ProgressBar {
    ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10))
        .with_style(ProgressStyle::with_template("{msg}").expect("valid progress template"))
}

/// A phase with no library counters, kept visibly alive without inventing a percentage.
pub struct Activity {
    bar: Option<ProgressBar>,
    label: &'static str,
}

impl Activity {
    pub fn new(enabled: bool, label: &'static str) -> Self {
        let bar = if enabled && std::io::stderr().is_terminal() {
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

    pub fn clear(mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }

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

fn count_line(phase: &str, count: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total != 0 => format!(
            "{phase}: {:3}% ({count}/{total})",
            count.saturating_mul(100) / total
        ),
        _ => format!("{phase}: {count}"),
    }
}

fn remote_line(bytes: &[u8]) -> String {
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    format!("remote: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_phases_do_not_draw() {
        let mut progress = Progress::new(false);
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
        let mut progress = Progress::new(false);
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
        let mut progress = Progress::new(false);
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
    fn disabled_progress_does_not_retain_server_bytes_or_counts() {
        let mut progress = Progress::new(false);
        progress.remote(&vec![b'x'; MAX_REMOTE_LINE * 2]);
        progress.update("Reading", 100, None);
        assert!(progress.remote.is_empty());
        assert!(progress.phase.is_empty());
        assert!(progress.latest.is_empty());
    }
}
