use std::collections::HashSet;
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use super::{FetchError as Error, FetchLimits, ReceivedFetch, check_cancelled};
use crate::ObjectId;
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
    mut progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, Error> {
    let mut wire = Wire {
        reader,
        remaining: limits.max_wire_bytes,
        cancel,
    };
    let advertisement = advertise(&mut wire, limits)?;
    check_cancelled(cancel)?;
    let selected = select(&advertisement);
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
        if !advertised.contains(&id) {
            return Err(Error::Unadvertised(id));
        }
        if seen.insert(id) {
            wants.push(id);
        }
    }
    if wants.is_empty() {
        put(writer, b"0000", cancel)?;
        writer.flush()?;
        wire.end()?;
        return Ok(ReceivedFetch::empty(advertisement));
    }
    if !advertisement.has(b"side-band-64k") {
        return Err(Error::Unsupported("side-band-64k is required"));
    }
    for (i, id) in wants.iter().enumerate() {
        let caps = if i != 0 {
            ""
        } else if advertisement.has(b"ofs-delta") {
            " side-band-64k ofs-delta"
        } else {
            " side-band-64k"
        };
        packet(writer, format!("want {id}{caps}\n").as_bytes(), cancel)?;
    }
    put(writer, b"0000", cancel)?;
    packet(writer, b"done\n", cancel)?;
    writer.flush()?;
    let nak = wire
        .packet()?
        .ok_or(Error::Protocol("flush instead of NAK"))?;
    if line(&nak) != b"NAK" {
        return Err(Error::Protocol("expected NAK without haves"));
    }
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
    ReceivedFetch::validate(advertisement, wants, pack, limits, cancel)
}

impl Advertisement {
    fn has(&self, capability: &[u8]) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

fn advertise(wire: &mut Wire<'_, impl Read>, limits: FetchLimits) -> Result<Advertisement, Error> {
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
            .ok_or(Error::Protocol("advertised object ID"))?;
        if id == ObjectId::from_bytes([0; 20]) {
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

struct Wire<'a, R> {
    reader: &'a mut R,
    remaining: usize,
    cancel: &'a AtomicBool,
}
impl<R: Read> Wire<'_, R> {
    fn packet(&mut self) -> Result<Option<Vec<u8>>, Error> {
        self.charge(4)?;
        let mut header = [0; 4];
        self.exact(&mut header)?;
        if !header.iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::Protocol("pkt-line header"));
        }
        let length = usize::from_str_radix(std::str::from_utf8(&header).unwrap(), 16).unwrap();
        if length == 0 {
            return Ok(None);
        }
        if !(4..=65520).contains(&length) {
            return Err(Error::Protocol("pkt-line length"));
        }
        self.charge(length - 4)?;
        let mut bytes = vec![0; length - 4];
        self.exact(&mut bytes)?;
        if let Some(message) = bytes.strip_prefix(b"ERR ") {
            return Err(Error::Remote(message.to_vec()));
        }
        Ok(Some(bytes))
    }
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(Error::Limit("wire bytes"))?;
        Ok(())
    }
    fn exact(&mut self, mut bytes: &mut [u8]) -> Result<(), Error> {
        while !bytes.is_empty() {
            check_cancelled(self.cancel)?;
            let read = self.reader.read(bytes)?;
            if read == 0 {
                return Err(Error::Protocol("truncated pkt-line"));
            }
            bytes = &mut bytes[read..];
        }
        Ok(())
    }
    fn end(&mut self) -> Result<(), Error> {
        check_cancelled(self.cancel)?;
        if self.reader.read(&mut [0])? != 0 {
            return Err(Error::Protocol("trailing response bytes"));
        }
        Ok(())
    }
}

fn packet(writer: &mut impl Write, bytes: &[u8], cancel: &AtomicBool) -> Result<(), Error> {
    put(
        writer,
        format!("{:04x}", bytes.len() + 4).as_bytes(),
        cancel,
    )?;
    put(writer, bytes, cancel)
}
fn put(writer: &mut impl Write, mut bytes: &[u8], cancel: &AtomicBool) -> Result<(), Error> {
    while !bytes.is_empty() {
        check_cancelled(cancel)?;
        let written = writer.write(bytes)?;
        if written == 0 {
            return Err(Error::Io(std::io::ErrorKind::WriteZero.into()));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}
