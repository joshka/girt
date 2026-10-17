//! Resolution of parsed revisions against one repository.

use std::sync::atomic::AtomicBool;

use super::{Base, Peel, Range, Revision, RevisionError, Step};
use crate::refs::{RefName, References, Target};
use crate::{
    Commit, HistoryLimits, Object, ObjectId, ObjectIdPrefix, ObjectKind, Objects, PeelLimits,
    ReadLimits, Repository, ResolvePrefixError, Tree,
};

/// Resolves [`Revision`]s and [`Range`]s in one repository.
///
/// A resolver borrows the repository and an open [`Objects`] store, and opens the reference
/// store once. Each call reads current reference and reflog state; nothing is cached between
/// calls, so a resolver sees references updated by other processes.
///
/// See the [module documentation](super) for the supported syntax and an example.
pub struct Resolver<'r> {
    pub(super) repository: &'r Repository,
    objects: &'r Objects,
    pub(super) references: References<'r>,
    read_limits: ReadLimits,
}

impl std::fmt::Debug for Resolver<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("repository", self.repository)
            .finish_non_exhaustive()
    }
}

/// The commits a set of [`Range`]s selects: those reachable from any of `include` and from none
/// of `exclude`.
///
/// Every ID is a commit. Pass both lists to a history walk; a symmetric range `A...B` contributes
/// both sides to `include` and their merge bases to `exclude`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Selection {
    /// Commits whose ancestry is selected, in argument order.
    pub include: Vec<ObjectId>,
    /// Commits whose ancestry is removed from the selection, in argument order.
    pub exclude: Vec<ObjectId>,
}

impl<'r> Resolver<'r> {
    /// Creates a resolver for `repository` reading objects from `objects`.
    ///
    /// # Errors
    ///
    /// Reports a failure to open the reference store.
    pub fn new(repository: &'r Repository, objects: &'r Objects) -> Result<Self, RevisionError> {
        Ok(Self {
            repository,
            objects,
            references: repository.references()?,
            read_limits: ReadLimits::default().with_max_object_bytes(1 << 30),
        })
    }

    /// Sets the read limits used for commits, trees and tags; the default allows 1 GiB objects.
    pub fn with_read_limits(mut self, limits: ReadLimits) -> Self {
        self.read_limits = limits;
        self
    }

    /// Resolves a revision to an object ID.
    ///
    /// The result is whatever kind the expression names: a bare reference to an annotated tag
    /// yields the tag. Use [`Self::resolve_to`] when a command needs a particular kind.
    ///
    /// # Errors
    ///
    /// Reports unknown names, missing parents or paths, unreadable storage, and syntax girt
    /// does not support yet, as [`RevisionError`] variants.
    pub fn resolve(&self, revision: &Revision) -> Result<ObjectId, RevisionError> {
        let mut id = self.resolve_base(revision.base(), wanted_kind(revision))?;
        for step in revision.steps() {
            id = self.step(id, *step)?;
        }
        match revision.path() {
            Some(path) => self.tree_path(self.peel(id, ObjectKind::Tree)?, path),
            None => Ok(id),
        }
    }

    /// Resolves a revision and peels it to `kind`, as `rev^{kind}` does.
    ///
    /// Annotated tags are followed; a commit peels to its tree when `kind` is
    /// [`ObjectKind::Tree`].
    ///
    /// # Errors
    ///
    /// Reports [`RevisionError::WrongKind`] when the object cannot become `kind`, and the
    /// failures of [`Self::resolve`].
    pub fn resolve_to(
        &self,
        revision: &Revision,
        kind: ObjectKind,
    ) -> Result<ObjectId, RevisionError> {
        self.peel(self.resolve(revision)?, kind)
    }

    /// Resolves ranges to the commits a history command should walk.
    ///
    /// Every revision is peeled to a commit. `A..B` includes `B` and excludes `A`; `A...B`
    /// includes both and excludes all their merge bases.
    ///
    /// # Errors
    ///
    /// Reports the failures of [`Self::resolve_to`] and history-walk failures while finding
    /// merge bases.
    pub fn select(&self, ranges: &[Range]) -> Result<Selection, RevisionError> {
        let mut selection = Selection::default();
        let commit = |revision| self.resolve_to(revision, ObjectKind::Commit);
        for range in ranges {
            match range {
                Range::Include(revision) => selection.include.push(commit(revision)?),
                Range::Exclude(revision) => selection.exclude.push(commit(revision)?),
                Range::Between { from, to } => {
                    selection.exclude.push(commit(from)?);
                    selection.include.push(commit(to)?);
                }
                Range::Symmetric { left, right } => {
                    let (left, right) = (commit(left)?, commit(right)?);
                    let bases =
                        self.objects
                            .merge_bases_with(left, right, HistoryLimits::trusted())?;
                    selection.include.extend([left, right]);
                    selection.exclude.extend(bases);
                }
            }
        }
        Ok(selection)
    }

    /// Expands a short reference name the way Git does and returns the first existing match.
    ///
    /// Candidates come from [`ref_candidates`], in Git's order. Symbolic references are followed. A
    /// reference that exists but points nowhere, such as an unborn `HEAD`, does not match.
    ///
    /// # Errors
    ///
    /// Reports reference storage failures.
    pub fn expand_ref(&self, name: &str) -> Result<Option<(RefName, ObjectId)>, RevisionError> {
        for candidate in ref_candidates(name) {
            let resolution = self.references.resolve(&candidate)?;
            if let Some(id) = resolution.id {
                return Ok(Some((candidate, id)));
            }
        }
        Ok(None)
    }

    fn resolve_base(
        &self,
        base: &Base,
        wanted: Option<ObjectKind>,
    ) -> Result<ObjectId, RevisionError> {
        match base {
            Base::Head => self.resolve_name("HEAD", wanted),
            Base::Name(name) => self.resolve_name(name, wanted),
            Base::Reflog { name, index } => self.reflog_entry(name.as_deref(), *index),
            Base::Upstream { branch } => self.upstream(branch.as_deref()),
            Base::Push { branch } => self.push_destination(branch.as_deref()),
        }
    }

    /// A full object ID, then references, then an abbreviated object ID: Git's order.
    fn resolve_name(
        &self,
        name: &str,
        wanted: Option<ObjectKind>,
    ) -> Result<ObjectId, RevisionError> {
        if let Ok(id) = ObjectId::from_hex(self.objects.object_format(), name) {
            return Ok(id);
        }
        if let Some((_, id)) = self.expand_ref(name)? {
            return Ok(id);
        }
        if name.len() >= 4 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
            return self.abbreviated(name, wanted);
        }
        if is_pseudo_ref(name) {
            return Err(RevisionError::Unsupported(
                "pseudo-references other than HEAD (`ORIG_HEAD`, `FETCH_HEAD`, ...)",
            ));
        }
        Err(RevisionError::NotFound(name.to_owned()))
    }

    /// Resolves an abbreviated object ID.
    ///
    /// When several objects share the prefix and the expression needs a particular kind, such as
    /// a commit for `abc1~2` or a tree for `abc1:path`, the candidates that peel to that kind are
    /// kept; a single survivor is the answer, as in Git.
    fn abbreviated(
        &self,
        hex: &str,
        wanted: Option<ObjectKind>,
    ) -> Result<ObjectId, RevisionError> {
        let Ok(prefix) = ObjectIdPrefix::from_hex(self.objects.object_format(), hex) else {
            return Err(RevisionError::NotFound(hex.to_owned()));
        };
        match self.objects.resolve_prefix(prefix) {
            Ok(id) => Ok(id),
            Err(ResolvePrefixError::Missing(_)) => Err(RevisionError::NotFound(hex.to_owned())),
            Err(ResolvePrefixError::Ambiguous { prefix, candidates }) => {
                if let Some(kind) = wanted {
                    let mut matching = Vec::new();
                    for candidate in &candidates {
                        if self.peel(*candidate, kind).is_ok() {
                            matching.push(*candidate);
                        }
                    }
                    if let [only] = matching[..] {
                        return Ok(only);
                    }
                }
                Err(ResolvePrefixError::Ambiguous { prefix, candidates }.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn step(&self, id: ObjectId, step: Step) -> Result<ObjectId, RevisionError> {
        match step {
            Step::Ancestor(count) => {
                let mut id = self.peel(id, ObjectKind::Commit)?;
                for _ in 0..count {
                    id = self.parent(id, 1)?;
                }
                Ok(id)
            }
            Step::Parent(0) => self.peel(id, ObjectKind::Commit),
            Step::Parent(n) => self.parent(self.peel(id, ObjectKind::Commit)?, n),
            Step::Peel(Peel::Tags) => Ok(self.peel_tags(id)?.0),
            Step::Peel(Peel::Object) => {
                self.read(id)?;
                Ok(id)
            }
            Step::Peel(Peel::Kind(kind)) => self.peel(id, kind),
        }
    }

    /// The `n`th parent of a commit; shallow roots have none.
    fn parent(&self, commit: ObjectId, n: usize) -> Result<ObjectId, RevisionError> {
        let no_parent = RevisionError::NoParent { commit, parent: n };
        if self.objects.shallow_roots().contains(commit) {
            return Err(no_parent);
        }
        let parsed = self.commit(commit)?;
        parsed.parents().get(n - 1).copied().ok_or(no_parent)
    }

    /// Peels tags, then a commit to its tree when a tree is wanted. A tag is wanted only when
    /// `id` itself is one.
    pub(super) fn peel(&self, id: ObjectId, kind: ObjectKind) -> Result<ObjectId, RevisionError> {
        if kind == ObjectKind::Tag {
            let found = self.read(id)?.kind();
            return if found == ObjectKind::Tag {
                Ok(id)
            } else {
                Err(RevisionError::WrongKind {
                    id,
                    found,
                    wanted: ObjectKind::Tag,
                })
            };
        }
        let (target, found) = self.peel_tags(id)?;
        match (found, kind) {
            (found, wanted) if found == wanted => Ok(target),
            (ObjectKind::Commit, ObjectKind::Tree) => Ok(self.commit(target)?.tree()),
            (found, wanted) => Err(RevisionError::WrongKind {
                id: target,
                found,
                wanted,
            }),
        }
    }

    fn peel_tags(&self, id: ObjectId) -> Result<(ObjectId, ObjectKind), RevisionError> {
        let cancel = AtomicBool::new(false);
        let limits = PeelLimits::default().with_read(self.read_limits);
        let peeled = self.objects.peel(id, limits, &cancel)?;
        Ok((peeled.target, peeled.kind))
    }

    fn read(&self, id: ObjectId) -> Result<Object, RevisionError> {
        self.objects
            .read_with(id, self.read_limits)?
            .ok_or(RevisionError::MissingObject(id))
    }

    fn commit(&self, id: ObjectId) -> Result<Commit, RevisionError> {
        let object = self.read(id)?;
        if object.kind() != ObjectKind::Commit {
            return Err(RevisionError::WrongKind {
                id,
                found: object.kind(),
                wanted: ObjectKind::Commit,
            });
        }
        Commit::parse(object.object_format(), object.data())
            .map_err(|source| RevisionError::Commit { id, source })
    }

    /// Walks `path` one component at a time from `tree`.
    fn tree_path(&self, tree: ObjectId, path: &str) -> Result<ObjectId, RevisionError> {
        let mut id = tree;
        let no_path = || RevisionError::NoPath {
            tree,
            path: path.to_owned(),
        };
        // Git accepts one trailing `/` but no empty, `.` or `..` components.
        let trimmed = path.strip_suffix('/').unwrap_or(path);
        if trimmed.is_empty() {
            return if path.is_empty() {
                Ok(tree)
            } else {
                Err(no_path())
            };
        }
        for component in trimmed.split('/') {
            if matches!(component, "" | "." | "..") {
                return Err(no_path());
            }
            let object = self.read(id)?;
            if object.kind() != ObjectKind::Tree {
                return Err(no_path());
            }
            let parsed = Tree::parse(object.object_format(), object.data())
                .map_err(|source| RevisionError::Tree { id, source })?;
            id = parsed
                .entries()
                .iter()
                .find(|entry| entry.name == component.as_bytes())
                .ok_or_else(no_path)?
                .id;
        }
        // A trailing `/` names a directory.
        if path.ends_with('/') && self.read(id)?.kind() != ObjectKind::Tree {
            return Err(no_path());
        }
        Ok(id)
    }

    /// The branch `HEAD` points to.
    pub(super) fn current_branch(&self) -> Result<RefName, RevisionError> {
        match self
            .references
            .read(&RefName::new("HEAD").expect("valid name"))?
        {
            Some(Target::Symbolic(name)) if name.as_bytes().starts_with(b"refs/heads/") => Ok(name),
            _ => Err(RevisionError::DetachedHead),
        }
    }

    fn reflog_entry(&self, name: Option<&str>, index: usize) -> Result<ObjectId, RevisionError> {
        let reference = match name {
            // A detached HEAD uses its own log, as Git does.
            None => match self.current_branch() {
                Err(RevisionError::DetachedHead) => RefName::new("HEAD").expect("valid name"),
                branch => branch?,
            },
            Some(name) => self.reflog_name(name)?,
        };
        let display = String::from_utf8_lossy(reference.as_bytes()).into_owned();
        let entries = self
            .references
            .reflog(&reference)?
            .ok_or_else(|| RevisionError::NoReflog(display.clone()))?;
        let too_short = || RevisionError::ReflogTooShort {
            name: name.map_or_else(|| display.clone(), str::to_owned),
            entries: entries.len(),
        };
        if index < entries.len() {
            return Ok(entries[entries.len() - 1 - index].new);
        }
        // One step past the oldest entry is that entry's prior value, when it recorded one.
        match entries.first() {
            Some(oldest) if index == entries.len() && !oldest.old.is_null() => Ok(oldest.old),
            _ => Err(too_short()),
        }
    }

    /// The first expansion of `name` that has a reflog, as Git chooses a log.
    fn reflog_name(&self, name: &str) -> Result<RefName, RevisionError> {
        for candidate in ref_candidates(name) {
            if self.references.has_reflog(&candidate)? {
                return Ok(candidate);
            }
        }
        Err(RevisionError::NoReflog(name.to_owned()))
    }

    pub(super) fn resolve_ref(&self, name: &RefName) -> Result<ObjectId, RevisionError> {
        self.references
            .resolve(name)?
            .id
            .ok_or_else(|| RevisionError::NotFound(String::from_utf8_lossy(name.as_bytes()).into()))
    }
}

/// The kind the first navigation needs from the base, used to choose between objects sharing an
/// abbreviated ID.
fn wanted_kind(revision: &Revision) -> Option<ObjectKind> {
    match revision.steps().first() {
        Some(Step::Ancestor(_) | Step::Parent(_)) => Some(ObjectKind::Commit),
        Some(Step::Peel(Peel::Kind(kind))) => Some(*kind),
        Some(Step::Peel(_)) => None,
        None => revision.path().map(|_| ObjectKind::Tree),
    }
}

/// The full reference names Git tries for a short name, in order, skipping invalid ones.
///
/// The name itself comes first when it is `HEAD` or starts with `refs/`, followed by `refs/<name>`,
/// `refs/tags/<name>`, `refs/heads/<name>`, `refs/remotes/<name>` and
/// `refs/remotes/<name>/HEAD`. [`Resolver::expand_ref`] returns the first that resolves; use this
/// directly when a different existence test applies, such as accepting an unborn symbolic
/// reference.
///
/// ```
/// use girt::revision::ref_candidates;
/// let names: Vec<_> = ref_candidates("main")
///     .map(|name| name.as_bytes().to_vec())
///     .collect();
/// assert_eq!(
///     names[..3],
///     [&b"refs/main"[..], b"refs/tags/main", b"refs/heads/main"]
/// );
/// assert_eq!(ref_candidates("HEAD").next().unwrap().as_bytes(), b"HEAD");
/// ```
pub fn ref_candidates(name: &str) -> impl Iterator<Item = RefName> + use<> {
    let literal = (name == "HEAD" || name.starts_with("refs/")).then(|| name.to_owned());
    let expanded = [
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    literal
        .into_iter()
        .chain(expanded)
        .filter_map(|candidate| RefName::new(candidate).ok())
}

/// Whether Git would look `name` up as a file directly under the Git directory, like
/// `ORIG_HEAD`. girt's reference store reads only `HEAD` and names under `refs/`.
fn is_pseudo_ref(name: &str) -> bool {
    name != "HEAD" && !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}
