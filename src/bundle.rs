//! Portable reference advertisements and packs for offline transfer.
//!
//! A bundle records prerequisites that the recipient must already possess, advertised reference
//! tips, and a pack. Version 3 adds required capabilities, including the object format. Parsing a
//! header does not verify its pack or establish that prerequisite history is complete.
//!
//! ```
//! use girt::bundle::{Bundle, Version};
//!
//! let header = b"# v2 git bundle\n0123456789012345678901234567890123456789 refs/heads/main\n\n";
//! let bundle = Bundle::parse(header)?;
//! assert_eq!(bundle.version(), Version::V2);
//! assert_eq!(bundle.references()[0].name.as_bytes(), b"refs/heads/main");
//! // Listing the header does not require a pack. receive/verify must validate one before use.
//! # Ok::<(), girt::bundle::Error>(())
//! ```

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use crate::refs::RefName;
use crate::{ObjectFormat, ObjectId};

/// Bundle header format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Version {
    /// SHA-1 bundles without capability records.
    V2,
    /// Bundles with required capabilities.
    V3,
}

/// Bounds for parsing a header already held in memory.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum header bytes, including the terminating blank line.
    pub max_header_bytes: usize,
    /// Maximum total reference and prerequisite records.
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_header_bytes: 16 << 20,
            max_entries: 1 << 20,
        }
    }
}

/// One advertised reference. HEAD is allowed alongside ordinary ref names.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Reference {
    /// Tip identity.
    pub id: ObjectId,
    /// Reference name, preserved without Unicode conversion.
    pub name: RefName,
}

/// A parsed bundle borrowing its pack bytes.
#[derive(Debug)]
pub struct Bundle<'a> {
    version: Version,
    format: ObjectFormat,
    prerequisites: Vec<ObjectId>,
    references: Vec<Reference>,
    filter: Option<&'a [u8]>,
    pack: &'a [u8],
}

/// Bundle framing, capability or transfer-validation failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Header framing or an identity/reference is invalid.
    #[error("malformed bundle: {0}")]
    Malformed(&'static str),
    /// An unknown required capability prevents safe interpretation.
    #[error("unsupported bundle capability: {0:?}")]
    Unsupported(Vec<u8>),
    /// A valid operation needs behaviour outside this API's supported scope.
    #[error("unsupported bundle operation: {0}")]
    UnsupportedOperation(&'static str),
    /// A parser budget was exhausted.
    #[error("bundle limit exceeded: {0}")]
    Limit(&'static str),
    /// Pack decoding, prerequisite connectivity or installation validation failed.
    #[error(transparent)]
    Fetch(#[from] Box<crate::fetch::FetchError>),
    /// A declared prerequisite does not have verified complete destination history.
    #[error("missing bundle prerequisite {0}")]
    MissingPrerequisite(ObjectId),
    /// Fetch reference mapping failed before publication.
    #[error(transparent)]
    FetchPlan(#[from] Box<crate::fetch::FetchPlanError>),
    /// Clone branch selection failed before creating the destination.
    #[error(transparent)]
    ClonePlan(#[from] Box<crate::clone::ClonePlanError>),
    /// Object collection or pack encoding failed.
    #[error(transparent)]
    Create(#[from] Box<crate::Error>),
    /// Filtered bundles require promisor publication, which this API does not perform.
    #[error("filtered bundle requires promisor installation")]
    Filtered,
}

impl Reference {
    /// Creates an advertised tip; creation and receive validate its format against the bundle.
    pub fn new(name: RefName, id: ObjectId) -> Self {
        Self { id, name }
    }
}

impl<'a> Bundle<'a> {
    /// Parses a v2/v3 bundle header under default limits, retaining the pack without decoding it.
    ///
    /// # Errors
    ///
    /// Returns malformed framing, unsupported capabilities or exhausted header limits.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        Self::parse_with(bytes, Limits::default())
    }

    /// Parses with explicit header bounds. Unknown capabilities are refused, never ignored.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::parse`]. Input storage remains caller-owned and unchanged.
    pub fn parse_with(bytes: &'a [u8], limits: Limits) -> Result<Self, Error> {
        let mut rest = bytes;
        let mut consumed = 0usize;
        let mut line = || -> Result<&'a [u8], Error> {
            let available = limits.max_header_bytes.saturating_sub(consumed);
            let window = rest.get(..rest.len().min(available)).unwrap_or_default();
            let end = window
                .iter()
                .position(|&byte| byte == b'\n')
                .ok_or(Error::Limit("header bytes or unterminated line"))?;
            let value = &rest[..end];
            consumed += end + 1;
            rest = &rest[end + 1..];
            Ok(value)
        };
        let version = match line()? {
            b"# v2 git bundle" => Version::V2,
            b"# v3 git bundle" => Version::V3,
            _ => return Err(Error::Malformed("signature")),
        };
        let mut format = ObjectFormat::Sha1;
        let mut filter = None;
        let mut capabilities = BTreeSet::new();
        let mut prerequisites = Vec::new();
        let mut references = Vec::new();
        let mut names = BTreeSet::new();
        let mut records = false;
        let mut refs_started = false;
        loop {
            let value = line()?;
            if value.is_empty() {
                break;
            }
            if let Some(capability) = value.strip_prefix(b"@") {
                if version != Version::V3 || records {
                    return Err(Error::Malformed("capability position"));
                }
                let (key, value) =
                    crate::bytes::split_once(capability, b'=').unwrap_or((capability, &[]));
                if !capabilities.insert(key) {
                    return Err(Error::Malformed("duplicate capability"));
                }
                match (key, value) {
                    (b"object-format", b"sha1") => format = ObjectFormat::Sha1,
                    (b"object-format", b"sha256") => format = ObjectFormat::Sha256,
                    (b"filter", value) if !value.is_empty() => filter = Some(value),
                    _ => return Err(Error::Unsupported(capability.to_vec())),
                }
                continue;
            }
            records = true;
            if prerequisites.len() + references.len() >= limits.max_entries {
                return Err(Error::Limit("header entries"));
            }
            let (prerequisite, value) = value
                .strip_prefix(b"-")
                .map_or((false, value), |value| (true, value));
            let (raw_id, name) = crate::bytes::split_once(value, b' ')
                .ok_or(Error::Malformed("record separator"))?;
            let id = std::str::from_utf8(raw_id)
                .ok()
                .and_then(|id| ObjectId::from_hex(format, id).ok())
                .filter(|id| !id.is_null())
                .ok_or(Error::Malformed("object identity"))?;
            if prerequisite {
                if refs_started {
                    return Err(Error::Malformed("prerequisite after reference"));
                }
                prerequisites.push(id);
            } else {
                refs_started = true;
                let name = RefName::new(name).map_err(|_| Error::Malformed("reference name"))?;
                if !names.insert(name.clone()) {
                    return Err(Error::Malformed("duplicate reference"));
                }
                references.push(Reference { id, name });
            }
        }
        if references.is_empty() {
            return Err(Error::Malformed("no references"));
        }
        Ok(Self {
            version,
            format,
            prerequisites,
            references,
            filter,
            pack: rest,
        })
    }

    /// Header version.
    pub fn version(&self) -> Version {
        self.version
    }
    /// Object identity format declared by the header (SHA-1 when omitted).
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }
    /// Objects excluded from the pack whose complete histories the recipient needs.
    pub fn prerequisites(&self) -> &[ObjectId] {
        &self.prerequisites
    }
    /// Advertised tips in stored order, suitable for list-heads output.
    pub fn references(&self) -> &[Reference] {
        &self.references
    }
    /// Raw required filter expression, if present.
    pub fn filter(&self) -> Option<&[u8]> {
        self.filter
    }
    /// Pack bytes following the terminating blank line, without verification.
    pub fn pack(&self) -> &[u8] {
        self.pack
    }

    pub(crate) fn advertisement(&self) -> crate::fetch::Advertisement {
        let refs = self
            .references
            .iter()
            .map(|reference| {
                crate::fetch::AdvertisedRef::new(
                    reference.name.clone(),
                    reference.id,
                    crate::fetch::AdvertisementKind::Tip,
                )
            })
            .collect();
        crate::fetch::Advertisement::new(
            refs,
            vec![format!("object-format={}", self.format).into_bytes()],
        )
    }

    /// Verifies prerequisites against a repository, then validates the pack and all advertised
    /// tips.
    ///
    /// # Errors
    ///
    /// Missing or incomplete prerequisite history, filters and pack defects are refused. This does
    /// not install objects or publish references.
    pub fn verify(
        &self,
        repository: &crate::Repository,
        limits: crate::fetch::FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<crate::fetch::ReceivedFetch, Error> {
        let objects = repository
            .objects_with(limits.source_packs)
            .map_err(|error| Error::Create(Box::new(error.into())))?;
        let known = crate::fetch::KnownHistory::new(&objects, &self.prerequisites, limits, cancel)
            .map_err(|error| Error::Fetch(Box::new(error)))?;
        self.receive(&known, limits, cancel)
    }

    /// Verifies pack objects and tip connectivity through the existing fetch validator.
    ///
    /// The caller supplies destination history, including prerequisites, through `known`.
    /// Successful validation returns the normal fetch artifact for explicit installation. No refs
    /// or objects are published here. Filtered bundles are explicitly refused until promisor
    /// installation is supported. Memory includes one bounded copy of the input pack.
    ///
    /// # Errors
    ///
    /// Returns filter refusal or fetch object, connectivity, format, limit and cancellation errors.
    pub fn receive(
        &self,
        known: &crate::fetch::KnownHistory,
        limits: crate::fetch::FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<crate::fetch::ReceivedFetch, Error> {
        for &id in &self.prerequisites {
            if !known.bundle_prerequisite(id) {
                return Err(Error::MissingPrerequisite(id));
            }
        }
        if self.filter.is_some() {
            return Err(Error::Filtered);
        }
        if self.pack.len() > limits.max_pack_bytes {
            return Err(Error::Limit("pack bytes"));
        }
        let advertisement = self.advertisement();
        crate::fetch::ReceivedFetch::validate_known(
            advertisement,
            self.references
                .iter()
                .map(|reference| reference.id)
                .collect(),
            self.pack.to_vec(),
            known,
            Vec::new(),
            crate::fetch::FetchOptions::default().with_limits(limits),
            &mut crate::fetch::ValidationObserver::new(cancel, &mut |_| {}),
        )
        .map_err(|error| Error::Fetch(Box::new(error)))
    }
}

/// Bounds for collecting history and encoding a portable pack.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CreateLimits {
    /// Maximum distinct objects in the included and prerequisite closures together.
    pub max_objects: usize,
    /// Maximum decoded bytes retained while constructing the pack.
    pub max_bytes: usize,
    /// Maximum typed edges queued across both closures.
    pub max_edges: usize,
    /// Maximum bytes in the encoded reference/prerequisite header.
    pub max_header_bytes: usize,
    /// Per-object storage read bounds.
    pub read: crate::ReadLimits,
    /// Output pack and compression bounds.
    pub pack: crate::PackWriteLimits,
}

impl Default for CreateLimits {
    fn default() -> Self {
        Self {
            max_objects: 1 << 20,
            max_bytes: 1 << 30,
            max_edges: 1 << 24,
            max_header_bytes: 16 << 20,
            read: Default::default(),
            pack: Default::default(),
        }
    }
}

/// Creates a v2 SHA-1 or v3 SHA-256 bundle from resolved tips, excluding prerequisite histories.
///
/// Resolve revision expressions before calling; pass all repository refs to capture all tips.
/// Prerequisites must name commits with complete history in `objects`. Gitlinks remain external.
/// Shallow repositories are refused because prerequisites are not shallow boundaries. This builds
/// a self-contained pack, without external deltas, even when prerequisite histories are excluded.
/// Both included and prerequisite closures count against the same object, byte and edge budgets.
/// Memory is bounded by `limits.max_bytes` for decoded objects plus the output pack limit.
///
/// # Errors
///
/// Returns invalid tips/prerequisites, incomplete history, storage, cancellation or limit errors.
/// No repository data is changed and no output is returned on failure.
pub fn create(
    objects: &crate::Objects,
    references: &[Reference],
    prerequisites: &[ObjectId],
    limits: CreateLimits,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    let format = objects.object_format();
    if !objects.shallow_roots().is_empty() {
        return Err(Error::UnsupportedOperation("shallow source"));
    }
    if references.is_empty() {
        return Err(Error::Malformed("no references"));
    }
    let header_bytes = references
        .iter()
        .try_fold(128usize, |total, reference| {
            total
                .checked_add(reference.name.as_bytes().len())?
                .checked_add(format.digest_len() * 2 + 2)
        })
        .and_then(|total| {
            total.checked_add(
                prerequisites
                    .len()
                    .checked_mul(format.digest_len() * 2 + 15)?,
            )
        })
        .ok_or(Error::Limit("header bytes"))?;
    if header_bytes > limits.max_header_bytes {
        return Err(Error::Limit("header bytes"));
    }
    let mut retained = BTreeMap::new();
    let mut pending: Vec<_> = prerequisites
        .iter()
        .copied()
        .map(|id| (id, Some(crate::ObjectKind::Commit)))
        .collect();
    let mut visited = BTreeMap::new();
    let mut bytes = 0usize;
    let mut edges = 0usize;
    if references.len().saturating_add(prerequisites.len()) > limits.max_objects {
        return Err(Error::Limit("header entries"));
    }
    for excluding in [true, false] {
        if !excluding {
            pending.extend(references.iter().map(|reference| (reference.id, None)));
        }
        while let Some((id, expected)) = pending.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Fetch(Box::new(crate::fetch::FetchError::Cancelled)));
            }
            if let Some(&actual) = visited.get(&id) {
                if expected.is_some_and(|expected| expected != actual) {
                    return Err(Error::Malformed("wrong object kind"));
                }
                continue;
            }
            if visited.len() >= limits.max_objects {
                return Err(Error::Limit("objects"));
            }
            let object = objects
                .read_controlled(id, limits.read, cancel)
                .map_err(|error| Error::Create(Box::new(error.into())))?
                .ok_or(Error::Malformed("missing object"))?;
            if expected.is_some_and(|expected| expected != object.kind()) {
                return Err(Error::Malformed("wrong object kind"));
            }
            visited.insert(id, object.kind());
            bytes = bytes
                .checked_add(object.data().len())
                .ok_or(Error::Limit("decoded bytes"))?;
            if bytes > limits.max_bytes {
                return Err(Error::Limit("decoded bytes"));
            }
            let result: Result<(), WalkError> =
                crate::edges::visit(id, object.kind(), object.data(), |id, kind| {
                    if edges >= limits.max_edges {
                        return Err(WalkError::Limit);
                    }
                    edges += 1;
                    pending.push((id, Some(kind)));
                    Ok(())
                });
            match result {
                Ok(()) => {}
                Err(WalkError::Parse) => return Err(Error::Malformed("object graph")),
                Err(WalkError::Limit) => return Err(Error::Limit("graph edges")),
            }
            if !excluding {
                retained.insert(id, object);
            }
        }
    }
    if retained.is_empty() {
        return Err(Error::Malformed("empty bundle"));
    }
    let input: Vec<_> = retained
        .iter()
        .map(|(&id, object)| crate::PackObject {
            id,
            kind: object.kind(),
            data: object.data(),
        })
        .collect();
    let mut pack = Vec::new();
    crate::write_pack(format, &input, &mut pack, &mut std::io::sink(), limits.pack)
        .map_err(|error| Error::Create(Box::new(error.into())))?;
    let mut output = if format == ObjectFormat::Sha1 {
        b"# v2 git bundle\n".to_vec()
    } else {
        b"# v3 git bundle\n@object-format=sha256\n".to_vec()
    };
    for id in prerequisites {
        output.extend_from_slice(format!("-{id} prerequisite\n").as_bytes());
    }
    let mut names = BTreeSet::new();
    for reference in references {
        if reference.id.format() != format || !names.insert(&reference.name) {
            return Err(Error::Malformed("reference identity or duplicate name"));
        }
        output.extend_from_slice(format!("{} ", reference.id).as_bytes());
        output.extend_from_slice(reference.name.as_bytes());
        output.push(b'\n');
    }
    output.push(b'\n');
    output.extend_from_slice(&pack);
    Ok(output)
}

// Keeps resource exhaustion distinct from malformed graph payloads during edge callbacks.
enum WalkError {
    Parse,
    Limit,
}
impl From<crate::edges::Error> for WalkError {
    fn from(_: crate::edges::Error) -> Self {
        Self::Parse
    }
}
