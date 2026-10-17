//! `girt diff`: compare two trees as a change list, renames or a unified patch.

use std::io::Write;
use std::ops::Range;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _};
use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
use girt::display::quote_path;
use girt::rewrites::{Copies, Kind, Options};
use girt::tree_diff::Entry;
use girt::{ChangeStatus, EntryMode, ObjectId, Objects, ReadLimits, TreeChange, TreeValue};

use crate::common::{resolve_tree, stdout};
use crate::{Context, complete};

/// Lines of unchanged context around each patch hunk.
const CONTEXT: usize = 3;

/// Compare two trees.
///
/// Both revisions are peeled to trees. The default output is one change per line: a status
/// letter (`A`dded, `D`eleted, `M`odified, `T`ype changed) and the path.
///
/// With `--git`, output mirrors `git diff-tree -r --name-status <a> <b>` (add `-M` / `-C` for
/// `--renames` / `--copies`), and `--patch` mirrors `git diff --no-renames <a> <b>` (or
/// `git diff -M <a> <b>` with `--renames`). Known patch differences: girt's line diff is plain
/// Myers without Git's indent heuristic, so ambiguous hunks may align differently; binary detection
/// scans whole blobs for NUL rather than the first 8000 bytes; attributes, textconv and funcname
/// drivers are ignored.
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
        let options = Options::default().with_copies(if args.copies {
            Copies::Modified
        } else {
            Copies::Disabled
        });
        let comparison = objects
            .diff_with_renames_with(
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
        for entry in &entries {
            if context.color {
                let mut colored = PatchColor::new(&mut out);
                write_patch(&mut colored, &objects, entry, width)?;
                colored.flush()?;
            } else {
                write_patch(&mut out, &objects, entry, width)?;
            }
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

/// Writes the patch for one entry, splitting type changes into a deletion and an addition.
fn write_patch(out: &mut impl Write, objects: &Objects, entry: &Entry, width: usize) -> Result<()> {
    match entry {
        Entry::Change(change) => match (change.old, change.new) {
            (Some(old), Some(new)) if change.status() == ChangeStatus::TypeChanged => {
                let path = &change.path;
                write_file_patch(out, objects, (path, path), Some(old), None, None, width)?;
                write_file_patch(out, objects, (path, path), None, Some(new), None, width)
            }
            (old, new) => write_file_patch(
                out,
                objects,
                (&change.path, &change.path),
                old,
                new,
                None,
                width,
            ),
        },
        Entry::Rewrite {
            kind,
            similarity,
            source,
            target,
            old,
            new,
        } => write_file_patch(
            out,
            objects,
            (source, target),
            Some(*old),
            Some(*new),
            Some((*kind, *similarity)),
            width,
        ),
        _ => color_eyre::eyre::bail!("unsupported tree diff entry"),
    }
}

fn write_file_patch(
    out: &mut impl Write,
    objects: &Objects,
    (old_path, new_path): (&[u8], &[u8]),
    old: Option<TreeValue>,
    new: Option<TreeValue>,
    rewrite: Option<(Kind, u8)>,
    width: usize,
) -> Result<()> {
    let a = quote_path(&[b"a/", old_path].concat()).to_string();
    let b = quote_path(&[b"b/", new_path].concat()).to_string();
    writeln!(out, "diff --git {a} {b}")?;
    match (old, new) {
        (None, Some(new)) => writeln!(out, "new file mode {}", new.mode.octal())?,
        (Some(old), None) => writeln!(out, "deleted file mode {}", old.mode.octal())?,
        (Some(old), Some(new)) if old.mode != new.mode => {
            writeln!(
                out,
                "old mode {}\nnew mode {}",
                old.mode.octal(),
                new.mode.octal()
            )?;
        }
        _ => {}
    }
    if let Some((kind, similarity)) = rewrite {
        let verb = if kind == Kind::Rename {
            "rename"
        } else {
            "copy"
        };
        writeln!(out, "similarity index {similarity}%")?;
        writeln!(out, "{verb} from {}", quote_path(old_path))?;
        writeln!(out, "{verb} to {}", quote_path(new_path))?;
    }
    let zero = |value: Option<TreeValue>| value.map(|value| value.id);
    let (old_id, new_id) = (zero(old), zero(new));
    if old_id == new_id {
        return Ok(());
    }
    let mode = match (old, new) {
        (Some(old), Some(new)) if old.mode == new.mode => format!(" {}", new.mode.octal()),
        _ => String::new(),
    };
    let abbrev = |id: Option<ObjectId>| match id {
        Some(id) => objects.abbreviate(id, width).map(|p| p.to_string()),
        None => Ok("0".repeat(width.min(objects.object_format().digest_len() * 2))),
    };
    writeln!(out, "index {}..{}{mode}", abbrev(old_id)?, abbrev(new_id)?)?;

    let (old_bytes, new_bytes) = read_sides(objects, old, new)?;
    let a = if old.is_some() {
        a.as_str()
    } else {
        "/dev/null"
    };
    let b = if new.is_some() {
        b.as_str()
    } else {
        "/dev/null"
    };
    let limits = DiffLimits::default();
    match diff(
        &old_bytes,
        &new_bytes,
        BinaryMode::Auto,
        limits,
        &AtomicBool::new(false),
    )
    .wrap_err("diffing contents")?
    {
        ContentDiff::Unchanged => {}
        ContentDiff::Binary { .. } => writeln!(out, "Binary files {a} and {b} differ")?,
        ContentDiff::Text(text) => {
            writeln!(out, "--- {a}\n+++ {b}")?;
            write_hunks(out, text.old_bytes(), text.new_bytes(), text.edits())?;
        }
        _ => color_eyre::eyre::bail!("girt does not support this content diff kind yet"),
    }
    Ok(())
}

/// Reads both sides' bytes; gitlinks render as `Subproject commit <id>` lines like Git.
fn read_sides(
    objects: &Objects,
    old: Option<TreeValue>,
    new: Option<TreeValue>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    Ok((read_side(objects, old)?, read_side(objects, new)?))
}

fn read_side(objects: &Objects, value: Option<TreeValue>) -> Result<Vec<u8>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.mode == EntryMode::Gitlink {
        return Ok(format!("Subproject commit {}\n", value.id).into_bytes());
    }
    let object = objects
        .read_with(value.id, ReadLimits::trusted())
        .wrap_err_with(|| format!("reading {}", value.id))?
        .ok_or_else(|| color_eyre::eyre::eyre!("missing object {}", value.id))?;
    Ok(object.into_data())
}

/// Byte ranges of each line, including its LF.
fn lines(bytes: &[u8]) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(start..index + 1);
            start = index + 1;
        }
    }
    if start < bytes.len() {
        lines.push(start..bytes.len());
    }
    lines
}

/// Renders edits as unified hunks, merging edits whose gap fits within twice the context.
fn write_hunks(
    out: &mut impl Write,
    old: &[u8],
    new: &[u8],
    edits: &[girt::content_diff::ContentEdit],
) -> Result<()> {
    let (old_lines, new_lines) = (lines(old), lines(new));
    let mut index = 0;
    while index < edits.len() {
        let mut end = index;
        while end + 1 < edits.len()
            && edits[end + 1].old_lines.start - edits[end].old_lines.end <= 2 * CONTEXT
        {
            end += 1;
        }
        let (first, last) = (&edits[index], &edits[end]);
        let old_start = first.old_lines.start.saturating_sub(CONTEXT);
        let new_start = first.new_lines.start - (first.old_lines.start - old_start);
        let old_end = (last.old_lines.end + CONTEXT).min(old_lines.len());
        let new_end = last.new_lines.end + (old_end - last.old_lines.end);
        write!(
            out,
            "@@ -{} +{} @@",
            range(old_start, old_end - old_start),
            range(new_start, new_end - new_start)
        )?;
        if let Some(name) = function_name(old, &old_lines[..old_start]) {
            out.write_all(b" ")?;
            out.write_all(name)?;
        }
        out.write_all(b"\n")?;
        let mut position = old_start;
        for edit in &edits[index..=end] {
            for line in &old_lines[position..edit.old_lines.start] {
                write_line(out, b' ', &old[line.clone()])?;
            }
            for line in &old_lines[edit.old_lines.clone()] {
                write_line(out, b'-', &old[line.clone()])?;
            }
            for line in &new_lines[edit.new_lines.clone()] {
                write_line(out, b'+', &new[line.clone()])?;
            }
            position = edit.old_lines.end;
        }
        for line in &old_lines[position..old_end] {
            write_line(out, b' ', &old[line.clone()])?;
        }
        index = end + 1;
    }
    Ok(())
}

/// Formats a hunk range as Git does: `start,count`, omitting `,1`, with start 0-based if empty.
fn range(start: usize, count: usize) -> String {
    match count {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{count}", start + 1),
    }
}

/// Git's default hunk-header function context: the nearest preceding line starting with a
/// letter, `_` or `$`, trailing whitespace removed and truncated to 80 bytes.
fn function_name<'a>(bytes: &'a [u8], before: &[Range<usize>]) -> Option<&'a [u8]> {
    before.iter().rev().find_map(|line| {
        let text = &bytes[line.clone()];
        let first = *text.first()?;
        if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
            return None;
        }
        let text = &text[..text.len().min(80)];
        let trimmed = text.trim_ascii_end();
        Some(trimmed)
    })
}

fn write_line(out: &mut impl Write, prefix: u8, line: &[u8]) -> Result<()> {
    out.write_all(&[prefix])?;
    out.write_all(line)?;
    if !line.ends_with(b"\n") {
        out.write_all(b"\n\\ No newline at end of file\n")?;
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
