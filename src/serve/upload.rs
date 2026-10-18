//! Protocol v2 upload-pack: capability advertisement, `ls-refs` and `fetch`.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

mod v0;

use super::shallow::{self, Deepen};
use super::{Protocol, ServeError};
use crate::hide_refs::HideRules;
use crate::packet::{self, Frame, Wire};
use crate::refs::{RefName, Target};
use crate::{
    IdSet, ObjectFormat, ObjectId, ObjectKind, Objects, PackCompression, PackLimits,
    PackWriteLimits, PeelLimits, ReadLimits, Repository,
};

/// Largest sideband payload: the pkt-line maximum less the header and band byte.
const SIDEBAND_DATA: usize = 65515;

/// Bounds on one upload-pack session.
///
/// The defaults suit untrusted clients. [`Self::trusted`] lifts them for clients the caller
/// trusts, as Git does for every client.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct UploadLimits {
    /// Request bytes read in one session, including pkt-line framing (default 64 MiB).
    pub max_request_bytes: usize,
    /// `want` lines in one `fetch` request (default 100,000).
    pub max_wants: usize,
    /// `have` lines in one `fetch` request (default 100,000).
    pub max_haves: usize,
    /// Edge occurrences followed while selecting objects to send, and separately while marking
    /// the client's common history (default 4 million each).
    pub max_edges: usize,
    /// Per-object storage decoding bounds.
    pub read: ReadLimits,
    /// Bounds on the generated pack. The pack is streamed, so the byte bounds limit output rather
    /// than memory.
    pub pack: PackWriteLimits,
    /// Compression for objects that have no reusable stored form.
    pub compression: PackCompression,
    /// Bounds for opening the served repository's packs.
    pub packs: PackLimits,
    /// Bounds on the commit walk that [`WantPolicy::Reachable`] uses.
    pub history: crate::HistoryLimits,
}

impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 64 * 1024 * 1024,
            max_wants: 100_000,
            max_haves: 100_000,
            max_edges: 4_000_000,
            read: ReadLimits::default(),
            pack: PackWriteLimits::default(),
            compression: PackCompression::Delta(crate::DeltaOptions::DEFAULT),
            packs: PackLimits::default(),
            history: crate::HistoryLimits::default(),
        }
    }
}

impl UploadLimits {
    /// Returns limits large enough that only available memory bounds the work.
    pub const fn trusted() -> Self {
        const UNBOUNDED: usize = usize::MAX / 4;
        Self {
            max_request_bytes: UNBOUNDED,
            max_wants: UNBOUNDED,
            max_haves: UNBOUNDED,
            max_edges: UNBOUNDED,
            read: ReadLimits::trusted(),
            pack: PackWriteLimits::trusted(),
            compression: PackCompression::Delta(crate::DeltaOptions::DEFAULT),
            packs: PackLimits::trusted(),
            history: crate::HistoryLimits::trusted(),
        }
    }

    /// Sets [`Self::max_request_bytes`].
    #[must_use]
    pub fn with_max_request_bytes(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    /// Sets [`Self::max_wants`].
    #[must_use]
    pub fn with_max_wants(mut self, max_wants: usize) -> Self {
        self.max_wants = max_wants;
        self
    }

    /// Sets [`Self::max_haves`].
    #[must_use]
    pub fn with_max_haves(mut self, max_haves: usize) -> Self {
        self.max_haves = max_haves;
        self
    }

    /// Sets [`Self::max_edges`].
    #[must_use]
    pub fn with_max_edges(mut self, max_edges: usize) -> Self {
        self.max_edges = max_edges;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }

    /// Sets [`Self::pack`].
    #[must_use]
    pub fn with_pack(mut self, pack: PackWriteLimits) -> Self {
        self.pack = pack;
        self
    }

    /// Sets [`Self::compression`].
    #[must_use]
    pub fn with_compression(mut self, compression: PackCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Sets [`Self::packs`].
    #[must_use]
    pub fn with_packs(mut self, packs: PackLimits) -> Self {
        self.packs = packs;
        self
    }

    /// Sets [`Self::history`].
    #[must_use]
    pub fn with_history(mut self, history: crate::HistoryLimits) -> Self {
        self.history = history;
        self
    }

    fn selection(&self) -> crate::push::PushLimits {
        crate::push::PushLimits {
            max_refs: self.max_haves,
            max_edges: self.max_edges,
            read: self.read,
            pack: self.pack,
            compression: self.compression,
            ..Default::default()
        }
    }
}

/// Which objects a client may name in a `want` line.
///
/// Each policy allows everything the previous one does. Like Git, protocol v2 accepts any object
/// unless [`UploadPack::with_want_policy`] says otherwise. Protocol v0 follows
/// `uploadpack.allowTipSHA1InWant`, `uploadpack.allowReachableSHA1InWant` and
/// `uploadpack.allowAnySHA1InWant`; an explicit false `allowAnySHA1InWant` turns off the other
/// two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum WantPolicy {
    /// Objects that a visible reference names, directly or by peeling a tag (Git's default).
    #[default]
    Advertised,
    /// Also the objects that hidden references name.
    Tips,
    /// Also commits reachable from any reference, hidden or not, and, as in Git, any object
    /// that isn't a commit.
    Reachable,
    /// Any object in the repository.
    Any,
}

/// Serves fetches from one repository, as `git upload-pack` does.
///
/// Hidden references follow `uploadpack.hideRefs` and `transfer.hideRefs`. Clients may only
/// want objects that a visible reference names directly or by peeling a tag.
pub struct UploadPack<'r> {
    repository: &'r Repository,
    hide: HideRules,
    /// The policy from `uploadpack.allow*SHA1InWant`, which Git applies to protocol v0 only.
    configured_wants: WantPolicy,
    /// A caller's policy, applied to every protocol.
    wants: Option<WantPolicy>,
    /// Whether the v2 `object-info` command is offered.
    object_info: bool,
    limits: UploadLimits,
}

impl std::fmt::Debug for UploadPack<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadPack")
            .field("git_dir", &self.repository.git_dir())
            .field("configured_wants", &self.configured_wants)
            .field("wants", &self.wants)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// One served reference: its name, direct value, symbolic target and peeled value.
struct Advertised {
    name: RefName,
    id: ObjectId,
    symref: Option<RefName>,
    peeled: Option<ObjectId>,
    /// Kind of the object reached by peeling every tag.
    kind: ObjectKind,
    /// Tag objects passed while peeling, outermost first.
    tags: Vec<ObjectId>,
    /// Whether hide rules keep the reference out of `ls-refs`.
    hidden: bool,
}

/// The parsed options of one `fetch` request.
#[derive(Default)]
struct FetchRequest {
    wants: Vec<ObjectId>,
    haves: Vec<ObjectId>,
    done: bool,
    ofs_delta: bool,
    include_tag: bool,
    thin_pack: bool,
    wait_for_done: bool,
    /// The client's current boundary commits, in request order.
    shallow: Vec<ObjectId>,
    shallow_seen: IdSet,
    deepen: Deepen,
    /// `deepen-not` names, resolved after parsing.
    deepen_not: Vec<Vec<u8>>,
}

impl<'r> UploadPack<'r> {
    /// Prepares to serve `repository` with the default [`UploadLimits`].
    ///
    /// # Errors
    ///
    /// Returns [`ServeError::Config`] when a hide rule is implicit or empty.
    pub fn new(repository: &'r Repository) -> Result<Self, ServeError> {
        let mut configured_wants = WantPolicy::Advertised;
        for (name, policy) in [
            ("allowtipsha1inwant", WantPolicy::Tips),
            ("allowreachablesha1inwant", WantPolicy::Reachable),
        ] {
            if config_boolean(repository, name)? == Some(true) {
                configured_wants = configured_wants.max(policy);
            }
        }
        match config_boolean(repository, "allowanysha1inwant")? {
            Some(true) => configured_wants = WantPolicy::Any,
            Some(false) => configured_wants = WantPolicy::Advertised,
            None => {}
        }
        Ok(Self {
            repository,
            hide: HideRules::parse(repository, "uploadpack").map_err(ServeError::Config)?,
            configured_wants,
            wants: None,
            object_info: section_boolean(repository, "transfer", "advertiseobjectinfo")?
                .unwrap_or(false),
            limits: UploadLimits::default(),
        })
    }

    /// Sets which objects clients may want in every protocol, replacing Git's defaults.
    ///
    /// Use this to keep hidden references private from protocol v2 clients, which Git doesn't.
    #[must_use]
    pub fn with_want_policy(mut self, wants: WantPolicy) -> Self {
        self.wants = Some(wants);
        self
    }

    /// Sets the session bounds.
    #[must_use]
    pub fn with_limits(mut self, limits: UploadLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Serves a full-duplex session, such as stdin and stdout under SSH: advertises references or
    /// capabilities, then answers requests until the client closes its side. A v0 session serves
    /// one fetch.
    ///
    /// Requests the client could correct are reported to it in an `ERR` packet before returning
    /// the error. Cancellation is checked between packets, between object reads and between pack
    /// writes.
    ///
    /// # Errors
    ///
    /// Returns the failure that ended the session.
    pub fn serve(
        &self,
        protocol: Protocol,
        input: &mut impl Read,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        self.advertise(protocol, output, cancel)?;
        let mut wire = Wire {
            reader: input,
            remaining: self.limits.max_request_bytes,
            cancel,
        };
        if protocol == Protocol::V0 {
            let result = self.serve_v0(&mut wire, output, cancel);
            if let Err(error) = &result {
                report(error, output, cancel);
            }
            return result;
        }
        while self.answer(&mut wire, output, cancel)? {}
        Ok(())
    }

    /// Writes the capability advertisement that starts every session.
    ///
    /// Stateless transports such as smart HTTP send this alone, then answer each later request
    /// with [`Self::respond`].
    ///
    /// # Errors
    ///
    /// Returns the failure to read the references or write the advertisement.
    pub fn advertise(
        &self,
        protocol: Protocol,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        if protocol == Protocol::V0 {
            return self.advertise_v0(output, cancel);
        }
        let format = self.repository.object_format();
        let mut lines = vec![
            "version 2".to_owned(),
            format!("agent=girt/{}", env!("CARGO_PKG_VERSION")),
            "ls-refs=unborn".to_owned(),
            "fetch=shallow wait-for-done".to_owned(),
            "server-option".to_owned(),
            format!("object-format={format}"),
        ];
        if self.object_info {
            lines.push("object-info=size type".to_owned());
        }
        for line in lines {
            packet::packet(output, format!("{line}\n").as_bytes(), cancel)?;
        }
        packet::put(output, b"0000", cancel)?;
        output.flush()?;
        Ok(())
    }

    /// Answers exactly one request, as a stateless transport does after the advertisement.
    ///
    /// # Errors
    ///
    /// Returns [`ServeError::Protocol`] when `input` holds no request, and otherwise the failure
    /// that stopped the answer.
    pub fn respond(
        &self,
        input: &mut impl Read,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let mut wire = Wire {
            reader: input,
            remaining: self.limits.max_request_bytes,
            cancel,
        };
        if self.answer(&mut wire, output, cancel)? {
            Ok(())
        } else {
            Err(ServeError::Protocol("empty request"))
        }
    }

    /// Answers one request; `false` when the client closed the stream before a request.
    fn answer<R: Read>(
        &self,
        wire: &mut Wire<'_, R>,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<bool, ServeError> {
        let result = self.answer_unreported(wire, output, cancel);
        if let Err(error) = &result {
            report(error, output, cancel);
        }
        result
    }

    fn answer_unreported<R: Read>(
        &self,
        wire: &mut Wire<'_, R>,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<bool, ServeError> {
        // A flush between requests is a keep-alive, not a request.
        let command = loop {
            match wire.frame_or_end()? {
                None => return Ok(false),
                Some(Frame::Flush) => continue,
                Some(Frame::Data(line)) => break line,
                Some(Frame::Delim | Frame::ResponseEnd) => {
                    return Err(ServeError::Protocol("expected a command"));
                }
            }
        };
        let command = trim(&command)
            .strip_prefix(b"command=")
            .ok_or(ServeError::Protocol("expected a command"))?
            .to_vec();
        let mut arguments = Vec::new();
        let mut in_arguments = false;
        // A client that names no object format asks for SHA-1.
        let mut format = ObjectFormat::Sha1;
        loop {
            match wire.frame()? {
                Frame::Flush => break,
                Frame::Delim if !in_arguments => in_arguments = true,
                Frame::Data(line) if in_arguments => arguments.push(trim(&line).to_vec()),
                Frame::Data(line) => {
                    if let Some(named) = self.check_capability(trim(&line))? {
                        format = named;
                    }
                }
                Frame::Delim | Frame::ResponseEnd => {
                    return Err(ServeError::Protocol("unexpected special packet"));
                }
            }
        }
        if format != self.repository.object_format() {
            return Err(ServeError::Unsupported("object format"));
        }
        match command.as_slice() {
            b"ls-refs" => self.ls_refs(&arguments, output, cancel)?,
            b"object-info" if self.object_info => self.object_info(&arguments, output, cancel)?,
            b"fetch" => self.fetch(&arguments, output, cancel)?,
            _ => return Err(ServeError::Unsupported("unknown command")),
        }
        output.flush()?;
        Ok(true)
    }

    /// Accepts the request capabilities girt advertises, returning a named object format.
    fn check_capability(&self, line: &[u8]) -> Result<Option<ObjectFormat>, ServeError> {
        if line.starts_with(b"agent=") || line.starts_with(b"server-option=") {
            return Ok(None);
        }
        match line.strip_prefix(b"object-format=") {
            Some(b"sha1") => Ok(Some(ObjectFormat::Sha1)),
            Some(b"sha256") => Ok(Some(ObjectFormat::Sha256)),
            Some(_) => Err(ServeError::Unsupported("object format")),
            None => Err(ServeError::Unsupported("request capability")),
        }
    }

    /// Lists visible references with HEAD first, as `git upload-pack` does.
    ///
    /// Only references that `include` accepts are read past their name.
    fn advertised(
        &self,
        include: impl Fn(&RefName) -> bool,
        cancel: &AtomicBool,
    ) -> Result<(Vec<Advertised>, Option<RefName>), ServeError> {
        let refs = self.repository.references()?;
        let objects = self.objects()?;
        let entry = |name: RefName, id: ObjectId, symref, hidden| -> Result<_, ServeError> {
            packet::check_cancelled(cancel)?;
            let limits = PeelLimits {
                read: self.limits.read,
                ..PeelLimits::default()
            };
            let peeled = objects.peel(id, limits, cancel)?;
            Ok(Advertised {
                name,
                id,
                symref,
                peeled: (!peeled.tags.is_empty()).then_some(peeled.target),
                kind: peeled.kind,
                tags: peeled.tags,
                hidden,
            })
        };
        let mut advertised = Vec::new();
        let head = refs.resolve(&RefName::HEAD)?;
        let mut unborn = None;
        let head_hidden = self.hide.matches(&RefName::HEAD) || self.hide.matches(&head.name);
        let symref = (head.name != RefName::HEAD).then(|| head.name.clone());
        match head.id {
            Some(_) if !include(&RefName::HEAD) => {}
            Some(id) => advertised.push(entry(RefName::HEAD, id, symref, head_hidden)?),
            None if !head_hidden => unborn = symref,
            None => {}
        }
        for reference in refs.list()? {
            if !include(&reference.name) {
                continue;
            }
            let hidden = self.hide.matches(&reference.name);
            let (id, symref) = match reference.target {
                Target::Direct(id) => (id, None),
                Target::Symbolic(target) => match refs.resolve(&reference.name)?.id {
                    Some(id) => (id, Some(target)),
                    None => continue,
                },
            };
            advertised.push(entry(reference.name, id, symref, hidden)?);
        }
        Ok((advertised, unborn))
    }

    /// Sets whether the v2 `object-info` command is offered, replacing
    /// `transfer.advertiseObjectInfo`.
    #[must_use]
    pub fn with_object_info(mut self, object_info: bool) -> Self {
        self.object_info = object_info;
        self
    }

    /// Answers `object-info` with the requested `size` and `type` of each object, as Git does.
    ///
    /// The reply names the attributes, then has one line per object. A missing object's line has
    /// no values. A request without objects gets an empty reply.
    fn object_info(
        &self,
        arguments: &[Vec<u8>],
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        // Like Git, each attribute is reported once, in a fixed order.
        let (mut size, mut kind) = (false, false);
        let mut ids = Vec::new();
        for argument in arguments {
            match argument.as_slice() {
                b"size" => size = true,
                b"type" => kind = true,
                argument => {
                    let hex = argument
                        .strip_prefix(b"oid ")
                        .ok_or(ServeError::Unsupported("object-info argument"))?;
                    if ids.len() == self.limits.max_wants {
                        return Err(ServeError::Limit("object-info objects"));
                    }
                    ids.push(hex);
                }
            }
        }
        if !ids.is_empty() {
            if size {
                packet::packet(output, b"size", cancel)?;
            }
            if kind {
                packet::packet(output, b"type", cancel)?;
            }
            let objects = self.objects()?;
            let format = self.repository.object_format();
            for hex in ids {
                let id = std::str::from_utf8(hex)
                    .ok()
                    .and_then(|hex| ObjectId::from_hex(format, hex).ok())
                    .ok_or(ServeError::Protocol("object-info expected an object ID"))?;
                // Git echoes the ID as the client spelled it.
                let mut line = hex.to_vec();
                match objects.header(id, self.limits.read)? {
                    Some(header) => {
                        if size {
                            line.extend_from_slice(format!(" {}", header.size()).as_bytes());
                        }
                        if kind {
                            line.push(b' ');
                            line.extend_from_slice(header.kind().as_str().as_bytes());
                        }
                    }
                    // Like Git, a missing object's line ends in one space.
                    None => line.push(b' '),
                }
                packet::packet(output, &line, cancel)?;
            }
        }
        packet::put(output, b"0000", cancel)?;
        Ok(())
    }

    fn ls_refs(
        &self,
        arguments: &[Vec<u8>],
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let mut peel = false;
        let mut symrefs = false;
        let mut unborn = false;
        let mut prefixes = Vec::new();
        for argument in arguments {
            match argument.as_slice() {
                b"peel" => peel = true,
                b"symrefs" => symrefs = true,
                b"unborn" => unborn = true,
                argument => match argument.strip_prefix(b"ref-prefix ") {
                    Some(prefix) => prefixes.push(prefix.to_vec()),
                    None => return Err(ServeError::Unsupported("ls-refs argument")),
                },
            }
        }
        let selected = |name: &RefName| {
            prefixes.is_empty() || prefixes.iter().any(|p| name.as_bytes().starts_with(p))
        };
        let (advertised, unborn_head) = self.advertised(selected, cancel)?;
        // Like Git, an unborn HEAD is listed only with its symbolic target.
        if let Some(target) = unborn_head.filter(|_| unborn && symrefs && selected(&RefName::HEAD))
        {
            let mut line = b"unborn HEAD symref-target:".to_vec();
            line.extend_from_slice(target.as_bytes());
            line.push(b'\n');
            packet::packet(output, &line, cancel)?;
        }
        for reference in advertised.iter().filter(|r| !r.hidden && selected(&r.name)) {
            let mut line = format!("{} ", reference.id).into_bytes();
            line.extend_from_slice(reference.name.as_bytes());
            if let Some(target) = reference.symref.as_ref().filter(|_| symrefs) {
                line.extend_from_slice(b" symref-target:");
                line.extend_from_slice(target.as_bytes());
            }
            if let Some(peeled) = reference.peeled.filter(|_| peel) {
                line.extend_from_slice(format!(" peeled:{peeled}").as_bytes());
            }
            line.push(b'\n');
            packet::packet(output, &line, cancel)?;
        }
        packet::put(output, b"0000", cancel)?;
        Ok(())
    }

    fn parse_fetch(&self, arguments: &[Vec<u8>]) -> Result<FetchRequest, ServeError> {
        let format = self.repository.object_format();
        let id = |hex: &[u8]| -> Result<ObjectId, ServeError> {
            std::str::from_utf8(hex)
                .ok()
                .and_then(|hex| ObjectId::from_hex(format, hex).ok())
                .ok_or(ServeError::Protocol("object ID"))
        };
        let mut request = FetchRequest::default();
        for argument in arguments {
            match argument.as_slice() {
                b"done" => request.done = true,
                b"ofs-delta" => request.ofs_delta = true,
                b"include-tag" => request.include_tag = true,
                // Progress is never sent.
                b"thin-pack" => request.thin_pack = true,
                b"wait-for-done" => request.wait_for_done = true,
                b"no-progress" => {}
                b"deepen-relative" => request.deepen.relative = true,
                argument => {
                    if let Some(hex) = argument.strip_prefix(b"want ") {
                        if request.wants.len() == self.limits.max_wants {
                            return Err(ServeError::Limit("wants"));
                        }
                        request.wants.push(id(hex)?);
                    } else if let Some(hex) = argument.strip_prefix(b"have ") {
                        if request.haves.len() == self.limits.max_haves {
                            return Err(ServeError::Limit("haves"));
                        }
                        request.haves.push(id(hex)?);
                    } else if let Some(hex) = argument.strip_prefix(b"shallow ") {
                        if request.shallow.len() == self.limits.max_haves {
                            return Err(ServeError::Limit("shallow"));
                        }
                        let id = id(hex)?;
                        if request.shallow_seen.insert(id) {
                            request.shallow.push(id);
                        }
                    } else if let Some(depth) = argument.strip_prefix(b"deepen ") {
                        let depth = number(depth)?;
                        request.deepen.depth = Some(if depth == 0 {
                            return Err(ServeError::Protocol("deepen 0"));
                        } else {
                            depth
                        });
                    } else if let Some(since) = argument.strip_prefix(b"deepen-since ") {
                        request.deepen.since = Some(number(since)?);
                    } else if let Some(name) = argument.strip_prefix(b"deepen-not ") {
                        if request.deepen_not.len() == self.limits.max_wants {
                            return Err(ServeError::Limit("deepen-not"));
                        }
                        request.deepen_not.push(name.to_vec());
                    } else {
                        return Err(ServeError::Unsupported("fetch argument"));
                    }
                }
            }
        }
        if request.wants.is_empty() {
            return Err(ServeError::Protocol("fetch without wants"));
        }
        if request.deepen.depth.is_some()
            && (request.deepen.since.is_some() || !request.deepen_not.is_empty())
        {
            return Err(ServeError::Protocol(
                "deepen and deepen-since (or deepen-not) cannot be used together",
            ));
        }
        if request.deepen.relative && request.deepen.depth.is_none() {
            return Err(ServeError::Protocol("deepen-relative without deepen"));
        }
        Ok(request)
    }

    fn fetch(
        &self,
        arguments: &[Vec<u8>],
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let mut request = self.parse_fetch(arguments)?;
        // The pack walk would reach the missing parents of the repository's own boundaries.
        if !self.repository.shallow_roots().is_empty() {
            return Err(ServeError::Unsupported("fetch from a shallow repository"));
        }
        let (advertised, _) = self.advertised(|_| true, cancel)?;
        self.check_wants(&request.wants, &advertised)?;
        let objects = self.objects()?;
        for name in std::mem::take(&mut request.deepen_not) {
            let id = self
                .resolve_short(&name)?
                .ok_or(ServeError::Protocol("deepen-not is not a ref"))?;
            request
                .deepen
                .not
                .push(self.peel_commit(&objects, id, cancel)?);
        }
        // Like Git, a have that is a parent of an earlier common commit is already known to be
        // common and isn't acknowledged again.
        let mut common = Vec::new();
        let mut acknowledged = Vec::new();
        let mut near = IdSet::default();
        for &have in &request.haves {
            packet::check_cancelled(cancel)?;
            if near.contains(&have) {
                common.push(have);
                self.extend_near(&objects, &mut near, have);
            } else if objects.contains(have)? {
                common.push(have);
                acknowledged.push(have);
                self.extend_near(&objects, &mut near, have);
            }
        }
        // A request without haves has nothing to negotiate, and Git answers it as if done.
        if !request.done && !request.haves.is_empty() {
            packet::packet(output, b"acknowledgments\n", cancel)?;
            if acknowledged.is_empty() {
                packet::packet(output, b"NAK\n", cancel)?;
            }
            for id in &acknowledged {
                packet::packet(output, format!("ACK {id}\n").as_bytes(), cancel)?;
            }
            // Until every want reaches the common history the client is left to offer older
            // history, as with Git, and a client that asked to wait sends `done` first.
            let ready = !common.is_empty() && {
                let mut budget = self.limits.history.max_commits;
                self.reaches_common(&objects, &request.wants, &near, &mut budget, cancel)?
            };
            if request.wait_for_done || !ready {
                packet::put(output, b"0000", cancel)?;
                return Ok(());
            }
            packet::packet(output, b"ready\n", cancel)?;
            packet::put(output, b"0001", cancel)?;
        }
        let tags = if request.include_tag {
            advertised
                .into_iter()
                .filter(|reference| {
                    !reference.hidden && reference.name.as_bytes().starts_with(b"refs/tags/")
                })
                .collect()
        } else {
            Vec::new()
        };
        let boundaries = if request.deepen.requested() {
            let mut wants = Vec::new();
            for &want in &request.wants {
                wants.push(self.peel_commit(&objects, want, cancel)?);
            }
            shallow::boundaries(
                &objects,
                &wants,
                &request.shallow,
                &request.deepen,
                self.limits.history,
                cancel,
            )?
        } else {
            shallow::Boundaries::default()
        };
        if request.deepen.requested() || !request.shallow.is_empty() {
            packet::packet(output, b"shallow-info\n", cancel)?;
            for id in &boundaries.shallow {
                packet::packet(output, format!("shallow {id}").as_bytes(), cancel)?;
            }
            for id in &boundaries.unshallow {
                packet::packet(output, format!("unshallow {id}").as_bytes(), cancel)?;
            }
            packet::put(output, b"0001", cancel)?;
        }
        packet::packet(output, b"packfile\n", cancel)?;
        let band = Band::Packets(SIDEBAND_DATA);
        self.send_pack(
            &objects,
            &request,
            &common,
            &boundaries,
            &tags,
            band,
            output,
            cancel,
        )?;
        packet::put(output, b"0000", cancel)?;
        Ok(())
    }

    /// Resolves a `deepen-not` name as Git does: as given, then under `refs/`, `refs/tags/`,
    /// `refs/heads/` and `refs/remotes/`. Hidden references don't resolve.
    fn resolve_short(&self, name: &[u8]) -> Result<Option<ObjectId>, ServeError> {
        let refs = self.repository.references()?;
        for prefix in [
            &b""[..],
            b"refs/",
            b"refs/tags/",
            b"refs/heads/",
            b"refs/remotes/",
        ] {
            let Ok(full) = RefName::new([prefix, name].concat()) else {
                continue;
            };
            if self.hide.matches(&full) {
                continue;
            }
            if let Some(id) = refs.resolve(&full)?.id {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Peels `id` to a commit for the shallow walks.
    fn peel_commit(
        &self,
        objects: &Objects,
        id: ObjectId,
        cancel: &AtomicBool,
    ) -> Result<ObjectId, ServeError> {
        let limits = PeelLimits {
            read: self.limits.read,
            ..PeelLimits::default()
        };
        let peeled = objects.peel(id, limits, cancel)?;
        if peeled.kind != ObjectKind::Commit {
            return Err(ServeError::Unsupported("shallow fetch of a non-commit"));
        }
        Ok(peeled.target)
    }

    /// Whether every want reaches a commit in `near`, so a pack can be sent.
    ///
    /// The walk skips commits older than the oldest in `near`. Every commit read is charged to
    /// `budget`, which a session shares across negotiation rounds, and cancellation is checked
    /// between reads.
    pub(super) fn reaches_common(
        &self,
        objects: &crate::Objects,
        wants: &[ObjectId],
        near: &IdSet,
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<bool, ServeError> {
        let date = |id: ObjectId| -> Option<(i64, Vec<ObjectId>)> {
            let commit = objects.read_commit_with(id, self.limits.read).ok()?;
            let seconds = commit
                .committer()
                .ok()
                .flatten()
                .and_then(|person| person.date().ok())
                .map_or(0, |date| date.seconds);
            Some((seconds, commit.parents().to_vec()))
        };
        let mut oldest = i64::MAX;
        for &id in near {
            packet::check_cancelled(cancel)?;
            *budget = budget
                .checked_sub(1)
                .ok_or(ServeError::Limit("negotiation commits"))?;
            if let Some((seconds, _)) = date(id) {
                oldest = oldest.min(seconds);
            }
        }
        for &want in wants {
            let mut seen = IdSet::default();
            let mut pending = vec![want];
            let mut found = false;
            while let Some(id) = pending.pop() {
                if near.contains(&id) {
                    found = true;
                    break;
                }
                if !seen.insert(id) {
                    continue;
                }
                packet::check_cancelled(cancel)?;
                *budget = budget
                    .checked_sub(1)
                    .ok_or(ServeError::Limit("negotiation commits"))?;
                let Some((seconds, parents)) = date(id) else {
                    continue;
                };
                if seconds >= oldest {
                    pending.extend(parents);
                }
            }
            if !found {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Adds a common object and, for a commit, its parents, which Git also treats as common.
    pub(super) fn extend_near(&self, objects: &Objects, near: &mut IdSet, id: ObjectId) {
        near.insert(id);
        if let Ok(commit) = objects.read_commit_with(id, self.limits.read) {
            near.extend(commit.parents());
        }
    }

    /// Refuses the first want that the want policy doesn't allow.
    fn check_wants(&self, wants: &[ObjectId], advertised: &[Advertised]) -> Result<(), ServeError> {
        // Every request here is protocol v2.
        self.check_wants_with(wants, advertised, self.wants.unwrap_or(WantPolicy::Any))
    }

    /// Refuses the first want that `policy` doesn't allow.
    fn check_wants_with(
        &self,
        wants: &[ObjectId],
        advertised: &[Advertised],
        policy: WantPolicy,
    ) -> Result<(), ServeError> {
        let named = |reference: &&Advertised| std::iter::once(reference.id).chain(reference.peeled);
        let mut allowed: IdSet = advertised
            .iter()
            .filter(|reference| !reference.hidden || policy >= WantPolicy::Tips)
            .flat_map(|reference| named(&reference))
            .collect();
        let mut missing: Vec<ObjectId> = wants
            .iter()
            .copied()
            .filter(|want| !allowed.contains(want))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let objects = self.objects()?;
        match policy {
            WantPolicy::Any => {
                for &want in &missing {
                    if !objects.contains(want)? {
                        return Err(ServeError::NotOurRef(want));
                    }
                }
                return Ok(());
            }
            WantPolicy::Reachable => {
                let roots: Vec<ObjectId> = advertised
                    .iter()
                    .filter(|reference| reference.kind == ObjectKind::Commit)
                    .map(|reference| reference.peeled.unwrap_or(reference.id))
                    .collect();
                allowed.extend(objects.walk_with(&roots, self.limits.history)?);
                missing.retain(|want| !allowed.contains(want));
                // Like Git, only commit wants are checked for reachability; any other object the
                // repository has is allowed.
                let mut commits = Vec::new();
                for want in missing {
                    match objects.header(want, self.limits.read)? {
                        Some(header) if header.kind() != ObjectKind::Commit => {}
                        _ => commits.push(want),
                    }
                }
                missing = commits;
            }
            WantPolicy::Advertised | WantPolicy::Tips => {}
        }
        match missing.first() {
            Some(&want) => Err(ServeError::NotOurRef(want)),
            None => Ok(()),
        }
    }

    /// Counts the objects to send, then streams them as the `packfile` section.
    #[allow(clippy::too_many_arguments)]
    fn send_pack(
        &self,
        objects: &Objects,
        request: &FetchRequest,
        common: &[ObjectId],
        boundaries: &shallow::Boundaries,
        tags: &[Advertised],
        band: Band,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let limits = self.limits.selection();
        let mut wants = request.wants.clone();
        wants.extend(&boundaries.tips);
        // The client's boundaries stay in place unless lifted, so history behind them isn't sent.
        let lifted: IdSet = boundaries.unshallow.iter().copied().collect();
        let mut send_cut = boundaries.cut.clone();
        send_cut.extend(request.shallow.iter().filter(|id| !lifted.contains(id)));
        // When deepening, Git counts the client's boundary commits as held, without their
        // parents; an ordinary fetch from a shallow client sends them again.
        let mut known = common.to_vec();
        if request.deepen.requested() {
            known.extend(&request.shallow);
        }
        let mut selection = crate::push::graph::select_wants(
            objects,
            &wants,
            &known,
            request.shallow_seen.clone(),
            send_cut,
            limits,
            cancel,
            |_| {},
        )?;
        // `include-tag` adds annotated tags whose peeled object is sent, with any tags they nest.
        let sent: IdSet = selection.objects.iter().copied().collect();
        let mut added = IdSet::default();
        for tag in tags {
            let Some(peeled) = tag.peeled else { continue };
            if sent.contains(&peeled) {
                for &id in &tag.tags {
                    if !sent.contains(&id) && added.insert(id) {
                        selection.objects.push(id);
                    }
                }
            }
        }
        let framing = if request.ofs_delta {
            crate::pack::DeltaFraming::Offset
        } else {
            crate::pack::DeltaFraming::Identity
        };
        let mut band = Sideband {
            output,
            band,
            buffer: Vec::new(),
            cancel,
        };
        // A thin pack may delta against objects the client is known to have.
        let bases: Vec<_> = if request.thin_pack {
            selection
                .delta_bases
                .iter()
                .map(|&(id, kind, _)| (id, kind))
                .collect()
        } else {
            Vec::new()
        };
        let result = crate::pack::write_transfer(
            objects,
            &selection.objects,
            &bases,
            (&mut band, &mut std::io::sink()),
            limits.pack,
            limits.compression,
            (limits.read, cancel),
            limits.read.max_delta_depth,
            framing,
            &mut || {
                if cancel.load(Ordering::Relaxed) {
                    Err(std::io::Error::other("serving cancelled").into())
                } else {
                    Ok(())
                }
            },
            &mut |_, _| {},
        );
        packet::check_cancelled(cancel)?;
        result.map_err(|error| match error {
            crate::pack::ReuseError::Read { id, source } => {
                crate::push::PushFailure::Read { id, source }
            }
            crate::pack::ReuseError::Write(error) => crate::push::PushFailure::Pack(error),
        })?;
        band.drain()?;
        Ok(())
    }

    fn objects(&self) -> Result<Objects, ServeError> {
        Ok(self.repository.objects_with(self.limits.packs)?)
    }

    /// The served repository's object format.
    pub fn object_format(&self) -> ObjectFormat {
        self.repository.object_format()
    }
}

/// Parses a decimal number argument.
fn number<T: std::str::FromStr>(digits: &[u8]) -> Result<T, ServeError> {
    std::str::from_utf8(digits)
        .ok()
        .and_then(|digits| digits.parse().ok())
        .ok_or(ServeError::Protocol("number"))
}

/// Reads an `uploadpack` boolean, `None` when unset.
fn config_boolean(repository: &Repository, name: &str) -> Result<Option<bool>, ServeError> {
    section_boolean(repository, "uploadpack", name)
}

/// Reads a boolean from `section`, `None` when unset.
fn section_boolean(
    repository: &Repository,
    section: &str,
    name: &str,
) -> Result<Option<bool>, ServeError> {
    match repository.config().value(section, None, name) {
        None => Ok(None),
        Some(None) => Ok(Some(true)),
        Some(Some(value)) => crate::config::boolean(Some(value))
            .map(Some)
            .ok_or(ServeError::Config("boolean setting")),
    }
}

/// Tells the client why its request failed, as Git does, when the reason is the client's to fix.
/// A failure to write the report is ignored: the original error is the one to return.
fn report(error: &ServeError, output: &mut impl Write, cancel: &AtomicBool) {
    let message = match error {
        ServeError::NotOurRef(id) => format!("upload-pack: not our ref {id}"),
        ServeError::Protocol(_) | ServeError::Unsupported(_) | ServeError::Limit(_) => {
            format!("upload-pack: {error}")
        }
        _ => return,
    };
    let _ = packet::packet(output, format!("ERR {message}\n").as_bytes(), cancel);
    let _ = output.flush();
}

/// Strips one trailing newline, which pkt-line senders may include.
fn trim(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// How pack bytes reach the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Band {
    /// Unframed, for a v0 client without `side-band`.
    Raw,
    /// Sideband 1 packets carrying at most this many pack bytes.
    Packets(usize),
}

/// Frames written bytes as sideband 1 packets, or passes them through for [`Band::Raw`].
struct Sideband<'a, W> {
    output: &'a mut W,
    band: Band,
    buffer: Vec<u8>,
    cancel: &'a AtomicBool,
}

impl<W: Write> Sideband<'_, W> {
    fn drain(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let mut frame = Vec::with_capacity(self.buffer.len() + 1);
        frame.push(1);
        frame.append(&mut self.buffer);
        packet::packet(self.output, &frame, self.cancel).map_err(|error| match error {
            packet::Error::Io(error) => error,
            error => std::io::Error::other(format!("{error:?}")),
        })
    }
}

impl<W: Write> Write for Sideband<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Band::Packets(capacity) = self.band else {
            packet::check_cancelled(self.cancel)
                .map_err(|_| std::io::Error::other("serving cancelled"))?;
            return self.output.write(bytes);
        };
        let room = capacity - self.buffer.len();
        let taken = bytes.len().min(room);
        self.buffer
            .extend_from_slice(bytes.get(..taken).unwrap_or_default());
        if self.buffer.len() == capacity {
            self.drain()?;
        }
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
