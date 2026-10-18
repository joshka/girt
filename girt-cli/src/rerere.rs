//! `girt rerere`: record and replay conflict resolutions in Git's `rr-cache`.

use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use color_eyre::eyre::Result;
use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
use girt::rerere::{self, Rerere};

use crate::Context;
use crate::common::{self, display_bytes};

/// Reuse recorded conflict resolutions (like `git rerere`).
///
/// Without a subcommand, records the resolutions of tracked conflicts that no longer have
/// markers, then records or replays every unmerged path in the index.
#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// List the paths whose conflicts are being tracked.
    Status,
    /// List tracked paths that still have conflict markers.
    Remaining,
    /// Show how each tracked file differs from its recorded conflict.
    Diff,
    /// Forget the recorded resolutions of unmerged paths.
    Forget {
        /// Paths to forget.
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Stop tracking the current conflicts and drop their unresolved records.
    Clear,
    /// Remove old records (`gc.rerereResolved`, `gc.rerereUnresolved`).
    Gc,
}

pub fn run(args: Args, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let mut out = common::stdout();
    // Git does nothing, not even for subcommands, while rerere is disabled.
    if !rerere::is_enabled(&repository)? {
        return Ok(ExitCode::SUCCESS);
    }
    match args.command {
        None => {
            let report = Rerere::open(&repository)?.run_on_index()?;
            for path in &report.recorded_conflicts {
                eprintln!("Recorded preimage for '{}'", display_bytes(path));
            }
            // With autoUpdate Git reports a replayed path once, as staged.
            for path in report
                .replayed
                .iter()
                .filter(|p| !report.staged.contains(p))
            {
                eprintln!(
                    "Resolved '{}' using previous resolution.",
                    display_bytes(path)
                );
            }
            for path in &report.staged {
                eprintln!(
                    "Staged '{}' using previous resolution.",
                    display_bytes(path)
                );
            }
            for path in &report.recorded_resolutions {
                eprintln!("Recorded resolution for '{}'.", display_bytes(path));
            }
        }
        Some(Command::Status) => {
            for path in Rerere::open(&repository)?.status() {
                common::write_line(&mut out, path)?;
            }
        }
        Some(Command::Remaining) => {
            let mut paths = Rerere::open(&repository)?.remaining()?;
            paths.extend(rerere::unhandled(&repository)?);
            paths.sort();
            for path in paths {
                common::write_line(&mut out, &path)?;
            }
        }
        Some(Command::Diff) => {
            let rerere = Rerere::open(&repository)?;
            for path in rerere.status() {
                let Some(files) = rerere.diff(path)? else {
                    continue;
                };
                let ContentDiff::Text(text) = diff(
                    &files.preimage,
                    &files.current,
                    BinaryMode::Auto,
                    DiffLimits::default(),
                    &AtomicBool::new(false),
                )?
                else {
                    continue;
                };
                out.write_all(b"--- a/")?;
                common::write_line(&mut out, path)?;
                out.write_all(b"+++ b/")?;
                common::write_line(&mut out, path)?;
                girt::patch::write_hunks(&mut out, &text, 3)?;
            }
        }
        Some(Command::Forget { paths }) => {
            let paths: Vec<Vec<u8>> = paths.into_iter().map(String::into_bytes).collect();
            let mut rerere = Rerere::open(&repository)?;
            for path in rerere.forget(&paths)? {
                eprintln!("Forgot resolution for '{}'", display_bytes(&path));
            }
        }
        Some(Command::Clear) => Rerere::open(&repository)?.clear()?,
        Some(Command::Gc) => {
            rerere::gc(&repository, SystemTime::now())?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}
