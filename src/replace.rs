//! Replace references and grafts: Git's substitution of one object, or one commit's parents, for
//! another.
//!
//! A reference `refs/replace/<original>` pointing at `<replacement>` tells Git to read
//! `<replacement>`'s content wherever `<original>` is looked up. Its typical use is stitching
//! history together without rewriting it: replacing the root of a new history with a copy whose
//! parent is the tip of an imported old one makes `log` and `merge-base` see a single history,
//! while every stored object and ID stays unchanged. Grafts, one `<commit> [<parent>...]` line per
//! commit in `info/grafts`, are the older mechanism; they change only the parents that history
//! traversal sees.
//!
//! # Raw reads stay exact
//!
//! [`Objects`] never substitutes: a read of an ID returns the object stored under that ID. That is
//! what pack writing, verification, retention, transfer and any caller that stores objects by ID
//! (such as jj) need. Substitution is a separate view, [`ReplacedObjects`], which callers create
//! when they want the history Git's `log`, `rev-list`, `merge-base` and `cat-file` show. Because it
//! is a different type, it cannot be handed to an API that expects exact reads.
//!
//! # Load and read
//!
//! [`Settings::from_config`] reads `core.useReplaceRefs`; [`Settings::with_environment`] applies
//! `GIT_NO_REPLACE_OBJECTS` and `GIT_REPLACE_REF_BASE` from an explicit
//! [`Environment`](crate::workflow::Environment). [`Repository::replacements`] loads a
//! [`Replacements`] snapshot and [`ReplacedObjects::new`] combines it with an object reader.
//!
//! ```
//! use girt::refs::{Expected, RefName, Target};
//! use girt::replace::{ReplacedObjects, Settings};
//! use girt::{Commit, InitKind, ObjectFormat, Repository, Signature, Tree};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r.git"),
//!     InitKind::Bare,
//! )?;
//! let loose = repo.loose_objects();
//! let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//! let commit = |parents: Vec<_>, message: &str| {
//!     let mut builder = Commit::builder(tree, person.clone(), message);
//!     for parent in parents {
//!         builder = builder.parent(parent);
//!     }
//!     loose.write_commit(&builder.build()?)
//! };
//!
//! // An imported old history and a new one that starts from scratch.
//! let old = commit(vec![], "old tip\n")?;
//! let new_root = commit(vec![], "new root\n")?;
//! let new_tip = commit(vec![new_root], "new tip\n")?;
//!
//! // Replace the new root with a copy whose parent is the old tip.
//! let stitched = commit(vec![old], "new root\n")?;
//! let name = RefName::new(format!("refs/replace/{new_root}"))?;
//! repo.references()?
//!     .update_without_reflog(&name, Target::Direct(stitched), Expected::Absent)?;
//!
//! let settings = Settings::from_config(repo.config())?;
//! let view = ReplacedObjects::new(repo.objects()?, repo.replacements(&settings)?);
//! assert_eq!(view.walk(&[new_tip])?, [new_tip, new_root, old]);
//!
//! // Raw reads and raw history are unchanged.
//! assert_eq!(view.raw().walk(&[new_tip])?, [new_tip, new_root]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Revision syntax follows the same view when the resolver is built with
//! [`Resolver::with_replacements`](crate::revision::Resolver::with_replacements): `main~2`
//! steps through replaced and grafted parents, and `A...B` excludes the merge bases the view
//! finds, as Git's commands do.
//!
//! # Rules
//!
//! - Replacements chain: if A is replaced by B and B by C, reading A returns C. Four links resolve;
//!   a fifth, a cycle or a self-replacement fails with [`ObjectReadError::ReplaceDepth`], as Git
//!   fails with `replace depth too high`.
//! - The replacement's kind is what reads report, even when it differs from the original's. History
//!   reports [`HistoryError::NotCommit`] when a commit is replaced by another kind.
//! - A replacement that is absent fails with [`ObjectReadError::MissingReplacement`].
//! - Disabling replace references does not disable grafts, as in Git.
//! - A graft on a commit wins over the parents of its replacement. Shallow boundaries win over
//!   both.
//! - While any replacement or graft is active, history ignores the commit-graph, whose parents
//!   describe stored commits.
//! - Displayed IDs are the requested ones: [`ReplacedObjects::walk`] reports `new_root` above, not
//!   `stitched`.
//!
//! Reference names under the base that are not a full object ID, and symbolic replace
//! references, are skipped and listed by [`Replacements::ignored_refs`]; Git warns
//! `bad replace ref name` for the former. Malformed and duplicate graft lines are skipped and
//! listed by [`Replacements::ignored_graft_lines`]; for a duplicate, the first line wins, as in
//! Git.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::InvalidValue;
use crate::refs::{Expected, RefName, ReferenceError, Target};
use crate::{
    AheadBehind, Commit, Config, HistoryError, HistoryLimits, IdMap, Object, ObjectFormat,
    ObjectId, ObjectKind, ObjectReadError, Objects, ReadCommitError, ReadLimits, Repository,
};

/// Links followed before a lookup fails as too deep, matching Git.
const MAX_LINKS: usize = 4;

/// Bytes of `info/grafts` read before loading fails.
const MAX_GRAFT_BYTES: u64 = 64 << 20;

/// Which substitutions [`Repository::replacements`] loads.
///
/// [`Settings::default`] is Git's default: replace references enabled under `refs/replace/`.
/// Grafts are always read; Git has no switch for them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    use_replace_refs: bool,
    base: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            use_replace_refs: true,
            base: "refs/replace/".to_owned(),
        }
    }
}

impl Settings {
    /// Reads `core.useReplaceRefs` (default `true`) from repository configuration.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] when the value is not a Git boolean; Git refuses to run then too.
    pub fn from_config(config: &Config) -> Result<Self, InvalidValue> {
        let enabled = config.boolean("core", None, "useReplaceRefs")?;
        Ok(Self::default().with_replace_refs(enabled.unwrap_or(true)))
    }

    /// Applies `GIT_NO_REPLACE_OBJECTS` and `GIT_REPLACE_REF_BASE` from `environment`.
    ///
    /// `GIT_NO_REPLACE_OBJECTS` disables replace references when set to any value, including an
    /// empty one or `0`, as in Git.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError::RefBase`] for a `GIT_REPLACE_REF_BASE` that is not a reference
    /// prefix ending in `/`. Git misbehaves for such values rather than refusing them.
    pub fn with_environment(
        self,
        environment: &crate::workflow::Environment,
    ) -> Result<Self, SettingsError> {
        let mut settings = self;
        if environment.get("GIT_NO_REPLACE_OBJECTS").is_some() {
            settings.use_replace_refs = false;
        }
        if let Some(base) = environment.get("GIT_REPLACE_REF_BASE") {
            let base = base
                .to_str()
                .ok_or_else(|| SettingsError::RefBase(base.to_string_lossy().into_owned()))?;
            settings = settings.with_ref_base(base)?;
        }
        Ok(settings)
    }

    /// Enables or disables replace references, like `core.useReplaceRefs`.
    ///
    /// Grafts stay active either way.
    #[must_use]
    pub fn with_replace_refs(mut self, enabled: bool) -> Self {
        self.use_replace_refs = enabled;
        self
    }

    /// Reads replace references under `base` instead of `refs/replace/`, like
    /// `GIT_REPLACE_REF_BASE`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError::RefBase`] unless `base` is a valid reference prefix ending in `/`.
    pub fn with_ref_base(mut self, base: &str) -> Result<Self, SettingsError> {
        let invalid = || SettingsError::RefBase(base.to_owned());
        if !base.ends_with('/') || !base.starts_with("refs/") {
            return Err(invalid());
        }
        RefName::new(format!("{base}0")).map_err(|_| invalid())?;
        self.base = base.to_owned();
        Ok(self)
    }

    /// Returns whether replace references are read.
    pub fn uses_replace_refs(&self) -> bool {
        self.use_replace_refs
    }

    /// Returns the reference prefix replace references are read from, ending in `/`.
    pub fn ref_base(&self) -> &[u8] {
        self.base.as_bytes()
    }
}

/// Invalid replace settings.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    /// The replace reference base is not a reference prefix ending in `/`.
    #[error("invalid replace reference base {0:?}")]
    RefBase(String),
}

/// An immutable snapshot of replace references and grafts.
///
/// Load it with [`Repository::replacements`]; it does not observe later reference or graft
/// changes. An empty snapshot makes [`ReplacedObjects`] behave like raw reads.
#[derive(Clone, Debug, Default)]
pub struct Replacements {
    objects: IdMap<ObjectId>,
    grafts: IdMap<Vec<ObjectId>>,
    ignored_refs: Vec<RefName>,
    ignored_graft_lines: Vec<usize>,
}

impl Replacements {
    /// Returns the replacement recorded for `original`, without following chains.
    pub fn get(&self, original: ObjectId) -> Option<ObjectId> {
        self.objects.get(&original).copied()
    }

    /// Returns the grafted parents recorded for `commit`.
    pub fn graft(&self, commit: ObjectId) -> Option<&[ObjectId]> {
        self.grafts.get(&commit).map(Vec::as_slice)
    }

    /// Returns whether no replacement or graft is active.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.grafts.is_empty()
    }

    /// Returns the number of replace references in effect.
    pub fn replaced_count(&self) -> usize {
        self.objects.len()
    }

    /// Returns the number of grafts in effect.
    pub fn graft_count(&self) -> usize {
        self.grafts.len()
    }

    /// Replace references skipped because their name is not a full object ID or their target is
    /// symbolic.
    pub fn ignored_refs(&self) -> &[RefName] {
        &self.ignored_refs
    }

    /// One-based `info/grafts` line numbers skipped as malformed or duplicate.
    pub fn ignored_graft_lines(&self) -> &[usize] {
        &self.ignored_graft_lines
    }

    /// Follows replacement links from `id` to the ID whose stored object is read.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectReadError::ReplaceDepth`] after more than four links, including cycles.
    pub fn resolve(&self, id: ObjectId) -> Result<ObjectId, ObjectReadError> {
        let mut current = id;
        for _ in 0..=MAX_LINKS {
            match self.get(current) {
                None => return Ok(current),
                Some(next) => current = next,
            }
        }
        Err(ObjectReadError::ReplaceDepth(id))
    }

    fn load(repository: &Repository, settings: &Settings) -> Result<Self, LoadError> {
        let format = repository.object_format();
        let mut replacements = Self::default();
        if settings.use_replace_refs {
            let base = settings.base.as_bytes();
            for reference in repository.references()?.list_prefix(base)? {
                let suffix = &reference.name.as_bytes()[base.len()..];
                let original = std::str::from_utf8(suffix)
                    .ok()
                    .and_then(|hex| ObjectId::from_hex(format, hex).ok());
                match (original, reference.target) {
                    (Some(original), Target::Direct(replacement)) => {
                        if replacements.objects.insert(original, replacement).is_some() {
                            return Err(LoadError::Duplicate(reference.name));
                        }
                    }
                    _ => replacements.ignored_refs.push(reference.name),
                }
            }
        }
        let path = repository.common_dir().join("info").join("grafts");
        if let Some(bytes) = read_grafts(&path)? {
            replacements.parse_grafts(format, &bytes);
        }
        Ok(replacements)
    }

    fn parse_grafts(&mut self, format: ObjectFormat, bytes: &[u8]) {
        for (index, line) in bytes.split(|&byte| byte == b'\n').enumerate() {
            // Git accepts trailing spaces, tabs and a CR.
            let line = line.trim_ascii_end();
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            match parse_graft(format, line) {
                Some((commit, parents)) if !self.grafts.contains_key(&commit) => {
                    self.grafts.insert(commit, parents);
                }
                _ => self.ignored_graft_lines.push(index + 1),
            }
        }
    }
}

/// Parses `<commit>( <parent>)*` with single space or tab separators, as Git accepts.
fn parse_graft(format: ObjectFormat, line: &[u8]) -> Option<(ObjectId, Vec<ObjectId>)> {
    let width = format.digest_len() * 2;
    let parse = |hex: &[u8]| {
        std::str::from_utf8(hex)
            .ok()
            .and_then(|hex| ObjectId::from_hex(format, hex).ok())
    };
    let commit = parse(line.get(..width)?)?;
    let mut rest = &line[width..];
    let mut parents = Vec::new();
    while !rest.is_empty() {
        if !matches!(rest[0], b' ' | b'\t') {
            return None;
        }
        parents.push(parse(rest.get(1..=width)?)?);
        rest = &rest[width + 1..];
    }
    Some((commit, parents))
}

fn read_grafts(path: &Path) -> Result<Option<Vec<u8>>, LoadError> {
    use std::io::Read as _;
    let io = |source| LoadError::Grafts {
        path: path.to_owned(),
        source,
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_GRAFT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_GRAFT_BYTES {
        return Err(LoadError::GraftsTooLarge(path.to_owned()));
    }
    Ok(Some(bytes))
}

/// Failure to load [`Replacements`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// Replace references could not be listed.
    #[error("cannot list replace references")]
    References(#[from] ReferenceError),
    /// Two replace references name the same object, such as upper- and lowercase spellings of one
    /// ID. Git refuses with `duplicate replace ref`.
    #[error("duplicate replace reference {}", String::from_utf8_lossy(.0.as_bytes()))]
    Duplicate(RefName),
    /// `info/grafts` exists but cannot be read.
    #[error("cannot read {path}")]
    Grafts {
        /// The grafts file.
        path: PathBuf,
        /// The read failure.
        #[source]
        source: std::io::Error,
    },
    /// `info/grafts` is larger than 64 MiB.
    #[error("grafts file {0} exceeds 64 MiB")]
    GraftsTooLarge(PathBuf),
}

impl Repository {
    /// Loads the replace references and grafts selected by `settings`.
    ///
    /// Replace references are listed under [`Settings::ref_base`] when enabled; grafts are read
    /// from `info/grafts` in the common Git directory. Replacement targets are not read until a
    /// lookup uses them. See the [module documentation](crate::replace) for an example.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] when references cannot be listed, two references replace the same
    /// object or the grafts file cannot be read.
    pub fn replacements(&self, settings: &Settings) -> Result<Replacements, LoadError> {
        Replacements::load(self, settings)
    }
}

/// Whether [`Repository::replace`] and [`Repository::replace_graft`] may overwrite or mix kinds,
/// like `git replace -f`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Force {
    /// Refuse an existing replace reference and, for [`Repository::replace`], a replacement of a
    /// different kind.
    #[default]
    No,
    /// Overwrite an existing replace reference and allow a replacement of another kind.
    Yes,
}

/// Failure to create or delete a replace reference.
///
/// Errors found before writing leave the repository unchanged. Two later failures keep partial
/// effects:
///
/// - [`Repository::replace_graft`] stores the replacement commit before writing the reference. When
///   that write then fails, including [`WriteError::Exists`] because another writer created the
///   reference first, the unreferenced commit is left for garbage collection.
/// - [`Repository::delete_replacement`] can fail with [`WriteError::Reference`] after removing a
///   packed copy of the reference but not its loose file. Read the reference again before retrying.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WriteError {
    /// An object named by the request is absent.
    #[error("object {0} not found")]
    Missing(ObjectId),
    /// The replacement's kind differs from the original's and [`Force::Yes`] was not given.
    #[error(
        "{original} is a {}, but replacement {replacement} is a {}",
        original_kind.as_str(),
        replacement_kind.as_str()
    )]
    KindMismatch {
        /// The object to replace.
        original: ObjectId,
        /// Its kind.
        original_kind: ObjectKind,
        /// The requested replacement.
        replacement: ObjectId,
        /// Its kind.
        replacement_kind: ObjectKind,
    },
    /// The object would replace itself, which makes every read of it fail.
    ///
    /// Git creates such a reference; girt refuses it.
    #[error("{0} cannot replace itself")]
    SelfReplacement(ObjectId),
    /// A replace reference for the object exists and [`Force::Yes`] was not given.
    #[error("replace reference for {0} already exists")]
    Exists(ObjectId),
    /// No replace reference exists for the object being deleted.
    #[error("replace reference for {0} not found")]
    NotFound(ObjectId),
    /// A graft names an object that is not a commit, or grafts a non-commit.
    #[error("{id} is a {}, not a commit", kind.as_str())]
    NotCommit {
        /// The object.
        id: ObjectId,
        /// Its kind.
        kind: ObjectKind,
    },
    /// The grafted commit would equal the original.
    #[error("new commit is the same as the old one: {0}")]
    Unchanged(ObjectId),
    /// The commit carries a `mergetag` header, whose handling under a graft is unsupported.
    #[error("commit {0} has a mergetag header; grafting it is unsupported")]
    MergeTag(ObjectId),
    /// The commit bytes are malformed.
    #[error("malformed commit {id}")]
    Commit {
        /// The commit.
        id: ObjectId,
        /// The parse failure.
        #[source]
        source: crate::CommitError,
    },
    /// Reading objects failed.
    #[error("cannot read objects")]
    Read(#[from] ObjectReadError),
    /// Writing the replacement commit failed.
    #[error("cannot write replacement commit")]
    Write(#[from] crate::loose::Error),
    /// Reading or updating the replace reference failed.
    #[error("cannot update replace reference")]
    Reference(#[from] ReferenceError),
}

/// The outcome of [`Repository::replace_graft`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Grafted {
    /// The stored replacement commit.
    pub replacement: ObjectId,
    /// Whether a `gpgsig` or `gpgsig-sha256` signature was dropped; Git warns when it drops one.
    pub dropped_signature: bool,
}

impl Repository {
    /// Makes `replacement` stand for `original` by writing `refs/replace/<original>`, like
    /// `git replace <original> <replacement>`.
    ///
    /// Both objects must exist. Their kinds must match unless `force` is [`Force::Yes`], which
    /// also overwrites an existing replace reference. Replace references have no reflog, as in
    /// Git. The reference is written under `refs/replace/`, regardless of any
    /// `GIT_REPLACE_REF_BASE` a reader uses.
    ///
    /// ```
    /// use girt::replace::{Force, ReplacedObjects, Settings};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let original = repo.loose_objects().write_blob(b"typo\n")?;
    /// let fixed = repo.loose_objects().write_blob(b"fixed\n")?;
    ///
    /// repo.replace(original, fixed, Force::No)?;
    /// let view = ReplacedObjects::new(repo.objects()?, repo.replacements(&Settings::default())?);
    /// assert_eq!(view.read(original)?.ok_or("absent")?.data(), b"fixed\n");
    ///
    /// assert_eq!(repo.delete_replacement(original)?, fixed);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::Missing`], [`WriteError::SelfReplacement`],
    /// [`WriteError::KindMismatch`], [`WriteError::Exists`], or a read or reference failure.
    pub fn replace(
        &self,
        original: ObjectId,
        replacement: ObjectId,
        force: Force,
    ) -> Result<(), WriteError> {
        if original == replacement {
            return Err(WriteError::SelfReplacement(original));
        }
        let objects = self.objects()?;
        let original_kind = kind(&objects, original)?;
        let replacement_kind = kind(&objects, replacement)?;
        if original_kind != replacement_kind && force == Force::No {
            return Err(WriteError::KindMismatch {
                original,
                original_kind,
                replacement,
                replacement_kind,
            });
        }
        self.write_replace_ref(original, replacement, force)
    }

    /// Replaces `commit` with a copy whose parents are `parents`, like
    /// `git replace --graft <commit> <parent>...`.
    ///
    /// The copy keeps every byte of the original except its `parent` lines and any `gpgsig` or
    /// `gpgsig-sha256` signature, which no longer matches. Duplicate parents are kept. Every
    /// parent must be a commit. The copy is stored as a loose object, then the replace reference
    /// is written as by [`Self::replace`].
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::Missing`] or [`WriteError::NotCommit`] for `commit` or a parent,
    /// [`WriteError::Unchanged`] when the parents already match, [`WriteError::MergeTag`] for a
    /// commit with a `mergetag` header, [`WriteError::Exists`] without [`Force::Yes`], and read,
    /// write or reference failures.
    pub fn replace_graft(
        &self,
        commit: ObjectId,
        parents: &[ObjectId],
        force: Force,
    ) -> Result<Grafted, WriteError> {
        let objects = self.objects()?;
        for &id in std::iter::once(&commit).chain(parents) {
            let kind = kind(&objects, id)?;
            if kind != ObjectKind::Commit {
                return Err(WriteError::NotCommit { id, kind });
            }
        }
        if force == Force::No && self.replace_ref_target(commit)?.is_some() {
            return Err(WriteError::Exists(commit));
        }
        let object = objects
            .read_with(commit, ReadLimits::default())?
            .ok_or(WriteError::Missing(commit))?;
        Commit::parse(self.object_format(), object.data())
            .map_err(|source| WriteError::Commit { id: commit, source })?;
        let (bytes, dropped_signature) = graft_bytes(commit, object.data(), parents)?;
        if self.object_format().hash_object(ObjectKind::Commit, &bytes) == commit {
            return Err(WriteError::Unchanged(commit));
        }
        let replacement =
            self.loose_objects()
                .write_object(self.object_format(), ObjectKind::Commit, &bytes)?;
        self.write_replace_ref(commit, replacement, force)?;
        Ok(Grafted {
            replacement,
            dropped_signature,
        })
    }

    /// Deletes `refs/replace/<original>`, like `git replace -d`, returning its former target.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::NotFound`] when no replace reference exists for `original`, and
    /// reference failures.
    pub fn delete_replacement(&self, original: ObjectId) -> Result<ObjectId, WriteError> {
        let name = replace_ref_name(original);
        let target = self
            .replace_ref_target(original)?
            .ok_or(WriteError::NotFound(original))?;
        self.references()?
            .delete_without_reflog(&name, Expected::Value(Target::Direct(target)))?;
        Ok(target)
    }

    fn replace_ref_target(&self, original: ObjectId) -> Result<Option<ObjectId>, WriteError> {
        Ok(
            match self.references()?.read(&replace_ref_name(original))? {
                Some(Target::Direct(id)) => Some(id),
                Some(Target::Symbolic(_)) | None => None,
            },
        )
    }

    fn write_replace_ref(
        &self,
        original: ObjectId,
        replacement: ObjectId,
        force: Force,
    ) -> Result<(), WriteError> {
        let expected = match force {
            Force::No => Expected::Absent,
            Force::Yes => Expected::Any,
        };
        let result = self.references()?.update_without_reflog(
            &replace_ref_name(original),
            Target::Direct(replacement),
            expected,
        );
        match result {
            Err(ReferenceError::Mismatch { .. }) => Err(WriteError::Exists(original)),
            other => Ok(other?),
        }
    }
}

fn replace_ref_name(original: ObjectId) -> RefName {
    RefName::new(format!("refs/replace/{original}")).expect("hex IDs form valid names")
}

fn kind(objects: &Objects, id: ObjectId) -> Result<ObjectKind, WriteError> {
    Ok(objects
        .header(id, ReadLimits::default())?
        .ok_or(WriteError::Missing(id))?
        .kind())
}

/// Rewrites a commit's graph records, the leading `tree` line and the `parent` lines directly
/// after it, and drops signature headers, keeping every other byte.
///
/// Later lines named `tree` or `parent` are opaque headers or message text and are kept, as Git
/// keeps them.
fn graft_bytes(
    id: ObjectId,
    data: &[u8],
    parents: &[ObjectId],
) -> Result<(Vec<u8>, bool), WriteError> {
    let end = data
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .map_or(data.len(), |position| position + 1);
    let (header, body) = data.split_at(end);
    let mut lines = header.split_inclusive(|&byte| byte == b'\n').peekable();
    let mut out = Vec::with_capacity(data.len());
    // `Commit::parse` succeeded, so the first line is the tree.
    out.extend_from_slice(lines.next().unwrap_or_default());
    while lines.next_if(|line| line.starts_with(b"parent ")).is_some() {}
    for parent in parents {
        out.extend_from_slice(format!("parent {parent}\n").as_bytes());
    }
    let mut dropped_signature = false;
    let mut skipping = false;
    for line in lines {
        if line.starts_with(b" ") {
            if !skipping {
                out.extend_from_slice(line);
            }
            continue;
        }
        skipping = false;
        let name = line
            .split(|&byte| byte == b' ' || byte == b'\n')
            .next()
            .unwrap_or_default();
        match name {
            b"gpgsig" | b"gpgsig-sha256" => {
                dropped_signature = true;
                skipping = true;
            }
            b"mergetag" => return Err(WriteError::MergeTag(id)),
            _ => out.extend_from_slice(line),
        }
    }
    out.extend_from_slice(body);
    Ok((out, dropped_signature))
}

/// An object reader that substitutes replacements and grafts, as Git's history commands do.
///
/// Reads and history queries go through [`Replacements`]; [`Self::raw`] returns the exact
/// reader. Create a new view after replace references, grafts or shallow boundaries change.
/// Methods mirror the [`Objects`] methods of the same name; their limits, ordering and failure
/// contracts apply, plus the substitution failures in the [module rules](crate::replace#rules).
#[derive(Clone, Debug)]
pub struct ReplacedObjects {
    raw: Objects,
    view: Objects,
    replacements: Arc<Replacements>,
}

impl ReplacedObjects {
    /// Combines a reader with a substitution snapshot.
    ///
    /// The commit-graph attached to `objects` is used for history only when `replacements` is
    /// empty.
    pub fn new(objects: Objects, replacements: Replacements) -> Self {
        let replacements = Arc::new(replacements);
        let mut view = objects.clone();
        if !replacements.is_empty() {
            view.commit_graph = None;
            view.replacements = Some(Arc::clone(&replacements));
        }
        // `view` is never handed out: it would substitute behind an `Objects` type.
        Self {
            raw: objects,
            view,
            replacements,
        }
    }

    /// Returns the exact reader, which never substitutes.
    pub fn raw(&self) -> &Objects {
        &self.raw
    }

    /// The substituting reader, for crate APIs that accept a replacement view explicitly.
    pub(crate) fn view(&self) -> &Objects {
        &self.view
    }

    /// Returns the substitution snapshot.
    pub fn replacements(&self) -> &Replacements {
        &self.replacements
    }

    /// Reads the object `id` stands for, following replacement links, with default limits.
    ///
    /// The returned object's [`Object::id`] is the replacement's ID when one applies. Grafts do
    /// not change object content.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Objects::read`], [`ObjectReadError::ReplaceDepth`] and
    /// [`ObjectReadError::MissingReplacement`].
    pub fn read(&self, id: ObjectId) -> Result<Option<Object>, ObjectReadError> {
        self.read_with(id, ReadLimits::default())
    }

    /// Reads like [`Self::read`] under explicit limits.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read`].
    pub fn read_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<Object>, ObjectReadError> {
        self.view.read_with(id, limits)
    }

    /// Reads and parses the commit `id` stands for, following replacement links.
    ///
    /// The parsed commit keeps its stored parents; use [`Self::parents`] for the parents history
    /// follows.
    ///
    /// # Errors
    ///
    /// Returns [`ReadCommitError::Kind`] when the object or its replacement is not a commit, and
    /// the errors of [`Self::read_with`].
    pub fn read_commit_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Commit, ReadCommitError> {
        let object = self
            .read_with(id, limits)?
            .ok_or(ReadCommitError::Missing(id))?;
        if object.kind() != ObjectKind::Commit {
            return Err(ReadCommitError::Kind {
                id,
                kind: object.kind(),
            });
        }
        Ok(Commit::parse(object.object_format(), object.data())?)
    }

    /// Returns the parents history follows from `commit`, read for `id`: its graft if one is
    /// recorded for `id`, otherwise the commit's own parents.
    ///
    /// Shallow boundaries are not applied; check [`Objects::shallow_roots`] on [`Self::raw`].
    pub fn parents<'a>(&'a self, id: ObjectId, commit: &'a Commit) -> &'a [ObjectId] {
        self.replacements.graft(id).unwrap_or(commit.parents())
    }

    /// Collects commits reachable from `roots` through substituted parents.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Objects::walk`]; substitution failures arrive as
    /// [`HistoryError::Read`].
    pub fn walk(&self, roots: &[ObjectId]) -> Result<Vec<ObjectId>, HistoryError> {
        self.view.walk(roots)
    }

    /// Walks like [`Self::walk`] under explicit limits.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::walk`].
    pub fn walk_with(
        &self,
        roots: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.view.walk_with(roots, limits)
    }

    /// Collects commits reachable from `include` but not `exclude`, like `git rev-list`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Objects::walk_range`].
    pub fn walk_range(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.view.walk_range(include, exclude)
    }

    /// Walks a range like [`Self::walk_range`] under explicit limits.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::walk_range`].
    pub fn walk_range_with(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.view.walk_range_with(include, exclude, limits)
    }

    /// Tests whether `ancestor` is reachable from `descendant` through substituted parents.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Objects::is_ancestor`].
    pub fn is_ancestor(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
    ) -> Result<bool, HistoryError> {
        self.view.is_ancestor(ancestor, descendant)
    }

    /// Tests ancestry like [`Self::is_ancestor`] under explicit limits.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::is_ancestor`].
    pub fn is_ancestor_with(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
        limits: HistoryLimits,
    ) -> Result<bool, HistoryError> {
        self.view.is_ancestor_with(ancestor, descendant, limits)
    }

    /// Returns every best common ancestor through substituted parents, like `git merge-base
    /// --all`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Objects::merge_bases`].
    pub fn merge_bases(
        &self,
        left: ObjectId,
        right: ObjectId,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.view.merge_bases(left, right)
    }

    /// Finds merge bases like [`Self::merge_bases`] under explicit limits.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::merge_bases`].
    pub fn merge_bases_with(
        &self,
        left: ObjectId,
        right: ObjectId,
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.view.merge_bases_with(left, right, limits)
    }

    /// Counts commits on each side of a symmetric range through substituted parents.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Objects::ahead_behind`].
    pub fn ahead_behind(
        &self,
        left: ObjectId,
        right: ObjectId,
    ) -> Result<AheadBehind, HistoryError> {
        self.view.ahead_behind(left, right)
    }
}
