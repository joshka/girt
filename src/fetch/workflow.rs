use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    Advertisement, FetchError, FetchFinishLimits, FetchInstalled, FetchLimits, FetchUpdateLimits,
    KnownHistory, ReceivedFetch,
};
use crate::refs::{Expected, RefEdit, RefEditOutcome, RefName, ReferenceError, Reflog, Target};
use crate::remote::{Direction, Mapping, MappingError, Refspecs};
use crate::transport::TransportControl;
use crate::{ObjectId, Repository};

/// Resource budgets for controlled fetch preparation on an already-open repository.
///
/// Linked worktrees are not admitted yet; see [`FetchPlanError::LinkedWorktrees`]. Filesystem
/// calls and bounded parsing complete before cancellation is observed. These limits do not
/// control later advertisement mapping, transport, object verification or publication. Repository
/// opening and its already-loaded shallow metadata are caller work, outside these budgets.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct FetchPrepareLimits {
    /// Directory entries and packed names visited by the destination inventory.
    pub max_ref_entries: usize,
    /// Aggregate files-backend reference input bytes, excluding HEAD.
    pub max_ref_bytes: usize,
    /// Files-backend HEAD input bytes.
    pub max_head_bytes: usize,
    /// Maximum declared shallow roots copied into the request.
    pub max_shallow_roots: usize,
    /// Encoded/decoded reftable limits, separately for inventory and HEAD snapshots.
    pub reftable: crate::refs::reftable::StackLimits,
}

impl Default for FetchPrepareLimits {
    fn default() -> Self {
        Self {
            max_ref_entries: 100_000,
            max_ref_bytes: 16 * 1024 * 1024,
            max_head_bytes: 4096,
            max_shallow_roots: 100_000,
            reftable: crate::refs::reftable::StackLimits::default(),
        }
    }
}

/// A synchronous destination snapshot and explicit policy for one fetch.
///
/// # Workflow
///
/// [`Self::prepare`] captures destination values. Choose optional depth, pruning, and reflog
/// policy before [`Self::receive_local`] or a network adapter reads the actual advertisement.
/// [`FetchReady::finish`] installs validated objects and publishes eligible refs; inspect
/// [`FetchReport`] or [`FetchFinishError`] for completed effects and retry decisions. Use
/// [`Self::plan`] only to preview how an advertisement would map, not to reserve its tips.
///
/// Construct with a named remote's [`crate::remote::Remote::fetch_refspecs`] or an explicit list.
/// Endpoint selection and credentials remain separate transport arguments. Only `refs/remotes/*`
/// and `refs/tags/*` destinations are supported by default. A caller may add tag-like namespaces
/// with [`Self::with_tag_destination_namespace`]. Branch and symbolic destinations are rejected.
/// Remote-tracking refs accept all validated object kinds. When both old/new tips peel to commits,
/// non-fast-forward updates require force intent and name authorization; other kind replacements
/// need neither. Replacing a tag requires both `+` and its name in `authorized_force`.
/// Neither source-only selection nor an unchanged tag requires force authorization.
///
/// Preparation performs filesystem work; transport selection subsequently maps the actual
/// advertisement using only this owned snapshot. HTTP/SSH downloads retain owned validation state
/// for caller-controlled worker handoff. No runtime, endpoint discovery or worker is created here.
///
/// Callers must exclude checkout, symbolic-HEAD/branch changes, worktree registration changes and
/// GC/pruning from preparation through publication. Ordinary destination writers may run
/// concurrently: changed destinations use exact expected values in a reference transaction.
/// Unchanged destinations are reported but not written or locked, and may change concurrently after
/// the snapshot. HEAD chains in the main and registered linked worktrees must stay within
/// `refs/heads/*` (or be detached), so rejecting branch destinations also protects checked-out
/// references. Unusual HEAD chains and inaccessible worktree metadata fail closed, even for
/// source-only fetches.
/// Verified known history must include exactly the destination's captured shallow roots, so a
/// depth response cannot silently drop boundaries belonging to other local references.
///
/// Exact selectors missing from the advertisement are reported while other valid selections
/// continue. [`Self::with_prune`] permits scoped remote-tracking and authorized tag-namespace
/// deletion. Shallow depth changes
/// use a conditional metadata lock and are reported separately from object/ref effects.
///
/// This workflow does not write `FETCH_HEAD`, infer tags, clone, check out, edit
/// configuration, discover credentials, or implement the full Git CLI. See
/// `examples/fetch_remote.rs`.
///
/// # Examples
///
/// Preview what a fetch would do, using an advertisement from [`super::discover_local`]. The
/// preview is only advisory: the transfer replans from the advertisement it actually receives.
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{FetchLimits, FetchRequest, FetchUpdateKind, discover_local};
/// use girt::refs::{RefName, Reflog};
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
///
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
/// let request = FetchRequest::prepare(
///     repo.clone(),
///     origin.fetch_refspecs().clone(),
///     BTreeSet::new(),
///     Reflog::Preserve,
/// )?;
///
/// let cancel = AtomicBool::new(false);
/// let discovery = discover_local(
///     upstream.git_dir(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
/// )?;
///
/// let updates = request.plan(&discovery.advertisement)?;
/// assert_eq!(updates.len(), 1);
/// assert_eq!(
///     updates[0].mapping.destination,
///     Some(RefName::new("refs/remotes/origin/main")?)
/// );
/// assert_eq!(updates[0].kind, FetchUpdateKind::Create);
/// assert_eq!(
///     updates[0].mapping.source.as_ref().map(|source| source.id),
///     Some(main)
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct FetchRequest {
    repository: Repository,
    specs: Refspecs,
    snapshot: BTreeMap<RefName, Target>,
    authorized_force: BTreeSet<RefName>,
    reflog: Reflog,
    namespace_reflogs: Vec<(RefName, Reflog, Reflog)>,
    kind_reflogs: Vec<(RefName, FetchUpdateKind, Reflog)>,
    additional_tag_namespaces: Vec<RefName>,
    retained_missing_sources: BTreeSet<Vec<u8>>,
    prune: bool,
    pub(super) depth: Option<NonZeroU32>,
    pub(super) filter: Option<super::ObjectFilter>,
    shallow_before: Vec<ObjectId>,
}

impl FetchRequest {
    /// Captures destination values and validates the supported worktree layout without writing.
    ///
    /// `authorized_force` authorizes replacement only for those exact destination names; it does
    /// not turn an unforced refspec into a forced one. `reflog` chooses the default action for
    /// changed refs; [`Self::with_reflog_for_namespace`] may override it. Reference enumeration has
    /// its existing unbounded metadata allocation contract; transfer and object budgets are
    /// supplied separately.
    ///
    /// # Errors
    ///
    /// Rejects push refspecs, unsupported HEAD chains, inaccessible worktrees, or reference errors.
    /// Mapping-specific failures are returned when the transfer advertisement is available.
    ///
    /// # Examples
    ///
    /// A `+` refspec states the intent to force; `authorized_force` grants it. When upstream
    /// rewrites `main`, the fetch refuses to move `refs/remotes/origin/main` backwards until the
    /// caller authorizes that destination:
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::fetch::{
    ///     FetchFinishFailure, FetchLimits, FetchRequest, FetchUpdateError, FetchUpdateKind,
    ///     FetchUpdateLimits, KnownHistory, LocalFetchObserver,
    /// };
    /// use girt::refs::{Expected, RefName, Reflog, Target};
    /// use girt::remote::Remote;
    /// use girt::transport::TransportControl;
    /// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
    /// # let commit = include!("../doctest/commit.rs");
    ///
    /// let cancel = AtomicBool::new(false);
    /// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
    /// let fetch = |authorized: BTreeSet<RefName>| -> Result<_, Box<dyn std::error::Error>> {
    ///     let request = FetchRequest::prepare(
    ///         repo.clone(),
    ///         origin.fetch_refspecs().clone(),
    ///         authorized,
    ///         Reflog::Preserve,
    ///     )?;
    ///
    ///     let ready = request.receive_local(
    ///         upstream.git_dir(),
    ///         &KnownHistory::default(),
    ///         FetchLimits::default(),
    ///         TransportControl::new(&cancel),
    ///         LocalFetchObserver::default(),
    ///     )?;
    ///     Ok(ready.finish(FetchUpdateLimits::default(), &cancel))
    /// };
    /// fetch(BTreeSet::new())??;
    ///
    /// // Upstream replaces `main` with an unrelated commit.
    /// let rewritten = commit(&upstream, &[], "Rewritten history")?;
    /// let main_ref = RefName::new("refs/heads/main")?;
    /// upstream.references()?.update_without_reflog(
    ///     &main_ref,
    ///     Target::Direct(rewritten),
    ///     Expected::Value(Target::Direct(main)),
    /// )?;
    ///
    /// // `+` alone is not enough.
    /// let error = fetch(BTreeSet::new())?.unwrap_err();
    /// assert!(matches!(
    ///     *error.source,
    ///     FetchFinishFailure::Update(FetchUpdateError::NonFastForward(_))
    /// ));
    ///
    /// let tracking = RefName::new("refs/remotes/origin/main")?;
    /// let report = fetch(BTreeSet::from([tracking.clone()]))??;
    /// assert_eq!(report.updates[0].kind, FetchUpdateKind::ForcedTracking);
    /// assert_eq!(
    ///     repo.references()?.read(&tracking)?,
    ///     Some(Target::Direct(rewritten))
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn prepare(
        repository: Repository,
        specs: Refspecs,
        authorized_force: BTreeSet<RefName>,
        reflog: Reflog,
    ) -> Result<Self, FetchPlanError> {
        Self::prepare_from::<false>(
            repository,
            specs,
            authorized_force,
            reflog,
            FetchPrepareLimits::default(),
            &AtomicBool::new(false),
            |repository| Ok(repository.references()?.list()?),
        )
    }

    /// Captures destinations with bounded inventory and cooperative cancellation.
    ///
    /// Uses the caller's already-open handle; it does not reopen repository metadata. Requires
    /// the same exclusion of checkout, HEAD/branch, registration and GC changes as
    /// [`Self::prepare`]. Linked callers and any registered linked worktree return
    /// [`FetchPlanError::LinkedWorktrees`] before their metadata is opened. HEAD chains use the
    /// bounded inventory, stop after 32 symbolic edges and must stay inside `refs/heads/`.
    /// Cancellation is checked between inventory entries and every 64 copied records. A single
    /// bounded parse or filesystem call can finish before cancellation. No files, refs or
    /// objects are written.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::fetch::{FetchPrepareLimits, FetchRequest};
    /// use girt::refs::Reflog;
    /// use girt::remote::{Direction, Refspecs};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let specs = Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"])?;
    /// let request = FetchRequest::prepare_controlled(
    ///     repo,
    ///     specs,
    ///     BTreeSet::new(),
    ///     Reflog::Preserve,
    ///     FetchPrepareLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// # let _ = request;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns typed admission, cancellation, reference-limit and HEAD-chain failures with no
    /// partial request. Shallow roots exceeding the selected count return
    /// [`FetchPlanError::PrepareLimit`].
    pub fn prepare_controlled(
        repository: Repository,
        specs: Refspecs,
        authorized_force: BTreeSet<RefName>,
        reflog: Reflog,
        limits: FetchPrepareLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchPlanError> {
        Self::prepare_from::<true>(
            repository,
            specs,
            authorized_force,
            reflog,
            limits,
            cancel,
            |repository| {
                let refs = repository
                    .references()?
                    .with_reftable_limits(limits.reftable);
                Ok(refs.list_controlled(limits.max_ref_entries, limits.max_ref_bytes, cancel)?)
            },
        )
    }

    /// Prepares like [`Self::prepare`] from a reference listing the caller already took.
    ///
    /// The listing becomes the snapshot every edit is conditional on, so it must come from this
    /// repository, and the caller must take it no earlier than anything else it derived from it.
    pub(crate) fn prepare_listed(
        repository: Repository,
        specs: Refspecs,
        authorized_force: BTreeSet<RefName>,
        reflog: Reflog,
        references: Vec<crate::refs::Reference>,
    ) -> Result<Self, FetchPlanError> {
        Self::prepare_from::<false>(
            repository,
            specs,
            authorized_force,
            reflog,
            FetchPrepareLimits::default(),
            &AtomicBool::new(false),
            |_| Ok(references),
        )
    }

    fn prepare_from<const CONTROLLED: bool>(
        repository: Repository,
        specs: Refspecs,
        authorized_force: BTreeSet<RefName>,
        reflog: Reflog,
        limits: FetchPrepareLimits,
        cancel: &AtomicBool,
        list: impl FnOnce(&Repository) -> Result<Vec<crate::refs::Reference>, FetchPlanError>,
    ) -> Result<Self, FetchPlanError> {
        if CONTROLLED && cancel.load(Ordering::Relaxed) {
            return Err(FetchPlanError::Cancelled);
        }
        if specs.direction() != Direction::Fetch {
            return Err(FetchPlanError::Mapping(MappingError::Direction));
        }
        if CONTROLLED {
            super::worktree::admit_controlled(&repository, cancel)?;
        } else {
            super::worktree::check(&repository)?;
        }
        let references = list(&repository)?;
        if CONTROLLED {
            super::worktree::check_head_controlled(&repository, &references, limits, cancel)?;
            if repository.shallow_roots().iter().len() > limits.max_shallow_roots {
                return Err(FetchPlanError::PrepareLimit("shallow roots"));
            }
        }
        let (snapshot, shallow_before) = if CONTROLLED {
            let mut snapshot = BTreeMap::new();
            for (index, reference) in references.into_iter().enumerate() {
                if index % 64 == 0 && cancel.load(Ordering::Relaxed) {
                    return Err(FetchPlanError::Cancelled);
                }
                snapshot.insert(reference.name, reference.target);
            }
            let mut shallow_before = Vec::new();
            for (index, id) in repository.shallow_roots().iter().enumerate() {
                if index % 64 == 0 && cancel.load(Ordering::Relaxed) {
                    return Err(FetchPlanError::Cancelled);
                }
                shallow_before.push(id);
            }
            (snapshot, shallow_before)
        } else {
            let snapshot = references
                .into_iter()
                .map(|reference| (reference.name, reference.target))
                .collect();
            let shallow_before = repository.shallow_roots().iter().collect();
            (snapshot, shallow_before)
        };
        if CONTROLLED && cancel.load(Ordering::Relaxed) {
            return Err(FetchPlanError::Cancelled);
        }
        Ok(Self {
            repository,
            specs,
            snapshot,
            authorized_force,
            reflog,
            namespace_reflogs: Vec::new(),
            kind_reflogs: Vec::new(),
            additional_tag_namespaces: Vec::new(),
            retained_missing_sources: BTreeSet::new(),
            prune: false,
            depth: None,
            filter: None,
            shallow_before,
        })
    }

    /// Allows tag-like destinations below one caller-owned reference namespace.
    ///
    /// Only descendants of `namespace` are added; the namespace itself remains unsupported.
    /// Replacements require both force intent and authorization, as for `refs/tags/*`. When
    /// pruning is enabled, absent sources can prune these destinations. The caller must choose
    /// a namespace it owns and keep unrelated references outside it.
    pub fn with_tag_destination_namespace(mut self, namespace: RefName) -> Self {
        self.additional_tag_namespaces.push(namespace);
        self
    }

    /// Chooses reflog actions for changed destinations below one reference namespace.
    ///
    /// `update` applies to creates and updates and must not be [`Reflog::Delete`]. `prune` applies
    /// to deletions requested by
    /// [`Self::with_prune`]. The most specific matching namespace wins. Names outside configured
    /// namespaces keep the policy passed to [`Self::prepare`]. This does not authorize a
    /// destination or alter its update rules. Use [`Reflog::Delete`] for `prune` when a removed
    /// reference should lose its log, as Git does for pruned remote-tracking refs.
    pub fn with_reflog_for_namespace(
        mut self,
        namespace: RefName,
        update: Reflog,
        prune: Reflog,
    ) -> Self {
        self.namespace_reflogs.push((namespace, update, prune));
        self
    }

    /// Overrides a namespace's reflog action for one changed update kind.
    ///
    /// This permits different messages for creation, fast-forward, forced update and other
    /// outcomes. The caller supplies the complete message and identity; this workflow does not
    /// synthesize Git CLI command text. A kind-specific action takes priority over a matching
    /// [`Self::with_reflog_for_namespace`] action, and the most specific matching namespace wins.
    /// `SourceOnly` and `Unchanged` never write a reference or a reflog.
    pub fn with_reflog_for_update_kind(
        mut self,
        namespace: RefName,
        kind: FetchUpdateKind,
        reflog: Reflog,
    ) -> Self {
        self.kind_reflogs.push((namespace, kind, reflog));
        self
    }

    fn reflog_for(&self, name: &RefName, kind: FetchUpdateKind) -> Reflog {
        if let Some((_, _, reflog)) = self
            .kind_reflogs
            .iter()
            .filter(|(namespace, selected, _)| *selected == kind && is_descendant(name, namespace))
            .max_by_key(|(namespace, _, _)| namespace.as_bytes().len())
        {
            return reflog.clone();
        }
        self.namespace_reflogs
            .iter()
            .filter(|(namespace, _, _)| is_descendant(name, namespace))
            .max_by_key(|(namespace, _, _)| namespace.as_bytes().len())
            .map(|(_, update, prune)| {
                if kind == FetchUpdateKind::Prune {
                    prune
                } else {
                    update
                }
            })
            .unwrap_or(&self.reflog)
            .clone()
    }

    fn additional_tag_destination(&self, name: &RefName) -> bool {
        self.additional_tag_namespaces
            .iter()
            .any(|namespace| is_descendant(name, namespace))
    }

    /// Requests a positive commit depth on HTTP/SSH upload-pack or depth one from a native local
    /// source. Local depth requires one selected commit tip and a nonshallow source. This permits
    /// a resulting shallow-boundary change during coordinated publication.
    /// Network validation materializes any reachable known-local dependencies in a nonempty
    /// received pack. An empty object transfer verifies local dependencies during publication;
    /// the caller must continue excluding concurrent collection for that existing history.
    ///
    /// # Examples
    ///
    /// Fetch only the newest commit of `main`, like `git fetch --depth 1`:
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use std::num::NonZeroU32;
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::fetch::{FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory, LocalFetchObserver};
    /// use girt::refs::{Expected, RefName, Reflog, Target};
    /// use girt::remote::Remote;
    /// use girt::transport::TransportControl;
    /// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
    /// # let commit = include!("../doctest/commit.rs");
    /// // Upstream has two commits on `main`.
    /// let second = commit(&upstream, &[main], "Second commit")?;
    /// # upstream.references()?.update_without_reflog(&RefName::new("refs/heads/main")?, Target::Direct(second), Expected::Value(Target::Direct(main)))?;
    ///
    /// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
    /// let request =
    ///     FetchRequest::prepare(repo.clone(), origin.fetch_refspecs().clone(), BTreeSet::new(), Reflog::Preserve)?
    ///         .with_depth(NonZeroU32::MIN);
    ///
    /// let cancel = AtomicBool::new(false);
    /// let ready = request.receive_local(
    ///     upstream.git_dir(),
    ///     &KnownHistory::default(),
    ///     FetchLimits::default(),
    ///     TransportControl::new(&cancel),
    ///     LocalFetchObserver::default(),
    /// )?;
    ///
    /// let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
    /// assert!(report.shallow_published);
    ///
    /// // `second` is now a shallow boundary: its parent was not fetched.
    /// let repo = girt::Repository::open(repo.git_dir())?;
    /// assert!(repo.shallow_roots().iter().any(|root| root == second));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_depth(mut self, depth: NonZeroU32) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Requests a partial fetch that leaves out the objects `filter` matches, like `git fetch
    /// --filter`.
    ///
    /// Upload-pack sources that advertise filtering send a pack that omits those objects, and
    /// [`FetchReady::finish`] installs it with a `.promisor` marker so Git treats the omitted
    /// objects as promised by the remote. Other sources send a complete pack; the received
    /// result's [`super::ReceivedFetch::filter_applied`] reports which happened. Native local
    /// sources and bundles ignore the filter, as `git clone --filter` does for a local path.
    ///
    /// This does not record the filter in configuration; [`crate::workflow::fetch`] does.
    ///
    /// ```
    /// use std::collections::BTreeSet;
    ///
    /// use girt::fetch::{FetchRequest, ObjectFilter};
    /// use girt::refs::Reflog;
    /// use girt::remote::Remote;
    /// # let (_directory, _upstream, repo, _main, _) = include!("../doctest/remote.rs");
    ///
    /// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
    /// let request = FetchRequest::prepare(
    ///     repo.clone(),
    ///     origin.fetch_refspecs().clone(),
    ///     BTreeSet::new(),
    ///     Reflog::Preserve,
    /// )?
    /// .with_filter(ObjectFilter::BlobNone);
    /// # let _ = request;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn with_filter(mut self, filter: super::ObjectFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Enables deletion of stale direct remote-tracking and added tag-namespace destinations
    /// owned by these refspecs. Exact snapshot values are required at publication. Built-in
    /// `refs/tags/*` destinations and excluded sources are retained.
    ///
    /// Choose what happens to a pruned reference's reflog with
    /// [`Self::with_reflog_for_update_kind`] and [`FetchUpdateKind::Prune`]; Git deletes it
    /// ([`Reflog::Delete`]).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::fetch::{
    ///     FetchLimits, FetchRequest, FetchUpdateKind, FetchUpdateLimits, KnownHistory,
    ///     LocalFetchObserver,
    /// };
    /// use girt::refs::{Expected, RefName, Reflog, Target};
    /// use girt::remote::Remote;
    /// use girt::transport::TransportControl;
    /// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
    ///
    /// let cancel = AtomicBool::new(false);
    /// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
    /// let fetch = || -> Result<_, Box<dyn std::error::Error>> {
    ///     let request = FetchRequest::prepare(
    ///         repo.clone(),
    ///         origin.fetch_refspecs().clone(),
    ///         BTreeSet::new(),
    ///         Reflog::Preserve,
    ///     )?
    ///     .with_prune();
    ///
    ///     let ready = request.receive_local(
    ///         upstream.git_dir(),
    ///         &KnownHistory::default(),
    ///         FetchLimits::default(),
    ///         TransportControl::new(&cancel),
    ///         LocalFetchObserver::default(),
    ///     )?;
    ///     Ok(ready.finish(FetchUpdateLimits::default(), &cancel)?)
    /// };
    ///
    /// fetch()?;
    ///
    /// // `main` disappears upstream, so the next fetch prunes its remote-tracking reference.
    /// let main_ref = RefName::new("refs/heads/main")?;
    /// upstream
    ///     .references()?
    ///     .delete_without_reflog(&main_ref, Expected::Value(Target::Direct(main)))?;
    ///
    /// let report = fetch()?;
    /// assert_eq!(report.updates[0].kind, FetchUpdateKind::Prune);
    /// assert_eq!(
    ///     repo.references()?
    ///         .read(&RefName::new("refs/remotes/origin/main")?)?,
    ///     None
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_prune(mut self) -> Self {
        self.prune = true;
        self
    }

    /// Retains destinations for absent exact sources while pruning other stale refs.
    ///
    /// The caller supplies only exact positive source names whose cached destinations Git would
    /// keep after a missing-ref retry. This applies to the advertisement from this transfer, so
    /// no second discovery or fetch is needed. The source remains selected if it is advertised.
    pub fn with_retained_missing_sources(
        mut self,
        sources: impl IntoIterator<Item = RefName>,
    ) -> Self {
        self.retained_missing_sources
            .extend(sources.into_iter().map(|source| source.as_bytes().to_vec()));
        self
    }

    /// Maps an advertisement against the captured destination values, without I/O or mutation.
    ///
    /// Useful for previewing policy. Transfer adapters always recompute this plan from their own
    /// advertisement; a preview cannot substitute IDs from an earlier server connection.
    ///
    /// # Errors
    ///
    /// Rejects mapping collisions/missing sources, unsupported destinations, symbolic stored
    /// values, and tag replacements lacking both force intent and authorization. No objects are
    /// requested.
    pub fn plan(&self, advertisement: &Advertisement) -> Result<Vec<FetchUpdate>, FetchPlanError> {
        let mut updates: Vec<FetchUpdate> = self
            .specs
            .map_advertisement_with_missing(advertisement)?
            .0
            .into_iter()
            .map(|mapping| {
                let Some(name) = &mapping.destination else {
                    return Ok(FetchUpdate {
                        mapping,
                        previous: None,
                        kind: FetchUpdateKind::SourceOnly,
                    });
                };
                let bytes = name.as_bytes();
                let tag_destination =
                    bytes.starts_with(b"refs/tags/") || self.additional_tag_destination(name);
                if !bytes.starts_with(b"refs/remotes/") && !tag_destination {
                    return Err(FetchPlanError::Destination(name.clone()));
                }
                let previous = match self.snapshot.get(name) {
                    Some(Target::Symbolic(_)) => {
                        return Err(FetchPlanError::Symbolic(name.clone()));
                    }
                    Some(Target::Direct(id)) => Some(*id),
                    None => None,
                };
                let new = mapping
                    .source
                    .as_ref()
                    .expect("fetch mapping has a source")
                    .id;
                let kind = update_kind(
                    name,
                    previous,
                    new,
                    mapping.force == crate::remote::ForceIntent::Requested,
                    self.authorized_force.contains(name),
                    tag_destination,
                )?;
                Ok(FetchUpdate {
                    mapping,
                    previous,
                    kind,
                })
            })
            .collect::<Result<_, _>>()?;
        if self.prune {
            let advertised: BTreeSet<_> = advertisement
                .refs
                .iter()
                .filter(|reference| reference.kind == crate::fetch::AdvertisementKind::Tip)
                .map(|reference| reference.name.as_bytes().to_vec())
                .collect();
            let selected: BTreeSet<_> = updates
                .iter()
                .filter_map(|update| update.mapping.destination.as_ref())
                .cloned()
                .collect();
            for (name, target) in &self.snapshot {
                if !(name.as_bytes().starts_with(b"refs/remotes/")
                    || self.additional_tag_destination(name))
                    || selected.contains(name)
                {
                    continue;
                }
                let sources = self.specs.prune_sources(name);
                if sources.is_empty()
                    || sources.iter().any(|source| {
                        advertised.contains(source)
                            || self.retained_missing_sources.contains(source.as_slice())
                    })
                {
                    continue;
                }
                let Target::Direct(previous) = target else {
                    // Remote HEAD aliases are local symbolic policy, not stale remote tips.
                    continue;
                };
                updates.push(FetchUpdate {
                    mapping: Mapping {
                        source: None,
                        destination: Some(name.clone()),
                        force: crate::remote::ForceIntent::Normal,
                    },
                    previous: Some(*previous),
                    kind: FetchUpdateKind::Prune,
                });
            }
        }
        Ok(updates)
    }

    /// Validates a portable bundle and applies this request's normal reference mapping policy.
    ///
    /// No storage changes occur until the returned [`FetchReady`] is finished. Depth-limited
    /// requests are refused because bundles carry fixed history rather than negotiation.
    ///
    /// # Errors
    ///
    /// Returns unsupported depth, mapping, prerequisite or pack-validation errors.
    pub fn receive_bundle(
        self,
        bundle: &crate::bundle::Bundle<'_>,
        known: &KnownHistory,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<FetchReady, crate::bundle::Error> {
        if self.depth.is_some() {
            return Err(crate::bundle::Error::UnsupportedOperation(
                "bundle depth request",
            ));
        }
        self.check_known(known)
            .map_err(|error| crate::bundle::Error::Fetch(Box::new(error)))?;
        let updates = self.plan(&bundle.advertisement()).map_err(Box::new)?;
        let received = bundle.receive(known, limits, cancel)?;
        Ok(FetchReady {
            request: self,
            updates,
            received,
        })
    }

    /// Transfers and validates synchronously using a caller-selected local upload-pack endpoint.
    ///
    /// Pass explicit verified `known` history for incremental negotiation, or an empty history for
    /// a full transfer. The transport control bounds network/process lifetime, not later install.
    /// Mapping failures select no wants, then return the saved planning error; no storage changes.
    ///
    /// # Errors
    ///
    /// Returns mapping/policy failures or [`super::receive_local`]'s transfer failures.
    pub fn receive_local(
        self,
        source: impl AsRef<Path>,
        known: &KnownHistory,
        limits: FetchLimits,
        control: TransportControl<'_>,
        mut observer: super::LocalFetchObserver<'_>,
    ) -> Result<FetchReady, FetchWorkflowError> {
        self.receive_local_observed(
            source,
            known,
            limits,
            control,
            |message| {
                observer
                    .messages
                    .as_mut()
                    .map_or(ControlFlow::Continue(()), |notify| notify(message))
            },
            |event| {
                if let Some(notify) = observer.construction.as_mut() {
                    notify(event);
                }
            },
            |event| {
                if let Some(notify) = observer.validation.as_mut() {
                    notify(event);
                }
            },
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent construction and validation observers"
    )]
    fn receive_local_observed(
        self,
        source: impl AsRef<Path>,
        known: &KnownHistory,
        limits: FetchLimits,
        control: TransportControl<'_>,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
        mut observe: impl FnMut(super::LocalFetchProgress),
        validation: impl FnMut(super::ValidationProgress),
    ) -> Result<FetchReady, FetchWorkflowError> {
        self.check_known(known)?;
        let mut plan = None;
        let received = super::local::receive_local_observed(
            source,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            self.depth,
            false,
            limits,
            control,
            progress,
            |event| {
                if event != super::LocalFetchProgress::Complete {
                    observe(event);
                }
            },
            validation,
        );
        let updates = selected(plan, &received)?;
        let ready = FetchReady {
            request: self,
            updates,
            received: received?,
        };
        control.check().map_err(FetchError::from)?;
        observe(super::LocalFetchProgress::Complete);
        Ok(ready)
    }

    /// Transfers from a caller-owned v0/v1 upload-pack stream using this request's depth policy.
    /// The caller owns process/network lifetime and must close and reap it on every outcome.
    ///
    /// # Errors
    ///
    /// Returns planning or wire/pack validation errors without destination changes.
    ///
    /// # Examples
    ///
    /// Run a fetch over a `git upload-pack` process. It needs a Git executable, so it is not run:
    ///
    /// ```no_run
    /// use std::collections::BTreeSet;
    /// use std::ops::ControlFlow;
    /// use std::process::{Command, Stdio};
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::Repository;
    /// use girt::fetch::{FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory};
    /// use girt::refs::Reflog;
    /// use girt::remote::Remote;
    ///
    /// let repo = Repository::open("project")?;
    /// let origin = Remote::find(repo.config(), b"origin")?.ok_or("no origin")?;
    /// let request = FetchRequest::prepare(
    ///     repo.clone(),
    ///     origin.fetch_refspecs().clone(),
    ///     BTreeSet::new(),
    ///     Reflog::Preserve,
    /// )?;
    ///
    /// let mut server = Command::new("git")
    ///     .args(["upload-pack", "/srv/git/project.git"])
    ///     .stdin(Stdio::piped())
    ///     .stdout(Stdio::piped())
    ///     .spawn()?;
    ///
    /// let mut input = server.stdin.take().expect("piped");
    /// let mut output = server.stdout.take().expect("piped");
    ///
    /// let cancel = AtomicBool::new(false);
    /// let result = request.receive_session(
    ///     &mut output,
    ///     &mut input,
    ///     &KnownHistory::default(),
    ///     FetchLimits::default(),
    ///     &cancel,
    ///     |_message| ControlFlow::Continue(()),
    /// );
    ///
    /// // Close the pipes and reap the process whatever happened; stop it first if the fetch failed.
    /// drop((input, output));
    /// if result.is_err() {
    ///     server.kill().ok();
    /// }
    ///
    /// let status = server.wait()?;
    /// let ready = result?;
    ///
    /// // Publish only if the server also reported success.
    /// if !status.success() {
    ///     return Err(format!("git upload-pack failed: {status}").into());
    /// }
    /// ready.finish(FetchUpdateLimits::default(), &cancel)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn receive_session(
        self,
        reader: &mut impl Read,
        writer: &mut impl Write,
        known: &KnownHistory,
        limits: FetchLimits,
        cancel: &AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<FetchReady, FetchWorkflowError> {
        self.check_known(known)?;
        let mut plan = None;
        let received = super::receive_with_known_depth(
            reader,
            writer,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            super::FetchOptions {
                limits,
                depth: self.depth,
                filter: self.filter.clone(),
                explicit_wants: false,
            },
            cancel,
            progress,
        );
        let updates = selected(plan, &received)?;
        Ok(FetchReady {
            request: self,
            updates,
            received: received?,
        })
    }

    pub(super) fn check_known(&self, known: &KnownHistory) -> Result<(), FetchError> {
        if known.shallow != self.shallow_before {
            return Err(FetchError::Unsupported(
                "known shallow boundaries differ from destination",
            ));
        }
        Ok(())
    }
}

fn is_descendant(name: &RefName, namespace: &RefName) -> bool {
    name.as_bytes()
        .strip_prefix(namespace.as_bytes())
        .is_some_and(|suffix| suffix.starts_with(b"/") && suffix.len() > 1)
}

fn update_kind(
    name: &RefName,
    previous: Option<ObjectId>,
    new: ObjectId,
    force: bool,
    authorized: bool,
    tag_destination: bool,
) -> Result<FetchUpdateKind, FetchPlanError> {
    match previous {
        None => Ok(FetchUpdateKind::Create),
        Some(old) if old == new => Ok(FetchUpdateKind::Unchanged),
        Some(_) if tag_destination => {
            if !force || !authorized {
                return Err(FetchPlanError::TagReplacement(name.clone()));
            }
            Ok(FetchUpdateKind::ForcedTag)
        }
        Some(_) => Ok(FetchUpdateKind::Replace),
    }
}

pub(super) fn select(
    result: Result<Vec<FetchUpdate>, FetchPlanError>,
    saved: &mut Option<Result<Vec<FetchUpdate>, FetchPlanError>>,
) -> Vec<ObjectId> {
    let wants = result
        .as_ref()
        .map(|updates| {
            updates
                .iter()
                .filter_map(|update| update.mapping.source.as_ref().map(|s| s.id))
                .collect()
        })
        .unwrap_or_default();
    *saved = Some(result);
    wants
}

pub(super) fn selected<T>(
    plan: Option<Result<Vec<FetchUpdate>, FetchPlanError>>,
    transfer: &Result<T, FetchError>,
) -> Result<Vec<FetchUpdate>, FetchWorkflowError> {
    match plan {
        Some(plan) => Ok(plan?),
        None if transfer.is_err() => Ok(Vec::new()),
        None => Err(FetchError::Protocol("selection callback was not invoked").into()),
    }
}

/// A validated transfer tied to its actual advertisement, destination snapshot and repository.
///
/// Owns all state needed for synchronous installation/publication; can move to a caller's worker.
/// Dropping it has no storage side effects. No public API permits substituting a different
/// transfer, destination or mapping after validation.
///
/// # Examples
///
/// Inspect a validated transfer before deciding to publish it:
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{
///     FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory, LocalFetchObserver,
/// };
/// use girt::refs::Reflog;
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, _, _) = include!("../doctest/remote.rs");
///
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
/// let request = FetchRequest::prepare(
///     repo.clone(),
///     origin.fetch_refspecs().clone(),
///     BTreeSet::new(),
///     Reflog::Preserve,
/// )?;
///
/// let cancel = AtomicBool::new(false);
/// let ready = request.receive_local(
///     upstream.git_dir(),
///     &KnownHistory::default(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
///
/// // A commit and its empty tree, nothing written yet.
/// assert_eq!(ready.received().object_count(), 2);
/// assert!(repo.references()?.list()?.is_empty());
///
/// let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
/// assert!(report.installed.is_some());
/// assert_eq!(report.references.len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct FetchReady {
    pub(super) request: FetchRequest,
    pub(super) updates: Vec<FetchUpdate>,
    pub(super) received: ReceivedFetch,
}

impl FetchReady {
    // Clone supplies a fresh repository; use only the actual transfer advertisement.
    pub(crate) fn for_clone(
        request: FetchRequest,
        received: ReceivedFetch,
    ) -> Result<Self, FetchPlanError> {
        let updates = request.plan(received.advertisement())?;
        Ok(Self {
            request,
            updates,
            received,
        })
    }

    /// The mapping and decisions derived from the advertisement used by this transfer.
    pub fn updates(&self) -> &[FetchUpdate] {
        &self.updates
    }

    /// Validated object response, available for inspecting advertisement and transfer statistics.
    pub fn received(&self) -> &ReceivedFetch {
        &self.received
    }

    /// Installs a self-contained pack under retention before publishing shallow metadata or refs.
    ///
    /// Supports self-contained complete and depth-limited transfers, including refresh of an
    /// existing shallow destination. Known-local dependencies and empty transfers are refused.
    /// Holds the conditional shallow lock from installation through the returned phase's
    /// publication, including depth requests whose response has no boundaries. An existing pack or
    /// index is refused because a collector may already have selected it for removal.
    ///
    /// The returned phase permits caller-specific object checks before any shallow or reference
    /// publication. Call [`RetainedFetchReady::finish`] to publish, or
    /// [`RetainedFetchReady::into_recovery`] to abandon the phase with its effects and retention.
    /// Dropping the phase releases its shallow lock but leaves the retention marker on disk.
    /// The caller still owns the GC and HEAD/worktree exclusions on [`FetchRequest`]; retention
    /// protects only this newly installed pack. No references or reflogs change in this method.
    ///
    /// # Errors
    ///
    /// Returns validation, shallow-lock, or installation failures with completed effects. Once
    /// retention is acquired, errors retain its handle; a failed installation can leave partial
    /// pack artifacts even when the report has no successful installation. Do not retry through
    /// another fetch implementation after acquiring retention without inspecting those effects.
    pub fn install_retained(
        mut self,
        limits: FetchUpdateLimits,
        cancel: &AtomicBool,
    ) -> Result<RetainedFetchReady, RetainedFetchFinishError> {
        let mut report = self.take_report();
        let mut prepare = || {
            report.integrity = self
                .received
                .enforce_fsck(
                    self.request.repository.config(),
                    crate::fsck::Scope::Fetch,
                    self.request
                        .repository
                        .worktree()
                        .unwrap_or(self.request.repository.git_dir()),
                    cancel,
                )
                .map_err(|error| FetchFinishFailure::Integrity(Box::new(error)))?;
            let checksum = self
                .received
                .retention_checksum(&self.request.repository, cancel)
                .map_err(FetchFinishFailure::Installation)?;
            let shallow_lock = self.lock_shallow(true, cancel)?;
            Ok((checksum, shallow_lock))
        };
        let (checksum, shallow_lock) = match prepare() {
            Ok(prepared) => prepared,
            Err(source) => {
                return Err(RetainedFetchFinishError {
                    report: Box::new(report),
                    source,
                    retention: None,
                });
            }
        };
        let installation =
            super::retention::install_with(&self.request.repository, checksum, || {
                self.received.install_for_workflow::<false>(
                    &self.request.repository,
                    limits.snapshot,
                    cancel,
                    shallow_lock.is_some(),
                )
            });
        let (installed, retention) = match installation {
            Ok(result) => result,
            Err(error) => {
                return Err(RetainedFetchFinishError {
                    report: Box::new(report),
                    source: FetchFinishFailure::Installation(*error.source).into(),
                    retention: error.retention,
                });
            }
        };
        report.installed = Some(installed);
        Ok(RetainedFetchReady {
            ready: self,
            report,
            limits,
            shallow_lock,
            retention,
        })
    }

    /// Installs validated objects and shallow metadata, then publishes changed destinations.
    ///
    /// Rechecks known-local dependencies during installation, installed selected-tip readability,
    /// update ancestry and supported HEAD
    /// layout before publication. All changed destinations use the exact values captured by
    /// preparation, including absence. Source-only and unchanged entries produce no reference
    /// edits or reflog entries. Object installation can succeed even when later checks or
    /// reference publication fail. A shallow metadata change can also remain after a later
    /// reference failure. Cancellation is checked before installation and before the
    /// transaction, but does not interrupt a started reference transaction. Observe completion
    /// before releasing worker resources.
    ///
    /// The caller must coordinate GC and worktree/HEAD changes as described on [`FetchRequest`].
    /// Transaction locks do not protect objects from external deletion. Readers can observe partial
    /// reference publication; neither whole-operation rollback nor crash durability is promised.
    ///
    /// # Errors
    ///
    /// The returned report distinguishes installation success from publication. An installation
    /// failure can leave an unindexed pack (see [`ReceivedFetch::install`]); no ref edits were
    /// made. After successful installation, all installed objects remain on error. Transaction
    /// errors retain exact per-ref/per-log partial effects. No automatic cleanup, rollback or
    /// retry occurs.
    pub fn finish(
        self,
        limits: FetchUpdateLimits,
        cancel: &AtomicBool,
    ) -> Result<FetchReport, FetchFinishError> {
        self.finish_with::<false>(limits, FetchFinishLimits::default(), cancel)
    }

    pub(super) fn finish_controlled(
        self,
        limits: FetchUpdateLimits,
        admission: FetchFinishLimits,
        cancel: &AtomicBool,
    ) -> Result<FetchReport, FetchFinishError> {
        self.finish_with::<true>(limits, admission, cancel)
    }

    fn finish_with<const CONTROLLED: bool>(
        mut self,
        limits: FetchUpdateLimits,
        admission: FetchFinishLimits,
        cancel: &AtomicBool,
    ) -> Result<FetchReport, FetchFinishError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.finish",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let mut report = self.take_report();
            let result = self.install_publish::<CONTROLLED>(limits, admission, cancel, &mut report);
            match result {
                Ok(()) => Ok(report),
                Err(source) => Err(FetchFinishError {
                    report: Box::new(report),
                    source,
                }),
            }
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| {
            crate::trace::fetch_finish(error, &span)
        });

        result
    }

    fn install_publish<const CONTROLLED: bool>(
        &self,
        limits: FetchUpdateLimits,
        admission: FetchFinishLimits,
        cancel: &AtomicBool,
        report: &mut FetchReport,
    ) -> Result<(), Box<FetchFinishFailure>> {
        if CONTROLLED {
            super::finish::admit(&self.request.repository, admission, cancel)?;
            if self.request.snapshot.len() > admission.preparation.max_ref_entries {
                return Err(FetchFinishFailure::Safety(FetchPlanError::PrepareLimit(
                    "prepared references",
                ))
                .into());
            }
            if self.request.shallow_before.len() > admission.preparation.max_shallow_roots {
                return Err(FetchFinishFailure::Safety(FetchPlanError::PrepareLimit(
                    "prepared shallow roots",
                ))
                .into());
            }
            if self.received.shallow_roots().len() > admission.preparation.max_shallow_roots {
                return Err(FetchFinishFailure::Safety(FetchPlanError::PrepareLimit(
                    "received shallow roots",
                ))
                .into());
            }
        }
        report.integrity = self
            .received
            .enforce_fsck(
                self.request.repository.config(),
                crate::fsck::Scope::Fetch,
                self.request
                    .repository
                    .worktree()
                    .unwrap_or(self.request.repository.git_dir()),
                cancel,
            )
            .map_err(|error| FetchFinishFailure::Integrity(Box::new(error)))?;
        let mut shallow_lock = self.lock_shallow(false, cancel)?;
        report.installed = Some(
            self.received
                .install_for_workflow::<CONTROLLED>(
                    &self.request.repository,
                    limits.snapshot,
                    cancel,
                    shallow_lock.is_some(),
                )
                .map_err(FetchFinishFailure::Installation)?,
        );
        self.publish_installed::<CONTROLLED>(limits, admission, cancel, &mut shallow_lock, report)
    }

    fn take_report(&mut self) -> FetchReport {
        FetchReport {
            updates: std::mem::take(&mut self.updates),
            pack_bytes: self.received.pack_bytes(),
            objects: self.received.object_count(),
            installed: None,
            integrity: None,
            references: Vec::new(),
            shallow_published: false,
            missing: self
                .request
                .specs
                .map_advertisement_with_missing(self.received.advertisement())
                .expect("validated fetch mapping")
                .1,
        }
    }

    fn lock_shallow(
        &self,
        retained: bool,
        cancel: &AtomicBool,
    ) -> Result<Option<super::shallow::Lock>, Box<FetchFinishFailure>> {
        let resulting_roots = self.received.shallow_roots();
        if self.received.wants().is_empty() && self.request.shallow_before != resulting_roots {
            return Err(FetchFinishFailure::Safety(FetchPlanError::Shallow).into());
        }
        if self.request.depth.is_none() && self.request.shallow_before != resulting_roots {
            return Err(FetchFinishFailure::Safety(FetchPlanError::Shallow).into());
        }
        let shallow_lock =
            if retained || !self.request.shallow_before.is_empty() || !resulting_roots.is_empty() {
                Some(
                    super::shallow::Lock::acquire(
                        self.request.repository.common_dir(),
                        self.request.repository.object_format(),
                        &self.request.shallow_before,
                        cancel,
                    )
                    .map_err(FetchFinishFailure::Shallow)?,
                )
            } else {
                None
            };
        Ok(shallow_lock)
    }

    fn publish_installed<const CONTROLLED: bool>(
        &self,
        limits: FetchUpdateLimits,
        admission: FetchFinishLimits,
        cancel: &AtomicBool,
        shallow_lock: &mut Option<super::shallow::Lock>,
        report: &mut FetchReport,
    ) -> Result<(), Box<FetchFinishFailure>> {
        if CONTROLLED {
            super::finish::admit(&self.request.repository, admission, cancel)?;
        }
        let resulting_roots = self.received.shallow_roots();
        if !self.received.wants().is_empty() && self.request.shallow_before != resulting_roots {
            let objects = if CONTROLLED {
                self.request.repository.objects_controlled(
                    limits.snapshot,
                    Default::default(),
                    cancel,
                )
            } else {
                self.request.repository.objects_with(limits.snapshot)
            }
            .map_err(|error| {
                FetchFinishFailure::BeforePublication(FetchError::Destination(error))
            })?;
            let mut verify_roots = self.received.wants().to_vec();
            verify_roots.extend(self.request.shallow_before.iter().copied());
            let verified = KnownHistory::new_with_boundaries(
                &objects,
                &verify_roots,
                resulting_roots,
                limits.verification,
                cancel,
            )
            .map_err(FetchFinishFailure::BeforePublication)?;
            if resulting_roots
                .iter()
                .any(|root| !verified.objects.contains_key(root))
            {
                return Err(FetchFinishFailure::BeforePublication(FetchError::Protocol(
                    "unrelated shallow boundary",
                ))
                .into());
            }
        }
        if let Some(lock) = shallow_lock {
            report.shallow_published = lock
                .publish(resulting_roots)
                .map_err(FetchFinishFailure::Shallow)?;
        }
        super::check_cancelled(cancel).map_err(FetchFinishFailure::BeforePublication)?;
        if !CONTROLLED {
            super::worktree::check(&self.request.repository).map_err(FetchFinishFailure::Safety)?;
        }
        if !self.received.wants().is_empty() {
            let reopened = if CONTROLLED {
                super::finish::admit(&self.request.repository, admission, cancel)?
            } else {
                Repository::open(self.request.repository.git_dir())
                    .map_err(FetchFinishFailure::Reopen)?
            };
            let objects = if CONTROLLED {
                reopened.objects_controlled(limits.snapshot, Default::default(), cancel)
            } else {
                reopened.objects_with(limits.snapshot)
            }
            .map_err(|error| {
                FetchFinishFailure::BeforePublication(FetchError::Destination(error))
            })?;
            // Prove the received representation is still installed, and honor destination lookup
            // precedence before any ref can name this graph. Different copies stay checked.
            // History reachable from references that existed before the fetch was verified when
            // those references were published; only new history is walked. Validation found the
            // complete dependencies through verified edges from those same references.
            let mut verify_roots = self.received.wants().to_vec();
            verify_roots.extend(self.request.shallow_before.iter().copied());
            let mut published: crate::IdSet = self
                .request
                .snapshot
                .values()
                .filter_map(|target| match target {
                    Target::Direct(id) => Some(*id),
                    Target::Symbolic(_) => None,
                })
                .collect();
            // Stop only where the representation lookup now selects has the kind that evidence
            // requires; anything else fails before a reference can name this graph.
            for (&id, &required) in self.received.complete_dependencies() {
                super::check_cancelled(cancel).map_err(FetchFinishFailure::BeforePublication)?;
                let located = objects
                    .locate(id, limits.verification.known_read, cancel)
                    .map_err(|source| {
                        FetchFinishFailure::BeforePublication(FetchError::LocalRead { id, source })
                    })?;
                match located {
                    None => {
                        return Err(
                            FetchFinishFailure::BeforePublication(FetchError::Missing(id)).into(),
                        );
                    }
                    Some((kind, _)) if kind != required => {
                        return Err(
                            FetchFinishFailure::BeforePublication(FetchError::Kind(id)).into()
                        );
                    }
                    Some(_) => {
                        published.insert(id);
                    }
                }
            }
            let shallow: Vec<_> = objects.shallow_roots().iter().collect();
            let verified =
                super::verified::InstalledObjects::bind(&objects, &self.received, cancel)
                    .map_err(FetchFinishFailure::BeforePublication)?;
            KnownHistory::verify_installed(
                &verified,
                &verify_roots,
                self.received.dependencies(),
                &shallow,
                &published,
                limits.verification,
                cancel,
            )
            .map_err(FetchFinishFailure::BeforePublication)?;
            let validation = super::update::validate(
                &mut report.updates,
                &verified,
                &self.request.authorized_force,
                limits,
                cancel,
            );
            validation.map_err(FetchFinishFailure::Update)?;
        }
        super::check_cancelled(cancel).map_err(FetchFinishFailure::BeforePublication)?;
        if CONTROLLED {
            let maximum = match self.request.repository.reference_backend() {
                crate::refs::Backend::Files => admission.files_transaction.max_edits,
                crate::refs::Backend::Reftable => admission.preparation.reftable.records.records,
            };
            let mut changes = 0;
            for (position, update) in report.updates.iter().enumerate() {
                if position % 64 == 0 {
                    super::check_cancelled(cancel)
                        .map_err(FetchFinishFailure::BeforePublication)?;
                }
                if update.kind.changes_ref() {
                    if changes >= maximum {
                        return Err(FetchFinishFailure::Safety(FetchPlanError::PrepareLimit(
                            "reference edits",
                        ))
                        .into());
                    }
                    changes += 1;
                }
            }
        }
        let edits: Vec<_> = report
            .updates
            .iter()
            .filter(|update| update.kind.changes_ref())
            .map(|update| {
                let name = update
                    .mapping
                    .destination
                    .as_ref()
                    .expect("changed destination");
                RefEdit {
                    name: name.clone(),
                    dereference: crate::refs::Dereference::Stored,
                    target: update
                        .mapping
                        .source
                        .as_ref()
                        .map(|source| Target::Direct(source.id)),
                    expected: update
                        .previous
                        .map(|id| Expected::Value(Target::Direct(id)))
                        .unwrap_or(Expected::Absent),
                    reflog: self.request.reflog_for(name, update.kind),
                }
            })
            .collect();
        let refs = self
            .request
            .repository
            .references()
            .map_err(FetchPlanError::from)
            .map_err(FetchFinishFailure::Safety)?;
        let refs = if CONTROLLED {
            refs.with_reftable_limits(admission.preparation.reftable)
        } else {
            refs
        };
        // Like `git fetch --prune`, prune before updating so a pruned name can become a
        // namespace for new references (or the reverse).
        let (prunes, updates): (Vec<_>, Vec<_>) =
            edits.into_iter().partition(|edit| edit.target.is_none());
        report.references = if CONTROLLED {
            super::finish::transaction(
                &refs,
                self.request.repository.reference_backend(),
                &prunes,
                admission,
                cancel,
            )
        } else {
            refs.transaction(&prunes)
        }
        .map_err(FetchFinishFailure::Publication)?;
        let updated = if CONTROLLED {
            super::finish::transaction(
                &refs,
                self.request.repository.reference_backend(),
                &updates,
                admission,
                cancel,
            )
        } else {
            refs.transaction(&updates)
        };
        match updated {
            Ok(outcomes) => report.references.extend(outcomes),
            Err(crate::refs::TransactionError::Prepare { source, .. })
                if !report.references.is_empty() =>
            {
                return Err(Box::new(FetchFinishFailure::Publication(
                    crate::refs::TransactionError::Publish {
                        outcomes: std::mem::take(&mut report.references),
                        source,
                    },
                )));
            }
            Err(crate::refs::TransactionError::Publish {
                mut outcomes,
                source,
            }) => {
                let mut all = std::mem::take(&mut report.references);
                all.append(&mut outcomes);
                return Err(Box::new(FetchFinishFailure::Publication(
                    crate::refs::TransactionError::Publish {
                        outcomes: all,
                        source,
                    },
                )));
            }
            Err(error) => return Err(Box::new(FetchFinishFailure::Publication(error))),
        }
        Ok(())
    }
}

/// An installed pack retained through caller validation and shallow/reference publication.
///
/// Owns the same shallow lock acquired before installation. No shallow metadata, refs, or reflogs
/// have changed yet. Read [`Self::updates`] and installed objects to apply caller-specific checks,
/// then consume this phase with [`Self::finish`] or [`Self::into_recovery`]. Dropping it releases
/// the lock but deliberately leaves its Git `.keep` marker for recovery.
///
/// # Examples
///
/// Install the pack, check it before publishing anything, then publish and release the
/// retention marker:
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{
///     FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory, LocalFetchObserver,
/// };
/// use girt::refs::{RefName, Reflog, Target};
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
///
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
/// let request = FetchRequest::prepare(
///     repo.clone(),
///     origin.fetch_refspecs().clone(),
///     BTreeSet::new(),
///     Reflog::Preserve,
/// )?;
///
/// let cancel = AtomicBool::new(false);
/// let ready = request.receive_local(
///     upstream.git_dir(),
///     &KnownHistory::default(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
///
/// let retained = ready.install_retained(FetchUpdateLimits::default(), &cancel)?;
/// // The pack is installed and protected; no reference has moved yet.
/// assert!(retained.report().installed.is_some());
/// let tracking = RefName::new("refs/remotes/origin/main")?;
/// assert_eq!(repo.references()?.read(&tracking)?, None);
///
/// // ... the application's own checks of the installed objects go here ...
///
/// let (report, retention) = retained.finish(&cancel)?;
/// assert_eq!(report.references.len(), 1);
/// assert_eq!(
///     repo.references()?.read(&tracking)?,
///     Some(Target::Direct(main))
/// );
/// retention.release()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[must_use = "publish references or recover the retained installation explicitly"]
pub struct RetainedFetchReady {
    ready: FetchReady,
    report: FetchReport,
    limits: FetchUpdateLimits,
    shallow_lock: Option<super::shallow::Lock>,
    retention: super::FetchRetention,
}

impl RetainedFetchReady {
    /// Original planned updates, before final object-kind and ancestry validation.
    pub fn updates(&self) -> &[FetchUpdate] {
        &self.report.updates
    }

    /// Completed installation effects; shallow and reference publication have not started.
    pub fn report(&self) -> &FetchReport {
        &self.report
    }

    /// Owned pack retention, available for recording its recovery path.
    pub fn retention(&self) -> &super::FetchRetention {
        &self.retention
    }

    /// Publishes shallow boundaries and conditional refs while holding the installation's lock.
    ///
    /// Performs the same installed-object, ancestry, worktree and reference checks as
    /// [`FetchReady::finish`], without installing again or reacquiring the shallow lock.
    /// Returns retention for explicit release after persistent roots are established. Source-only
    /// callers must establish their own roots before releasing it.
    ///
    /// # Errors
    ///
    /// Returns completed installation/shallow effects, exact transaction failure details, and
    /// retention. Objects and published boundaries remain on failure; dropping the error leaves
    /// the marker intact. No rollback or retry occurs. See [`FetchReady::finish`] for the
    /// reference transaction's cancellation and partial-publication contract.
    pub fn finish(
        mut self,
        cancel: &AtomicBool,
    ) -> Result<(FetchReport, super::FetchRetention), RetainedFetchFinishError> {
        let result = self.ready.publish_installed::<false>(
            self.limits,
            FetchFinishLimits::default(),
            cancel,
            &mut self.shallow_lock,
            &mut self.report,
        );
        match result {
            Ok(()) => Ok((self.report, self.retention)),
            Err(source) => Err(RetainedFetchFinishError {
                report: Box::new(self.report),
                source,
                retention: Some(self.retention),
            }),
        }
    }

    /// Abandons publication and releases the shallow lock, preserving effects and pack retention.
    ///
    /// No shallow metadata or references have changed. The caller can retain the returned marker
    /// while inspecting objects, or explicitly release it when abandoning the installation.
    pub fn into_recovery(self) -> (FetchReport, super::FetchRetention) {
        (self.report, self.retention)
    }
}

/// Retained workflow failure with completed effects and ownership of any acquired marker.
///
/// Like [`FetchFinishError`], with one addition: if the `.keep` marker was acquired,
/// `retention` owns it. Dropping the error leaves the marker on disk, protecting the installed
/// pack from repacking. Release it with [`FetchRetention::release`](super::FetchRetention::release)
/// once references that keep the objects reachable are published, or when abandoning them.
#[derive(Debug, thiserror::Error)]
#[error("fetch finish failed")]
#[non_exhaustive]
pub struct RetainedFetchFinishError {
    /// Completed effects. No successful installation does not exclude partial pack artifacts.
    pub report: Box<FetchReport>,
    /// Failed phase, including exact partial reference and reflog effects on transaction failure.
    #[source]
    pub source: Box<FetchFinishFailure>,
    /// Acquired retention; dropping this error leaves its marker on disk for recovery.
    pub retention: Option<super::FetchRetention>,
}

/// One advertised source and its proposed local effect, preserving refspec order.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct FetchUpdate {
    /// Actual advertised source, optional destination and force intent.
    pub mapping: Mapping,
    /// Captured direct destination value, or absence (also `None` for source-only selections).
    pub previous: Option<ObjectId>,
    /// Update rule applied to the captured values.
    pub kind: FetchUpdateKind,
}

/// Decision based on the advertised identity and captured destination, before publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FetchUpdateKind {
    /// Objects selected without a reference destination; no `FETCH_HEAD` is written.
    SourceOnly,
    /// Destination was absent.
    Create,
    /// Destination already named the advertised ID; it will not be locked or rewritten.
    Unchanged,
    /// Remote-tracking replacement awaiting object checks; after successful finish, at least one
    /// tip does not peel to a commit, so no ancestry rule applies.
    Replace,
    /// Both tips peel to commits and the old commit is an ancestor of the new commit.
    FastForward,
    /// Non-fast-forward commit-valued remote-tracking replacement with intent and authorization.
    ForcedTracking,
    /// Explicitly authorized replacement of a tag with a forced refspec.
    ForcedTag,
    /// Delete a stale direct remote-tracking reference in the requested prune scope.
    Prune,
    /// Replace a tip below `refs/prefetch/` without an ancestry or force requirement.
    ///
    /// Only the internal maintenance prefetch planner produces this outcome. Generic fetch
    /// planning and public receive entry points never produce it. Installed-object validation still
    /// runs before publication.
    PrefetchReplace,
}
impl FetchUpdateKind {
    fn changes_ref(self) -> bool {
        matches!(
            self,
            Self::Create
                | Self::Replace
                | Self::FastForward
                | Self::ForcedTracking
                | Self::ForcedTag
                | Self::Prune
                | Self::PrefetchReplace
        )
    }
}

/// Transfer statistics and separately observable installation/publication effects.
///
/// # Examples
///
/// Summarize a fetch the way `git fetch` reports it:
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{
///     FetchLimits, FetchRequest, FetchUpdateKind, FetchUpdateLimits, KnownHistory,
///     LocalFetchObserver,
/// };
/// use girt::refs::Reflog;
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
///
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
/// let request = FetchRequest::prepare(
///     repo.clone(),
///     origin.fetch_refspecs().clone(),
///     BTreeSet::new(),
///     Reflog::Preserve,
/// )?;
///
/// let cancel = AtomicBool::new(false);
/// let ready = request.receive_local(
///     upstream.git_dir(),
///     &KnownHistory::default(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
///
/// let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
///
/// for update in &report.updates {
///     let Some(destination) = &update.mapping.destination else {
///         continue;
///     };
///
///     let marker = match update.kind {
///         FetchUpdateKind::Create => "* [new]",
///         FetchUpdateKind::FastForward => "  [updated]",
///         FetchUpdateKind::ForcedTracking | FetchUpdateKind::ForcedTag => "+ [forced]",
///         FetchUpdateKind::Prune => "- [deleted]",
///         _ => continue,
///     };
///     println!(
///         "{marker} {}",
///         String::from_utf8_lossy(destination.as_bytes())
///     );
/// }
///
/// assert_eq!(report.updates[0].kind, FetchUpdateKind::Create);
/// assert_eq!(report.objects, 2);
/// # let _ = main;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct FetchReport {
    /// Decisions against the captured local values; not evidence of publication by themselves.
    pub updates: Vec<FetchUpdate>,
    /// Validated pack bytes, zero for an empty or known-only selection.
    pub pack_bytes: usize,
    /// Verified received objects, including extras; not a count of newly stored objects.
    pub objects: usize,
    /// Successful installation/reuse, including known-only installation with no pack.
    /// `None` on failure does not exclude an unindexed residual pack.
    pub installed: Option<FetchInstalled>,
    /// Optional accepted received-pack check, completed before installation.
    /// Rejected findings are retained in the failure's [`crate::fsck::TransferError::Rejected`].
    pub integrity: Option<crate::fsck::TransferReport>,
    /// Shallow metadata was replaced after object installation, before reference publication.
    pub shallow_published: bool,
    /// Successful transaction effects for changed entries only. On transaction failure, consult
    /// [`FetchFinishFailure::Publication`] for partial effects instead.
    pub references: Vec<RefEditOutcome>,
    /// Exact positive sources absent from the advertisement; other valid selections continue.
    pub missing: Vec<Vec<u8>>,
}

/// Planning failed without changing repository contents.
///
/// # Examples
///
/// A fetch may not write local branches, so a refspec that maps into `refs/heads/` is refused
/// once the advertisement is known, before anything is transferred:
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{
///     FetchLimits, FetchPlanError, FetchRequest, FetchWorkflowError, KnownHistory,
///     LocalFetchObserver,
/// };
/// use girt::refs::Reflog;
/// use girt::remote::{Direction, Refspecs};
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, _, _) = include!("../doctest/remote.rs");
///
/// let specs = Refspecs::parse(Direction::Fetch, ["refs/heads/main:refs/heads/main"])?;
/// let request = FetchRequest::prepare(repo.clone(), specs, BTreeSet::new(), Reflog::Preserve)?;
///
/// let cancel = AtomicBool::new(false);
/// let result = request.receive_local(
///     upstream.git_dir(),
///     &KnownHistory::default(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// );
///
/// assert!(matches!(
///     result,
///     Err(FetchWorkflowError::Plan(FetchPlanError::Destination(_)))
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchPlanError {
    /// Received boundaries differ without an explicit depth request.
    #[error("shallow boundary change requires explicit depth request")]
    Shallow,
    /// Invalid or ambiguous advertisement/refspec mapping.
    #[error(transparent)]
    Mapping(#[from] MappingError),
    /// A destination is outside the supported remote-tracking and tag namespaces.
    #[error("unsupported fetch destination: {0:?}")]
    Destination(RefName),
    /// Updating a symbolic destination could bypass namespace rules.
    #[error("symbolic fetch destination: {0:?}")]
    Symbolic(RefName),
    /// Existing tag replacement requires both force intent and explicit name authorization.
    #[error("tag replacement requires force intent and authorization: {0:?}")]
    TagReplacement(RefName),
    /// Reference metadata could not be read.
    #[error(transparent)]
    Reference(#[from] ReferenceError),
    /// A main/linked worktree could not be opened; stale registrations fail closed.
    #[error(transparent)]
    Worktree(#[from] crate::OpenError),
    /// Worktree enumeration failed.
    #[error("worktree enumeration")]
    Io(#[from] std::io::Error),
    /// HEAD or its symbolic branch chain escapes the protected branch namespace.
    #[error("unsupported HEAD chain at {0:?}")]
    Head(RefName),
    /// Controlled preparation observed cancellation; no request or filesystem effects remain.
    #[error("fetch preparation cancelled")]
    Cancelled,
    /// Controlled preparation exceeded a non-reference metadata budget.
    #[error("fetch preparation limit exceeded: {0}")]
    PrepareLimit(&'static str),
    /// Controlled preparation cannot safely admit linked worktrees yet.
    ///
    /// Maintenance callers may report this as skipped. The legacy entry point still supports
    /// linked worktrees with trusted metadata. Bounded opening is tracked in
    /// [#896](https://github.com/joshka/girt/issues/896).
    #[error("controlled fetch preparation does not admit linked worktrees")]
    LinkedWorktrees,
}

/// A failure before any installation or reference publication.
///
/// The repository is unchanged, so the fetch can be retried as is.
/// [`Plan`](Self::Plan) means the refspecs or destination policy cannot apply to what the remote
/// advertised; retrying won't help without changing them. [`Transfer`](Self::Transfer) covers
/// the transport, the protocol, validation, limits, cancellation and deadlines; see
/// [`FetchError`] for each cause.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchWorkflowError {
    /// Mapping or destination policy rejected the actual advertisement.
    #[error(transparent)]
    Plan(#[from] FetchPlanError),
    /// Transfer or synchronous validation failed, with no installed objects.
    #[error(transparent)]
    Transfer(#[from] FetchError),
}

/// Installation/publication failure retaining completed effects.
///
/// Returned by [`FetchReady::finish`] after the transfer was validated. Nothing is rolled back,
/// so inspect `report`, and [`FetchFinishFailure`] for how far the fetch got, before deciding
/// what to do:
///
/// - `report.installed` is `Some` only if the pack was installed; a failed installation can leave
///   an unindexed pack file behind.
/// - `report.shallow_published` says whether `.git/shallow` changed.
/// - A [`FetchFinishFailure::Publication`] error carries the reference and reflog edits that were
///   published before the failure.
///
/// Fetching again with a fresh [`FetchRequest`] is always safe. See [`FetchRequest::prepare`] for
/// an example that matches a refused non-fast-forward update.
#[derive(Debug, thiserror::Error)]
#[error("fetch finish failed")]
#[non_exhaustive]
pub struct FetchFinishError {
    /// Transfer and installation effects before failure; installed objects are never rolled back.
    pub report: Box<FetchReport>,
    /// Failed phase, including the existing transaction's partial-effect contract.
    /// Boxed to keep the returned error compact across platforms.
    #[source]
    pub source: Box<FetchFinishFailure>,
}

/// Failed phase after validated transfer.
///
/// [`FetchReady::finish`] takes the shallow lock if the boundaries change or the destination is
/// already shallow, installs the pack,
/// verifies the installed history, publishes the shallow file, rechecks worktrees, checks the
/// update rules and finally publishes references. The variant says which step failed; the report
/// in [`FetchFinishError`] has the exact effects.
///
/// | Variant | Pack installed | `.git/shallow` | References |
/// | --- | --- | --- | --- |
/// | [`Installation`](Self::Installation) | no, but maybe an unindexed pack | unchanged | unchanged |
/// | [`Shallow`](Self::Shallow) | no if the lock was refused, yes if publishing failed | unchanged | unchanged |
/// | [`Safety`](Self::Safety) | no for a refused shallow change, which is checked first; otherwise yes | maybe changed | unchanged |
/// | [`BeforePublication`](Self::BeforePublication) | yes | maybe changed | unchanged |
/// | [`Reopen`](Self::Reopen) | yes | maybe changed | unchanged |
/// | [`Update`](Self::Update) | yes | maybe changed | unchanged |
/// | [`Publication`](Self::Publication) | yes | maybe changed | possibly some; see the transaction error |
///
/// Fetching again with a new [`FetchRequest`] is safe after any of them. Before retrying,
/// address the cause: [`Safety`](Self::Safety) means a worktree's `HEAD` or registration changed
/// during the fetch, or the response would change shallow boundaries the request did not ask to
/// change, [`Update`](Self::Update) is usually a refused non-fast-forward, and
/// [`Publication`](Self::Publication) usually means another writer moved a destination.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchFinishFailure {
    /// Configured received-pack integrity policy refused the pack before installation.
    #[error("fetch integrity policy")]
    Integrity(#[from] Box<crate::fsck::TransferError>),
    /// Installation failed; refs unchanged, possibly leaving an unindexed pack.
    #[error("fetch installation")]
    Installation(#[source] FetchError),
    /// Shallow lock, snapshot comparison, or metadata publication failed.
    #[error("fetch shallow publication")]
    Shallow(#[from] super::FetchShallowError),
    /// Controlled repository admission or a reopen with fresh shallow boundaries failed.
    #[error("fetch repository reopen")]
    Reopen(#[from] crate::OpenError),
    /// Cancellation or installed-object verification failed before the reference transaction.
    #[error("before fetch publication")]
    BeforePublication(#[source] FetchError),
    /// Controlled admission or a worktree/HEAD safety recheck failed; refs unchanged.
    #[error("fetch publication safety")]
    Safety(#[from] FetchPlanError),
    /// Object-kind/ancestry validation or update authorization failed; installed objects remain,
    /// but no refs have changed.
    #[error("fetch update validation")]
    Update(#[from] super::FetchUpdateError),
    /// Exact transaction preparation or partial publication failure; objects remain installed.
    #[error("fetch reference publication")]
    Publication(#[from] crate::refs::TransactionError),
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "single_pass_tests.rs"]
mod single_pass_tests;

#[cfg(test)]
#[path = "prefetch_plan_tests.rs"]
mod prefetch_plan_tests;

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "internal prefetch transfer composition follows separately"
    )
)]
#[path = "prefetch_plan.rs"]
mod prefetch_plan;
