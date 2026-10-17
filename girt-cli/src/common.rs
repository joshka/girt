//! Shared state and argument helpers for every command.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::refs::{RefName, References};
use girt::revision::{Range, Resolver, Revision, Selection};
use girt::{Commit, ObjectId, ObjectKind, Objects, PackLimits, ReadLimits, Repository};

/// Maximum symbolic-ref hops followed while resolving a reference.
const MAX_SYMREF_DEPTH: usize = 5;

/// Global options shared by every command.
#[derive(Debug)]
pub struct Context {
    directory: PathBuf,
    /// Print the equivalent Git command's exact output.
    pub git: bool,
    /// Color human output on stdout.
    pub color: bool,
    /// Allow progress when stderr is a terminal.
    pub progress: bool,
}

impl Context {
    /// `-C` is applied by changing the process directory, so every relative path, including
    /// file arguments, resolves the way it does under `git -C`.
    pub fn new(git: bool) -> Self {
        Self {
            directory: PathBuf::from("."),
            git,
            color: false,
            progress: !git,
        }
    }

    /// The directory the command runs in.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Discovers the repository containing the working directory.
    pub fn repository(&self) -> Result<Repository> {
        Repository::discover(&self.directory)
            .wrap_err_with(|| format!("no repository at {}", self.directory.display()))
    }
}

/// A never-set cancellation flag for commands that run to completion.
pub fn not_cancelled() -> &'static AtomicBool {
    static FLAG: AtomicBool = AtomicBool::new(false);
    &FLAG
}

/// Opens the repository's object storage with default limits.
pub fn objects(repository: &Repository) -> Result<Objects> {
    repository
        .objects(PackLimits::default())
        .wrap_err("opening object storage")
}

/// Generous read limits for interactive use: objects up to 1 GiB.
pub fn read_limits() -> ReadLimits {
    ReadLimits::default().with_max_object_bytes(1 << 30)
}

/// Resolves a revision expression such as `main~2`, `v1.0^{tree}` or `HEAD:path`; see
/// [`girt::revision`] for the syntax.
pub fn resolve(repository: &Repository, revision: &str) -> Result<ObjectId> {
    let objects = objects(repository)?;
    resolve_with(repository, &objects, revision, None)
}

/// Resolves a full ref name through symbolic refs; `None` when absent or unborn.
pub fn resolve_ref(references: &References<'_>, name: &RefName) -> Result<Option<ObjectId>> {
    let resolution = references
        .resolve(name, MAX_SYMREF_DEPTH)
        .wrap_err_with(|| format!("resolving {}", display_bytes(name.as_bytes())))?;
    Ok(resolution.id)
}

/// Resolves a revision and peels it to an object of `kind`; commits peel to their tree when
/// `kind` is [`ObjectKind::Tree`].
pub fn resolve_kind(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
    kind: ObjectKind,
) -> Result<ObjectId> {
    resolve_with(repository, objects, revision, Some(kind))
}

fn resolve_with(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
    kind: Option<ObjectKind>,
) -> Result<ObjectId> {
    let parsed: Revision = revision.parse()?;
    let resolver = Resolver::new(repository, objects)?.with_read_limits(read_limits());
    let id = match kind {
        Some(kind) => resolver.resolve_to(&parsed, kind),
        None => resolver.resolve(&parsed),
    };
    id.wrap_err_with(|| format!("resolving `{revision}`"))
}

/// Resolves revision and range arguments (`A..B`, `A...B`, `^A`) to the commits to walk;
/// no arguments means `HEAD`.
pub fn select(repository: &Repository, objects: &Objects, ranges: &[String]) -> Result<Selection> {
    let parsed = if ranges.is_empty() {
        vec![Range::Include(Revision::head())]
    } else {
        ranges
            .iter()
            .map(|range| range.parse())
            .collect::<Result<Vec<Range>, _>>()?
    };
    let resolver = Resolver::new(repository, objects)?.with_read_limits(read_limits());
    Ok(resolver.select(&parsed)?)
}

/// Resolves a revision to a commit.
pub fn resolve_commit(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
) -> Result<ObjectId> {
    resolve_kind(repository, objects, revision, ObjectKind::Commit)
}

/// Resolves a revision to a tree, peeling commits.
pub fn resolve_tree(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
) -> Result<ObjectId> {
    resolve_kind(repository, objects, revision, ObjectKind::Tree)
}

/// Reads and parses a commit.
pub fn read_commit(objects: &Objects, id: ObjectId) -> Result<Commit> {
    let object = objects
        .read(id, read_limits())
        .wrap_err_with(|| format!("reading {id}"))?
        .ok_or_else(|| eyre!("missing object {id}"))?;
    if object.kind() != ObjectKind::Commit {
        bail!("{id} is a {:?}, not a commit", object.kind());
    }
    Commit::parse(objects.object_format(), object.data()).wrap_err_with(|| format!("parsing {id}"))
}

/// Formats bytes for display, replacing invalid UTF-8.
pub fn display_bytes(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// Locked, buffered standard output. Broken pipes end the command quietly in `main`.
pub fn stdout() -> io::BufWriter<io::StdoutLock<'static>> {
    io::BufWriter::new(io::stdout().lock())
}

/// Writes raw bytes followed by a newline.
pub fn write_line(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(bytes)?;
    out.write_all(b"\n")
}

/// Resolves `core.abbrev` once for an output operation; collisions are lengthened per ID.
pub fn abbrev_len(repository: &Repository, objects: &Objects) -> Result<usize> {
    let Some(value) = repository.config().value("core", None, "abbrev") else {
        return Ok(objects.auto_abbrev_len());
    };
    let value = value.ok_or_else(|| eyre!("core.abbrev requires a value"))?;
    if value.eq_ignore_ascii_case(b"auto") {
        return Ok(objects.auto_abbrev_len());
    }
    if [b"false".as_slice(), b"no", b"off", b""]
        .iter()
        .any(|v| value.eq_ignore_ascii_case(v))
    {
        // Git uses its maximum supported hash width for disabled abbreviation; worktree
        // columns retain this width even in SHA-1 repositories.
        return Ok(64);
    }
    let length = repository
        .config()
        .integer("core", None, "abbrev")?
        .unwrap_or(0);
    if length < 4 {
        bail!("invalid core.abbrev: {}", display_bytes(value));
    }
    Ok(usize::try_from(i32::try_from(length)?)?)
}
