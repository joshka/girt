//! Protocol v0 upload-pack: the reference advertisement and stateful negotiation.

use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

use super::{
    Band, FetchRequest, FilterPolicy, SIDEBAND_DATA, UploadPack, WantPolicy, number, parse_filter,
    shallow, trim,
};
use crate::packet::{self, Frame, Wire};
use crate::serve::ServeError;
use crate::{IdSet, ObjectFormat, ObjectId};

/// Pack bytes per `side-band` packet: the 1000-byte packet limit less header and band byte.
const SMALL_SIDEBAND_DATA: usize = 995;

/// How the client asked to hear about common objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acks {
    /// One `ACK` for the first common object.
    Plain,
    /// `multi_ack`: `ACK <oid> continue` for each.
    Continue,
    /// `multi_ack_detailed`: `ACK <oid> common`, then `ACK <oid> ready`.
    Detailed,
}

/// The capabilities and negotiation state of one v0 session.
struct Negotiation {
    acks: Acks,
    common: IdSet,
    /// Common commits and their parents, which Git also treats as common.
    near: IdSet,
    last_common: Option<ObjectId>,
    /// Whether the server has decided it can send a pack.
    ready: bool,
    /// The common object the last `ACK <oid> ready` named.
    announced: Option<ObjectId>,
    /// Whether [`Acks::Plain`] has sent its one `ACK`.
    acked: bool,
    /// Commit reads left for readiness walks across the whole session.
    budget: usize,
    /// The size of `near` at the last readiness walk; an unchanged set needs no new walk.
    checked: usize,
}

impl UploadPack<'_> {
    /// The policy for v0 wants: the caller's, or else the configured one.
    fn v0_wants(&self) -> WantPolicy {
        self.wants.unwrap_or(self.configured_wants)
    }

    /// Writes the v0 reference advertisement, with capabilities after the first reference.
    pub(super) fn advertise_v0(
        &self,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let (advertised, _) = self.advertised(|_| true, cancel)?;
        let mut capabilities = b"multi_ack thin-pack side-band side-band-64k ofs-delta shallow \
            deepen-since deepen-not deepen-relative no-progress include-tag multi_ack_detailed"
            .to_vec();
        let policy = self.v0_wants();
        // As in Git, `allowReachableSHA1InWant` alone doesn't advertise tips.
        if matches!(policy, WantPolicy::Tips | WantPolicy::Any) {
            capabilities.extend_from_slice(b" allow-tip-sha1-in-want");
        }
        if policy >= WantPolicy::Reachable {
            capabilities.extend_from_slice(b" allow-reachable-sha1-in-want");
        }
        let visible: Vec<_> = advertised.iter().filter(|r| !r.hidden).collect();
        if let Some(head) = visible
            .iter()
            .find(|r| r.name == crate::refs::RefName::HEAD)
            && let Some(target) = &head.symref
        {
            capabilities.extend_from_slice(b" symref=HEAD:");
            capabilities.extend_from_slice(target.as_bytes());
        }
        if self.filters == FilterPolicy::Allow {
            capabilities.extend_from_slice(b" filter");
        }
        capabilities.extend_from_slice(
            format!(
                " object-format={} agent=girt/{}",
                self.repository.object_format(),
                env!("CARGO_PKG_VERSION")
            )
            .as_bytes(),
        );
        let mut first = true;
        let mut line = |id: ObjectId, name: &[u8], output: &mut _| -> Result<(), ServeError> {
            let mut bytes = format!("{id} ").into_bytes();
            bytes.extend_from_slice(name);
            if std::mem::take(&mut first) {
                bytes.push(0);
                bytes.extend_from_slice(&capabilities);
            }
            bytes.push(b'\n');
            packet::packet(output, &bytes, cancel)?;
            Ok(())
        };
        if visible.is_empty() {
            let null = ObjectId::null(self.repository.object_format());
            line(null, b"capabilities^{}", output)?;
        }
        for reference in visible {
            line(reference.id, reference.name.as_bytes(), output)?;
            if let Some(peeled) = reference.peeled {
                line(
                    peeled,
                    &[reference.name.as_bytes(), b"^{}"].concat(),
                    output,
                )?;
            }
        }
        packet::put(output, b"0000", cancel)?;
        output.flush()?;
        Ok(())
    }

    /// Serves one v0 session after the advertisement: wants, shallow lines, negotiation and pack.
    pub(super) fn serve_v0<R: Read>(
        &self,
        wire: &mut Wire<'_, R>,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let mut request = FetchRequest::default();
        let mut negotiation = Negotiation {
            acks: Acks::Plain,
            common: IdSet::default(),
            near: IdSet::default(),
            last_common: None,
            ready: false,
            announced: None,
            acked: false,
            budget: self.limits.history.max_commits,
            checked: 0,
        };
        let mut band = Band::Raw;
        // Unlike v2, a v0 client that names no object format accepts the advertised one.
        let mut format = None;
        let mut deepen_not = Vec::new();
        // Like Git, a `filter` line needs the `filter` capability, which needs filtering allowed.
        let mut filtering = false;
        loop {
            let line = match wire.frame_or_end()? {
                None => return Ok(()),
                Some(Frame::Flush) => break,
                Some(Frame::Data(line)) => line,
                Some(Frame::Delim | Frame::ResponseEnd) => {
                    return Err(ServeError::Protocol("unexpected special packet"));
                }
            };
            let line = trim(&line);
            if let Some(rest) = line.strip_prefix(b"want ") {
                let (hex, capabilities) = match rest.iter().position(|&b| b == b' ') {
                    Some(space) => rest.split_at(space),
                    None => (rest, &b""[..]),
                };
                if request.wants.is_empty() {
                    for capability in capabilities.split(|&b| b == b' ') {
                        match capability {
                            b"multi_ack" if negotiation.acks == Acks::Plain => {
                                negotiation.acks = Acks::Continue;
                            }
                            b"multi_ack_detailed" => negotiation.acks = Acks::Detailed,
                            b"side-band" if band == Band::Raw => {
                                band = Band::Packets(SMALL_SIDEBAND_DATA);
                            }
                            b"side-band-64k" => band = Band::Packets(SIDEBAND_DATA),
                            b"thin-pack" => request.thin_pack = true,
                            b"ofs-delta" => request.ofs_delta = true,
                            b"include-tag" => request.include_tag = true,
                            b"deepen-relative" => request.deepen.relative = true,
                            b"filter" => filtering = self.filters == FilterPolicy::Allow,
                            b"object-format=sha1" => format = Some(ObjectFormat::Sha1),
                            b"object-format=sha256" => format = Some(ObjectFormat::Sha256),
                            capability if capability.starts_with(b"object-format=") => {
                                return Err(ServeError::Unsupported("object format"));
                            }
                            // Progress is never sent; other capabilities change nothing here.
                            _ => {}
                        }
                    }
                }
                if request.wants.len() == self.limits.max_wants {
                    return Err(ServeError::Limit("wants"));
                }
                request.wants.push(self.parse_id(hex)?);
            } else if let Some(hex) = line.strip_prefix(b"shallow ") {
                if request.shallow.len() == self.limits.max_haves {
                    return Err(ServeError::Limit("shallow"));
                }
                let id = self.parse_id(hex)?;
                if request.shallow_seen.insert(id) {
                    request.shallow.push(id);
                }
            } else if let Some(depth) = line.strip_prefix(b"deepen ") {
                let depth: u32 = number(depth)?;
                if depth == 0 {
                    return Err(ServeError::Protocol("deepen 0"));
                }
                request.deepen.depth = Some(depth);
            } else if let Some(since) = line.strip_prefix(b"deepen-since ") {
                request.deepen.since = Some(number(since)?);
            } else if let Some(name) = line.strip_prefix(b"deepen-not ") {
                if deepen_not.len() == self.limits.max_wants {
                    return Err(ServeError::Limit("deepen-not"));
                }
                deepen_not.push(name.to_vec());
            } else if let Some(spec) = line.strip_prefix(b"filter ") {
                if !filtering {
                    return Err(ServeError::Protocol("filtering capability not negotiated"));
                }
                request.filter = Some(parse_filter(spec)?);
            } else {
                return Err(ServeError::Unsupported("upload-pack request line"));
            }
        }
        if request.wants.is_empty() {
            // A client that only lists references sends a flush and leaves.
            return Ok(());
        }
        if format.is_some_and(|format| format != self.repository.object_format()) {
            return Err(ServeError::Unsupported("object format"));
        }
        // `deepen-relative` is a capability in v0; without `deepen` it has no effect.
        if request.deepen.depth.is_none() {
            request.deepen.relative = false;
        }
        if !self.repository.shallow_roots().is_empty() {
            return Err(ServeError::Unsupported("fetch from a shallow repository"));
        }
        let (advertised, _) = self.advertised(|_| true, cancel)?;
        self.check_wants_with(&request.wants, &advertised, self.v0_wants())?;
        let objects = self.objects()?;
        for name in deepen_not {
            let id = self
                .resolve_short(&name)?
                .ok_or(ServeError::Protocol("deepen-not is not a ref"))?;
            request
                .deepen
                .not
                .push(self.peel_commit(&objects, id, cancel)?);
        }
        let boundaries = if request.deepen.requested() {
            let mut wants = Vec::new();
            for &want in &request.wants {
                wants.push(self.peel_commit(&objects, want, cancel)?);
            }
            let boundaries = shallow::boundaries(
                &objects,
                &wants,
                &request.shallow,
                &request.deepen,
                self.limits.history,
                cancel,
            )?;
            for id in &boundaries.shallow {
                packet::packet(output, format!("shallow {id}").as_bytes(), cancel)?;
            }
            for id in &boundaries.unshallow {
                packet::packet(output, format!("unshallow {id}").as_bytes(), cancel)?;
            }
            packet::put(output, b"0000", cancel)?;
            output.flush()?;
            boundaries
        } else {
            shallow::Boundaries::default()
        };
        // Like Git, a fetch with shallow boundaries never stops negotiation early.
        let deepen = !boundaries.cut.is_empty() || !request.shallow.is_empty();
        let mut haves = 0;
        loop {
            match wire.frame_or_end()? {
                None => return Ok(()),
                Some(Frame::Data(line)) => {
                    let line = trim(&line);
                    if line == b"done" {
                        break;
                    }
                    let hex = line
                        .strip_prefix(b"have ")
                        .ok_or(ServeError::Protocol("expected have or done"))?;
                    haves += 1;
                    if haves > self.limits.max_haves {
                        return Err(ServeError::Limit("haves"));
                    }
                    let id = self.parse_id(hex)?;
                    let fresh = !negotiation.common.contains(&id) && objects.contains(id)?;
                    if fresh {
                        negotiation.common.insert(id);
                        negotiation.last_common = Some(id);
                        self.extend_near(&objects, &mut negotiation.near, id);
                    }
                    let ack = match (negotiation.acks, negotiation.ready, fresh) {
                        (Acks::Detailed, _, true) => Some("common"),
                        (Acks::Detailed, true, false) => Some("ready"),
                        (Acks::Continue, true, _) | (Acks::Continue, false, true) => {
                            Some("continue")
                        }
                        (Acks::Plain, _, true) if !negotiation.acked => {
                            negotiation.acked = true;
                            Some("")
                        }
                        _ => None,
                    };
                    if let Some(kind) = ack {
                        let separator = if kind.is_empty() { "" } else { " " };
                        let line = format!("ACK {id}{separator}{kind}\n");
                        packet::packet(output, line.as_bytes(), cancel)?;
                    }
                }
                Some(Frame::Flush) => {
                    match negotiation.acks {
                        Acks::Plain => {
                            if negotiation.common.is_empty() {
                                packet::packet(output, b"NAK\n", cancel)?;
                            }
                        }
                        Acks::Continue | Acks::Detailed => {
                            if !negotiation.ready
                                && !deepen
                                && !negotiation.common.is_empty()
                                && negotiation.checked != negotiation.near.len()
                            {
                                negotiation.checked = negotiation.near.len();
                                negotiation.ready = self.reaches_common(
                                    &objects,
                                    &request.wants,
                                    &negotiation.near,
                                    &mut negotiation.budget,
                                    cancel,
                                )?;
                            }
                            if negotiation.acks == Acks::Detailed
                                && negotiation.ready
                                && negotiation.announced != negotiation.last_common
                                && let Some(last) = negotiation.last_common
                            {
                                negotiation.announced = Some(last);
                                let line = format!("ACK {last} ready\n");
                                packet::packet(output, line.as_bytes(), cancel)?;
                            }
                            packet::packet(output, b"NAK\n", cancel)?;
                        }
                    }
                    output.flush()?;
                }
                Some(Frame::Delim | Frame::ResponseEnd) => {
                    return Err(ServeError::Protocol("unexpected special packet"));
                }
            }
        }
        match negotiation.last_common {
            Some(last) => packet::packet(output, format!("ACK {last}\n").as_bytes(), cancel)?,
            None => packet::packet(output, b"NAK\n", cancel)?,
        }
        let tags = if request.include_tag {
            advertised
                .into_iter()
                .filter(|r| !r.hidden && r.name.as_bytes().starts_with(b"refs/tags/"))
                .collect()
        } else {
            Vec::new()
        };
        let common: Vec<ObjectId> = negotiation.common.iter().copied().collect();
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
        if band != Band::Raw {
            packet::put(output, b"0000", cancel)?;
        }
        output.flush()?;
        Ok(())
    }

    fn parse_id(&self, hex: &[u8]) -> Result<ObjectId, ServeError> {
        std::str::from_utf8(hex)
            .ok()
            .and_then(|hex| ObjectId::from_hex(self.repository.object_format(), hex).ok())
            .ok_or(ServeError::Protocol("object ID"))
    }
}
