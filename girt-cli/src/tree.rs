//! `girt tree`: inspect tree objects.

use std::io::{self, Write};
use std::process::ExitCode;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _};
use girt::{EntryMode, ObjectId, Objects, ReadLimits, Tree, TreeEntry};

use crate::common::{resolve_tree, stdout, write_path};
use crate::{Context, complete};

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
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
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
    let objects = repository.objects()?;
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
            write_path(out, prefix, git)?;
        } else {
            write_entry(out, entry, prefix, git)?;
        }
        prefix.truncate(length);
    }
    Ok(())
}

fn read_tree(objects: &Objects, id: ObjectId) -> Result<Tree> {
    objects
        .read_tree_with(id, ReadLimits::trusted())
        .wrap_err_with(|| format!("reading tree {id}"))
}

/// Writes one `<mode> <kind> <id>\t<path>` line, quoting the path like Git when `git` is set.
pub fn write_entry(
    out: &mut impl Write,
    entry: &TreeEntry,
    path: &[u8],
    git: bool,
) -> io::Result<()> {
    let (mode, kind) = (entry.mode.octal(), entry.mode.object_kind().as_str());
    write!(out, "{mode} {kind} {}\t", entry.id)?;
    write_path(out, path, git)
}
