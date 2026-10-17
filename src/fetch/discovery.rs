//! Read-only remote reference and HEAD interpretation.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::v2::Capabilities;
use super::{Advertisement, FetchError, FetchLimits, local_native, protocol};
use crate::packet::{Wire, packet};
use crate::refs::RefName;
use crate::transport::TransportControl;
use crate::{ObjectFormat, ObjectId, Repository};

/// Protocol family used to obtain the reference inventory.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// Modern servers send a `symref=HEAD:<branch>` hint, which gives [`Symbolic`](Self::Symbolic).
/// Without one, only HEAD's object ID is known: girt names a branch only when exactly one has that
/// ID ([`Inferred`](Self::Inferred)) and otherwise reports the ambiguity instead of guessing.
///
/// # Examples
///
/// ```
/// use girt::fetch::{AdvertisedRef, Advertisement, AdvertisementKind, RemoteDiscovery, RemoteHead};
/// use girt::refs::RefName;
/// use girt::{ObjectFormat, ObjectKind};
///
/// let id = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"tip");
/// let tip = |name: &str| -> Result<AdvertisedRef, Box<dyn std::error::Error>> {
///     Ok(AdvertisedRef::new(RefName::new(name)?, id, AdvertisementKind::Tip))
/// };
///
/// // No symref hint, one branch with HEAD's ID: inferred.
/// let one = Advertisement::new(vec![tip("HEAD")?, tip("refs/heads/main")?], Vec::new());
/// let head = RemoteDiscovery::from_advertisement(one)?.head;
/// assert!(matches!(head, RemoteHead::Inferred { ref branch, .. } if branch.as_bytes() == b"refs/heads/main"));
///
/// // Two branches share HEAD's ID: girt does not pick one.
/// let two = Advertisement::new(
///     vec![tip("HEAD")?, tip("refs/heads/main")?, tip("refs/heads/release")?],
///     Vec::new(),
/// );
/// let head = RemoteDiscovery::from_advertisement(two)?.head;
/// assert!(matches!(head, RemoteHead::Ambiguous { ref branches, .. } if branches.len() == 2));
///
/// // A symref hint settles it.
/// let hinted = Advertisement::new(
///     vec![tip("HEAD")?, tip("refs/heads/main")?, tip("refs/heads/release")?],
///     vec![b"symref=HEAD:refs/heads/release".to_vec()],
/// );
/// let head = RemoteDiscovery::from_advertisement(hinted)?.head;
/// assert!(matches!(head, RemoteHead::Symbolic { ref branch, .. } if branch.as_bytes() == b"refs/heads/release"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum RemoteHead {
    /// HEAD explicitly names a branch. Its ID is HEAD's; when the branch is also listed, both IDs
    /// agree. Only a prefix-selected v2 inventory may omit the branch itself.
    Symbolic {
        /// Branch named by the symbolic hint.
        branch: RefName,
        /// Advertised ID of HEAD, and of the branch when it is listed.
        id: ObjectId,
    },
    /// HEAD explicitly names a branch that has no tip yet.
    Unborn {
        /// Unborn branch named by the symbolic hint.
        branch: RefName,
    },
    /// HEAD has an ID but no symbolic hint and matches one advertised branch.
    Inferred {
        /// The only advertised branch whose ID matches HEAD.
        branch: RefName,
        /// Advertised ID of HEAD.
        id: ObjectId,
    },
    /// HEAD has an ID but names no uniquely identifiable branch.
    Detached {
        /// Advertised ID of HEAD.
        id: ObjectId,
    },
    /// HEAD was not advertised and no symbolic hint identifies an unborn branch.
    Missing,
    /// Multiple advertised branches match an unhinted HEAD ID.
    Ambiguous {
        /// Advertised ID of HEAD.
        id: ObjectId,
        /// Every advertised branch whose ID matches HEAD.
        branches: Vec<RefName>,
    },
}

/// Validated reference discovery, independent of any later fetch or push session.
///
/// A preview can become stale before transfer. Consumers must plan mutations against the
/// advertisement from the actual transfer session. Discovery retains unknown capability tokens
/// for inspection but never requests them. No object is downloaded or local repository mutated.
///
/// # Examples
///
/// Check what a local source offers before deciding to fetch:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::ObjectFormat;
/// use girt::fetch::{FetchLimits, ProtocolVersion, discover_local};
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, _, _) = include!("../doctest/remote.rs");
///
/// let cancel = AtomicBool::new(false);
/// let discovery = discover_local(
///     upstream.git_dir(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
/// )?;
/// // A local repository is read directly, with no wire protocol.
/// assert_eq!(discovery.version, ProtocolVersion::Native);
/// // Fails early if the source uses SHA-256 and the destination SHA-1, or the reverse.
/// discovery.require_format(repo.object_format())?;
/// assert_eq!(discovery.object_format, ObjectFormat::Sha1);
/// assert!(discovery.supports_current_fetch());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
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
        Self::from_inventory(advertisement, Inventory::Complete)
    }

    fn from_inventory(
        advertisement: Advertisement,
        inventory: Inventory,
    ) -> Result<Self, FetchError> {
        let object_format = advertisement.object_format()?;
        let mut names = HashSet::new();
        for reference in &advertisement.refs {
            if reference.id.is_null() || !names.insert((reference.name.clone(), reference.kind)) {
                return Err(FetchError::Protocol("invalid advertised ref"));
            }
        }
        for reference in &advertisement.refs {
            if reference.kind == crate::fetch::AdvertisementKind::PeeledHint
                && (!reference.name.as_bytes().starts_with(b"refs/tags/")
                    || !names
                        .contains(&(reference.name.clone(), crate::fetch::AdvertisementKind::Tip)))
            {
                return Err(FetchError::Protocol("unmatched peeled hint"));
            }
        }
        let head = interpret_head_in(&advertisement, inventory)?;
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
    /// `side-band-64k`; v2 transfer requires the `fetch` command. This is a
    /// capability check, not authorization to reuse a stale preview or evidence that all selected
    /// objects will be available.
    pub fn supports_current_fetch(&self) -> bool {
        match self.version {
            ProtocolVersion::Native => true,
            ProtocolVersion::V0 | ProtocolVersion::V1 => self.advertises(b"side-band-64k"),
            ProtocolVersion::V2 => self
                .advertisement
                .capabilities
                .iter()
                .any(|capability| capability == b"fetch" || capability.starts_with(b"fetch=")),
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
///
/// # Examples
///
/// A v0 server opens with its references as pkt-lines: a four-digit hexadecimal length, then
/// the line. The first line also carries the capabilities after a NUL byte, including the
/// `symref` hint that names `HEAD`'s branch:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{FetchLimits, ProtocolVersion, RemoteHead, discover};
///
/// fn pkt(out: &mut Vec<u8>, line: &str) {
///     out.extend(format!("{:04x}{line}", line.len() + 4).bytes());
/// }
///
/// let id = "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0";
/// let mut advertisement = Vec::new();
/// pkt(&mut advertisement, &format!("{id} HEAD\0side-band-64k ofs-delta symref=HEAD:refs/heads/main\n"));
/// pkt(&mut advertisement, &format!("{id} refs/heads/main\n"));
/// advertisement.extend(b"0000");
///
/// let discovery = discover(&mut advertisement.as_slice(), FetchLimits::default(), &AtomicBool::new(false))?;
/// assert_eq!(discovery.version, ProtocolVersion::V0);
/// assert_eq!(discovery.advertisement.refs.len(), 2);
/// assert!(discovery.advertises(b"side-band-64k"));
/// assert!(matches!(discovery.head, RemoteHead::Symbolic { ref branch, .. } if branch.as_bytes() == b"refs/heads/main"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
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
    discover_session_with_prefixes(reader, writer, &[], limits, cancel)
}

/// Negotiates discovery like [`discover_session`], asking a v2 server for selected references.
///
/// Each prefix becomes one v2 `ref-prefix` argument, asking the server to list only references
/// whose full name starts with one of them; an empty list asks for every reference. Prefixes are an
/// optimisation, not a filter: v0/v1 servers always advertise every reference and a v2 server may
/// return extra references, so callers must still select from what is returned. A HEAD whose
/// symbolic target falls outside the selection is reported as [`RemoteHead::Symbolic`].
///
/// # Errors
///
/// Returns [`discover_session`]'s errors. Empty prefixes, prefixes longer than 4096 bytes or
/// containing NUL or LF, and more than [`FetchLimits::max_refs`] prefixes are rejected before any
/// request is written.
///
/// # Examples
///
/// A protocol v2 server opens with capabilities instead of references; the client then asks for
/// references with an `ls-refs` command. Here both directions are in memory, so the example shows
/// the request girt writes:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{FetchLimits, ProtocolVersion, discover_session_with_prefixes};
///
/// fn pkt(out: &mut Vec<u8>, line: &str) {
///     out.extend(format!("{:04x}{line}", line.len() + 4).bytes());
/// }
///
/// let id = "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0";
/// let mut server = Vec::new();
/// for capability in [
///     "version 2\n",
///     "ls-refs=unborn\n",
///     "fetch=shallow\n",
///     "object-format=sha1\n",
/// ] {
///     pkt(&mut server, capability);
/// }
/// server.extend(b"0000");
/// // The server's answer to `ls-refs`.
/// pkt(
///     &mut server,
///     &format!("{id} HEAD symref-target:refs/heads/main\n"),
/// );
/// pkt(&mut server, &format!("{id} refs/heads/main\n"));
/// server.extend(b"0000");
///
/// let mut request = Vec::new();
/// let discovery = discover_session_with_prefixes(
///     &mut server.as_slice(),
///     &mut request,
///     &[b"refs/heads/"],
///     FetchLimits::default(),
///     &AtomicBool::new(false),
/// )?;
/// assert_eq!(discovery.version, ProtocolVersion::V2);
/// assert_eq!(discovery.advertisement.refs.len(), 2);
///
/// let request = String::from_utf8(request)?;
/// assert!(request.starts_with("0014command=ls-refs\n"));
/// assert!(request.contains("ref-prefix refs/heads/\n"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn discover_session_with_prefixes(
    reader: &mut impl Read,
    writer: &mut impl Write,
    prefixes: &[&[u8]],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<RemoteDiscovery, FetchError> {
    traced("session", || {
        super::v2::validate_prefixes(prefixes, limits)?;
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
            return discover_v2(wire, writer, prefixes, limits);
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
    prefixes: &[&[u8]],
    limits: FetchLimits,
) -> Result<RemoteDiscovery, FetchError> {
    let capabilities = Capabilities::read(&mut wire, limits)?;
    let advertisement = capabilities.ls_refs(&mut wire, writer, prefixes, limits)?;
    let inventory = if prefixes.is_empty() {
        Inventory::Complete
    } else {
        Inventory::Selected
    };
    let mut discovered = RemoteDiscovery::from_inventory(advertisement, inventory)?;
    discovered.version = ProtocolVersion::V2;
    Ok(discovered)
}

/// Whether an advertisement lists every reference or only a prefix selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Inventory {
    Complete,
    Selected,
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
            Repository::open(source).map_err(|error| FetchError::Source(Box::new(error)))?;
        let advertisement = local_native::advertise(&source, limits, control)?;
        let mut discovered = RemoteDiscovery::from_advertisement(advertisement)?;
        discovered.version = ProtocolVersion::Native;
        let head = RefName::HEAD;
        if let Some(crate::refs::Target::Direct(id)) = source
            .references()
            .map_err(|_| FetchError::Protocol("local references"))?
            .read(&head)
            .map_err(|_| FetchError::Protocol("local HEAD"))?
            && !discovered.advertisement.refs.iter().any(|reference| {
                reference.name == head
                    && reference.id == id
                    && reference.kind == crate::fetch::AdvertisementKind::Tip
            })
        {
            return Err(FetchError::Protocol("local HEAD changed during discovery"));
        }
        // Like upload-pack's advertisement, a detached HEAD matching exactly one branch is
        // reported as that branch (`RemoteHead::Inferred`).
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
/// Uses the endpoint's owned trust, credential, redirect and deadline policy. Requests the
/// endpoint's [`crate::transport::http::HttpRemote::protocol`]; a v2 server is listed with one
/// `ls-refs` request, a v0/v1 server by its advertisement.
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
        let (advertisement, _, version, _) = super::http::open(remote, limits, control).await?;
        let mut discovered = RemoteDiscovery::from_advertisement(advertisement)?;
        discovered.version = version;
        Ok(discovered)
    })
    .await
}

/// Discovers a Unix OpenSSH upload-pack endpoint in one process session.
///
/// Requests the remote's [`crate::transport::ssh::SshRemote::protocol`]. A v2 server is listed
/// with one `ls-refs` command. A final flush ends the session so the service exits. The configured
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
    discover_ssh_with_diagnostics(remote, limits, control, |_| {}).await
}

/// Discovers an SSH endpoint while delivering raw local process diagnostics.
///
/// The callback receives stderr bytes while service I/O runs, including unterminated messages.
/// Chunks are at most 8192 bytes; chunk boundaries have no line or encoding meaning. No transcript
/// is retained by girt. Diagnostic bytes may contain credentials, paths and untrusted terminal
/// escapes: the caller owns display, redaction and any retained-buffer limits. Errors, Debug and
/// tracing remain redacted. The default [`discover_ssh`] discards these diagnostics.
///
/// The callback runs synchronously and must return promptly to preserve cancellation and deadline
/// responsiveness. It has no return value and does not select retry or transport outcomes. A
/// callback panic unwinds normally and drops the owned session. Successful completion drains final
/// diagnostics before returning; cancellation and transport failure may leave unread diagnostics.
/// No authentication or terminal policy is changed, and a launched failure is never retried.
///
/// # Errors
///
/// Returns the same transport, protocol, limit and interruption errors as [`discover_ssh`].
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub async fn discover_ssh_with_diagnostics(
    remote: &crate::transport::ssh::SshRemote,
    limits: FetchLimits,
    control: TransportControl<'_>,
    mut diagnostics: impl FnMut(&[u8]),
) -> Result<RemoteDiscovery, FetchError> {
    traced_async("ssh", async {
        control.check()?;
        let mut session = remote.connect("git-upload-pack", control)?;
        let (advertisement, _, version, used) = super::ssh::open(
            &mut session,
            remote.protocol(),
            limits,
            control,
            &mut diagnostics,
        )
        .await?;
        let mut discovered = RemoteDiscovery::from_advertisement(advertisement)?;
        discovered.version = version;
        // A flush ends both a v0 selection and a v2 session; the server then exits.
        let (body, result, _) = session
            .exchange_with_diagnostics(
                b"0000",
                &[],
                limits.max_wire_bytes - used,
                control,
                &mut diagnostics,
            )
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
    interpret_head_in(advertisement, Inventory::Complete)
}

fn interpret_head_in(
    advertisement: &Advertisement,
    inventory: Inventory,
) -> Result<RemoteHead, FetchError> {
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
        .filter(|reference| {
            reference.name.as_bytes() == b"HEAD"
                && reference.kind == crate::fetch::AdvertisementKind::Tip
        })
        .map(|reference| reference.id)
        .next();
    if let Some(branch) = hint {
        let target = advertisement
            .refs
            .iter()
            .find(|reference| {
                reference.name == branch && reference.kind == crate::fetch::AdvertisementKind::Tip
            })
            .map(|reference| reference.id);
        return match (head, target) {
            (Some(id), Some(target)) if id == target => Ok(RemoteHead::Symbolic { branch, id }),
            // A complete inventory must list the target; a prefix selection may exclude it.
            (Some(id), None) if inventory == Inventory::Selected => {
                Ok(RemoteHead::Symbolic { branch, id })
            }
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
            reference.kind == crate::fetch::AdvertisementKind::Tip
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
            kind: crate::fetch::AdvertisementKind::Tip,
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
    fn omitted_target_is_symbolic_only_in_a_selected_inventory() {
        let advertisement = Advertisement {
            refs: vec![reference(b"HEAD", ObjectId::Sha1([1; 20]))],
            capabilities: vec![b"symref=HEAD:refs/heads/main".to_vec()],
        };
        assert!(matches!(
            interpret_head_in(&advertisement, Inventory::Selected),
            Ok(RemoteHead::Symbolic { .. })
        ));
        assert!(matches!(
            interpret_head(&advertisement),
            Err(FetchError::Protocol("inconsistent symbolic HEAD"))
        ));
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

    fn v2_discovery(capabilities: &[&[u8]], refs: &[&[u8]]) -> Result<RemoteDiscovery, FetchError> {
        let cancel = AtomicBool::new(false);
        let mut input = Vec::new();
        packet(&mut input, b"version 2\n", &cancel).unwrap();
        for line in capabilities {
            packet(&mut input, line, &cancel).unwrap();
        }
        input.extend_from_slice(b"0000");
        for line in refs {
            packet(&mut input, line, &cancel).unwrap();
        }
        input.extend_from_slice(b"0000");
        discover_session(
            &mut input.as_slice(),
            &mut Vec::new(),
            FetchLimits::default(),
            &cancel,
        )
    }

    #[test]
    fn v2_symref_capability_cannot_invent_unborn_head() {
        let discovered = v2_discovery(
            &[
                b"ls-refs\n",
                b"symref=HEAD:refs/heads/main\n",
                b"other=kept\n",
            ],
            &[],
        )
        .unwrap();
        assert_eq!(discovered.head, RemoteHead::Missing);
        assert!(discovered.advertises(b"other=kept"));
        assert!(!discovered.advertises(b"symref=HEAD:refs/heads/main"));
    }

    #[test]
    fn v2_symref_capability_does_not_duplicate_ls_refs_head() {
        let id = "1111111111111111111111111111111111111111";
        let head = format!("{id} HEAD symref-target:refs/heads/main\n");
        let main = format!("{id} refs/heads/main\n");
        let discovered = v2_discovery(
            &[b"ls-refs\n", b"symref=HEAD:refs/heads/other\n"],
            &[head.as_bytes(), main.as_bytes()],
        )
        .unwrap();
        assert!(matches!(
            discovered.head,
            RemoteHead::Symbolic { branch, .. } if branch.as_bytes() == b"refs/heads/main"
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
