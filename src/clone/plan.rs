use crate::ObjectId;
use crate::fetch::{Advertisement, RemoteHead, interpret_head};
use crate::refs::RefName;
use crate::remote::{Direction, Mapping, MappingError, Refspecs};

/// Explicit choice of initial local HEAD; all branches and tags are fetched in either case.
///
/// # Examples
///
/// Asking for a branch the remote doesn't have fails before anything is created:
///
/// ```
/// use std::ops::ControlFlow;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::InitKind;
/// use girt::clone::{BranchSelection, ClonePlanError, CloneRequest, CloneTransferError};
/// use girt::fetch::FetchLimits;
/// use girt::refs::{RefName, Reflog};
/// use girt::transport::TransportControl;
/// # let (directory, upstream, _, _, _) = include!("../doctest/remote.rs");
///
/// let destination = directory.path().join("clone");
/// let request = CloneRequest::prepare_tracking(
///     &destination,
///     InitKind::Worktree,
///     b"../upstream.git",
///     BranchSelection::Branch(RefName::new("refs/heads/release")?),
///     Reflog::Preserve,
/// )?;
///
/// let cancel = AtomicBool::new(false);
/// let result = request.receive_local(
///     upstream.git_dir(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     |_| ControlFlow::Continue(()),
/// );
///
/// assert!(matches!(
///     result,
///     Err(CloneTransferError::Plan(ClonePlanError::MissingBranch(_)))
/// ));
/// assert!(!destination.exists());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum BranchSelection {
    /// Honor advertised symbolic HEAD; keep a nonsymbolic HEAD detached. Empty v0 advertisements
    /// without an unborn-name hint use `refs/heads/main`. Missing HEAD in a nonempty advertisement
    /// fails rather than guessing among branches that might share an ID.
    Default,
    /// Select this full `refs/heads/*` name, or create it unborn if no tips are advertised.
    /// Tags, short names and revision expressions are unsupported.
    Branch(RefName),
}

/// HEAD chosen from the advertisement used for the transfer.
///
/// # Examples
///
/// ```
/// use girt::clone::CloneHead;
///
/// fn describe(head: &CloneHead) -> String {
///     match head {
///         CloneHead::Branch { name, .. } => {
///             format!("on {}", String::from_utf8_lossy(name.as_bytes()))
///         }
///         CloneHead::Detached(id) => format!("detached at {id}"),
///         // The remote is empty: the branch exists only as `HEAD`'s target.
///         CloneHead::Unborn(name) => {
///             format!("on unborn {}", String::from_utf8_lossy(name.as_bytes()))
///         }
///         _ => "unknown".to_owned(),
///     }
/// }
/// # let _ = describe;
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CloneHead {
    /// A local branch with the advertised commit ID.
    Branch {
        /// Full local branch name.
        name: RefName,
        /// Advertised tip; finish verifies it is a commit.
        id: ObjectId,
    },
    /// A nonsymbolic remote HEAD, preserved without guessing a branch by equal IDs.
    Detached(ObjectId),
    /// Symbolic HEAD without a branch ref or commit.
    Unborn(RefName),
}

/// In-memory preview; only a plan made inside the actual receive operation can be finished.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::InitKind;
/// use girt::clone::{BranchSelection, CloneHead, CloneRequest};
/// use girt::fetch::{FetchLimits, discover_local};
/// use girt::refs::{RefName, Reflog};
/// use girt::transport::TransportControl;
/// # let (directory, upstream, _, main, _) = include!("../doctest/remote.rs");
///
/// let request = CloneRequest::prepare_tracking(
///     directory.path().join("clone"),
///     InitKind::Worktree,
///     b"../upstream.git",
///     BranchSelection::Default,
///     Reflog::Preserve,
/// )?;
///
/// let cancel = AtomicBool::new(false);
/// let discovery = discover_local(
///     upstream.git_dir(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
/// )?;
/// let plan = request.plan(&discovery.advertisement)?;
///
/// assert_eq!(
///     plan.head,
///     CloneHead::Branch {
///         name: RefName::new("refs/heads/main")?,
///         id: main
///     }
/// );
///
/// // `main` maps to `refs/remotes/origin/main`; `HEAD` is fetched without a destination.
/// let tracking = Some(RefName::new("refs/remotes/origin/main")?);
/// assert!(
///     plan.mappings
///         .iter()
///         .any(|mapping| mapping.destination == tracking)
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct ClonePlan {
    /// Selected initial HEAD.
    pub head: CloneHead,
    /// All advertised branches/tags, plus source-only HEAD when advertised.
    pub mappings: Vec<Mapping>,
}

/// Advertisement or branch policy rejected before destination creation.
///
/// Nothing was created. Most of these are fixed by choosing the branch explicitly:
///
/// - [`MissingBranch`](Self::MissingBranch): the requested branch isn't on the remote. Pick one
///   that is, or use [`BranchSelection::Default`].
/// - [`MissingHead`](Self::MissingHead): the remote advertises branches but no `HEAD`, so there is
///   no default. Pass [`BranchSelection::Branch`].
/// - [`Branch`](Self::Branch): the requested name, or the branch the remote's `HEAD` names, is not
///   a full `refs/heads/…` name.
/// - [`SymbolicHead`](Self::SymbolicHead) and [`Mapping`](Self::Mapping): the advertisement is
///   inconsistent, for example a `HEAD` hint disagreeing with its branch or two references with the
///   same name. Retrying the same remote gives the same result.
///
/// # Examples
///
/// ```
/// use girt::clone::{BranchSelection, ClonePlanError, CloneRequest};
/// use girt::fetch::{AdvertisedRef, Advertisement, AdvertisementKind};
/// use girt::refs::{RefName, Reflog};
/// use girt::{InitKind, ObjectFormat, ObjectKind};
///
/// let directory = tempfile::tempdir()?;
/// let request = CloneRequest::prepare_tracking(
///     directory.path().join("clone"),
///     InitKind::Worktree,
///     b"https://example.com/project.git",
///     BranchSelection::Default,
///     Reflog::Preserve,
/// )?;
///
/// // A remote that lists branches but no HEAD has no default branch.
/// let id = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"tip");
/// let branch = AdvertisedRef::new(RefName::new("refs/heads/main")?, id, AdvertisementKind::Tip);
/// let advertisement = Advertisement::new(vec![branch], Vec::new());
/// assert!(matches!(
///     request.plan(&advertisement),
///     Err(ClonePlanError::MissingHead)
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClonePlanError {
    /// Invalid duplicate/colliding references or unsupported refspec mapping.
    #[error(transparent)]
    Mapping(#[from] MappingError),
    /// Explicit or symbolic HEAD target must be a full branch name.
    #[error("clone requires a full refs/heads branch name")]
    Branch,
    /// A requested branch is absent from a nonempty advertisement.
    #[error("selected clone branch is not advertised: {0:?}")]
    MissingBranch(RefName),
    /// No HEAD/default can be established; explicitly select a branch.
    #[error("remote has no advertised HEAD; select a branch explicitly")]
    MissingHead,
    /// Duplicate/invalid symbolic HEAD capability or disagreement with advertised branch ID.
    #[error("inconsistent or unsupported symbolic HEAD advertisement")]
    SymbolicHead,
}

pub(super) fn specs(has_head: bool) -> Refspecs {
    let mut specs = vec![
        "refs/heads/*:refs/remotes/origin/*",
        "refs/tags/*:refs/tags/*",
    ];
    if has_head {
        specs.push("HEAD");
    }
    Refspecs::parse(Direction::Fetch, specs).expect("fixed clone refspecs")
}

pub(super) fn branch(name: &RefName) -> Result<(), ClonePlanError> {
    if !name.as_bytes().starts_with(b"refs/heads/") {
        return Err(ClonePlanError::Branch);
    }
    Ok(())
}

pub(super) fn plan(
    selection: &BranchSelection,
    ad: &Advertisement,
) -> Result<ClonePlan, ClonePlanError> {
    let head = ad
        .refs
        .iter()
        .find(|r| r.kind == crate::fetch::AdvertisementKind::Tip && r.name.as_bytes() == b"HEAD");
    let mappings = specs(head.is_some()).map_advertisement(ad)?;
    let tips: Vec<_> = ad
        .refs
        .iter()
        .filter(|r| r.kind == crate::fetch::AdvertisementKind::Tip)
        .collect();
    let selected = match selection {
        BranchSelection::Branch(name) => {
            branch(name)?;
            match tips.iter().find(|r| &r.name == name) {
                Some(r) => CloneHead::Branch {
                    name: name.clone(),
                    id: r.id,
                },
                None if tips.is_empty() => CloneHead::Unborn(name.clone()),
                None => return Err(ClonePlanError::MissingBranch(name.clone())),
            }
        }
        BranchSelection::Default => {
            match interpret_head(ad).map_err(|_| ClonePlanError::SymbolicHead)? {
                RemoteHead::Symbolic { branch, id } => CloneHead::Branch { name: branch, id },
                RemoteHead::Unborn { branch } if tips.is_empty() => CloneHead::Unborn(branch),
                RemoteHead::Unborn { .. } => return Err(ClonePlanError::SymbolicHead),
                RemoteHead::Inferred { id, .. }
                | RemoteHead::Detached { id }
                | RemoteHead::Ambiguous { id, .. } => CloneHead::Detached(id),
                RemoteHead::Missing if tips.is_empty() => {
                    CloneHead::Unborn(crate::repository::initial_branch())
                }
                RemoteHead::Missing => return Err(ClonePlanError::MissingHead),
            }
        }
    };
    Ok(ClonePlan {
        head: selected,
        mappings,
    })
}

pub(super) fn select(
    result: Result<ClonePlan, ClonePlanError>,
    saved: &mut Option<Result<ClonePlan, ClonePlanError>>,
) -> Vec<ObjectId> {
    let wants = result
        .as_ref()
        .map(|p| {
            p.mappings
                .iter()
                .filter_map(|m| m.source.as_ref().map(|s| s.id))
                .collect()
        })
        .unwrap_or_default();
    *saved = Some(result);
    wants
}

#[cfg(test)]
mod tests;
