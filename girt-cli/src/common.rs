//! Shared state and argument helpers for every command.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::refs::{RefName, References};
use girt::{Commit, ObjectId, ObjectKind, Objects, PackLimits, PeelLimits, ReadLimits, Repository};

/// Maximum symbolic-ref hops followed while resolving a revision.
const MAX_SYMREF_DEPTH: usize = 5;

/// Global options shared by every command.
#[derive(Debug)]
pub struct Context {
    directory: PathBuf,
    /// Print the equivalent Git command's exact output.
    pub git: bool,
}

impl Context {
    /// `-C` is applied by changing the process directory, so every relative path, including
    /// file arguments, resolves the way it does under `git -C`.
    pub fn new(git: bool) -> Self {
        Self {
            directory: PathBuf::from("."),
            git,
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

/// Resolves a revision: a full object ID, `HEAD`, a full ref name, or a short ref name expanded
/// the way Git does (`refs/<name>`, then tags, branches, remotes and `refs/remotes/<name>/HEAD`).
pub fn resolve(repository: &Repository, revision: &str) -> Result<ObjectId> {
    if let Ok(id) = revision.parse::<ObjectId>() {
        return Ok(id);
    }
    let references = repository.references().wrap_err("opening references")?;
    for candidate in candidates(revision) {
        let Ok(name) = RefName::new(&candidate) else {
            continue;
        };
        if let Some(id) = resolve_ref(&references, &name)? {
            return Ok(id);
        }
    }
    bail!("unknown revision `{revision}`")
}

/// Resolves a full ref name through symbolic refs; `None` when absent or unborn.
pub fn resolve_ref(references: &References<'_>, name: &RefName) -> Result<Option<ObjectId>> {
    let resolution = references
        .resolve(name, MAX_SYMREF_DEPTH)
        .wrap_err_with(|| format!("resolving {}", display_bytes(name.as_bytes())))?;
    Ok(resolution.id)
}

/// Full ref names tried for a short name, in Git's order.
pub fn candidates(name: &str) -> Vec<String> {
    if name == "HEAD" || name.starts_with("refs/") {
        return vec![name.to_owned()];
    }
    vec![
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ]
}

/// Resolves a revision and peels tags until reaching an object of `kind`; commits peel to their
/// tree when `kind` is [`ObjectKind::Tree`].
pub fn resolve_kind(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
    kind: ObjectKind,
) -> Result<ObjectId> {
    let id = resolve(repository, revision)?;
    let peeled = objects
        .peel(id, PeelLimits::default(), not_cancelled())
        .wrap_err_with(|| format!("peeling {revision}"))?;
    match (peeled.kind, kind) {
        (found, wanted) if found == wanted => Ok(peeled.target),
        (ObjectKind::Commit, ObjectKind::Tree) => Ok(read_commit(objects, peeled.target)?.tree()),
        (found, wanted) => bail!("{revision} is a {found:?}, not a {wanted:?}"),
    }
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
