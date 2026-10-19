//! Read-only local reference advertisement under explicit admission budgets.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::fetch::{AdvertisedRef, Advertisement, AdvertisementKind, FetchError};
use crate::refs::{RefName, Reference, ReferenceError, Target};
use crate::{
    AlternateLimits, ObjectFormat, ObjectId, OpenLimits, OpenOptions, PackLimits, PeelLimits,
    Repository,
};

/// Source and output budgets for controlled local advertisement.
///
/// Defaults admit one million inventory entries and advertised entries, 128 MiB each of
/// reference input and advertisement output, a 4 KiB files HEAD and 32 symbolic hops.
/// Pack indexes may occupy 512 MiB, admitting the Linux kernel fixture. Opening, alternates,
/// reftable stacks and each tag peel use their documented defaults. These are per-phase
/// bounds; object peels are bounded per advertised tag, not by an aggregate payload budget.
// New limit fields must remain Copy.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LocalAdvertisementLimits {
    /// Source location/configuration admission.
    pub open: OpenLimits,
    /// Object snapshot pack/index budgets.
    pub packs: PackLimits,
    /// Alternate object-store admission.
    pub alternates: AlternateLimits,
    /// Inventory directory entries and packed records.
    pub max_ref_entries: usize,
    /// Aggregate files-backend reference input bytes, excluding HEAD.
    pub max_ref_bytes: usize,
    /// Files-backend HEAD bytes; reftable HEAD uses the stack budgets.
    pub max_head_bytes: usize,
    /// Symbolic edges followed against the captured inventory; zero permits direct targets.
    pub max_symbolic_depth: usize,
    /// Reftable limits, independently applied to inventory and HEAD snapshots.
    pub reftable: crate::refs::reftable::StackLimits,
    /// Verified tag-depth and object-read bounds for each tag reference.
    pub peel: PeelLimits,
    /// Advertised entries, including HEAD and peeled hints.
    pub max_refs: usize,
    /// Encoded advertisement estimate, including names, IDs and capability bytes.
    pub max_bytes: usize,
}

impl Default for LocalAdvertisementLimits {
    fn default() -> Self {
        Self {
            open: OpenLimits::default(),
            packs: PackLimits::default().with_max_index_bytes(512 * 1024 * 1024),
            alternates: AlternateLimits::default(),
            max_ref_entries: 1_000_000,
            max_ref_bytes: 128 * 1024 * 1024,
            max_head_bytes: 4096,
            max_symbolic_depth: 32,
            reftable: crate::refs::reftable::StackLimits::default(),
            peel: PeelLimits::default(),
            max_refs: 1_000_000,
            max_bytes: 128 * 1024 * 1024,
        }
    }
}

impl LocalAdvertisementLimits {
    /// Sets [`Self::open`].
    #[must_use]
    pub fn with_open(mut self, limits: OpenLimits) -> Self {
        self.open = limits;
        self
    }
    /// Sets [`Self::packs`].
    #[must_use]
    pub fn with_packs(mut self, limits: PackLimits) -> Self {
        self.packs = limits;
        self
    }
    /// Sets [`Self::alternates`].
    #[must_use]
    pub fn with_alternates(mut self, limits: AlternateLimits) -> Self {
        self.alternates = limits;
        self
    }
    /// Sets [`Self::max_ref_entries`].
    #[must_use]
    pub fn with_max_ref_entries(mut self, value: usize) -> Self {
        self.max_ref_entries = value;
        self
    }
    /// Sets [`Self::max_ref_bytes`].
    #[must_use]
    pub fn with_max_ref_bytes(mut self, value: usize) -> Self {
        self.max_ref_bytes = value;
        self
    }
    /// Sets [`Self::max_head_bytes`].
    #[must_use]
    pub fn with_max_head_bytes(mut self, value: usize) -> Self {
        self.max_head_bytes = value;
        self
    }
    /// Sets [`Self::max_symbolic_depth`].
    #[must_use]
    pub fn with_max_symbolic_depth(mut self, value: usize) -> Self {
        self.max_symbolic_depth = value;
        self
    }
    /// Sets [`Self::reftable`].
    #[must_use]
    pub fn with_reftable(mut self, limits: crate::refs::reftable::StackLimits) -> Self {
        self.reftable = limits;
        self
    }
    /// Sets [`Self::peel`].
    #[must_use]
    pub fn with_peel(mut self, limits: PeelLimits) -> Self {
        self.peel = limits;
        self
    }
    /// Sets [`Self::max_refs`].
    #[must_use]
    pub fn with_max_refs(mut self, value: usize) -> Self {
        self.max_refs = value;
        self
    }
    /// Sets [`Self::max_bytes`].
    #[must_use]
    pub fn with_max_bytes(mut self, value: usize) -> Self {
        self.max_bytes = value;
        self
    }
}

/// Controlled, read-only advertisement from an explicit local repository path.
///
/// The default uses [`LocalAdvertisementLimits::default`]. Linked worktrees and any registered
/// linked worktree are refused conservatively. Symbolic references resolve against the captured
/// inventory and HEAD, without per-reference filesystem rereads. Inventory and HEAD acquisition
/// are not one atomic repository snapshot. A HEAD target absent from that inventory is omitted,
/// like any dangling symbolic target. One bounded open, snapshot, read or tag peel may finish
/// before cancellation is observed. This does not perform a transfer or publish destination refs.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{LocalAdvertisementLimits, LocalAdvertisementOptions};
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let root = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     root.path().join("r.git"),
///     InitKind::Bare,
/// )?;
/// let advertisement = LocalAdvertisementOptions::default()
///     .with_limits(LocalAdvertisementLimits::default().with_max_refs(100))
///     .advertise(repo.git_dir(), &AtomicBool::new(false))?;
/// assert!(advertisement.refs.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct LocalAdvertisementOptions {
    limits: LocalAdvertisementLimits,
}

impl LocalAdvertisementOptions {
    /// Replaces source and output budgets.
    #[must_use]
    pub fn with_limits(mut self, limits: LocalAdvertisementLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Opens an admitted source and returns its complete advertisement.
    ///
    /// # Errors
    ///
    /// Returns [`FetchError::LinkedWorktrees`] for a linked source or registration; bounded source,
    /// reference and peel errors preserve their context. Limits or cancellation return no partial
    /// advertisement. No source handle or destination effect escapes on failure.
    pub fn advertise(
        &self,
        path: impl AsRef<Path>,
        cancel: &AtomicBool,
    ) -> Result<Advertisement, FetchError> {
        Ok(admit(path.as_ref(), self.limits, cancel)?.advertisement)
    }
}

// Retain these together for later controlled transfer composition, without reopening or
// rediscovery.
struct AdmittedSource {
    _repository: Repository,
    _objects: crate::Objects,
    advertisement: Advertisement,
}

fn admit(
    path: &Path,
    limits: LocalAdvertisementLimits,
    cancel: &AtomicBool,
) -> Result<AdmittedSource, FetchError> {
    let mut poll = Poll::new(cancel)?;
    let repository = OpenOptions::default()
        .with_limits(limits.open)
        .open(path, cancel)
        .map_err(|error| FetchError::Source(Box::new(error)))?;
    if repository.git_dir() != repository.common_dir() {
        return Err(FetchError::LinkedWorktrees);
    }
    match std::fs::read_dir(repository.common_dir().join("worktrees")) {
        Ok(mut entries) => {
            if let Some(entry) = entries.next() {
                entry?;
                return Err(FetchError::LinkedWorktrees);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(FetchError::Io(error)),
    }
    let objects = repository
        .objects_controlled(limits.packs, limits.alternates, cancel)
        .map_err(FetchError::SourceObjects)?;
    let references = repository
        .references()
        .map_err(refs_error)?
        .with_reftable_limits(limits.reftable);
    let listed = references
        .list_controlled(limits.max_ref_entries, limits.max_ref_bytes, cancel)
        .map_err(refs_error)?;
    let head = references
        .head_controlled(limits.max_head_bytes, cancel)
        .map_err(refs_error)?;
    let mut output = Output {
        refs: Vec::new(),
        remaining: limits.max_bytes,
        maximum: limits.max_refs,
    };
    if let Some(id) = resolve(
        head.as_ref(),
        head.as_ref(),
        &listed,
        limits.max_symbolic_depth,
        &mut poll,
    )? {
        output.push(&RefName::HEAD, id, false)?;
    }
    for reference in &listed {
        poll.step()?;
        let Some(id) = resolve(
            Some(&reference.target),
            head.as_ref(),
            &listed,
            limits.max_symbolic_depth,
            &mut poll,
        )?
        else {
            continue;
        };
        output.push(&reference.name, id, false)?;
        if reference.name.as_bytes().starts_with(b"refs/tags/") {
            let peeled = objects
                .peel(id, limits.peel, cancel)
                .map_err(|error| FetchError::Peel(Box::new(error)))?;
            if !peeled.tags.is_empty() {
                output.push(&reference.name, peeled.target, true)?;
            }
        }
    }
    let mut capabilities = Vec::new();
    if let Some(Target::Symbolic(name)) = &head
        && name.as_bytes().starts_with(b"refs/heads/")
    {
        output.charge(b"symref=HEAD:".len())?;
        output.charge(name.as_bytes().len())?;
        output.charge(1)?;
        let mut hint = b"symref=HEAD:".to_vec();
        hint.extend_from_slice(name.as_bytes());
        capabilities.push(hint);
    }
    if repository.object_format() == ObjectFormat::Sha256 {
        output.charge(b"object-format=sha256".len() + 1)?;
        capabilities.push(b"object-format=sha256".to_vec());
    }
    check(cancel)?;
    let advertisement = Advertisement::new(output.refs, capabilities);
    Ok(AdmittedSource {
        _repository: repository,
        _objects: objects,
        advertisement,
    })
}

fn refs_error(error: ReferenceError) -> FetchError {
    FetchError::LocalReferences(Box::new(error))
}

fn resolve<'a>(
    mut target: Option<&'a Target>,
    head: Option<&'a Target>,
    listed: &'a [Reference],
    maximum: usize,
    poll: &mut Poll<'_>,
) -> Result<Option<ObjectId>, FetchError> {
    for depth in 0..=maximum {
        poll.step()?;
        match target {
            None => return Ok(None),
            Some(Target::Direct(id)) => return Ok(Some(*id)),
            Some(Target::Symbolic(_)) if depth == maximum => {
                return Err(refs_error(ReferenceError::Depth(maximum)));
            }
            Some(Target::Symbolic(next)) => {
                target = if next == &RefName::HEAD {
                    head
                } else {
                    listed
                        .binary_search_by(|reference| reference.name.cmp(next))
                        .ok()
                        .and_then(|index| listed.get(index))
                        .map(|reference| &reference.target)
                }
            }
        }
    }
    Err(refs_error(ReferenceError::Depth(maximum)))
}

struct Output {
    refs: Vec<AdvertisedRef>,
    remaining: usize,
    maximum: usize,
}

impl Output {
    fn charge(&mut self, bytes: usize) -> Result<(), FetchError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(FetchError::Limit("local advertisement bytes"))?;
        Ok(())
    }

    fn push(&mut self, name: &RefName, id: ObjectId, peeled: bool) -> Result<(), FetchError> {
        if self.refs.len() >= self.maximum {
            return Err(FetchError::Limit("local references"));
        }
        self.charge(name.as_bytes().len())?;
        self.charge(id.as_bytes().len() * 2 + 8)?;
        self.refs.push(AdvertisedRef::new(
            name.clone(),
            id,
            if peeled {
                AdvertisementKind::PeeledHint
            } else {
                AdvertisementKind::Tip
            },
        ));
        Ok(())
    }
}

struct Poll<'a> {
    cancel: &'a AtomicBool,
    remaining: usize,
}

impl<'a> Poll<'a> {
    fn new(cancel: &'a AtomicBool) -> Result<Self, FetchError> {
        check(cancel)?;
        Ok(Self {
            cancel,
            remaining: 64,
        })
    }

    fn step(&mut self) -> Result<(), FetchError> {
        self.remaining -= 1;
        if self.remaining == 0 {
            check(self.cancel)?;
            self.remaining = 64;
        }
        Ok(())
    }
}

fn check(cancel: &AtomicBool) -> Result<(), FetchError> {
    if cancel.load(Ordering::Relaxed) {
        Err(FetchError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_polled_at_the_batch_boundary() {
        let cancel = AtomicBool::new(false);
        let mut poll = Poll::new(&cancel).unwrap();
        for _ in 0..63 {
            poll.step().unwrap();
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(poll.step(), Err(FetchError::Cancelled)));
    }
}
