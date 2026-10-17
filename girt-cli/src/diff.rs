//! `girt diff`: compare two trees as a change list, renames or a unified patch.

use std::io::Write;
use std::ops::Range;
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _};
use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
use girt::rewrites::{Copies, Kind, Limits, Options};
use girt::{EntryMode, ObjectId, Objects, TreeChange, TreeCompareLimits, TreeValue};

use crate::Context;
use crate::common::{not_cancelled, objects, read_limits, resolve_tree, stdout};

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
    a: String,
    /// The new revision (commit or tree).
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

/// One line of the comparison: a plain change or an inferred rename/copy.
enum Entry {
    Change(TreeChange),
    Rewrite {
        kind: Kind,
        similarity: u8,
        source: Vec<u8>,
        target: Vec<u8>,
        old: TreeValue,
        new: TreeValue,
    },
}

impl Entry {
    /// The path Git sorts output by: the destination for rewrites.
    fn sort_path(&self) -> &[u8] {
        match self {
            Entry::Change(change) => &change.path,
            Entry::Rewrite { target, .. } => target,
        }
    }
}

pub fn run(args: DiffArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = objects(&repository)?;
    let old = resolve_tree(&repository, &objects, &args.a)?;
    let new = resolve_tree(&repository, &objects, &args.b)?;
    let entries = compare(&objects, old, new, args.renames || args.copies, args.copies)?;
    let mut out = stdout();
    let width = crate::common::abbrev_len(&repository, &objects)?;
    if args.patch {
        for entry in &entries {
            write_patch(&mut out, &objects, entry, width)?;
        }
    } else {
        for entry in &entries {
            write_status(&mut out, entry, context.git)?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Compares trees, optionally replacing rename sources/targets with rewrite entries.
fn compare(
    objects: &Objects,
    old: ObjectId,
    new: ObjectId,
    renames: bool,
    copies: bool,
) -> Result<Vec<Entry>> {
    let changes = objects
        .compare_trees(
            Some(old),
            Some(new),
            TreeCompareLimits::default(),
            not_cancelled(),
        )
        .wrap_err("comparing trees")?;
    if !renames {
        return Ok(changes.into_iter().map(Entry::Change).collect());
    }
    let options = Options::default().with_copies(if copies {
        Copies::Modified
    } else {
        Copies::Disabled
    });
    let rewrites = objects
        .detect_rewrites(
            Some(old),
            Some(new),
            options,
            Limits::default(),
            None,
            not_cancelled(),
        )
        .wrap_err("detecting renames")?;
    let find = |path: &[u8]| changes.iter().find(|change| change.path == path);
    let mut renamed_sources = Vec::new();
    let mut targets = Vec::new();
    let mut entries = Vec::new();
    for rewrite in rewrites {
        let (Some(old), Some(new)) = (
            find(&rewrite.source).and_then(|change| change.old),
            find(&rewrite.target).and_then(|change| change.new),
        ) else {
            color_eyre::eyre::bail!("rewrite paths missing from the tree comparison");
        };
        if rewrite.kind == Kind::Rename {
            renamed_sources.push(rewrite.source.clone());
        }
        targets.push(rewrite.target.clone());
        entries.push(Entry::Rewrite {
            kind: rewrite.kind,
            similarity: rewrite.similarity,
            source: rewrite.source,
            target: rewrite.target,
            old,
            new,
        });
    }
    for change in changes {
        let consumed = (change.new.is_none() && renamed_sources.contains(&change.path))
            || (change.old.is_none() && targets.contains(&change.path));
        if !consumed {
            entries.push(Entry::Change(change));
        }
    }
    entries.sort_by(|a, b| a.sort_path().cmp(b.sort_path()));
    Ok(entries)
}

/// Writes one `--name-status` style record; tab-separated and quoted like Git under `--git`.
fn write_status(out: &mut impl Write, entry: &Entry, git: bool) -> Result<()> {
    let separator = if git { "\t" } else { " " };
    let path = |bytes: &[u8]| if git { quote(bytes) } else { plain(bytes) };
    match entry {
        Entry::Change(change) => {
            writeln!(out, "{}{separator}{}", status(change), path(&change.path))?;
        }
        Entry::Rewrite {
            kind,
            similarity,
            source,
            target,
            ..
        } => {
            let letter = if *kind == Kind::Rename { 'R' } else { 'C' };
            writeln!(
                out,
                "{letter}{similarity:03}{separator}{}{separator}{}",
                path(source),
                path(target)
            )?;
        }
    }
    Ok(())
}

fn status(change: &TreeChange) -> char {
    match (change.old, change.new) {
        (None, _) => 'A',
        (_, None) => 'D',
        (Some(old), Some(new)) if kind_of(old.mode) != kind_of(new.mode) => 'T',
        _ => 'M',
    }
}

/// Groups modes that Git treats as the same file type.
fn kind_of(mode: EntryMode) -> u8 {
    match mode {
        EntryMode::Blob | EntryMode::Executable => 0,
        EntryMode::Symlink => 1,
        EntryMode::Gitlink => 2,
        _ => 3,
    }
}

fn octal(mode: EntryMode) -> &'static str {
    match mode {
        EntryMode::Blob => "100644",
        EntryMode::Executable => "100755",
        EntryMode::Symlink => "120000",
        EntryMode::Gitlink => "160000",
        _ => "040000",
    }
}

/// Writes the patch for one entry, splitting type changes into a deletion and an addition.
fn write_patch(out: &mut impl Write, objects: &Objects, entry: &Entry, width: usize) -> Result<()> {
    match entry {
        Entry::Change(change) => match (change.old, change.new) {
            (Some(old), Some(new)) if kind_of(old.mode) != kind_of(new.mode) => {
                let path = &change.path;
                write_file_patch(out, objects, path, path, Some(old), None, None, width)?;
                write_file_patch(out, objects, path, path, None, Some(new), None, width)
            }
            (old, new) => write_file_patch(
                out,
                objects,
                &change.path,
                &change.path,
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
            source,
            target,
            Some(*old),
            Some(*new),
            Some((*kind, *similarity)),
            width,
        ),
    }
}

fn write_file_patch(
    out: &mut impl Write,
    objects: &Objects,
    old_path: &[u8],
    new_path: &[u8],
    old: Option<TreeValue>,
    new: Option<TreeValue>,
    rewrite: Option<(Kind, u8)>,
    width: usize,
) -> Result<()> {
    let a = quote(&[b"a/", old_path].concat());
    let b = quote(&[b"b/", new_path].concat());
    writeln!(out, "diff --git {a} {b}")?;
    match (old, new) {
        (None, Some(new)) => writeln!(out, "new file mode {}", octal(new.mode))?,
        (Some(old), None) => writeln!(out, "deleted file mode {}", octal(old.mode))?,
        (Some(old), Some(new)) if old.mode != new.mode => {
            writeln!(
                out,
                "old mode {}\nnew mode {}",
                octal(old.mode),
                octal(new.mode)
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
        writeln!(out, "{verb} from {}", quote(old_path))?;
        writeln!(out, "{verb} to {}", quote(new_path))?;
    }
    let zero = |value: Option<TreeValue>| value.map(|value| value.id);
    let (old_id, new_id) = (zero(old), zero(new));
    if old_id == new_id {
        return Ok(());
    }
    let mode = match (old, new) {
        (Some(old), Some(new)) if old.mode == new.mode => format!(" {}", octal(new.mode)),
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
        not_cancelled(),
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
        .read(value.id, read_limits())
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

/// Quotes a path the way Git does with `core.quotePath=true`: wrapped in double quotes with C
/// escapes and octal non-ASCII bytes when it contains any byte needing escapes.
fn quote(bytes: &[u8]) -> String {
    let needs = |byte: u8| !(0x20..0x7f).contains(&byte) || byte == b'"' || byte == b'\\';
    if !bytes.iter().any(|byte| needs(*byte)) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut quoted = String::from("\"");
    for &byte in bytes {
        match byte {
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            b'\x07' => quoted.push_str("\\a"),
            b'\x08' => quoted.push_str("\\b"),
            b'\t' => quoted.push_str("\\t"),
            b'\n' => quoted.push_str("\\n"),
            b'\x0b' => quoted.push_str("\\v"),
            b'\x0c' => quoted.push_str("\\f"),
            b'\r' => quoted.push_str("\\r"),
            byte if needs(byte) => quoted.push_str(&format!("\\{byte:03o}")),
            byte => quoted.push(char::from(byte)),
        }
    }
    quoted.push('"');
    quoted
}
