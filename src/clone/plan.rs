use crate::ObjectId;
use crate::fetch::Advertisement;
use crate::refs::RefName;
use crate::remote::{Direction, Mapping, MappingError, Refspecs};

/// Explicit choice of initial local HEAD; all branches and tags are fetched in either case.
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Debug)]
pub struct ClonePlan {
    /// Selected initial HEAD.
    pub head: CloneHead,
    /// All advertised branches/tags, plus source-only HEAD when advertised.
    pub mappings: Vec<Mapping>,
}

/// Advertisement or branch policy rejected before destination creation.
#[derive(Debug, thiserror::Error)]
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
        .find(|r| !r.peeled && r.name.as_bytes() == b"HEAD");
    let mappings = specs(head.is_some()).map_advertisement(ad)?;
    let tips: Vec<_> = ad.refs.iter().filter(|r| !r.peeled).collect();
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
            let symbolic: Vec<_> = ad
                .capabilities
                .iter()
                .filter_map(|c| c.strip_prefix(b"symref=HEAD:"))
                .collect();
            match symbolic.as_slice() {
                [] => match head {
                    Some(r) => CloneHead::Detached(r.id),
                    None if tips.is_empty() => {
                        CloneHead::Unborn(RefName::new("refs/heads/main").unwrap())
                    }
                    None => return Err(ClonePlanError::MissingHead),
                },
                [target] => {
                    let name = RefName::new(target).map_err(|_| ClonePlanError::SymbolicHead)?;
                    branch(&name).map_err(|_| ClonePlanError::SymbolicHead)?;
                    match (head, tips.iter().find(|r| r.name == name)) {
                        (Some(h), Some(r)) if h.id == r.id => CloneHead::Branch { name, id: r.id },
                        (None, None) => CloneHead::Unborn(name),
                        _ => return Err(ClonePlanError::SymbolicHead),
                    }
                }
                _ => return Err(ClonePlanError::SymbolicHead),
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
