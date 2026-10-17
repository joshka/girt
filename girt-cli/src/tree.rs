//! `girt tree`: inspect tree objects.

use std::io::{self, Write};
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::{EntryMode, ObjectId, ObjectKind, Objects, Tree, TreeEntry};

use crate::Context;
use crate::common::{objects, read_limits, resolve_tree, stdout, write_line};

/// Tree commands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// List a tree's entries as `<mode> <kind> <id>\t<path>` (like `git ls-tree`).
    ///
    /// Commits and tags peel to their tree. Default output writes paths as raw bytes; `--git`
    /// quotes unusual paths the way Git does with the default `core.quotePath`.
    List(ListArgs),
}

/// Arguments for `tree list`.
#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Tree, commit or tag ID or ref name.
    rev: String,
    /// Recurse into subtrees, listing only non-tree entries with full paths.
    #[arg(long, short)]
    recursive: bool,
    /// Print only paths.
    #[arg(long)]
    name_only: bool,
}

/// Runs a tree command.
pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    match command {
        Command::List(args) => list(args, context),
    }
}

fn list(args: ListArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = objects(&repository)?;
    let id = resolve_tree(&repository, &objects, &args.rev)?;
    let mut out = stdout();
    walk(&objects, id, &mut Vec::new(), &args, context.git, &mut out)?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn walk(
    objects: &Objects,
    id: ObjectId,
    prefix: &mut Vec<u8>,
    args: &ListArgs,
    git: bool,
    out: &mut impl Write,
) -> Result<()> {
    let tree = read_tree(objects, id)?;
    for entry in tree.entries() {
        let length = prefix.len();
        if length > 0 {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(&entry.name);
        if args.recursive && entry.mode == EntryMode::Tree {
            walk(objects, entry.id, prefix, args, git, out)?;
        } else if args.name_only {
            write_line(out, &path_bytes(prefix, git))?;
        } else {
            write_entry(out, entry, prefix, git)?;
        }
        prefix.truncate(length);
    }
    Ok(())
}

fn read_tree(objects: &Objects, id: ObjectId) -> Result<Tree> {
    let object = objects
        .read(id, read_limits())
        .wrap_err_with(|| format!("reading {id}"))?
        .ok_or_else(|| eyre!("missing tree {id}"))?;
    if object.kind() != ObjectKind::Tree {
        bail!("{id} is a {}, not a tree", object.kind().as_str());
    }
    Tree::parse(object.object_format(), object.data()).wrap_err_with(|| format!("parsing {id}"))
}

/// Writes one `<mode> <kind> <id>\t<path>` line, quoting the path like Git when `git` is set.
pub fn write_entry(
    out: &mut impl Write,
    entry: &TreeEntry,
    path: &[u8],
    git: bool,
) -> io::Result<()> {
    let (mode, kind) = match entry.mode {
        EntryMode::Blob => ("100644", "blob"),
        EntryMode::Executable => ("100755", "blob"),
        EntryMode::Symlink => ("120000", "blob"),
        EntryMode::Tree => ("040000", "tree"),
        EntryMode::Gitlink => ("160000", "commit"),
        _ => ("000000", "unknown"),
    };
    write!(out, "{mode} {kind} {}\t", entry.id)?;
    write_line(out, &path_bytes(path, git))
}

/// Returns the path as Git prints it with `core.quotePath=true`, or unchanged.
fn path_bytes(path: &[u8], git: bool) -> std::borrow::Cow<'_, [u8]> {
    let needs_quote = |b: u8| !(0x20..0x7f).contains(&b) || b == b'"' || b == b'\\';
    if !git || !path.iter().copied().any(needs_quote) {
        return path.into();
    }
    let mut quoted = vec![b'"'];
    for &byte in path {
        match byte {
            b'"' => quoted.extend_from_slice(b"\\\""),
            b'\\' => quoted.extend_from_slice(b"\\\\"),
            b'\x07' => quoted.extend_from_slice(b"\\a"),
            b'\x08' => quoted.extend_from_slice(b"\\b"),
            b'\t' => quoted.extend_from_slice(b"\\t"),
            b'\n' => quoted.extend_from_slice(b"\\n"),
            b'\x0b' => quoted.extend_from_slice(b"\\v"),
            b'\x0c' => quoted.extend_from_slice(b"\\f"),
            b'\r' => quoted.extend_from_slice(b"\\r"),
            byte if needs_quote(byte) => {
                quoted.extend_from_slice(format!("\\{byte:03o}").as_bytes())
            }
            byte => quoted.push(byte),
        }
    }
    quoted.push(b'"');
    quoted.into()
}
