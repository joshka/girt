//! `girt object`: hash, read and inspect individual objects.

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::{
    Commit, ListLimits, Object, ObjectId, ObjectKind, ObjectStore, Objects, ReadLimits, Repository,
    Tag, Tree,
};

use crate::common::{resolve, stdout, write_line};
use crate::tree::write_entry;
use crate::{Context, complete};

/// Object commands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the object ID a revision names (like `git rev-parse --verify`).
    Id(RevArgs),
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
    /// List every object, primary store and alternates: `<id> <kind> <size>` per line, sorted
    /// by ID (like `git cat-file --batch-all-objects --batch-check`).
    ///
    /// Kinds and sizes come from stored headers; payloads are not decoded or verified. The
    /// output is the same with `--git`.
    List(ListArgs),
    /// Count loose and packed objects in the primary store (like `git count-objects -v`).
    ///
    /// Prints `key value` lines with sizes in bytes; with `--git`, prints Git's `key: value`
    /// lines with sizes in KiB. Garbage files are not counted, so the `garbage` and
    /// `size-garbage` lines are omitted.
    Count,
}

/// Arguments for `object list`.
#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// List only objects in packs.
    #[arg(long, conflicts_with = "loose")]
    packed: bool,
    /// List only loose objects.
    #[arg(long)]
    loose: bool,
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
    /// Revision, e.g. `main`, `v1^{}`, `HEAD~2` or `HEAD:path`.
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
    rev: String,
}

/// Arguments for `object info`.
#[derive(Debug, clap::Args)]
pub struct InfoArgs {
    /// Revision, e.g. `main`, `v1^{}`, `HEAD~2` or `HEAD:path`.
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
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
        Command::Id(args) => id(args, context),
        Command::Show(args) => show(args, context),
        Command::Info(args) => info(args, context),
        Command::Cat(args) => cat(args, context),
        Command::Exists(args) => exists(args, context),
        Command::Batch(args) => batch(args, context),
        Command::List(args) => list(&args, context),
        Command::Count => count(context),
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
    let objects = repository.objects()?;
    let id = resolve(&repository, rev)?;
    read_id(&objects, id)?.ok_or_else(|| eyre!("missing object {id}"))
}

fn read_id(objects: &Objects, id: ObjectId) -> Result<Option<Object>> {
    objects
        .read_with(id, ReadLimits::trusted())
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

fn id(args: RevArgs, context: &Context) -> Result<ExitCode> {
    let handle =
        girt::Repo::new(context.repository()?).with_environment(context.environment.clone());
    let id = handle.rev_parse(&args.rev)?;
    let mut out = stdout();
    writeln!(out, "{id}")?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn exists(args: RevArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
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
    let objects = repository.objects()?;
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

fn list(args: &ListArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let mut ids = if args.packed {
        objects.packs().flat_map(|pack| pack.ids()).collect()
    } else if args.loose {
        let mut ids = Vec::new();
        for store in objects.stores() {
            ids.extend(loose_ids(&store)?);
        }
        ids
    } else {
        objects
            .object_ids(ListLimits::default())
            .wrap_err("listing objects")?
    };
    ids.sort_unstable();
    ids.dedup();
    let mut headers = objects
        .headers(ids, ReadLimits::trusted())
        .wrap_err("reading object headers")?;
    headers.sort_unstable_by_key(|(id, _)| *id);
    let mut out = io::BufWriter::new(stdout());
    for (id, header) in headers {
        let header = header.ok_or_else(|| eyre!("object {id} vanished while listing"))?;
        writeln!(out, "{id} {} {}", header.kind().as_str(), header.size())?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Loose object IDs in one store.
pub(crate) fn loose_ids(store: &ObjectStore<'_>) -> Result<Vec<ObjectId>> {
    store
        .loose_objects()
        .map(|object| Ok(object?.id()))
        .collect::<Result<_, girt::ObjectReadError>>()
        .wrap_err_with(|| format!("scanning {}", store.directory().display()))
}

fn count(context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let mut stores = objects.stores();
    let primary = stores
        .next()
        .ok_or_else(|| eyre!("object storage has no primary store"))?;
    let (mut loose, mut loose_bytes) = (Vec::new(), 0);
    for object in primary.loose_objects() {
        let object =
            object.wrap_err_with(|| format!("scanning {}", primary.directory().display()))?;
        // Like Git, count disk usage; a file removed since the scan read it is not counted.
        match std::fs::symlink_metadata(object.path()) {
            Ok(metadata) => loose_bytes += disk_usage(&metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("reading {}", object.path().display()));
            }
        }
        loose.push(object.id());
    }
    let packs: Vec<_> = primary.packs().collect();
    let in_pack: usize = packs.iter().map(|pack| pack.object_count()).sum();
    let pack_bytes: u64 = packs
        .iter()
        .map(|pack| pack.pack_bytes() + pack.index_bytes())
        .sum();
    let mut prune_packable = 0;
    for &id in &loose {
        for pack in &packs {
            if pack.contains(id)? {
                prune_packable += 1;
                break;
            }
        }
    }
    let mut out = stdout();
    if context.git {
        writeln!(out, "count: {}", loose.len())?;
        writeln!(out, "size: {}", loose_bytes / 1024)?;
        writeln!(out, "in-pack: {in_pack}")?;
        writeln!(out, "packs: {}", packs.len())?;
        writeln!(out, "size-pack: {}", pack_bytes / 1024)?;
        writeln!(out, "prune-packable: {prune_packable}")?;
        for store in stores {
            writeln!(out, "alternate: {}", store.directory().display())?;
        }
    } else {
        writeln!(out, "loose {}", loose.len())?;
        writeln!(out, "loose-bytes {loose_bytes}")?;
        writeln!(out, "packed {in_pack}")?;
        writeln!(out, "packs {}", packs.len())?;
        writeln!(out, "pack-bytes {pack_bytes}")?;
        writeln!(out, "prune-packable {prune_packable}")?;
        for store in stores {
            writeln!(out, "alternate {}", store.directory().display())?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Allocated size, which Git reports for loose objects; the apparent size elsewhere.
fn disk_usage(metadata: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        std::os::unix::fs::MetadataExt::blocks(metadata) * 512
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}
