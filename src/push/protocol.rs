use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushFailure as Error, PushReport, Status};
use crate::ObjectId;
use crate::packet::{Wire, check_cancelled, put};
use crate::refs::RefName;

/// Publishes prepared commands through caller-owned receive-pack v0 blocking streams.
///
/// Reads a complete advertisement, requires `report-status` for nonempty pushes, checks every
/// expected old value, then writes commands and a raw non-thin pack. Only `report-status` is
/// requested, even when atomic or sideband capabilities are advertised. Unknown optional
/// capabilities are ignored; non-SHA-1 object formats, protocol versions, shallow advertisements,
/// peeled hints and report-status-v2-only servers are rejected. Roots used by
/// [`PreparedPush::new_excluding`] must appear as a current advertised tip or `.have`; otherwise
/// the push fails before transmission. Hidden refs are unadvertised and cannot be updated by
/// an expected existing value through this API.
///
/// Streams must end at EOF after the report flush, and cannot be reused. Callers must close both
/// streams after failure. Cancellation is checked between I/O calls and packets; setting the flag
/// cannot interrupt a blocked stream. Supply deadline-capable streams when needed. Interrupted
/// I/O is propagated without retry. No local references are updated.
///
/// # Errors
///
/// [`PushError::NotSent`] means no commands were attempted. Once transmission begins, I/O,
/// malformed/truncated reports, exceeded limits, cancellation and peer ERR packets produce
/// [`PushError::Uncertain`] with the valid acknowledgement prefix. Inspect remote refs before
/// retrying unknown outcomes. A complete rejection or partial success is an `Ok` report, not an
/// error: inspect [`PushReport::all_succeeded`] and per-ref results. A racing remote update is
/// checked by the server using the command's old ID; advertisement checks alone do not lock refs.
pub fn send(
    reader: &mut impl Read,
    writer: &mut impl Write,
    prepared: &PreparedPush,
    cancel: &AtomicBool,
) -> Result<PushReport, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.send",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = || {
        if prepared.format != crate::ObjectFormat::Sha1 {
            return Err(PushError::NotSent(Error::Unsupported("SHA-256 wire push")));
        }
        let mut wire = Wire {
            reader,
            remaining: prepared.limits.max_advertisement_bytes,
            cancel,
        };
        let preflight = advertise(&mut wire, prepared)
            .and_then(|()| check_cancelled(cancel).map_err(Error::from));
        preflight.map_err(PushError::NotSent)?;
        let mut report = PushReport::pending(&prepared.commands);
        wire.remaining = prepared.limits.max_status_bytes;
        let result = transmit(&mut wire, writer, prepared, &mut report, cancel);
        match result {
            Ok(()) => Ok(report),
            Err(cause) if prepared.commands.is_empty() => Err(PushError::NotSent(cause)),
            Err(cause) => Err(PushError::Uncertain {
                cause,
                report: Box::new(report),
            }),
        }
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, |error| crate::trace::push(error, &span));
    #[cfg(feature = "tracing")]
    if let Ok(report) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}

fn transmit(
    wire: &mut Wire<'_, impl Read>,
    writer: &mut impl Write,
    prepared: &PreparedPush,
    report: &mut PushReport,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    put(writer, &prepared.request, cancel)?;
    // Chunk large writes so cancellation can be observed without relying on a short-writing sink.
    for chunk in prepared.pack.chunks(65536) {
        put(writer, chunk, cancel)?;
    }
    check_cancelled(cancel)?;
    writer.flush()?;
    if !prepared.commands.is_empty() {
        read_status(wire, report)?;
    }
    wire.end()?;
    Ok(())
}

pub(super) fn advertise(
    wire: &mut Wire<'_, impl Read>,
    prepared: &PreparedPush,
) -> Result<(), Error> {
    if prepared.format != crate::ObjectFormat::Sha1 {
        return Err(Error::Unsupported("SHA-256 wire push"));
    }
    let mut refs = HashMap::new();
    let mut roots = HashSet::new();
    let mut count = 0usize;
    let mut report_status = false;
    let mut empty = false;
    while let Some(packet) = wire.packet()? {
        let bytes = line(&packet);
        if bytes.starts_with(b"version ") || bytes.starts_with(b"shallow ") {
            return Err(Error::Unsupported("protocol version or shallow server"));
        }
        let reference = if count == 0 {
            let (reference, caps) =
                split(bytes, 0).ok_or(Error::Protocol("missing capabilities"))?;
            if caps.iter().any(|b| b.is_ascii_control()) {
                return Err(Error::Protocol("invalid capabilities"));
            }
            for cap in caps.split(|b| *b == b' ') {
                if cap.starts_with(b"object-format=") && cap != b"object-format=sha1" {
                    return Err(Error::Unsupported("object format"));
                }
                report_status |= cap == b"report-status";
            }
            reference
        } else {
            bytes
        };
        let (id, name) =
            split(reference, b' ').ok_or(Error::Protocol("reference advertisement"))?;
        let id = parse_id(id)?;
        if id == ObjectId::Sha1([0; 20]) {
            if count != 0 || name != b"capabilities^{}" {
                return Err(Error::Protocol("zero advertised ID"));
            }
            empty = true;
        } else {
            if empty {
                return Err(Error::Protocol("refs after empty marker"));
            }
            if count >= prepared.limits.max_refs {
                return Err(Error::Limit("advertised refs"));
            }
            roots.insert(id);
            if name != b".have" {
                let name =
                    RefName::new(name).map_err(|_| Error::Protocol("advertised ref name"))?;
                if refs.insert(name, id).is_some() {
                    return Err(Error::Protocol("duplicate advertised ref"));
                }
            }
        }
        count = count
            .checked_add(1)
            .ok_or(Error::Limit("advertised refs"))?;
    }
    if count == 0 {
        return Err(Error::Protocol("missing advertisement"));
    }
    if !prepared.commands.is_empty() && !report_status {
        return Err(Error::Unsupported("report-status is required"));
    }
    for &id in &prepared.receiver_roots {
        check_cancelled(wire.cancel)?;
        if !roots.contains(&id) {
            return Err(Error::KnowledgeChanged(id));
        }
    }
    for command in &prepared.commands {
        check_cancelled(wire.cancel)?;
        let actual = refs.get(&command.name).copied();
        if actual != command.expected {
            return Err(Error::Stale {
                name: command.name.clone(),
                expected: command.expected,
                actual,
            });
        }
    }
    Ok(())
}

pub(super) fn read_status(
    wire: &mut Wire<'_, impl Read>,
    report: &mut PushReport,
) -> Result<(), Error> {
    let unpack = wire
        .packet()?
        .ok_or(Error::Protocol("missing unpack status"))?;
    let unpack = line(&unpack)
        .strip_prefix(b"unpack ")
        .ok_or(Error::Protocol("expected unpack status"))?;
    report.unpack = Some(status(unpack)?);
    let positions: HashMap<_, _> = report
        .refs
        .iter()
        .enumerate()
        .map(|(i, r)| (r.command.name.clone(), i))
        .collect();
    let mut seen = HashSet::new();
    while let Some(packet) = wire.packet()? {
        let bytes = line(&packet);
        let (name, result) = if let Some(name) = bytes.strip_prefix(b"ok ") {
            if report.unpack != Some(Status::Ok) {
                return Err(Error::Protocol("ref success after unpack failure"));
            }
            (name, Status::Ok)
        } else if let Some(failure) = bytes.strip_prefix(b"ng ") {
            let (name, reason) =
                split(failure, b' ').ok_or(Error::Protocol("missing ref rejection reason"))?;
            if reason.is_empty() {
                return Err(Error::Protocol("empty ref rejection reason"));
            }
            (name, Status::Rejected(reason.to_vec()))
        } else {
            return Err(Error::Protocol("unexpected ref status"));
        };
        let name = RefName::new(name).map_err(|_| Error::Protocol("status ref name"))?;
        let position = *positions
            .get(&name)
            .ok_or(Error::Protocol("status for unrequested ref"))?;
        if !seen.insert(position) {
            return Err(Error::Protocol("duplicate ref status"));
        }
        report.refs[position].status = Some(result);
    }
    if seen.len() != report.refs.len() {
        return Err(Error::Protocol("missing ref status"));
    }
    Ok(())
}
fn status(bytes: &[u8]) -> Result<Status, Error> {
    if bytes.is_empty() {
        return Err(Error::Protocol("empty unpack status"));
    }
    Ok(if bytes == b"ok" {
        Status::Ok
    } else {
        Status::Rejected(bytes.to_vec())
    })
}
fn parse_id(bytes: &[u8]) -> Result<ObjectId, Error> {
    if bytes.len() != 40 {
        return Err(Error::Unsupported("object format"));
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|id| id.parse().ok())
        .ok_or(Error::Protocol("advertised object ID"))
}
fn split(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let i = bytes.iter().position(|b| *b == separator)?;
    Some((&bytes[..i], &bytes[i + 1..]))
}
fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
