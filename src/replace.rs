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
use crate::refs::{RefName, ReferenceError, Target};
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
