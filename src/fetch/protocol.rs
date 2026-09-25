use std::collections::HashSet;
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use super::{FetchError as Error, FetchLimits, KnownHistory, ReceivedFetch, check_cancelled};
use crate::ObjectId;
use crate::packet::{Wire, packet, put};
use crate::refs::RefName;

/// One byte-preserving reference advertisement. Peeling hints cannot be selected as wants.
#[derive(Debug, Clone)]
pub struct AdvertisedRef {
    /// Full validated reference name (or HEAD); a peeled hint shares its tag's name.
    pub name: RefName,
    /// Advertised SHA-1 tip or peeled target.
    pub id: ObjectId,
    /// True for the `^{}` hint following an annotated tag.
    pub peeled: bool,
}

/// A complete v0 advertisement, independent of local references and policy.
#[derive(Debug, Clone)]
pub struct Advertisement {
    /// Entries in server order, including optional HEAD and peeled hints.
    pub refs: Vec<AdvertisedRef>,
    /// Raw capability tokens. Unknown optional capabilities are retained but never requested.
    pub capabilities: Vec<Vec<u8>>,
}

/// Receives a complete, validated fetch without changing storage.
///
/// Streams must already speak upload-pack v0 and terminate at EOF after the final flush. This is
/// a single session, not stateless HTTP or a reusable connection. `select` runs once after a
/// complete advertisement and returns explicit advertised tip IDs. Empty selection writes only a
/// flush and expects EOF. No haves are sent; a nonempty selection requests the complete reachable
/// history. Progress channel bytes are passed to `progress`; returning `Break(())` cancels the
/// transfer.
///
/// Cancellation is checked between I/O calls, packets, objects, and graph steps. It cannot
/// interrupt a blocked caller-owned stream or a single inflation/hash. Callers needing deadlines
/// must provide timeout/interrupt-capable streams. Interrupted I/O is returned immediately. The
/// caller must close both streams on any failure; they cannot be reused. Progress is advisory,
/// never a success signal.
///
/// # Errors
///
/// Rejects unsupported protocol/object formats, missing sideband capability, invalid
/// advertisements, unadvertised wants, unexpected ACKs, truncation, trailing bytes, peer errors,
/// corruption, missing internal bases or reachable objects, and exceeded limits. No partial fetch
/// is returned. All received object identities are computed; selected tip connectivity and edge
/// kinds are checked within the pack. Gitlinks are external submodule roots and are not followed.
/// Payload checks use girt's supported parsers and tree validation, not a claim of complete `git
/// fsck` equivalence.
pub fn receive(
    reader: &mut impl Read,
    writer: &mut impl Write,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    cancel: &AtomicBool,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, Error> {
    receive_with_known(
        reader,
        writer,
        select,
        &KnownHistory::default(),
        limits,
        cancel,
        progress,
    )
}

/// Receives a pack negotiated against explicitly verified local history.
///
/// Uses the same stream, cancellation and capability contract as [`receive`]. Sends at most
/// `min(max_haves, 32)` verified commits in one batch followed immediately by `done`, requesting
/// `multi_ack` when advertised. Without it, sends at most one have. ACKs must name offered IDs;
/// duplicate continuation ACKs, unsupported states and excess replies fail. No extra rounds or
/// unbounded ancestry walks occur. Selected IDs already verified locally are omitted from wire
/// wants; an entirely known selection sends only a flush. Tags and blobs can therefore be no-ops
/// too. Pack delta bases must remain internal; connectivity is checked across received and known
/// objects. [`ReceivedFetch::install`] rechecks local dependencies in the destination.
///
/// # Errors
///
/// Returns [`receive`]'s errors, including invalid ACKs and combined-graph connectivity failures.
/// Knowledge exceeding this call's local count or byte limits is rejected before transmission.
pub fn receive_with_known(
    reader: &mut impl Read,
    writer: &mut impl Write,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    cancel: &AtomicBool,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, Error> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.receive",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
    );

    let operation = || {
        validate_known(known, limits, cancel)?;
        let mut wire = Wire {
            reader,
            remaining: limits.max_wire_bytes,
            cancel,
        };
        let advertisement = advertise(&mut wire, limits)?;
        check_cancelled(cancel)?;
        let negotiation = request(
            writer,
            &advertisement,
            select(&advertisement),
            known,
            limits,
            cancel,
        )?;
        if !negotiation.needs_pack {
            wire.end()?;
            return ReceivedFetch::without_pack(
                advertisement,
                negotiation.wants,
                known,
                limits,
                cancel,
            );
        }
        response(
            &mut wire,
            advertisement,
            negotiation,
            known,
            limits,
            cancel,
            progress,
        )
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);

    result
}

pub(super) struct Negotiation {
    pub(super) wants: Vec<ObjectId>,
    haves: HashSet<ObjectId>,
    multi_ack: bool,
    pub(super) needs_pack: bool,
}

pub(super) fn validate_known(
    known: &KnownHistory,
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    check_cancelled(cancel)?;
    if known.objects.len() > limits.max_known_objects {
        return Err(Error::Limit("known objects"));
    }
    let mut remaining = limits.max_known_bytes;
    for object in known.objects.values() {
        check_cancelled(cancel)?;
        remaining = remaining
            .checked_sub(object.data().len())
            .ok_or(Error::Limit("known bytes"))?;
    }
    Ok(())
}

pub(super) fn request(
    writer: &mut impl Write,
    advertisement: &Advertisement,
    selected: Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<Negotiation, Error> {
    check_cancelled(cancel)?;
    if selected.len() > limits.max_wants {
        return Err(Error::Limit("wants"));
    }
    let advertised: HashSet<_> = advertisement
        .refs
        .iter()
        .filter(|reference| !reference.peeled)
        .map(|reference| reference.id)
        .collect();
    let mut wants = Vec::new();
    let mut seen = HashSet::new();
    for id in selected {
        id.require_sha1()?;
        if !advertised.contains(&id) {
            return Err(Error::Unadvertised(id));
        }
        if seen.insert(id) {
            wants.push(id);
        }
    }
    let wire_wants: Vec<_> = wants
        .iter()
        .filter(|id| !known.objects.contains_key(id))
        .copied()
        .collect();
    if wire_wants.is_empty() {
        put(writer, b"0000", cancel)?;
        writer.flush()?;
        return Ok(Negotiation {
            wants,
            haves: HashSet::new(),
            multi_ack: false,
            needs_pack: false,
        });
    }
    if !advertisement.has(b"side-band-64k") {
        return Err(Error::Unsupported("side-band-64k is required"));
    }
    let multi_ack =
        !known.haves.is_empty() && limits.max_haves != 0 && advertisement.has(b"multi_ack");
    let have_count = limits.max_haves.min(if multi_ack { 32 } else { 1 });
    for (i, id) in wire_wants.iter().enumerate() {
        let caps = if i != 0 {
            ""
        } else if advertisement.has(b"ofs-delta") {
            " side-band-64k ofs-delta"
        } else {
            " side-band-64k"
        };
        let ack_cap = if i == 0 && multi_ack {
            " multi_ack"
        } else {
            ""
        };
        packet(
            writer,
            format!("want {id}{caps}{ack_cap}\n").as_bytes(),
            cancel,
        )?;
    }
    put(writer, b"0000", cancel)?;
    let haves: HashSet<_> = known.haves.iter().take(have_count).copied().collect();
    for id in known.haves.iter().take(have_count) {
        packet(writer, format!("have {id}\n").as_bytes(), cancel)?;
    }
    packet(writer, b"done\n", cancel)?;
    writer.flush()?;
    Ok(Negotiation {
        wants,
        haves,
        multi_ack,
        needs_pack: true,
    })
}

pub(super) fn response(
    wire: &mut Wire<'_, impl Read>,
    advertisement: Advertisement,
    negotiation: Negotiation,
    known: &KnownHistory,
    limits: FetchLimits,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, Error> {
    read_ack(wire, &negotiation.haves, negotiation.multi_ack)?;
    let mut pack = Vec::new();
    while let Some(bytes) = wire.packet()? {
        match bytes.split_first() {
            Some((1, data)) => {
                if data.len() > limits.max_pack_bytes.saturating_sub(pack.len()) {
                    return Err(Error::Limit("pack bytes"));
                }
                pack.extend_from_slice(data);
            }
            Some((2, data)) => {
                if progress(data).is_break() {
                    return Err(Error::Cancelled);
                }
            }
            Some((3, data)) => return Err(Error::Remote(data.to_vec())),
            _ => return Err(Error::Protocol("invalid sideband channel")),
        }
    }
    wire.end()?;
    ReceivedFetch::validate_known(
        advertisement,
        negotiation.wants,
        pack,
        known,
        limits,
        cancel,
    )
}

// One batch of at most 32 haves keeps both directions below ordinary pipe capacity. Reading
// the bounded ACK sequence after done avoids assuming a flush/NAK boundary in single-ACK mode.
fn read_ack(
    wire: &mut Wire<'_, impl Read>,
    haves: &HashSet<ObjectId>,
    multi_ack: bool,
) -> Result<(), Error> {
    let mut continued = HashSet::new();
    for _ in 0..=haves.len() {
        let packet = wire
            .packet()?
            .ok_or(Error::Protocol("flush instead of ACK/NAK"))?;
        let bytes = line(&packet);
        if bytes == b"NAK" && continued.is_empty() {
            return Ok(());
        }
        let (raw, more) = if multi_ack && bytes.ends_with(b" continue") {
            (&bytes[..bytes.len() - 9], true)
        } else {
            (bytes, false)
        };
        let id = raw
            .strip_prefix(b"ACK ")
            .and_then(|id| std::str::from_utf8(id).ok())
            .and_then(|id| id.parse::<ObjectId>().ok())
            .filter(|id| id.format() == crate::ObjectFormat::Sha1)
            .filter(|id| haves.contains(id))
            .ok_or(Error::Protocol("expected ACK of an offered have"))?;
        if !more {
            return Ok(());
        }
        if !continued.insert(id) {
            return Err(Error::Protocol("duplicate ACK continue"));
        }
    }
    Err(Error::Limit("negotiation acknowledgements"))
}

impl Advertisement {
    fn has(&self, capability: &[u8]) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

pub(super) fn advertise(
    wire: &mut Wire<'_, impl Read>,
    limits: FetchLimits,
) -> Result<Advertisement, Error> {
    let mut advertisement = Advertisement {
        refs: vec![],
        capabilities: vec![],
    };
    let start = wire.remaining;
    let mut first = true;
    let mut empty_marker = false;
    let mut names = HashSet::new();
    // Narrow the wire budget before reading, so even a huge first advertisement packet is bounded.
    let saved = wire
        .remaining
        .saturating_sub(limits.max_advertisement_bytes);
    wire.remaining -= saved;
    while let Some(bytes) = wire.packet()? {
        let bytes = line(&bytes);
        if bytes.starts_with(b"version ") {
            return Err(Error::Unsupported("protocol version"));
        }
        if bytes.starts_with(b"shallow ") {
            return Err(Error::Unsupported("shallow server"));
        }
        let reference = if first {
            let (reference, caps) =
                split_byte(bytes, 0).ok_or(Error::Protocol("missing capabilities"))?;
            if caps.contains(&0) || caps.iter().any(|b| b.is_ascii_control()) {
                return Err(Error::Protocol("invalid capabilities"));
            }
            advertisement.capabilities = caps
                .split(|b| *b == b' ')
                .filter(|c| !c.is_empty())
                .map(<[u8]>::to_vec)
                .collect();
            for cap in &advertisement.capabilities {
                if cap.starts_with(b"object-format=") && cap != b"object-format=sha1" {
                    return Err(Error::Unsupported("object format"));
                }
            }
            reference
        } else {
            bytes
        };
        let (raw_id, name) =
            split_byte(reference, b' ').ok_or(Error::Protocol("reference advertisement"))?;
        let id = std::str::from_utf8(raw_id)
            .ok()
            .and_then(|id| id.parse::<ObjectId>().ok())
            .filter(|id| id.format() == crate::ObjectFormat::Sha1)
            .ok_or(Error::Protocol("advertised object ID"))?;
        if id == ObjectId::Sha1([0; 20]) {
            if !first || name != b"capabilities^{}" {
                return Err(Error::Protocol("zero advertised ID"));
            }
            empty_marker = true;
        } else {
            if empty_marker {
                return Err(Error::Protocol("refs after empty advertisement"));
            }
            if advertisement.refs.len() == limits.max_refs {
                return Err(Error::Limit("advertised refs"));
            }
            let peeled = name.ends_with(b"^{}");
            let name = if peeled {
                &name[..name.len() - 3]
            } else {
                name
            };
            let name = RefName::new(name).map_err(|_| Error::Protocol("advertised ref name"))?;
            if peeled
                && (!name.as_bytes().starts_with(b"refs/tags/")
                    || !names.contains(&(name.clone(), false)))
            {
                return Err(Error::Protocol("unmatched peeled hint"));
            }
            if !names.insert((name.clone(), peeled)) {
                return Err(Error::Protocol("duplicate ref"));
            }
            advertisement.refs.push(AdvertisedRef { name, id, peeled });
        }
        first = false;
    }
    wire.remaining += saved;
    debug_assert!(wire.remaining <= start);
    Ok(advertisement)
}

// Parse arbitrary reference/capability bytes without UTF-8 conversion.
fn split_byte(bytes: &[u8], byte: u8) -> Option<(&[u8], &[u8])> {
    let position = bytes.iter().position(|b| *b == byte)?;
    Some((&bytes[..position], &bytes[position + 1..]))
}

fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn acknowledgements(lines: &[&str], multi: bool) -> Result<(), Error> {
        let cancel = AtomicBool::new(false);
        let mut bytes = Vec::new();
        for line in lines {
            packet(&mut bytes, line.as_bytes(), &cancel).unwrap();
        }
        let mut reader = bytes.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 1024,
            cancel: &cancel,
        };
        let id = "1111111111111111111111111111111111111111".parse().unwrap();
        read_ack(&mut wire, &HashSet::from([id]), multi)
    }

    #[rstest]
    #[case::nak(&["NAK"])]
    #[case::plain(&["ACK 1111111111111111111111111111111111111111"])]
    #[case::multi(&["ACK 1111111111111111111111111111111111111111 continue", "ACK 1111111111111111111111111111111111111111"])]
    fn accepts_supported_ack_sequences(#[case] lines: &[&str]) {
        assert!(acknowledgements(lines, true).is_ok());
    }

    #[rstest]
    #[case::unknown(&["ACK 2222222222222222222222222222222222222222"])]
    #[case::ready(&["ACK 1111111111111111111111111111111111111111 ready"])]
    #[case::common(&["ACK 1111111111111111111111111111111111111111 common"])]
    #[case::truncated(&["ACK 1111111111111111111111111111111111111111 continue"])]
    #[case::nak_after_continue(&["ACK 1111111111111111111111111111111111111111 continue", "NAK"])]
    #[case::duplicate(&["ACK 1111111111111111111111111111111111111111 continue", "ACK 1111111111111111111111111111111111111111 continue"])]
    #[case::empty(&[])]
    fn rejects_invalid_or_unbounded_ack_sequences(#[case] lines: &[&str]) {
        assert!(acknowledgements(lines, true).is_err());
    }

    #[test]
    fn cancellation_after_advertisement_skips_selection() {
        struct CancelOnRead<'a>(&'a AtomicBool);
        impl Read for CancelOnRead<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                out.copy_from_slice(b"0000");
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(4)
            }
        }
        let cancel = AtomicBool::new(false);
        let result = receive(
            &mut CancelOnRead(&cancel),
            &mut Vec::new(),
            |_| panic!("selection after cancellation"),
            FetchLimits::default(),
            &cancel,
            |_| ControlFlow::Continue(()),
        );
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn rejects_unrequested_multi_ack() {
        assert!(
            acknowledgements(
                &["ACK 1111111111111111111111111111111111111111 continue"],
                false
            )
            .is_err()
        );
    }
}
