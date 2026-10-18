use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{FetchError as Error, FetchLimits, check_cancelled};
use crate::{IdMap, IdSet, Object, ObjectId, ObjectKind, Objects};

/// Local object availability and complete-history evidence, including shallow boundaries.
///
/// [`Self::new`] verifies the reachable graph from explicit roots, retaining its payloads.
/// [`Self::from_store`] reads objects on demand and trusts only commit targets of explicit
/// destination reference roots and commits discovered from them as complete. A merely present
/// object can be an external delta base or dependency without proving that its descendants exist.
///
/// Commit discovery is breadth-first in caller root and payload edge order, capped by
/// [`FetchLimits::max_haves`]. A small cap can reduce negotiation effectiveness, never correctness.
/// No local references are inferred by these constructors. Callers must coordinate with GC until
/// installation and any subsequent reference publication. The reader's implicit empty tree is
/// not physical availability: fetch must materialize it before relying on it for publication.
///
/// # Examples
///
/// Describe what the destination already has, so a second fetch transfers only new objects:
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::fetch::{FetchLimits, FetchRequest, KnownHistory, LocalFetchObserver};
/// use girt::fetch::FetchUpdateLimits;
/// use girt::refs::{Expected, RefName, Reflog, Target};
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
/// # let commit = include!("../doctest/commit.rs");
///
/// let cancel = AtomicBool::new(false);
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
///
/// let fetch = |known: &KnownHistory| -> Result<_, Box<dyn std::error::Error>> {
///     let request = FetchRequest::prepare(
///         repo.clone(),
///         origin.fetch_refspecs().clone(),
///         BTreeSet::new(),
///         Reflog::Preserve,
///     )?;
///     Ok(request.receive_local(
///         upstream.git_dir(),
///         known,
///         FetchLimits::default(),
///         TransportControl::new(&cancel),
///         LocalFetchObserver::default(),
///     )?)
/// };
///
/// fetch(&KnownHistory::default())?.finish(FetchUpdateLimits::default(), &cancel)?;
///
/// // Upstream adds a commit on top of `main`.
/// let second = commit(&upstream, &[main], "Second commit")?;
/// # upstream.references()?.update_without_reflog(&RefName::new("refs/heads/main")?, Target::Direct(second), Expected::Value(Target::Direct(main)))?;
///
/// // Without known history, the whole history is sent again.
/// let full = fetch(&KnownHistory::default())?.received().object_count();
///
/// // Offer the local tip as history the destination already has.
/// let objects = repo.objects_with(PackLimits::default())?;
/// let known = KnownHistory::from_store(objects, &[main], FetchLimits::default(), &cancel)?;
/// let incremental = fetch(&known)?.received().object_count();
/// assert!(incremental < full, "{incremental} objects instead of {full}");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default)]
pub struct KnownHistory {
    pub(super) objects: IdMap<Object>,
    /// Destination objects available as delta bases and dependencies, consulted after `objects`.
    pub(super) store: Option<Arc<Objects>>,
    /// Verified complete commits, plus local-receive trees established through their typed edges.
    pub(super) complete_roots: IdSet,
    /// A synthetic reader value is not a physical object that can support publication.
    pub(super) missing_empty_tree: Option<ObjectId>,
    pub(super) haves: Vec<ObjectId>,
    pub(super) shallow: Vec<ObjectId>,
    pub(super) only_when_all_wants_known: bool,
}

/// Objects the destination already has, for delta bases and connectivity.
pub(crate) trait KnownObjects {
    /// Returns a known object.
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>>;

    /// Returns a known object's kind.
    fn known_kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.get(id).map(|object| object.kind())
    }

    /// Whether a known object's history is trusted to be complete, so connectivity checks can
    /// stop at it rather than walking its history.
    fn trusted_complete(&self, _id: ObjectId) -> bool {
        false
    }
}

impl KnownObjects for IdMap<Object> {
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>> {
        HashMap::get(self, &id).map(Cow::Borrowed)
    }
}

impl KnownObjects for KnownHistory {
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>> {
        if self.missing_empty_tree == Some(id) {
            return None;
        }
        if let Some(object) = self.objects.get(&id) {
            return Some(Cow::Borrowed(object));
        }
        self.store
            .as_ref()?
            .read_with(id, crate::ReadLimits::trusted())
            .ok()
            .flatten()
            .map(Cow::Owned)
    }

    fn trusted_complete(&self, id: ObjectId) -> bool {
        self.complete_roots.contains(&id) && !self.shallow.contains(&id)
    }
}

const DISCOVERY_SUBTREES: usize = 256;
const DISCOVERY_OBJECT_BYTES: usize = 1024 * 1024;

#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct Ancestor {
    seconds: i64,
    id: ObjectId,
    parents: Vec<ObjectId>,
    tree: ObjectId,
}

impl Ancestor {
    fn prioritized(self, dates: &[i64]) -> Reverse<(u64, Self)> {
        // Dates are sorted by discovery. Only the neighbors of this insertion point can
        // be nearest; neither the dates nor this priority establish an ancestry edge.
        let index = dates.partition_point(|&date| date < self.seconds);
        let previous = dates[..index].last();
        let next = dates.get(index);
        let distance = previous
            .into_iter()
            .chain(next)
            .map(|&date| self.seconds.abs_diff(date))
            .min()
            .unwrap_or(0);
        Reverse((distance, self))
    }
}

pub(super) fn discovery_read(limits: FetchLimits, bytes: usize) -> crate::ReadLimits {
    let mut read = limits.known_read;
    read.max_object_bytes = read.max_object_bytes.min(bytes).min(DISCOVERY_OBJECT_BYTES);
    read.max_decode_bytes = read.max_decode_bytes.min(8 * DISCOVERY_OBJECT_BYTES);
    read.max_input_bytes = read.max_input_bytes.min(8 * DISCOVERY_OBJECT_BYTES);
    read.max_delta_bytes = read.max_delta_bytes.min(DISCOVERY_OBJECT_BYTES);
    read.max_delta_depth = read.max_delta_depth.min(64);
    read
}

/// A failed optimization read supplies no evidence; connectivity must verify that branch itself.
fn ancestor_entry(
    store: &Objects,
    id: ObjectId,
    limits: FetchLimits,
    bytes: &mut usize,
) -> Result<Option<Ancestor>, Error> {
    let read = discovery_read(limits, *bytes);
    let Ok(Some(object)) = store.read_with(id, read) else {
        return Ok(None);
    };
    *bytes = bytes
        .checked_sub(object.data().len())
        .ok_or(Error::Limit("known bytes"))?;
    if object.kind() != ObjectKind::Commit {
        return Ok(None);
    }
    let Ok(commit) = crate::Commit::parse(store.object_format(), object.data()) else {
        return Ok(None);
    };
    let seconds = commit
        .committer()
        .ok()
        .flatten()
        .and_then(|person| person.date().ok())
        .map_or(0, |date| date.seconds);
    Ok(Some(Ancestor {
        seconds,
        id,
        parents: commit.parents().to_vec(),
        tree: commit.tree(),
    }))
}

/// Verify the complete commit's tree and direct subtrees, so edited received trees can stop at
/// unchanged subtrees. Only verified typed tree edges establish these boundaries.
fn discover_tree_boundaries(
    store: &Objects,
    tree: ObjectId,
    limits: FetchLimits,
    bytes: &mut usize,
    complete: &mut IdSet,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    check_cancelled(cancel)?;
    let read = discovery_read(limits, *bytes);
    let Ok(Some(object)) = store.read_with(tree, read) else {
        return Ok(());
    };
    *bytes = bytes
        .checked_sub(object.data().len())
        .ok_or(Error::Limit("known bytes"))?;
    if object.kind() != ObjectKind::Tree {
        return Ok(());
    }
    let mut trees = Vec::new();
    if crate::edges::visit(tree, object.kind(), object.data(), |id, kind| {
        if kind == ObjectKind::Tree {
            trees.push(id);
        }
        Ok::<_, Error>(())
    })
    .is_err()
    {
        return Ok(());
    }
    complete.insert(tree);
    // Additional subtrees use ordinary connectivity verification.
    for id in trees.into_iter().take(DISCOVERY_SUBTREES) {
        check_cancelled(cancel)?;
        let read = discovery_read(limits, *bytes);
        let Ok(Some(object)) = store.read_with(id, read) else {
            continue;
        };
        *bytes = bytes
            .checked_sub(object.data().len())
            .ok_or(Error::Limit("known bytes"))?;
        if object.kind() == ObjectKind::Tree
            && crate::Tree::parse(store.object_format(), object.data()).is_ok()
        {
            complete.insert(id);
        }
    }
    Ok(())
}

impl KnownHistory {
    pub(super) fn kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.objects.get(&id).map(Object::kind).or_else(|| {
            self.store
                .as_ref()?
                .transfer_object(id, crate::ReadLimits::trusted(), &AtomicBool::new(false))
                .ok()
                .flatten()
                .map(|(kind, _, _)| kind)
        })
    }

    /// The kind of a stored object, from its header rather than a decode.
    pub(super) fn stored_kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.store
            .as_ref()?
            .locate(id, crate::ReadLimits::trusted(), &AtomicBool::new(false))
            .ok()
            .flatten()
            .map(|(kind, _)| kind)
    }

    pub(super) fn complete_kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.objects
            .get(&id)
            .filter(|_| self.missing_empty_tree.is_none())
            .map(Object::kind)
            .or_else(|| self.trusted_complete(id).then(|| self.kind(id)).flatten())
    }

    /// Peels reference roots to complete commits, without walking a wire have batch.
    pub(crate) fn from_store_roots(
        store: Objects,
        roots: &[ObjectId],
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let missing_empty_tree = missing_empty_tree(&store, cancel)?;
        let mut complete_roots = IdSet::default();
        for &root in roots {
            check_cancelled(cancel)?;
            if let Ok(peeled) = store.peel(root, crate::PeelLimits::default(), cancel)
                && peeled.kind == ObjectKind::Commit
            {
                complete_roots.insert(peeled.target);
            }
        }
        check_cancelled(cancel)?;
        Ok(Self {
            shallow: store.shallow_roots().iter().collect(),
            store: Some(Arc::new(store)),
            complete_roots,
            missing_empty_tree,
            ..Self::default()
        })
    }

    /// Extend reference-root evidence with verified ancestor discovery for local receive.
    /// Dates only prioritize work: even skewed dates cannot establish an ancestry edge. Objects
    /// outside this search still require the ordinary complete connectivity walk. Caller limits
    /// bound objects, parent edges and proof payload; no internal depth or seed cutoff applies.
    pub(crate) fn discover_ancestors(
        &mut self,
        targets: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut targets: IdSet = targets.iter().copied().collect();
        if targets.is_empty()
            || targets.len() > limits.max_known_objects
            || limits.max_known_objects == 0
        {
            return Ok(());
        }
        let mut bytes = limits.max_known_bytes;
        let mut dates = Vec::new();
        let mut ordered_targets: Vec<_> = targets.iter().copied().collect();
        ordered_targets.sort_unstable();
        for id in ordered_targets {
            check_cancelled(cancel)?;
            if let Some(entry) = ancestor_entry(store, id, limits, &mut bytes)? {
                if self.trusted_complete(id) {
                    discover_tree_boundaries(
                        store,
                        entry.tree,
                        limits,
                        &mut bytes,
                        &mut self.complete_roots,
                        cancel,
                    )?;
                    targets.remove(&id);
                } else {
                    dates.push(entry.seconds);
                }
            }
        }
        if targets.is_empty() {
            return Ok(());
        }
        dates.sort_unstable();
        dates.dedup();
        let mut pending = BinaryHeap::new();
        let mut seen = self.complete_roots.clone();
        let mut roots: Vec<_> = self.complete_roots.iter().copied().collect();
        roots.sort_unstable();
        let mut remaining = limits.max_known_objects;
        let mut edges = limits.max_known_edges;
        for id in roots {
            // Seeds share the caller's object budget. Too many roots leave discovery
            // incomplete, so ordinary connectivity verification supplies the proof.
            if remaining == 0 {
                return Ok(());
            }
            remaining -= 1;
            check_cancelled(cancel)?;
            if self.shallow.contains(&id) {
                continue;
            }
            if let Some(entry) = ancestor_entry(store, id, limits, &mut bytes)? {
                pending.push(entry.prioritized(&dates));
            }
        }
        while let Some(Reverse((_, entry))) = pending.pop() {
            for id in entry.parents {
                check_cancelled(cancel)?;
                if edges == 0 {
                    return Ok(());
                }
                edges -= 1;
                if remaining == 0 {
                    return Ok(());
                }
                if !seen.insert(id) || self.shallow.contains(&id) {
                    continue;
                }
                remaining -= 1;
                if let Some(entry) = ancestor_entry(store, id, limits, &mut bytes)? {
                    self.complete_roots.insert(id);
                    if targets.contains(&id) {
                        discover_tree_boundaries(
                            store,
                            entry.tree,
                            limits,
                            &mut bytes,
                            &mut self.complete_roots,
                            cancel,
                        )?;
                    }
                    targets.remove(&id);
                    if targets.is_empty() {
                        return Ok(());
                    }
                    pending.push(entry.prioritized(&dates));
                }
            }
        }
        Ok(())
    }

    /// Describes a destination store for negotiation without reading its complete history.
    ///
    /// Up to [`FetchLimits::max_haves`] commits reachable from `tips` (breadth-first, peeling
    /// tags) are offered as haves. The store's shallow roots are declared. Callers supply roots
    /// established by destination references; their peeled commit targets and commits discovered
    /// from them are trusted complete up to declared shallow boundaries. Other stored objects can
    /// supply delta bases and dependencies, but their presence alone does not stop connectivity
    /// validation. Callers must keep the roots and their histories available (e.g. exclude
    /// pruning) until the fetch publishes its references.
    ///
    /// # Errors
    ///
    /// Fails on cancellation. Unreadable tips are ignored; unreadable ancestors end their walk.
    pub fn from_store(
        store: Objects,
        tips: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let missing_empty_tree = missing_empty_tree(&store, cancel)?;
        let shallow: Vec<_> = store.shallow_roots().iter().collect();
        let shallow_set: IdSet = shallow.iter().copied().collect();
        let mut haves = Vec::new();
        let mut complete_roots = IdSet::default();
        let mut seen = IdSet::default();
        let mut pending = VecDeque::new();
        for &tip in tips {
            check_cancelled(cancel)?;
            let Ok(peeled) = store.peel(tip, crate::PeelLimits::default(), cancel) else {
                continue;
            };
            if peeled.kind == ObjectKind::Commit {
                complete_roots.insert(peeled.target);
                if seen.insert(peeled.target) {
                    pending.push_back(peeled.target);
                }
            }
        }
        while let Some(id) = pending.pop_front() {
            if haves.len() >= limits.max_haves {
                break;
            }
            check_cancelled(cancel)?;
            let Ok(Some(object)) = store.read_with(id, limits.known_read) else {
                continue;
            };
            if object.kind() != ObjectKind::Commit {
                continue;
            }
            haves.push(id);
            complete_roots.insert(id);
            if shallow_set.contains(&id) {
                continue;
            }
            let Ok(commit) = crate::Commit::parse(store.object_format(), object.data()) else {
                continue;
            };
            for &parent in commit.parents() {
                if seen.insert(parent) {
                    pending.push_back(parent);
                }
            }
        }
        check_cancelled(cancel)?;
        Ok(Self {
            objects: IdMap::default(),
            store: Some(Arc::new(store)),
            complete_roots,
            missing_empty_tree,
            haves,
            shallow,
            only_when_all_wants_known: false,
        })
    }

    /// Uses this history only when it contains every selected advertised tip.
    ///
    /// A selection with any unknown tip instead requests a complete transfer. This avoids
    /// producing a pack that depends on local objects when the caller can retain only the new
    /// pack against concurrent collection. The caller must still keep verified local objects
    /// available until a known-only result publishes its references.
    /// Explicit depth requests retain the snapshot even for unknown tips: validation includes
    /// reachable local dependencies in the resulting pack before retained installation.
    pub fn only_when_all_wants_known(mut self) -> Self {
        self.only_when_all_wants_known = true;
        self
    }

    /// Lets validation read the store's synthetic empty tree, returning its ID when unstored.
    ///
    /// The caller must store the tree before publishing anything that depends on it.
    pub(crate) fn assume_empty_tree(&mut self) -> Option<ObjectId> {
        self.missing_empty_tree.take()
    }

    pub(super) fn applies_to(&self, wants: &[ObjectId]) -> bool {
        !self.only_when_all_wants_known || wants.iter().all(|&id| self.complete_kind(id).is_some())
    }

    /// Keep the destination's shallow declarations without offering local objects as haves.
    /// An unknown selected tip still needs a self-contained retained pack, but the server must
    /// see existing boundaries so it can report any roots it removes during deepening.
    pub(super) fn shallow_only(&self) -> Self {
        Self {
            shallow: self.shallow.clone(),
            store: self.store.clone(),
            missing_empty_tree: self.missing_empty_tree,
            ..Self::default()
        }
    }
    /// Reads a bounded, identity-verified local graph without changing storage.
    ///
    /// Local object count, retained bytes, edges and per-read decoding use the `max_known_*` and
    /// `known_read` bounds. Root occurrences use `max_wants`. These budgets are separate from
    /// transfer/import budgets. Cancellation is checked between reads and edges; an individual
    /// filesystem read, decode, hash or parse cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Fails on missing/corrupt objects, malformed payloads, mistyped edges, cancellation or
    /// bounds. Declared shallow commits stop parent traversal and are sent to the peer. To request
    /// a full transfer into a complete destination, explicitly use [`KnownHistory::default`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::PackLimits;
    /// use girt::fetch::{FetchLimits, KnownHistory};
    /// # let (_directory, upstream, _, main, _) = include!("../doctest/remote.rs");
    ///
    /// // Read and verify everything reachable from `main`, within the limits.
    /// let objects = upstream.objects_with(PackLimits::default())?;
    /// let known = KnownHistory::new(
    ///     &objects,
    ///     &[main],
    ///     FetchLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(known.object_count(), 2);
    /// assert_eq!(known.haves(), [main]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(
        store: &Objects,
        roots: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let shallow: Vec<_> = store.shallow_roots().iter().collect();
        Self::new_with_boundaries(store, roots, &shallow, limits, cancel)
    }

    pub(super) fn new_with_boundaries(
        store: &Objects,
        roots: &[ObjectId],
        shallow: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::new_stopping_at(store, roots, shallow, &IdSet::default(), limits, cancel)
    }

    /// Validates history reachable from `roots`, treating `published` objects as verified.
    ///
    /// Objects named by existing references were verified when they were published, so the
    /// walk neither reads nor descends into them, like Git's connectivity check excluding
    /// everything reachable from existing references (`--not --all`). The result's objects
    /// therefore cover only history new to the repository.
    pub(super) fn new_stopping_at(
        store: &Objects,
        roots: &[ObjectId],
        shallow: &[ObjectId],
        published: &IdSet,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let mut result = Self {
            shallow: shallow.to_vec(),
            ..Self::default()
        };
        walk(
            WalkSource::Store(store),
            roots,
            shallow,
            published,
            limits,
            cancel,
            |id, object| {
                if object.kind() == ObjectKind::Commit && result.haves.len() < limits.max_haves {
                    result.haves.push(id);
                }
                result.objects.insert(id, object);
            },
        )?;
        let empty = store.object_format().hash_object(ObjectKind::Tree, b"");
        if result.objects.contains_key(&empty) {
            result.missing_empty_tree = missing_empty_tree(store, cancel)?;
            if result.missing_empty_tree.is_some() {
                result.haves.clear();
            }
        }
        result.shallow.retain(|id| result.objects.contains_key(id));
        Ok(result)
    }

    /// Checks that history reachable from `roots` is complete, without retaining payloads.
    ///
    /// Applies [`Self::new_stopping_at`]'s graph, resource and cancellation contract, and returns
    /// the verified object IDs. Byte-bound receive evidence can supply structure without another
    /// decode; other representations use ordinary checked reads. Memory is proportional to the
    /// number of objects, not their size.
    pub(super) fn verify_stopping_at(
        source: &super::verified::InstalledObjects<'_>,
        roots: &[ObjectId],
        shallow: &[ObjectId],
        published: &IdSet,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<IdSet, Error> {
        let kinds = walk(
            WalkSource::Installed(source),
            roots,
            shallow,
            published,
            limits,
            cancel,
            |_, _| {},
        )?;
        Ok(kinds.into_keys().collect())
    }

    /// Checks that history reachable from `roots` is complete after installation.
    ///
    /// Import identity-checked every received object and proved its typed closure over the
    /// received set and `dependencies`. When the installed pack is that validated artifact and
    /// lookup selects it for every received object, that proof holds for the installed graph, so
    /// only `dependencies` and roots outside the received set are walked, through ordinary checked
    /// reads down to `published`. Received objects still count against the walk budgets.
    /// Otherwise this falls back to [`Self::verify_stopping_at`] from `roots`.
    pub(super) fn verify_installed(
        source: &super::verified::InstalledObjects<'_>,
        roots: &[ObjectId],
        dependencies: &[ObjectId],
        shallow: &[ObjectId],
        published: &IdSet,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        if roots.len() > limits.max_wants {
            return Err(Error::Limit("known roots"));
        }
        let verified = source.verified();
        let (objects, bytes, edges) = verified.totals();
        let within = objects <= limits.max_known_objects
            && bytes <= limits.max_known_bytes
            && edges <= limits.max_known_edges;
        let proven = within
            && source
                .selects_every_received(limits.known_read, cancel)
                .map_err(Error::Destination)?;
        if !proven {
            return Self::verify_stopping_at(source, roots, shallow, published, limits, cancel)
                .map(drop);
        }
        let local: Vec<_> = roots
            .iter()
            .chain(dependencies)
            .copied()
            .filter(|id| !verified.objects.contains_key(id))
            .collect();
        let limits = FetchLimits {
            max_known_objects: limits.max_known_objects - objects,
            max_known_bytes: limits.max_known_bytes - bytes,
            max_known_edges: limits.max_known_edges - edges,
            ..limits
        };
        walk(
            WalkSource::Installed(source),
            &local,
            shallow,
            published,
            limits,
            cancel,
            |_, _| {},
        )
        .map(drop)
    }

    /// Number of distinct verified local objects retained for connectivity checks.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Verified commits available for the single have batch, in discovery order.
    pub fn haves(&self) -> &[ObjectId] {
        &self.haves
    }
}

fn missing_empty_tree(store: &Objects, cancel: &AtomicBool) -> Result<Option<ObjectId>, Error> {
    let id = store.object_format().hash_object(ObjectKind::Tree, b"");
    check_cancelled(cancel)?;
    let present = store.has_stored_empty_tree(cancel);
    check_cancelled(cancel)?;
    let present = present.map_err(|source| Error::LocalRead { id, source })?;
    Ok((!present).then_some(id))
}

#[cfg(test)]
thread_local! {
    /// Objects visited by destination history walks, for single-pass regression tests.
    pub(in crate::fetch) static WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Installed evidence carries its own store, so the walk cannot pair it with another snapshot.
enum WalkSource<'a> {
    Store(&'a Objects),
    Installed(&'a super::verified::InstalledObjects<'a>),
}

impl WalkSource<'_> {
    fn store(&self) -> &Objects {
        match self {
            Self::Store(store) => store,
            Self::Installed(installed) => installed.store(),
        }
    }
}

/// Walks history reachable from `roots`, handing each verified object to `visit`.
///
/// Stops at `published` objects without reading them, and at the parents of `shallow` commits.
/// Returns the kind of every visited object.
fn walk(
    source: WalkSource<'_>,
    roots: &[ObjectId],
    shallow: &[ObjectId],
    published: &IdSet,
    limits: FetchLimits,
    cancel: &AtomicBool,
    mut visit: impl FnMut(ObjectId, Object),
) -> Result<IdMap<ObjectKind>, Error> {
    let store = source.store();
    check_cancelled(cancel)?;
    if roots.len() > limits.max_wants {
        return Err(Error::Limit("known roots"));
    }
    if shallow.len() > limits.max_shallow_roots {
        return Err(Error::Limit("shallow roots"));
    }
    let shallow_set: IdSet = shallow.iter().copied().collect();
    let mut kinds = IdMap::default();
    let mut pending = VecDeque::new();
    let mut expected = IdMap::default();
    for &id in roots.iter().filter(|id| !published.contains(id)) {
        enqueue(id, None, &mut expected, &mut pending, limits)?;
    }
    let mut bytes = limits.max_known_bytes;
    let mut edges = limits.max_known_edges;
    while let Some(id) = pending.pop_front() {
        #[cfg(test)]
        WALKED.with(|walked| walked.set(walked.get() + 1));
        check_cancelled(cancel)?;
        let mut read = limits.known_read;
        read.max_object_bytes = read.max_object_bytes.min(bytes);
        let proof = match &source {
            WalkSource::Store(_) => None,
            WalkSource::Installed(installed) => installed
                .get(id, read, cancel)
                .map_err(|source| Error::LocalRead { id, source })?,
        };
        let object = if proof.is_none() {
            Some(
                store
                    .read_with(id, read)
                    .map_err(|source| Error::LocalRead { id, source })?
                    .ok_or(Error::Missing(id))?,
            )
        } else {
            None
        };
        let (kind, length) = proof.map(|o| (o.kind, o.bytes)).unwrap_or_else(|| {
            let object = object.as_ref().unwrap();
            (object.kind(), object.data().len())
        });
        if shallow_set.contains(&id) && kind != ObjectKind::Commit {
            return Err(Error::Kind(id));
        }
        if expected[&id].is_some_and(|expected| expected != kind) {
            return Err(Error::Kind(id));
        }
        bytes = bytes
            .checked_sub(length)
            .ok_or(Error::Limit("known bytes"))?;
        let shallow_commit = kind == ObjectKind::Commit && shallow_set.contains(&id);
        let mut edge = |target, kind| {
            if (shallow_commit && kind == ObjectKind::Commit) || published.contains(&target) {
                return Ok(());
            }
            check_cancelled(cancel)?;
            edges = edges.checked_sub(1).ok_or(Error::Limit("known edges"))?;
            if kinds.get(&target).is_some_and(|&known| known != kind) {
                return Err(Error::Kind(target));
            }
            enqueue(target, Some(kind), &mut expected, &mut pending, limits)
        };
        if let Some(proof) = proof {
            let edges = proof
                .edges()
                .expect("InstalledObjects::get requires parsed edges");
            for &(target, expected) in edges {
                edge(target, expected)?;
            }
        } else {
            let object = object.unwrap();
            crate::edges::visit(id, kind, object.data(), edge)?;
            visit(id, object);
        }
        kinds.insert(id, kind);
    }
    Ok(kinds)
}

fn enqueue(
    id: ObjectId,
    kind: Option<ObjectKind>,
    expected: &mut IdMap<Option<ObjectKind>>,
    pending: &mut VecDeque<ObjectId>,
    limits: FetchLimits,
) -> Result<(), Error> {
    if let Some(previous) = expected.get_mut(&id) {
        if previous.is_some() && kind.is_some() && *previous != kind {
            return Err(Error::Kind(id));
        }
        if kind.is_some() {
            *previous = kind;
        }
    } else {
        if expected.len() >= limits.max_known_objects {
            return Err(Error::Limit("known objects"));
        }
        expected.insert(id, kind);
        pending.push_back(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        Commit, CommitFields, EntryMode, LooseObjects, ObjectFormat, PackLimits, Signature, Tree,
        TreeEntry,
    };

    fn graph() -> (tempfile::TempDir, Objects, ObjectId, ObjectId) {
        let root = tempfile::tempdir().unwrap();
        let loose = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let blob = loose.write_blob(b"known blob").unwrap();
        let tree = loose
            .write_tree(
                &Tree::new(
                    crate::ObjectFormat::Sha1,
                    vec![
                        TreeEntry {
                            mode: EntryMode::Blob,
                            name: b"blob".to_vec(),
                            id: blob,
                        },
                        TreeEntry {
                            mode: EntryMode::Gitlink,
                            name: b"external".to_vec(),
                            id: ObjectId::for_blob(crate::ObjectFormat::Sha1, b"external"),
                        },
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        let author = Signature {
            name: b"A".to_vec(),
            email: b"a@example.com".to_vec(),
            seconds: 0,
            offset_minutes: 0,
        };
        let commit = loose
            .write_commit(
                &Commit::new(CommitFields {
                    tree,
                    parents: vec![],
                    author: author.clone(),
                    committer: author,
                    extra_headers: vec![],
                    message: vec![],
                })
                .unwrap(),
            )
            .unwrap();
        let objects = Objects::open(
            crate::ObjectFormat::Sha1,
            root.path(),
            PackLimits::default(),
            crate::AlternateLimits::default(),
        )
        .unwrap();
        (root, objects, commit, blob)
    }

    fn ancestor_graph() -> (tempfile::TempDir, Objects, ObjectId, ObjectId) {
        let (root, objects, parent, _) = graph();
        let object = objects
            .read_with(parent, crate::ReadLimits::default())
            .unwrap()
            .unwrap();
        let mut fields = Commit::parse(ObjectFormat::Sha1, object.data())
            .unwrap()
            .to_fields()
            .unwrap();
        fields.parents = vec![parent];
        fields.committer.seconds = -100; // Parent newer than child: dates cannot prune edges.
        let child = LooseObjects::new(root.path(), ObjectFormat::Sha1)
            .write_commit(&Commit::new(fields).unwrap())
            .unwrap();
        (root, objects, parent, child)
    }

    #[rstest]
    #[case::empty(100, &[], 0)]
    #[case::exact(100, &[0, 100, 200], 0)]
    #[case::duplicate(100, &[100, 100], 0)]
    #[case::before(50, &[100, 200], 50)]
    #[case::after(250, &[100, 200], 50)]
    #[case::closer_previous(125, &[100, 200], 25)]
    #[case::closer_next(175, &[100, 200], 25)]
    #[case::tie(150, &[100, 200], 50)]
    #[case::extremes(i64::MIN, &[i64::MAX], u64::MAX)]
    fn ancestor_priority_uses_nearest_sorted_date(
        #[case] seconds: i64,
        #[case] dates: &[i64],
        #[case] expected: u64,
    ) {
        let id = ObjectId::Sha1([0; 20]);
        let entry = Ancestor {
            seconds,
            id,
            parents: vec![],
            tree: id,
        };
        let Reverse((distance, _)) = entry.prioritized(dates);
        assert_eq!(distance, expected);
    }

    #[rstest]
    #[case::no_budget(0, false, false)]
    #[case::exact_budget(2, false, true)]
    #[case::shallow_seed(1, true, false)]
    fn discovery_requires_verified_ancestry_within_budget(
        #[case] count: usize,
        #[case] shallow: bool,
        #[case] complete: bool,
    ) {
        let (_root, objects, parent, child) = ancestor_graph();
        let cancel = AtomicBool::new(false);
        let mut known = KnownHistory::from_store_roots(objects, &[child], &cancel).unwrap();
        known.shallow = vec![child].into_iter().filter(|_| shallow).collect();
        known
            .discover_ancestors(
                &[parent],
                FetchLimits {
                    max_known_objects: count,
                    ..FetchLimits::default()
                },
                &cancel,
            )
            .unwrap();
        assert_eq!(known.trusted_complete(parent), complete);
    }

    #[rstest]
    #[case::no_edges(0, false)]
    #[case::one_edge(1, true)]
    fn discovery_respects_parent_edge_budget(#[case] edges: usize, #[case] complete: bool) {
        let (_root, objects, parent, child) = ancestor_graph();
        let cancel = AtomicBool::new(false);
        let mut known = KnownHistory::from_store_roots(objects, &[child], &cancel).unwrap();
        known
            .discover_ancestors(
                &[parent],
                FetchLimits {
                    max_known_edges: edges,
                    ..FetchLimits::default()
                },
                &cancel,
            )
            .unwrap();
        assert_eq!(known.trusted_complete(parent), complete);
    }

    #[test]
    fn discovery_proves_more_than_256_unique_targets() {
        let (_root, objects, targets, tip) = wide_ancestor_graph();
        let cancel = AtomicBool::new(false);
        let mut known = KnownHistory::from_store_roots(objects, &[tip], &cancel).unwrap();
        known
            .discover_ancestors(&targets, FetchLimits::default(), &cancel)
            .unwrap();
        assert!(targets.iter().all(|&id| known.trusted_complete(id)));
    }

    fn wide_ancestor_graph() -> (tempfile::TempDir, Objects, Vec<ObjectId>, ObjectId) {
        let (root, objects, parent, _) = ancestor_graph();
        let object = objects
            .read_with(parent, crate::ReadLimits::default())
            .unwrap()
            .unwrap();
        let mut fields = Commit::parse(ObjectFormat::Sha1, object.data())
            .unwrap()
            .to_fields()
            .unwrap();
        let loose = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let mut targets = Vec::new();
        for index in 0..300 {
            fields.message = format!("parent {index}").into_bytes();
            targets.push(
                loose
                    .write_commit(&Commit::new(fields.clone()).unwrap())
                    .unwrap(),
            );
        }
        fields.parents = targets.clone();
        fields.message = b"merge".to_vec();
        let tip = loose.write_commit(&Commit::new(fields).unwrap()).unwrap();
        (root, objects, targets, tip)
    }

    #[test]
    fn discovery_cannot_trust_mislabelled_parent_bytes() {
        let (root, objects, parent, child) = ancestor_graph();
        let other = child.to_string();
        let id = parent.to_string();
        std::fs::copy(
            root.path().join(&other[..2]).join(&other[2..]),
            root.path().join(&id[..2]).join(&id[2..]),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let mut known = KnownHistory::from_store_roots(objects, &[child], &cancel).unwrap();
        known
            .discover_ancestors(&[parent], FetchLimits::default(), &cancel)
            .unwrap();
        assert!(!known.trusted_complete(parent));
    }

    #[test]
    fn rejected_tree_kind_still_charges_proof_payload() {
        let (_root, objects, _, blob) = graph();
        let mut complete = IdSet::default();
        let mut bytes = 100;
        discover_tree_boundaries(
            &objects,
            blob,
            FetchLimits::default(),
            &mut bytes,
            &mut complete,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(bytes, 90);
        assert!(complete.is_empty());
    }

    #[test]
    fn tree_boundary_requires_subtree_identity() {
        let (root, objects, _, _) = graph();
        let loose = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let subtree = loose
            .write_tree(
                &Tree::new(
                    ObjectFormat::Sha1,
                    vec![TreeEntry {
                        mode: EntryMode::Blob,
                        name: b"f".to_vec(),
                        id: loose.write_blob(b"payload").unwrap(),
                    }],
                )
                .unwrap(),
            )
            .unwrap();
        let tree = loose
            .write_tree(
                &Tree::new(
                    ObjectFormat::Sha1,
                    vec![TreeEntry {
                        mode: EntryMode::Tree,
                        name: b"d".to_vec(),
                        id: subtree,
                    }],
                )
                .unwrap(),
            )
            .unwrap();
        let other = loose
            .write_tree(&Tree::new(ObjectFormat::Sha1, vec![]).unwrap())
            .unwrap()
            .to_string();
        let id = subtree.to_string();
        std::fs::copy(
            root.path().join(&other[..2]).join(&other[2..]),
            root.path().join(&id[..2]).join(&id[2..]),
        )
        .unwrap();
        let mut complete = IdSet::default();
        let mut bytes = FetchLimits::default().max_known_bytes;
        discover_tree_boundaries(
            &objects,
            tree,
            FetchLimits::default(),
            &mut bytes,
            &mut complete,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(complete.contains(&tree));
        assert!(!complete.contains(&subtree));
    }

    #[test]
    fn exhausted_discovery_preserves_full_connectivity_validation() {
        let (root, objects, parent, child) = ancestor_graph();
        let cancel = AtomicBool::new(false);
        let mut known = KnownHistory::from_store_roots(objects, &[child], &cancel).unwrap();
        known
            .discover_ancestors(
                &[parent],
                FetchLimits {
                    max_known_objects: 0,
                    ..FetchLimits::default()
                },
                &cancel,
            )
            .unwrap();
        let received = IdMap::<Object>::default();
        let result = super::super::connectivity::validate_with_boundaries(
            &received,
            &known,
            &[parent],
            &[],
            FetchLimits::default(),
            &cancel,
        );
        assert!(result.is_ok());
        assert!(!known.trusted_complete(parent));
        let blob = ObjectId::for_blob(ObjectFormat::Sha1, b"known blob");
        damage(root.path(), blob, false);
        let result = super::super::connectivity::validate_with_boundaries(
            &received,
            &known,
            &[parent],
            &[],
            FetchLimits::default(),
            &cancel,
        );
        assert!(matches!(result, Err(Error::Missing(id)) if id == blob));
    }

    #[test]
    fn cancelled_discovery_cannot_authorize_ancestors() {
        let (_root, objects, parent, child) = ancestor_graph();
        let mut known =
            KnownHistory::from_store_roots(objects, &[child], &AtomicBool::new(false)).unwrap();
        let result =
            known.discover_ancestors(&[parent], FetchLimits::default(), &AtomicBool::new(true));
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(!known.trusted_complete(parent));
    }

    #[rstest]
    #[case::objects(FetchLimits { max_known_objects: 2, ..FetchLimits::default() })]
    #[case::bytes(FetchLimits { max_known_bytes: 1, ..FetchLimits::default() })]
    #[case::edges(FetchLimits { max_known_edges: 1, ..FetchLimits::default() })]
    #[case::roots(FetchLimits { max_wants: 0, ..FetchLimits::default() })]
    fn rejects_local_work_bounds(#[case] limits: FetchLimits) {
        let (_root, objects, commit, _) = graph();
        assert!(KnownHistory::new(&objects, &[commit], limits, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn exact_count_and_edges_skip_gitlinks_and_bound_haves() {
        let (_root, objects, commit, _) = graph();
        let limits = FetchLimits {
            max_known_objects: 3,
            max_known_edges: 2,
            max_haves: 0,
            ..FetchLimits::default()
        };
        let known = KnownHistory::new(&objects, &[commit, commit], limits, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(known.object_count(), 3);
        assert!(known.haves().is_empty());
    }

    fn damage(root: &std::path::Path, blob: ObjectId, corrupt: bool) {
        let id = blob.to_string();
        let path = root.join(&id[..2]).join(&id[2..]);
        if corrupt {
            std::fs::write(path, b"corrupt").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[rstest]
    #[case::missing(false)]
    #[case::corrupt(true)]
    fn incomplete_history_cannot_be_offered(#[case] corrupt: bool) {
        let (root, objects, commit, blob) = graph();
        damage(root.path(), blob, corrupt);
        assert!(
            KnownHistory::new(
                &objects,
                &[commit],
                FetchLimits::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }

    #[test]
    fn corrupt_local_dependency_reports_identity_and_storage_cause() {
        let (root, objects, commit, blob) = graph();
        damage(root.path(), blob, true);
        let error = KnownHistory::new(
            &objects,
            &[commit],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::LocalRead { id, source: crate::ObjectReadError::Loose(_) } if id == blob)
        );
    }

    #[test]
    fn cancelled_preparation_retains_no_partial_knowledge() {
        let (_root, objects, commit, _) = graph();
        assert!(matches!(
            KnownHistory::new(
                &objects,
                &[commit],
                FetchLimits::default(),
                &AtomicBool::new(true)
            ),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn store_history_offers_commits_and_reads_objects_on_demand() {
        let (_root, objects, commit, blob) = graph();
        let known = KnownHistory::from_store(
            objects,
            &[commit],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(known.haves(), &[commit]);
        assert_eq!(known.object_count(), 0);
        assert!(known.trusted_complete(commit));
        assert!(!known.trusted_complete(blob));
        assert_eq!(known.get(blob).unwrap().kind(), ObjectKind::Blob);
    }

    #[test]
    fn unanchored_store_object_does_not_stop_connectivity() {
        let (root, objects, commit, blob) = graph();
        damage(root.path(), blob, false);
        let known = KnownHistory::from_store(
            objects,
            &[],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let result = super::super::connectivity::validate_with_boundaries(
            &IdMap::default(),
            &known,
            &[commit],
            &[],
            FetchLimits::default(),
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Missing(missing)) if missing == blob));
    }

    #[test]
    fn connectivity_stops_at_trusted_store_objects() {
        let (_root, objects, commit, _blob) = graph();
        let known = KnownHistory::from_store(
            objects,
            &[commit],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let dependencies = super::super::connectivity::validate_with_boundaries(
            &IdMap::default(),
            &known,
            &[commit],
            &[],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(dependencies, [commit]);
    }
}
