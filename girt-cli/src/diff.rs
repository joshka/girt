//! `girt diff`: compare two trees as a change list, renames or a unified patch.

use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _};
use girt::ReadLimits;
use girt::display::quote_path;
use girt::patch::{Limits, Options};
use girt::rewrites::{self, Copies};
use girt::tree_diff::Entry;

use crate::common::{resolve_tree, stdout};
use crate::{Context, complete};

/// Compare two trees.
///
/// Both revisions are peeled to trees. The default output is one change per line: a status
/// letter (`A`dded, `D`eleted, `M`odified, `T`ype changed) and the path.
///
/// With `--git`, output mirrors `git diff-tree -r --name-status <a> <b>` (add `-M` / `-C` for
/// `--renames` / `--copies`), and `--patch` mirrors `git diff --no-renames <a> <b>` (or
/// `git diff -M <a> <b>` with `--renames`). Known patch differences: girt's line diff is plain
/// Myers without Git's indent heuristic, so ambiguous hunks may align differently; binary detection
/// scans whole blobs for NUL rather than the first 8000 bytes. Attributes and `diff.<driver>`
/// settings apply as in `git diff`, but a driver with textconv or an external command is refused.
#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    /// The old revision (commit or tree).
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
    a: String,
    /// The new revision (commit or tree).
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
    b: String,
    /// List changed paths with a status letter (the default).
    #[arg(long, conflicts_with_all = ["renames", "patch"])]
    names: bool,
    /// Detect renames, shown as `R<similarity> <old> <new>`.
    #[arg(long, conflicts_with = "names")]
    renames: bool,
    /// Print a unified diff with three lines of context.
    #[arg(long)]
    patch: bool,
    /// Also detect copies from modified files (implies `--renames`).
    #[arg(long)]
    copies: bool,
}

pub fn run(args: DiffArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let old = resolve_tree(&repository, &objects, &args.a)?;
    let new = resolve_tree(&repository, &objects, &args.b)?;
    let entries = if args.renames || args.copies {
        let options = rewrites::Options::default().with_copies(if args.copies {
            Copies::Modified
        } else {
            Copies::Disabled
        });
        let comparison = objects
            .diff_trees_with(
                Some(old),
                Some(new),
                options,
                Default::default(),
                &AtomicBool::new(false),
            )
            .wrap_err("comparing trees and detecting renames")?;
        if let Some(skipped) = comparison.inexact_skipped {
            eprintln!(
                "warning: exhaustive rename detection skipped ({} sources, {} targets, limit {})",
                skipped.sources, skipped.targets, skipped.limit
            );
        }
        comparison.entries
    } else {
        objects
            .compare_trees(Some(old), Some(new))
            .wrap_err("comparing trees")?
            .into_iter()
            .map(Entry::Change)
            .collect()
    };
    let mut out = stdout();
    let width = crate::common::abbrev_len(&repository, &objects)?;
    if args.patch {
        // The CLI reads trusted local storage, so blobs are read without the default object cap.
        let options = Options::default().with_abbrev_len(Some(width));
        let limits = Limits::default().with_read(ReadLimits::trusted());
        let cancel = AtomicBool::new(false);
        let mut attributes = repository
            .diff_attributes(&context.environment)
            .wrap_err("reading diff attributes")?;
        let attributes = Some(&mut attributes);
        if context.color {
            let mut colored = PatchColor::new(&mut out);
            objects
                .write_patch_with(&mut colored, &entries, options, attributes, limits, &cancel)
                .wrap_err("writing patch")?;
            colored.flush()?;
        } else {
            objects
                .write_patch_with(&mut out, &entries, options, attributes, limits, &cancel)
                .wrap_err("writing patch")?;
        }
    } else {
        for entry in &entries {
            if context.color {
                let style = match entry {
                    Entry::Change(change) if change.new.is_none() => crate::color::RED,
                    Entry::Change(change) if change.old.is_none() => crate::color::GREEN,
                    _ => crate::color::YELLOW,
                };
                let mut record = Vec::new();
                write_status(&mut record, entry, context.git)?;
                // End the style before the record's newline, as patch output does.
                record.pop();
                out.write_all(style.as_bytes())?;
                out.write_all(&record)?;
                writeln!(out, "{}", crate::color::RESET)?;
            } else {
                write_status(&mut out, entry, context.git)?;
            }
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Writes one `--name-status` style record; tab-separated and quoted like Git under `--git`.
fn write_status(out: &mut impl Write, entry: &Entry, git: bool) -> Result<()> {
    let separator = if git { "\t" } else { " " };
    let path = |bytes: &[u8]| {
        if git {
            quote_path(bytes).to_string()
        } else {
            plain(bytes)
        }
    };
    match entry {
        Entry::Change(change) => {
            writeln!(
                out,
                "{}{separator}{}",
                change.status().letter(),
                path(&change.path)
            )?;
        }
        Entry::Rewrite {
            kind,
            similarity,
            source,
            target,
            ..
        } => {
            writeln!(
                out,
                "{}{similarity:03}{separator}{}{separator}{}",
                kind.letter(),
                path(source),
                path(target)
            )?;
        }
        _ => color_eyre::eyre::bail!("unsupported tree diff entry"),
    }
    Ok(())
}

/// A path with control bytes escaped, for default output.
fn plain(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).escape_debug().to_string()
}

/// Only the colored path buffers patch lines; plain patches retain their original writer.
struct PatchColor<W> {
    out: W,
    line: Vec<u8>,
    in_hunk: bool,
}

impl<W: Write> PatchColor<W> {
    fn new(out: W) -> Self {
        Self {
            out,
            line: Vec::new(),
            in_hunk: false,
        }
    }

    fn write_line(&mut self) -> std::io::Result<()> {
        if self.line.starts_with(b"diff ") {
            self.in_hunk = false;
        }
        let style = if self.line.starts_with(b"@@") {
            self.in_hunk = true;
            "\x1b[36m"
        } else if !self.in_hunk
            && (self.line.starts_with(b"diff ")
                || self.line.starts_with(b"index ")
                || self.line.starts_with(b"--- ")
                || self.line.starts_with(b"+++ ")
                || self.line.starts_with(b"old mode ")
                || self.line.starts_with(b"new ")
                || self.line.starts_with(b"deleted ")
                || self.line.starts_with(b"similarity ")
                || self.line.starts_with(b"rename ")
                || self.line.starts_with(b"copy "))
        {
            "\x1b[1m"
        } else if self.line.starts_with(b"+") {
            crate::color::GREEN
        } else if self.line.starts_with(b"-") {
            crate::color::RED
        } else {
            ""
        };
        self.out.write_all(style.as_bytes())?;
        let has_newline = self.line.ends_with(b"\n");
        let end = self.line.len() - usize::from(has_newline);
        self.out.write_all(&self.line[..end])?;
        if !style.is_empty() {
            self.out.write_all(crate::color::RESET.as_bytes())?;
        }
        if has_newline {
            self.out.write_all(b"\n")?;
        }
        self.line.clear();
        Ok(())
    }
}

impl<W: Write> Write for PatchColor<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            self.line.extend_from_slice(part);
            if part.ends_with(b"\n") {
                self.write_line()?;
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.line.is_empty() {
            self.write_line()?;
        }
        self.out.flush()
    }
}
