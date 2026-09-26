//! Read-only remote reference and HEAD interpretation.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{Advertisement, FetchError, FetchLimits, local_native, protocol};
use crate::packet::{Wire, packet, put};
use crate::refs::RefName;
use crate::transport::TransportControl;
use crate::{ObjectFormat, ObjectId, Repository};

/// Protocol family used to obtain the reference inventory.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProtocolVersion {
    /// Native local reference inspection, with no wire protocol.
    Native,
    /// V0 upload-pack reference advertisement.
    V0,
    /// V1 version marker followed by a v0-shaped reference advertisement.
    V1,
    /// V2 capability advertisement followed by `ls-refs`.
    V2,
}

/// The remote's advertised HEAD, with no branch guessed when the wire is ambiguous.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RemoteHead {
    /// HEAD explicitly names a branch and resolves to its advertised ID.
    Symbolic { branch: RefName, id: ObjectId },
    /// HEAD explicitly names a branch that has no tip yet.
    Unborn { branch: RefName },
    /// HEAD has an ID but no symbolic hint and matches one advertised branch.
    Inferred { branch: RefName, id: ObjectId },
    /// HEAD has an ID but names no uniquely identifiable branch.
    Detached { id: ObjectId },
    /// HEAD was not advertised and no symbolic hint identifies an unborn branch.
    Missing,
    /// Multiple advertised branches match an unhinted HEAD ID.
    Ambiguous {
        id: ObjectId,
        branches: Vec<RefName>,
    },
}

/// Validated reference discovery, independent of any later fetch or push session.
///
/// A preview can become stale before transfer. Consumers must plan mutations against the
/// advertisement from the actual transfer session. Discovery retains unknown capability tokens
/// for inspection but never requests them. No object is downloaded or local repository mutated.
#[derive(Debug, Clone)]
pub struct RemoteDiscovery {
    /// Advertised references and raw capability tokens.
    pub advertisement: Advertisement,
    /// Negotiated object format. An absent wire capability means SHA-1.
    pub object_format: ObjectFormat,
    /// Deterministic interpretation of advertised HEAD.
    pub head: RemoteHead,
    /// Protocol used for this inventory, or `Native` for local storage inspection.
    pub version: ProtocolVersion,
}

impl RemoteDiscovery {
    /// Validates format and HEAD hints in a complete advertisement.
    ///
    /// # Errors
    ///
    /// Rejects conflicting formats, invalid or repeated HEAD hints, and a symbolic HEAD whose
    /// advertised ID disagrees with its target branch. An unhinted ambiguous HEAD is reported in
    /// [`RemoteHead`] so the caller can require an explicit branch choice.
    pub fn from_advertisement(advertisement: Advertisement) -> Result<Self, FetchError> {
        let object_format = advertisement.object_format()?;
        let mut names = HashSet::new();
        for reference in &advertisement.refs {
            if reference.id.is_null() || !names.insert((reference.name.clone(), reference.peeled)) {
                return Err(FetchError::Protocol("invalid advertised ref"));
            }
        }
        for reference in &advertisement.refs {
            if reference.peeled
                && (!reference.name.as_bytes().starts_with(b"refs/tags/")
                    || !names.contains(&(reference.name.clone(), false)))
            {
                return Err(FetchError::Protocol("unmatched peeled hint"));
            }
        }
        let head = interpret_head(&advertisement)?;
        Ok(Self {
            advertisement,
            object_format,
            head,
            version: ProtocolVersion::V0,
        })
    }

    /// Rejects a destination format mismatch before any transfer or local mutation.
    ///
    /// # Errors
    ///
    /// Returns an object-format error if `expected` differs from the remote format.
    pub fn require_format(&self, expected: ObjectFormat) -> Result<(), FetchError> {
        if self.object_format == expected {
            Ok(())
        } else {
            Err(crate::ObjectFormatError {
                expected,
                actual: self.object_format,
            }
            .into())
        }
    }

    /// Reports whether the peer advertised one exact capability token.
    ///
    /// Unknown tokens are retained for inspection. An advertised token does not imply that
    /// girt's current transfer adapter implements the corresponding feature.
    pub fn advertises(&self, capability: &[u8]) -> bool {
        self.advertisement.has(capability)
    }

    /// Reports whether the current fetch adapters can request an object pack from this inventory.
    ///
    /// Native local transfer supports both formats. V0/v1 wire transfer requires
    /// `side-band-64k`; v2 `fetch` remains unsupported even when the peer advertises it. This is a
    /// capability check, not authorization to reuse a stale preview or evidence that all selected
    /// objects will be available.
    pub fn supports_current_fetch(&self) -> bool {
        match self.version {
            ProtocolVersion::Native => true,
            ProtocolVersion::V0 | ProtocolVersion::V1 => self.advertises(b"side-band-64k"),
            ProtocolVersion::V2 => false,
        }
    }
}

/// Parses a complete caller-owned protocol v0 or v1 upload-pack advertisement.
///
/// The stream must end after the advertisement flush. Cancellation is cooperative between
/// blocking reads; callers needing a deadline must supply an interruptible stream. No request is
/// written, and no local state changes.
///
/// # Errors
///
/// Rejects malformed, truncated, over-limit, unsupported-version and inconsistent responses.
pub fn discover(
    reader: &mut impl Read,
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<RemoteDiscovery, FetchError> {
    traced("stream", || {
        let mut wire = Wire {
            reader,
            remaining: limits.max_wire_bytes,
            cancel,
        };
        let (advertisement, version) = protocol::advertise_with_version(&mut wire, limits)?;
        wire.end()?;
        let mut discovered = RemoteDiscovery::from_advertisement(advertisement)?;
        discovered.version = version;
        Ok(discovered)
    })
}

/// Negotiates discovery on a caller-owned upload-pack service stream.
///
/// Accepts v0/v1 advertisements or v2 capability advertisements. For v2, writes one `ls-refs`
/// request with `peel` and `symrefs`, plus `unborn` only when advertised, then parses the bounded
/// response. For v0/v1 it writes nothing. The caller must close the session after the returned
/// inventory; the remote may wait for another command. Blocking I/O cannot be forcibly cancelled.
/// No object request or local mutation occurs.
///
/// # Errors
///
/// Rejects unsupported capabilities or object formats, malformed or inconsistent references,
/// truncation, exceeded limits, interruption and I/O failure. A failed v2 request may already
/// have sent bytes to the service, but has no local effects.
pub fn discover_session(
    reader: &mut impl Read,
    writer: &mut impl Write,
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<RemoteDiscovery, FetchError> {
    traced("session", || {
        let budget = limits.max_wire_bytes.min(limits.max_advertisement_bytes);
        let mut wire = Wire {
            reader,
            remaining: budget,
            cancel,
        };
        let first = wire
            .packet()?
            .ok_or(FetchError::Protocol("empty protocol advertisement"))?;
        if first == b"version 2\n" {
            return discover_v2(wire, writer, limits);
        }
        let mut prefix = Vec::new();
        packet(&mut prefix, &first, cancel)?;
        let mut stream = std::io::Cursor::new(prefix).chain(wire.reader);
        let mut legacy = Wire {
            reader: &mut stream,
            remaining: budget,
            cancel,
        };
        let (advertisement, version) = protocol::advertise_with_version(&mut legacy, limits)?;
        let mut discovered = RemoteDiscovery::from_advertisement(advertisement)?;
        discovered.version = version;
        Ok(discovered)
    })
}

fn discover_v2(
    mut wire: Wire<'_, impl Read>,
    writer: &mut impl Write,
    limits: FetchLimits,
) -> Result<RemoteDiscovery, FetchError> {
    let mut capabilities = read_v2_capabilities(&mut wire, limits)?;
    let format = capability_format(&capabilities)?;
    if !capabilities.iter().any(|cap| cap.starts_with(b"ls-refs")) {
        return Err(FetchError::Unsupported("v2 ls-refs"));
    }
    write_ls_refs(writer, format, &capabilities, wire.cancel)?;
    let (refs, head_hint) = read_v2_refs(&mut wire, limits, format)?;
    if let Some(branch) = head_hint {
        let mut hint = b"symref=HEAD:".to_vec();
        hint.extend_from_slice(branch.as_bytes());
        capabilities.push(hint);
    }
    let mut discovered = RemoteDiscovery::from_advertisement(Advertisement { refs, capabilities })?;
    discovered.version = ProtocolVersion::V2;
    Ok(discovered)
}

fn read_v2_capabilities(
    wire: &mut Wire<'_, impl Read>,
    limits: FetchLimits,
) -> Result<Vec<Vec<u8>>, FetchError> {
    let mut capabilities = Vec::new();
    while let Some(line) = wire.packet()? {
        let line = line
            .strip_suffix(b"\n")
            .ok_or(FetchError::Protocol("v2 capability line"))?;
        if line.is_empty() || line.contains(&0) || line.iter().any(u8::is_ascii_control) {
            return Err(FetchError::Protocol("v2 capability token"));
        }
        if capabilities.len() == limits.max_refs {
            return Err(FetchError::Limit("v2 capabilities"));
        }
        capabilities.push(line.to_vec());
    }
    Ok(capabilities)
}

fn write_ls_refs(
    writer: &mut impl Write,
    format: ObjectFormat,
    capabilities: &[Vec<u8>],
    cancel: &AtomicBool,
) -> Result<(), FetchError> {
    packet(writer, b"command=ls-refs\n", cancel)?;
    if format == ObjectFormat::Sha256 {
        packet(writer, b"object-format=sha256\n", cancel)?;
    }
    put(writer, b"0001", cancel)?;
    packet(writer, b"peel\n", cancel)?;
    packet(writer, b"symrefs\n", cancel)?;
    if capabilities.iter().any(|cap| cap == b"ls-refs=unborn") {
        packet(writer, b"unborn\n", cancel)?;
    }
    put(writer, b"0000", cancel)?;
    writer.flush()?;

    Ok(())
}

fn read_v2_refs(
    wire: &mut Wire<'_, impl Read>,
    limits: FetchLimits,
    format: ObjectFormat,
) -> Result<(Vec<super::AdvertisedRef>, Option<RefName>), FetchError> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let mut head_hint = None;
    while let Some(line) = wire.packet()? {
        let line = line
            .strip_suffix(b"\n")
            .ok_or(FetchError::Protocol("v2 reference line"))?;
        let mut parts = line.split(|byte| *byte == b' ');
        let raw_id = parts.next().ok_or(FetchError::Protocol("v2 reference"))?;
        let raw_name = parts.next().ok_or(FetchError::Protocol("v2 reference"))?;
        let name = RefName::new(raw_name).map_err(|_| FetchError::Protocol("v2 ref name"))?;
        if !seen.insert(name.clone()) {
            return Err(FetchError::Protocol("duplicate v2 ref"));
        }
        let unborn = raw_id == b"unborn";
        let id = if unborn {
            None
        } else {
            let value = std::str::from_utf8(raw_id)
                .ok()
                .and_then(|raw| ObjectId::from_hex(format, raw).ok())
                .filter(|id| !id.is_null())
                .ok_or(FetchError::Protocol("v2 object ID"))?;
            Some(value)
        };
        let mut peeled = None;
        let mut target = None;
        for attribute in parts {
            if let Some(raw) = attribute.strip_prefix(b"symref-target:") {
                let value =
                    RefName::new(raw).map_err(|_| FetchError::Protocol("v2 symref target"))?;
                if target.replace(value).is_some() {
                    return Err(FetchError::Protocol("duplicate v2 symref target"));
                }
            } else if let Some(raw) = attribute.strip_prefix(b"peeled:") {
                let value = std::str::from_utf8(raw)
                    .ok()
                    .and_then(|raw| ObjectId::from_hex(format, raw).ok())
                    .filter(|id| !id.is_null())
                    .ok_or(FetchError::Protocol("v2 peeled ID"))?;
                if peeled.replace(value).is_some() {
                    return Err(FetchError::Protocol("duplicate v2 peeled ID"));
                }
            } else {
                return Err(FetchError::Unsupported("v2 ref attribute"));
            }
        }
        if name.as_bytes() == b"HEAD" {
            head_hint = target;
        } else if unborn {
            return Err(FetchError::Unsupported("v2 non-HEAD unborn ref"));
        }
        if unborn && head_hint.is_none() {
            return Err(FetchError::Protocol("unborn HEAD without target"));
        }
        if let Some(id) = id {
            if refs.len() == limits.max_refs {
                return Err(FetchError::Limit("advertised refs"));
            }
            refs.push(super::AdvertisedRef {
                name: name.clone(),
                id,
                peeled: false,
            });
            if let Some(id) = peeled {
                if !name.as_bytes().starts_with(b"refs/tags/") {
                    return Err(FetchError::Protocol("v2 non-tag peel"));
                }
                if refs.len() == limits.max_refs {
                    return Err(FetchError::Limit("advertised refs"));
                }
                refs.push(super::AdvertisedRef {
                    name,
                    id,
                    peeled: true,
                });
            }
        } else if peeled.is_some() {
            return Err(FetchError::Protocol("unborn peeled ref"));
        }
    }
    Ok((refs, head_hint))
}

fn capability_format(capabilities: &[Vec<u8>]) -> Result<ObjectFormat, FetchError> {
    let advertisement = Advertisement {
        refs: Vec::new(),
        capabilities: capabilities.to_vec(),
    };
    advertisement.object_format()
}

/// Lists a trusted local repository through girt without starting Git or changing storage.
///
/// Tag peel hints are verified through the local object store. The reference snapshot can race
/// with another writer; a later transfer must revalidate its selected IDs. Cancellation and an
/// optional deadline are checked between filesystem operations.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{FetchLimits, RemoteHead, discover_local};
/// use girt::transport::TransportControl;
/// use girt::{InitKind, ObjectFormat, Repository};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let root = tempfile::tempdir()?;
/// let repository = Repository::init(
///     ObjectFormat::Sha256,
///     root.path().join("repo"),
///     InitKind::Bare,
/// )?;
/// let cancel = AtomicBool::new(false);
/// let discovered = discover_local(
///     repository.git_dir(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
/// )?;
/// assert_eq!(discovered.object_format, ObjectFormat::Sha256);
/// assert!(matches!(discovered.head, RemoteHead::Unborn { .. }));
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns repository, reference, tag, limit, format, or interruption failures.
pub fn discover_local(
    source: impl AsRef<Path>,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<RemoteDiscovery, FetchError> {
    traced("local", || {
        control.check()?;
        let source =
            Repository::open(source).map_err(|_| FetchError::Protocol("local repository"))?;
        let advertisement = local_native::advertise(&source, limits, control)?;
        let mut discovered = RemoteDiscovery::from_advertisement(advertisement)?;
        discovered.version = ProtocolVersion::Native;
        let head = RefName::new(b"HEAD").expect("valid HEAD");
        if let Some(crate::refs::Target::Direct(id)) = source
            .references()
            .map_err(|_| FetchError::Protocol("local references"))?
            .read(&head)
            .map_err(|_| FetchError::Protocol("local HEAD"))?
        {
            if !discovered
                .advertisement
                .refs
                .iter()
                .any(|reference| reference.name == head && reference.id == id && !reference.peeled)
            {
                return Err(FetchError::Protocol("local HEAD changed during discovery"));
            }
            discovered.head = RemoteHead::Detached { id };
        }
        control.check()?;
        Ok(discovered)
    })
}

fn traced(
    endpoint: &'static str,
    operation: impl FnOnce() -> Result<RemoteDiscovery, FetchError>,
) -> Result<RemoteDiscovery, FetchError> {
    #[cfg(feature = "tracing")]
    {
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.discover",
            endpoint,
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
        );
        let result = span.in_scope(operation);
        crate::trace::finish(&span, &result, crate::trace::fetch);
        result
    }
    #[cfg(not(feature = "tracing"))]
    {
        let _ = endpoint;
        operation()
    }
}

/// Discovers a smart-HTTP upload-pack endpoint without sending a fetch RPC.
///
/// Uses the endpoint's owned trust, credential, redirect and deadline policy. The HTTP service
/// currently requests v0; a v2-only response is rejected before any object or ref mutation.
///
/// # Errors
///
/// Returns transport, malformed advertisement, limit or interruption failures.
#[cfg(feature = "http")]
pub async fn discover_http(
    remote: &crate::transport::http::HttpRemote,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<RemoteDiscovery, FetchError> {
    traced_async("http", async {
        control.check()?;
        let (bytes, _) = remote
            .discover(
                "git-upload-pack",
                limits.max_advertisement_bytes.min(limits.max_wire_bytes),
                control,
            )
            .await?;
        discover(&mut bytes.as_slice(), limits, control.cancel)
    })
    .await
}

/// Discovers a Unix OpenSSH upload-pack endpoint in one process session.
///
/// Sends only the v0 empty selection flush after discovery so the service exits. The configured
/// endpoint controls SSH trust, authentication and process cleanup. No transfer is requested.
///
/// # Errors
///
/// Returns transport, malformed advertisement, limit or interruption failures.
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub async fn discover_ssh(
    remote: &crate::transport::ssh::SshRemote,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<RemoteDiscovery, FetchError> {
    traced_async("ssh", async {
        control.check()?;
        let mut session = remote.connect("git-upload-pack", control)?;
        let bytes = session
            .advertise(
                limits.max_advertisement_bytes.min(limits.max_wire_bytes),
                control,
            )
            .await?;
        let discovered = discover(&mut bytes.as_slice(), limits, control.cancel)?;
        let (body, result, _) = session
            .exchange(b"0000", &[], limits.max_wire_bytes - bytes.len(), control)
            .await;
        result?;
        if !body.is_empty() {
            return Err(FetchError::Protocol("trailing discovery response"));
        }
        Ok(discovered)
    })
    .await
}

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
async fn traced_async(
    endpoint: &'static str,
    operation: impl std::future::Future<Output = Result<RemoteDiscovery, FetchError>>,
) -> Result<RemoteDiscovery, FetchError> {
    #[cfg(feature = "tracing")]
    {
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.discover",
            endpoint,
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
        );
        let result = tracing::Instrument::instrument(operation, span.clone()).await;
        crate::trace::finish(&span, &result, crate::trace::fetch);
        result
    }
    #[cfg(not(feature = "tracing"))]
    {
        let _ = endpoint;
        operation.await
    }
}

pub(crate) fn interpret_head(advertisement: &Advertisement) -> Result<RemoteHead, FetchError> {
    let mut hint = None;
    for capability in &advertisement.capabilities {
        if let Some(target) = capability.strip_prefix(b"symref=HEAD:") {
            let branch =
                RefName::new(target).map_err(|_| FetchError::Protocol("HEAD symbolic target"))?;
            if !branch.as_bytes().starts_with(b"refs/heads/") || hint.replace(branch).is_some() {
                return Err(FetchError::Protocol("HEAD symbolic target"));
            }
        }
    }
    let head = advertisement
        .refs
        .iter()
        .filter(|reference| reference.name.as_bytes() == b"HEAD" && !reference.peeled)
        .map(|reference| reference.id)
        .next();
    if let Some(branch) = hint {
        let target = advertisement
            .refs
            .iter()
            .find(|reference| reference.name == branch && !reference.peeled)
            .map(|reference| reference.id);
        return match (head, target) {
            (Some(id), Some(target)) if id == target => Ok(RemoteHead::Symbolic { branch, id }),
            (None, None) => Ok(RemoteHead::Unborn { branch }),
            _ => Err(FetchError::Protocol("inconsistent symbolic HEAD")),
        };
    }
    let Some(id) = head else {
        return Ok(RemoteHead::Missing);
    };
    let mut branches: Vec<_> = advertisement
        .refs
        .iter()
        .filter(|reference| {
            !reference.peeled
                && reference.name.as_bytes().starts_with(b"refs/heads/")
                && reference.id == id
        })
        .map(|reference| reference.name.clone())
        .collect();
    branches.sort();
    match branches.len() {
        0 => Ok(RemoteHead::Detached { id }),
        1 => Ok(RemoteHead::Inferred {
            branch: branches.remove(0),
            id,
        }),
        _ => Ok(RemoteHead::Ambiguous { id, branches }),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn reference(name: &[u8], id: ObjectId) -> super::super::AdvertisedRef {
        super::super::AdvertisedRef {
            name: RefName::new(name).unwrap(),
            id,
            peeled: false,
        }
    }

    #[rstest]
    #[case::unborn(vec![], vec![b"symref=HEAD:refs/heads/main".to_vec()], RemoteHead::Unborn { branch: RefName::new(b"refs/heads/main").unwrap() })]
    #[case::missing(vec![], vec![], RemoteHead::Missing)]
    #[case::symbolic(vec![reference(b"HEAD", ObjectId::Sha1([1; 20])), reference(b"refs/heads/main", ObjectId::Sha1([1; 20]))], vec![b"symref=HEAD:refs/heads/main".to_vec()], RemoteHead::Symbolic { branch: RefName::new(b"refs/heads/main").unwrap(), id: ObjectId::Sha1([1; 20]) })]
    #[case::inferred(vec![reference(b"HEAD", ObjectId::Sha1([1; 20])), reference(b"refs/heads/main", ObjectId::Sha1([1; 20]))], vec![], RemoteHead::Inferred { branch: RefName::new(b"refs/heads/main").unwrap(), id: ObjectId::Sha1([1; 20]) })]
    #[case::detached(vec![reference(b"HEAD", ObjectId::Sha1([1; 20]))], vec![], RemoteHead::Detached { id: ObjectId::Sha1([1; 20]) })]
    #[case::ambiguous(vec![reference(b"HEAD", ObjectId::Sha1([1; 20])), reference(b"refs/heads/z", ObjectId::Sha1([1; 20])), reference(b"refs/heads/a", ObjectId::Sha1([1; 20]))], vec![], RemoteHead::Ambiguous { id: ObjectId::Sha1([1; 20]), branches: vec![RefName::new(b"refs/heads/a").unwrap(), RefName::new(b"refs/heads/z").unwrap()] })]
    fn interprets_head_without_guessing(
        #[case] refs: Vec<super::super::AdvertisedRef>,
        #[case] capabilities: Vec<Vec<u8>>,
        #[case] expected: RemoteHead,
    ) {
        let discovered =
            RemoteDiscovery::from_advertisement(Advertisement { refs, capabilities }).unwrap();
        assert_eq!(discovered.head, expected);
    }

    #[test]
    fn rejects_conflicting_symbolic_head() {
        let advertisement = Advertisement {
            refs: vec![
                reference(b"HEAD", ObjectId::Sha1([1; 20])),
                reference(b"refs/heads/main", ObjectId::Sha1([2; 20])),
            ],
            capabilities: vec![b"symref=HEAD:refs/heads/main".to_vec()],
        };
        assert!(matches!(
            RemoteDiscovery::from_advertisement(advertisement),
            Err(FetchError::Protocol("inconsistent symbolic HEAD"))
        ));
    }

    #[test]
    fn format_mismatch_preserves_both_formats_in_error() {
        let discovery = RemoteDiscovery::from_advertisement(Advertisement {
            refs: vec![],
            capabilities: vec![b"object-format=sha256".to_vec()],
        })
        .unwrap();
        assert!(matches!(
            discovery.require_format(ObjectFormat::Sha1),
            Err(FetchError::ObjectFormat(crate::ObjectFormatError {
                expected: ObjectFormat::Sha1,
                actual: ObjectFormat::Sha256
            }))
        ));
    }

    #[test]
    fn v2_unborn_sha256_requests_advertised_options() {
        let cancel = AtomicBool::new(false);
        let mut input = Vec::new();
        for line in [
            b"version 2\n".as_slice(),
            b"ls-refs=unborn\n",
            b"object-format=sha256\n",
        ] {
            packet(&mut input, line, &cancel).unwrap();
        }
        input.extend_from_slice(b"0000");
        packet(
            &mut input,
            b"unborn HEAD symref-target:refs/heads/main\n",
            &cancel,
        )
        .unwrap();
        input.extend_from_slice(b"0000");
        let mut request = Vec::new();
        let discovered = discover_session(
            &mut input.as_slice(),
            &mut request,
            FetchLimits::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(discovered.object_format, ObjectFormat::Sha256);
        assert_eq!(discovered.version, ProtocolVersion::V2);
        assert!(matches!(discovered.head, RemoteHead::Unborn { .. }));
        assert!(
            request
                .windows(b"object-format=sha256".len())
                .any(|w| w == b"object-format=sha256")
        );
        assert!(request.windows(b"unborn\n".len()).any(|w| w == b"unborn\n"));
    }

    #[rstest]
    #[case::unknown_format(b"object-format=sha999\n")]
    #[case::duplicate_format(b"object-format=sha1\n")]
    fn rejects_invalid_v2_format_before_request(#[case] extra: &[u8]) {
        let cancel = AtomicBool::new(false);
        let mut input = Vec::new();
        for line in [
            b"version 2\n".as_slice(),
            b"ls-refs\n",
            b"object-format=sha1\n",
            extra,
        ] {
            packet(&mut input, line, &cancel).unwrap();
        }
        input.extend_from_slice(b"0000");
        let mut request = Vec::new();
        assert!(
            discover_session(
                &mut input.as_slice(),
                &mut request,
                FetchLimits::default(),
                &cancel
            )
            .is_err()
        );
        assert!(request.is_empty());
    }

    #[test]
    fn cancelled_session_does_not_read_or_write() {
        let cancel = AtomicBool::new(true);
        let mut output = Vec::new();
        let result = discover_session(
            &mut b"000e version".as_slice(),
            &mut output,
            FetchLimits::default(),
            &cancel,
        );
        assert!(matches!(result, Err(FetchError::Cancelled)));
        assert!(output.is_empty());
    }

    #[test]
    fn v2_ref_limit_rejects_second_entry() {
        let cancel = AtomicBool::new(false);
        let mut input = Vec::new();
        packet(&mut input, b"version 2\n", &cancel).unwrap();
        packet(&mut input, b"ls-refs\n", &cancel).unwrap();
        input.extend_from_slice(b"0000");
        packet(
            &mut input,
            b"1111111111111111111111111111111111111111 refs/heads/a\n",
            &cancel,
        )
        .unwrap();
        packet(
            &mut input,
            b"2222222222222222222222222222222222222222 refs/heads/b\n",
            &cancel,
        )
        .unwrap();
        input.extend_from_slice(b"0000");
        let limits = FetchLimits {
            max_refs: 1,
            ..FetchLimits::default()
        };
        let result = discover_session(&mut input.as_slice(), &mut Vec::new(), limits, &cancel);
        assert!(matches!(result, Err(FetchError::Limit("advertised refs"))));
    }

    #[test]
    fn v2_advertisement_byte_limit_prevents_request() {
        let cancel = AtomicBool::new(false);
        let mut request = Vec::new();
        let result = discover_session(
            &mut b"000eversion 2\n0000".as_slice(),
            &mut request,
            FetchLimits {
                max_advertisement_bytes: 8,
                ..FetchLimits::default()
            },
            &cancel,
        );
        assert!(matches!(result, Err(FetchError::Limit(_))));
        assert!(request.is_empty());
    }
}
