//! `girt object`: hash, read and inspect individual objects.

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::{Commit, Object, ObjectId, ObjectKind, Objects, Repository, Tag, Tree};

use crate::Context;
use crate::common::{objects, read_limits, resolve, stdout, write_line};
use crate::tree::write_entry;

/// Object commands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Compute an object ID for file contents (like `git hash-object`).
    Hash(HashArgs),
    /// Pretty-print an object: trees one entry per line, other kinds as stored (like
    /// `git cat-file -p`).
    Show(RevArgs),
    /// Print `<id> <kind> <size>` for an object (like `git cat-file -t` / `-s`).
    Info(InfoArgs),
    /// Write an object's raw payload bytes to stdout.
    Cat(RevArgs),
    /// Exit 0 if the object exists and 1 otherwise, printing nothing (like `git cat-file -e`).
    Exists(RevArgs),
    /// Read revisions from stdin, one per line, and print each object (like
    /// `git cat-file --batch`).
    ///
    /// Prints `<id> <kind> <size>`, then the payload and a newline; unknown input prints
    /// `<input> missing`.
    Batch(BatchArgs),
}

/// Arguments for `object hash`.
#[derive(Debug, clap::Args)]
pub struct HashArgs {
    /// File whose contents are hashed.
    #[arg(required_unless_present = "stdin", conflicts_with = "stdin")]
    file: Option<PathBuf>,
    /// Object kind to hash the contents as; trees, commits and tags must parse.
    #[arg(long, short = 't', default_value = "blob", value_parser = parse_kind)]
    kind: ObjectKind,
    /// Also write the object into the repository's loose object store.
    #[arg(long, short)]
    write: bool,
    /// Read the contents from stdin instead of a file.
    #[arg(long)]
    stdin: bool,
}

/// A single revision argument.
#[derive(Debug, clap::Args)]
pub struct RevArgs {
    /// Object ID or ref name.
    rev: String,
}

/// Arguments for `object info`.
#[derive(Debug, clap::Args)]
pub struct InfoArgs {
    /// Object ID or ref name.
    rev: String,
    /// Print only the kind (`git cat-file -t`).
    #[arg(long, conflicts_with = "size")]
    kind: bool,
    /// Print only the payload size in bytes (`git cat-file -s`).
    #[arg(long)]
    size: bool,
}

/// Arguments for `object batch`.
#[derive(Debug, clap::Args)]
pub struct BatchArgs {
    /// Print only `<id> <kind> <size>` lines, without payloads (`git cat-file --batch-check`).
    #[arg(long)]
    check: bool,
}

/// Runs an object command.
pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    match command {
        Command::Hash(args) => hash(args, context),
        Command::Show(args) => show(args, context),
        Command::Info(args) => info(args, context),
        Command::Cat(args) => cat(args, context),
        Command::Exists(args) => exists(args, context),
        Command::Batch(args) => batch(args, context),
    }
}

fn parse_kind(value: &str) -> Result<ObjectKind, String> {
    match value {
        "blob" => Ok(ObjectKind::Blob),
        "tree" => Ok(ObjectKind::Tree),
        "commit" => Ok(ObjectKind::Commit),
        "tag" => Ok(ObjectKind::Tag),
        _ => Err(format!("unknown object kind `{value}`")),
    }
}

fn hash(args: HashArgs, context: &Context) -> Result<ExitCode> {
    let bytes = match &args.file {
        Some(path) => {
            std::fs::read(path).wrap_err_with(|| format!("reading {}", path.display()))?
        }
        None => {
            let mut bytes = Vec::new();
            io::stdin()
                .read_to_end(&mut bytes)
                .wrap_err("reading stdin")?;
            bytes
        }
    };
    let repository = context.repository().ok();
    let format = repository
        .as_ref()
        .map(Repository::object_format)
        .unwrap_or(girt::ObjectFormat::Sha1);
    if args.write && repository.is_none() {
        bail!("--write needs a repository");
    }
    let id = match args.kind {
        ObjectKind::Blob => {
            if let Some(repository) = repository.as_ref().filter(|_| args.write) {
                repository.loose_objects().write_blob(&bytes)?
            } else {
                ObjectId::for_blob(format, &bytes)
            }
        }
        ObjectKind::Tree => {
            let tree = Tree::parse(format, &bytes).wrap_err("parsing tree")?;
            tree.validate().wrap_err("validating tree")?;
            match repository.as_ref().filter(|_| args.write) {
                Some(repository) => repository.loose_objects().write_tree(&tree)?,
                None => tree.id(),
            }
        }
        ObjectKind::Commit => {
            let commit = Commit::parse(format, &bytes).wrap_err("parsing commit")?;
            match repository.as_ref().filter(|_| args.write) {
                Some(repository) => repository.loose_objects().write_commit(&commit)?,
                None => commit.id(),
            }
        }
        ObjectKind::Tag => {
            let tag = Tag::parse(format, &bytes).wrap_err("parsing tag")?;
            match repository.as_ref().filter(|_| args.write) {
                Some(repository) => repository.loose_objects().write_tag(&tag)?,
                None => tag.id(),
            }
        }
    };
    let mut out = stdout();
    writeln!(out, "{id}")?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Opens storage and reads the object a revision names.
fn read(context: &Context, rev: &str) -> Result<Object> {
    let repository = context.repository()?;
    let objects = objects(&repository)?;
    let id = resolve(&repository, rev)?;
    read_id(&objects, id)?.ok_or_else(|| eyre!("missing object {id}"))
}

fn read_id(objects: &Objects, id: ObjectId) -> Result<Option<Object>> {
    objects
        .read(id, read_limits())
        .wrap_err_with(|| format!("reading {id}"))
}

fn show(args: RevArgs, context: &Context) -> Result<ExitCode> {
    let object = read(context, &args.rev)?;
    let mut out = stdout();
    if object.kind() == ObjectKind::Tree {
        let tree = Tree::parse(object.object_format(), object.data()).wrap_err("parsing tree")?;
        for entry in tree.entries() {
            write_entry(&mut out, entry, &entry.name, context.git)?;
        }
    } else {
        out.write_all(object.data())?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn info(args: InfoArgs, context: &Context) -> Result<ExitCode> {
    let object = read(context, &args.rev)?;
    let mut out = stdout();
    let kind = object.kind().as_str();
    let size = object.data().len();
    if args.kind {
        writeln!(out, "{kind}")?;
    } else if args.size {
        writeln!(out, "{size}")?;
    } else {
        writeln!(out, "{} {kind} {size}", object.id())?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn cat(args: RevArgs, context: &Context) -> Result<ExitCode> {
    let object = read(context, &args.rev)?;
    let mut out = stdout();
    out.write_all(object.data())?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn exists(args: RevArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = objects(&repository)?;
    let Ok(id) = resolve(&repository, &args.rev) else {
        return Ok(ExitCode::FAILURE);
    };
    Ok(match read_id(&objects, id)? {
        Some(_) => ExitCode::SUCCESS,
        None => ExitCode::FAILURE,
    })
}

fn batch(args: BatchArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = objects(&repository)?;
    let mut input = io::stdin().lock();
    let mut out = stdout();
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line).wrap_err("reading stdin")? == 0 {
            break;
        }
        let rev = line.trim_end_matches(['\n', '\r']);
        let object = match resolve(&repository, rev) {
            Ok(id) => read_id(&objects, id)?,
            Err(_) => None,
        };
        let Some(object) = object else {
            writeln!(out, "{rev} missing")?;
            continue;
        };
        writeln!(
            out,
            "{} {} {}",
            object.id(),
            object.kind().as_str(),
            object.data().len()
        )?;
        if !args.check {
            write_line(&mut out, object.data())?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}
