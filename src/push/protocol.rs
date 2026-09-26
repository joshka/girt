use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushFailure as Error, PushReport, RefRewrite, Status};
use crate::ObjectId;
use crate::packet::{Wire, check_cancelled, put};
use crate::refs::RefName;

/// Publishes prepared commands through caller-owned receive-pack v0 blocking streams.
///
/// Reads a complete advertisement, requires `report-status` for nonempty pushes, checks required
/// capabilities, then writes commands and a raw non-thin pack. Every command carries its exact
/// expected old value for the server to compare under its reference transaction. SHA-256 requests
/// `object-format=sha256`; deletions require `delete-refs`, and supplied options require
/// `push-options`. Report-status-v2 is requested when advertised, preserving proc-receive rewrite
/// options. [`PreparedPush::with_progress`] requests sideband when available; channel-2 frames
/// remain untrusted bytes in the report. Protocol versions, shallow advertisements and peeled
/// hints are rejected. Roots used
/// by [`PreparedPush::new_excluding`] must appear as a current advertised tip or `.have`; otherwise
/// the push fails before transmission. Hidden refs remain subject to the server's exact old-value
/// and visibility policies.
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
/// error: inspect [`PushReport::all_succeeded`] and per-ref results. Stale advertised values and
/// racing remote updates are checked by the server using each command's old ID.
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
        let mut wire = Wire {
            reader,
            remaining: prepared.limits.max_advertisement_bytes,
            cancel,
        };
        let caps = advertise(&mut wire, prepared).map_err(PushError::NotSent)?;
        check_cancelled(cancel).map_err(|error| PushError::NotSent(error.into()))?;
        let request = prepared
            .request_for(caps.report_v2, caps.sideband)
            .map_err(PushError::NotSent)?;
        let mut report = PushReport::pending(&prepared.commands);
        wire.remaining = prepared.limits.max_status_bytes;
        let mut counted = CountingWriter {
            inner: writer,
            written: 0,
        };
        let result = transmit(
            &mut wire,
            &mut counted,
            &request,
            prepared,
            &mut report,
            cancel,
            caps,
        );
        report.mark_attempted(&request, counted.written);
        match result {
            Ok(()) => Ok(report),
            Err(cause) if prepared.commands.is_empty() || counted.written == 0 => {
                Err(PushError::NotSent(cause))
            }
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

struct CountingWriter<'a, W> {
    inner: &'a mut W,
    written: usize,
}

impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(count);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn transmit(
    wire: &mut Wire<'_, impl Read>,
    writer: &mut impl Write,
    request: &[u8],
    prepared: &PreparedPush,
    report: &mut PushReport,
    cancel: &AtomicBool,
    caps: Capabilities,
) -> Result<(), Error> {
    put(writer, request, cancel)?;
    // Chunk large writes so cancellation can be observed without relying on a short-writing sink.
    for chunk in prepared.pack.chunks(65536) {
        put(writer, chunk, cancel)?;
    }
    check_cancelled(cancel)?;
    writer.flush()?;
    if !prepared.commands.is_empty() {
        read_response(wire, report, caps, prepared.limits.max_status_bytes)?;
    }
    wire.end()?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) struct Capabilities {
    pub report_v2: bool,
    pub sideband: bool,
}

pub(super) fn advertise(
    wire: &mut Wire<'_, impl Read>,
    prepared: &PreparedPush,
) -> Result<Capabilities, Error> {
    let mut refs = HashMap::new();
    let mut roots = HashSet::new();
    let mut count = 0usize;
    let mut report_status = false;
    let mut report_v2 = false;
    let mut sideband = false;
    let mut delete_refs = false;
    let mut push_options = false;
    let mut advertised_format = crate::ObjectFormat::Sha1;
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
                if cap == b"object-format=sha256" {
                    advertised_format = crate::ObjectFormat::Sha256;
                } else if cap.starts_with(b"object-format=") && cap != b"object-format=sha1" {
                    return Err(Error::Unsupported("object format"));
                }
                report_status |= cap == b"report-status";
                report_v2 |= cap == b"report-status-v2";
                sideband |= cap == b"side-band-64k";
                delete_refs |= cap == b"delete-refs";
                push_options |= cap == b"push-options";
            }
            reference
        } else {
            bytes
        };
        let (id, name) =
            split(reference, b' ').ok_or(Error::Protocol("reference advertisement"))?;
        let id = parse_id(id, prepared.format)?;
        if id.is_null() {
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
    if !prepared.commands.is_empty() && !report_status && !report_v2 {
        return Err(Error::Unsupported("report-status is required"));
    }
    if advertised_format != prepared.format {
        return Err(Error::Unsupported("object format"));
    }
    if prepared.commands.iter().any(super::PushCommand::deletes) && !delete_refs {
        return Err(Error::Unsupported("delete-refs is required"));
    }
    if prepared.has_options && !push_options {
        return Err(Error::Unsupported("push-options is required"));
    }
    for &id in &prepared.receiver_roots {
        check_cancelled(wire.cancel)?;
        if !roots.contains(&id) {
            return Err(Error::KnowledgeChanged(id));
        }
    }
    // Every command carries its exact old ID. The receiver checks that value under its ref
    // transaction, allowing a stale command to fail without suppressing independent commands.
    Ok(Capabilities {
        report_v2,
        sideband: sideband && prepared.progress,
    })
}

pub(super) fn read_response(
    wire: &mut Wire<'_, impl Read>,
    report: &mut PushReport,
    caps: Capabilities,
    limit: usize,
) -> Result<(), Error> {
    if !caps.sideband {
        return read_status(wire, report, caps.report_v2);
    }
    let mut status_bytes = Vec::new();
    let outer = loop {
        match wire.packet() {
            Ok(Some(packet)) if packet.first() == Some(&1) => {
                let data = &packet[1..];
                if data.len() > limit.saturating_sub(status_bytes.len()) {
                    break Err(Error::Limit("status bytes"));
                }
                status_bytes.extend_from_slice(data);
            }
            Ok(Some(packet)) if packet.first() == Some(&2) => {
                report.progress.push(packet[1..].to_vec())
            }
            Ok(Some(packet)) if packet.first() == Some(&3) => {
                break Err(Error::Remote(packet[1..].to_vec()));
            }
            Ok(Some(_)) => break Err(Error::Protocol("invalid sideband channel")),
            Ok(None) => break Ok(()),
            Err(error) => break Err(Error::from(error)),
        }
    };
    let mut reader = status_bytes.as_slice();
    let mut nested = Wire {
        reader: &mut reader,
        remaining: limit,
        cancel: wire.cancel,
    };
    let parsed = read_status(&mut nested, report, caps.report_v2)
        .and_then(|()| nested.end().map_err(Error::from));
    outer.and(parsed)
}

pub(super) fn read_status(
    wire: &mut Wire<'_, impl Read>,
    report: &mut PushReport,
    report_v2: bool,
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
    let mut active: Option<(usize, RefRewrite, bool)> = None;
    while let Some(packet) = wire.packet()? {
        let bytes = line(&packet);
        if let Some(option) = bytes.strip_prefix(b"option ") {
            if !report_v2 {
                return Err(Error::Protocol("unexpected report option"));
            }
            let (position, rewrite, changed) = active
                .as_mut()
                .ok_or(Error::Protocol("option without successful ref"))?;
            if let Err(error) =
                parse_rewrite_option(option, rewrite, report.refs[0].command.new.format())
            {
                report.refs[*position].status = None;
                return Err(error);
            }
            *changed = true;
            continue;
        }
        finish_success(&mut active, report, &mut seen);
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
        if seen.contains(&position) && (!report_v2 || result != Status::Ok) {
            return Err(Error::Protocol("duplicate ref status"));
        }
        if result == Status::Ok && report_v2 {
            report.refs[position].status = Some(Status::Ok);
            active = Some((position, RefRewrite::default(), false));
        } else {
            seen.insert(position);
            report.refs[position].status = Some(result);
            report.refs[position].rewrite_complete = true;
        }
    }
    finish_success(&mut active, report, &mut seen);
    if seen.len() != report.refs.len() {
        return Err(Error::Protocol("missing ref status"));
    }
    Ok(())
}

fn finish_success(
    active: &mut Option<(usize, RefRewrite, bool)>,
    report: &mut PushReport,
    seen: &mut HashSet<usize>,
) {
    if let Some((position, rewrite, changed)) = active.take() {
        seen.insert(position);
        report.refs[position].status = Some(Status::Ok);
        report.refs[position].rewrite_complete = true;
        if changed {
            report.refs[position].rewrites.push(rewrite);
        }
    }
}

fn parse_rewrite_option(
    option: &[u8],
    rewrite: &mut RefRewrite,
    format: crate::ObjectFormat,
) -> Result<(), Error> {
    if let Some(name) = option.strip_prefix(b"refname ") {
        if rewrite.name.is_some() {
            return Err(Error::Protocol("duplicate rewrite refname"));
        }
        rewrite.name = Some(RefName::new(name).map_err(|_| Error::Protocol("rewrite refname"))?);
    } else if let Some(old) = option.strip_prefix(b"old-oid ") {
        if rewrite.old.is_some() {
            return Err(Error::Protocol("duplicate rewrite old ID"));
        }
        rewrite.old = Some(parse_id(old, format)?);
    } else if let Some(new) = option.strip_prefix(b"new-oid ") {
        if rewrite.new.is_some() {
            return Err(Error::Protocol("duplicate rewrite new ID"));
        }
        rewrite.new = Some(parse_id(new, format)?);
    } else if option == b"forced-update" {
        if rewrite.forced {
            return Err(Error::Protocol("duplicate rewrite forced update"));
        }
        rewrite.forced = true;
    } else {
        return Err(Error::Protocol("unknown rewrite option"));
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
fn parse_id(bytes: &[u8], format: crate::ObjectFormat) -> Result<ObjectId, Error> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|id| ObjectId::from_hex(format, id).ok())
        .ok_or(Error::Protocol("advertised object ID"))
}
fn split(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let i = bytes.iter().position(|b| *b == separator)?;
    Some((&bytes[..i], &bytes[i + 1..]))
}
fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
