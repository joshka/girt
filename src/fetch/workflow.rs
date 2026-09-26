use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{
    Advertisement, FetchError, FetchInstalled, FetchLimits, FetchUpdateLimits, KnownHistory,
    ReceivedFetch,
};
use crate::refs::{Expected, RefEdit, RefEditOutcome, RefName, ReferenceError, Reflog, Target};
use crate::remote::{Direction, Mapping, MappingError, Refspecs};
use crate::transport::TransportControl;
use crate::{ObjectId, Repository};

/// A synchronous destination snapshot and explicit policy for one fetch.
///
/// Construct with a named remote's [`crate::remote::Remote::fetch_refspecs`] or an explicit list.
/// Endpoint selection and credentials remain separate transport arguments. Only `refs/remotes/*`
/// and `refs/tags/*` destinations are supported; branch and symbolic destinations are rejected.
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
/// continue. [`Self::with_prune`] permits scoped remote-tracking deletion. Shallow depth changes
/// use a conditional metadata lock and are reported separately from object/ref effects.
///
/// This workflow does not write `FETCH_HEAD`, infer tags, clone, check out, edit
/// configuration, discover credentials, or implement the full Git CLI. See
/// `examples/fetch_remote.rs`.
#[derive(Debug)]
pub struct FetchRequest {
    repository: Repository,
    specs: Refspecs,
    snapshot: BTreeMap<RefName, Target>,
    authorized_force: BTreeSet<RefName>,
    reflog: Reflog,
    prune: bool,
    pub(super) depth: Option<NonZeroU32>,
    shallow_before: Vec<ObjectId>,
}

impl FetchRequest {
    /// Captures destination values and validates the supported worktree layout without writing.
    ///
    /// `authorized_force` authorizes replacement only for those exact destination names; it does
    /// not turn an unforced refspec into a forced one. `reflog` explicitly chooses preservation
    /// or an append identity/message for changed refs. Reference enumeration has its existing
    /// unbounded metadata allocation contract; transfer and object budgets are supplied
    /// separately.
    ///
    /// # Errors
    ///
    /// Rejects push refspecs, unsupported HEAD chains, inaccessible worktrees, or reference errors.
    /// Mapping-specific failures are returned when the transfer advertisement is available.
    pub fn prepare(
        repository: Repository,
        specs: Refspecs,
        authorized_force: BTreeSet<RefName>,
        reflog: Reflog,
    ) -> Result<Self, FetchPlanError> {
        if specs.direction() != Direction::Fetch {
            return Err(FetchPlanError::Mapping(MappingError::Direction));
        }
        super::worktree::check(&repository)?;
        let snapshot = repository
            .references()?
            .list()?
            .into_iter()
            .map(|reference| (reference.name, reference.target))
            .collect();
        let shallow_before = repository.shallow_roots().iter().collect();
        Ok(Self {
            repository,
            specs,
            snapshot,
            authorized_force,
            reflog,
            prune: false,
            depth: None,
            shallow_before,
        })
    }

    /// Requests a positive commit depth on HTTP/SSH upload-pack and permits a resulting
    /// shallow-boundary change during coordinated publication.
    pub fn with_depth(mut self, depth: NonZeroU32) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Enables deletion of stale direct remote-tracking destinations owned by these refspecs.
    /// Exact snapshot values are required at publication. Tags and excluded sources are retained.
    pub fn with_prune(mut self) -> Self {
        self.prune = true;
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
                if !bytes.starts_with(b"refs/remotes/") && !bytes.starts_with(b"refs/tags/") {
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
                    mapping.force,
                    self.authorized_force.contains(name),
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
                .filter(|reference| !reference.peeled)
                .map(|reference| reference.name.as_bytes().to_vec())
                .collect();
            let selected: BTreeSet<_> = updates
                .iter()
                .filter_map(|update| update.mapping.destination.as_ref())
                .cloned()
                .collect();
            for (name, target) in &self.snapshot {
                if !name.as_bytes().starts_with(b"refs/remotes/") || selected.contains(name) {
                    continue;
                }
                let sources = self.specs.prune_sources(name);
                if sources.is_empty() || sources.iter().any(|source| advertised.contains(source)) {
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
                        force: false,
                    },
                    previous: Some(*previous),
                    kind: FetchUpdateKind::Prune,
                });
            }
        }
        Ok(updates)
    }

    /// Transfers and validates synchronously using a caller-selected local upload-pack endpoint.
    ///
    /// Pass explicit verified `known` history for incremental negotiation, or an empty history for
    /// a full transfer. The transport control bounds network/process lifetime, not later install.
    /// Mapping failures select no wants, then return the saved planning error; no storage changes.
    ///
    /// # Errors
    ///
    /// Returns mapping/policy failures or [`super::receive_local_with_known`]'s transfer failures.
    pub fn receive_local(
        self,
        source: impl AsRef<Path>,
        known: &KnownHistory,
        limits: FetchLimits,
        control: TransportControl<'_>,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<FetchReady, FetchWorkflowError> {
        self.check_known(known)?;
        if self.depth.is_some() {
            return Err(FetchError::Unsupported("native local depth").into());
        }
        let mut plan = None;
        let received = super::receive_local_with_known(
            source,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            limits,
            control,
            progress,
        );
        let updates = selected(plan, &received)?;
        Ok(FetchReady {
            request: self,
            updates,
            received: received?,
        })
    }

    /// Transfers from a caller-owned v0/v1 upload-pack stream using this request's depth policy.
    /// The caller owns process/network lifetime and must close and reap it on every outcome.
    ///
    /// # Errors
    ///
    /// Returns planning or wire/pack validation errors without destination changes.
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

fn update_kind(
    name: &RefName,
    previous: Option<ObjectId>,
    new: ObjectId,
    force: bool,
    authorized: bool,
) -> Result<FetchUpdateKind, FetchPlanError> {
    match previous {
        None => Ok(FetchUpdateKind::Create),
        Some(old) if old == new => Ok(FetchUpdateKind::Unchanged),
        Some(_) if name.as_bytes().starts_with(b"refs/tags/") => {
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
        mut self,
        limits: FetchUpdateLimits,
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
            let mut report = FetchReport {
                updates: std::mem::take(&mut self.updates),
                pack_bytes: self.received.pack_bytes(),
                objects: self.received.object_count(),
                installed: None,
                references: Vec::new(),
                shallow_published: false,
                missing: self
                    .request
                    .specs
                    .map_advertisement_with_missing(self.received.advertisement())
                    .expect("validated fetch mapping")
                    .1,
            };
            let result = self.install_publish(limits, cancel, &mut report);
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

    fn install_publish(
        &self,
        limits: FetchUpdateLimits,
        cancel: &AtomicBool,
        report: &mut FetchReport,
    ) -> Result<(), Box<FetchFinishFailure>> {
        let resulting_roots = self.received.shallow_roots();
        if self.received.wants().is_empty() && self.request.shallow_before != resulting_roots {
            return Err(FetchFinishFailure::Safety(FetchPlanError::Shallow).into());
        }
        if self.request.depth.is_none() && self.request.shallow_before != resulting_roots {
            return Err(FetchFinishFailure::Safety(FetchPlanError::Shallow).into());
        }
        let mut shallow_lock =
            if !self.request.shallow_before.is_empty() || !resulting_roots.is_empty() {
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
        report.installed = Some(
            self.received
                .install_for_workflow(
                    &self.request.repository,
                    limits.snapshot,
                    cancel,
                    shallow_lock.is_some(),
                )
                .map_err(FetchFinishFailure::Installation)?,
        );
        if !self.received.wants().is_empty() && self.request.shallow_before != resulting_roots {
            let objects = self
                .request
                .repository
                .objects(limits.snapshot)
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
        if let Some(lock) = &mut shallow_lock {
            report.shallow_published = lock
                .publish(resulting_roots)
                .map_err(FetchFinishFailure::Shallow)?;
        }
        super::check_cancelled(cancel).map_err(FetchFinishFailure::BeforePublication)?;
        super::worktree::check(&self.request.repository).map_err(FetchFinishFailure::Safety)?;
        if !self.received.wants().is_empty() {
            let reopened = Repository::open(self.request.repository.git_dir())
                .map_err(FetchFinishFailure::Reopen)?;
            let objects = reopened.objects(limits.snapshot).map_err(|error| {
                FetchFinishFailure::BeforePublication(FetchError::Destination(error))
            })?;
            // Installation does not repair corrupt loose objects shadowing the pack. Read through
            // the destination's actual lookup precedence before any ref can name this graph.
            let mut verify_roots = self.received.wants().to_vec();
            verify_roots.extend(self.request.shallow_before.iter().copied());
            KnownHistory::new(&objects, &verify_roots, limits.verification, cancel)
                .map_err(FetchFinishFailure::BeforePublication)?;
            let validation = super::update::validate(
                &mut report.updates,
                &objects,
                &self.request.authorized_force,
                limits,
                cancel,
            );
            validation.map_err(FetchFinishFailure::Update)?;
        }
        super::check_cancelled(cancel).map_err(FetchFinishFailure::BeforePublication)?;
        let edits: Vec<_> = report
            .updates
            .iter()
            .filter(|update| update.kind.changes_ref())
            .map(|update| RefEdit {
                name: update
                    .mapping
                    .destination
                    .clone()
                    .expect("changed destination"),
                dereference: false,
                target: update
                    .mapping
                    .source
                    .as_ref()
                    .map(|source| Target::Direct(source.id)),
                expected: update
                    .previous
                    .map(|id| Expected::Value(Target::Direct(id)))
                    .unwrap_or(Expected::Absent),
                reflog: self.request.reflog.clone(),
            })
            .collect();
        let refs = self
            .request
            .repository
            .references()
            .map_err(FetchPlanError::from)
            .map_err(FetchFinishFailure::Safety)?;
        report.references = refs
            .transaction(&edits)
            .map_err(FetchFinishFailure::Publication)?;
        Ok(())
    }
}

/// One advertised source and its proposed local effect, preserving refspec order.
#[derive(Clone, Debug)]
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
        )
    }
}

/// Transfer statistics and separately observable installation/publication effects.
#[derive(Debug)]
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
    /// Shallow metadata was replaced after object installation, before reference publication.
    pub shallow_published: bool,
    /// Successful transaction effects for changed entries only. On transaction failure, consult
    /// [`FetchFinishFailure::Publication`] for partial effects instead.
    pub references: Vec<RefEditOutcome>,
    /// Exact positive sources absent from the advertisement; other valid selections continue.
    pub missing: Vec<Vec<u8>>,
}

/// Planning failed without changing repository contents.
#[derive(Debug, thiserror::Error)]
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
    #[error("worktree enumeration: {0}")]
    Io(#[from] std::io::Error),
    /// HEAD or its symbolic branch chain escapes the protected branch namespace.
    #[error("unsupported HEAD chain at {0:?}")]
    Head(RefName),
}

/// A failure before any installation or reference publication.
#[derive(Debug, thiserror::Error)]
pub enum FetchWorkflowError {
    /// Mapping or destination policy rejected the actual advertisement.
    #[error(transparent)]
    Plan(#[from] FetchPlanError),
    /// Transfer or synchronous validation failed, with no installed objects.
    #[error(transparent)]
    Transfer(#[from] FetchError),
}

/// Installation/publication failure retaining completed effects.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct FetchFinishError {
    /// Transfer and installation effects before failure; installed objects are never rolled back.
    pub report: Box<FetchReport>,
    /// Failed phase, including the existing transaction's partial-effect contract.
    /// Boxed to keep the returned error compact across platforms.
    #[source]
    pub source: Box<FetchFinishFailure>,
}

/// Failed phase after validated transfer.
#[derive(Debug, thiserror::Error)]
pub enum FetchFinishFailure {
    /// Installation failed; refs unchanged, possibly leaving an unindexed pack.
    #[error("fetch installation: {0}")]
    Installation(#[source] FetchError),
    /// Shallow lock, snapshot comparison, or metadata publication failed.
    #[error("fetch shallow publication: {0}")]
    Shallow(#[from] super::FetchShallowError),
    /// Repository could not be reopened with freshly published shallow boundaries.
    #[error("fetch repository reopen: {0}")]
    Reopen(#[from] crate::OpenError),
    /// Cancellation or installed-object verification failed before the reference transaction.
    #[error("before fetch publication: {0}")]
    BeforePublication(#[source] FetchError),
    /// Worktree/HEAD safety recheck failed after installation; refs unchanged.
    #[error("fetch publication safety: {0}")]
    Safety(#[from] FetchPlanError),
    /// Object-kind/ancestry validation or update authorization failed; installed objects remain,
    /// but no refs have changed.
    #[error("fetch update validation: {0}")]
    Update(#[from] super::FetchUpdateError),
    /// Exact transaction preparation or partial publication failure; objects remain installed.
    #[error("fetch reference publication: {0}")]
    Publication(#[from] crate::refs::TransactionError),
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
