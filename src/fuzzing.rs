//! Bounded parser entry points for the `fuzz/` cargo-fuzz targets.
//!
//! Compiled only under `cfg(fuzzing)`, which cargo-fuzz sets, and for unit tests; it is not part
//! of the public API. Each function accepts arbitrary bytes, applies explicit resource limits,
//! and discards parse errors. A panic, unbounded allocation or hang is a finding. Assertions
//! check round-trip properties only where girt already encodes the parsed form.
//!
//! Where a target needs several inputs, its leading bytes select an object format or split the
//! remaining input; [`Input`] documents that framing.

use std::sync::atomic::AtomicBool;

use crate::{Commit, ObjectFormat, ObjectKind, ReadLimits, Tag, Tree};

/// Largest payload, object or decoded output any target accepts.
const MAX_BYTES: usize = 1 << 20;
/// Most entries or objects any target visits after a successful parse.
const MAX_VISITS: usize = 256;

static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

fn read_limits() -> ReadLimits {
    ReadLimits {
        max_object_bytes: MAX_BYTES,
        max_delta_bytes: MAX_BYTES,
        max_decode_bytes: 4 * MAX_BYTES,
        max_input_bytes: 4 * MAX_BYTES,
        max_delta_depth: 16,
    }
}

/// A cursor over fuzz input that hands out selectors and length-prefixed parts.
struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn byte(&mut self) -> u8 {
        let Some((&first, rest)) = self.0.split_first() else {
            return 0;
        };
        self.0 = rest;
        first
    }

    /// The low bit selects SHA-1 or SHA-256.
    fn format(&mut self) -> ObjectFormat {
        if self.byte() & 1 == 0 {
            ObjectFormat::Sha1
        } else {
            ObjectFormat::Sha256
        }
    }

    /// A part whose length is the next big-endian `u32`, truncated to the bytes remaining.
    fn part(&mut self) -> &'a [u8] {
        let length = u32::from_be_bytes([self.byte(), self.byte(), self.byte(), self.byte()]);
        let (part, rest) = self.0.split_at(self.0.len().min(length as usize));
        self.0 = rest;
        part
    }

    fn rest(self) -> &'a [u8] {
        self.0
    }
}

/// Parses `[format][kind][payload]` as a commit, tree or tag and checks re-encoding.
pub fn object(data: &[u8]) {
    let mut input = Input(data);
    let format = input.format();
    let kind = input.byte();
    object_payload(format, kind % 3, input.rest());
}

fn object_payload(format: ObjectFormat, kind: u8, payload: &[u8]) {
    match kind {
        0 => {
            let Ok(commit) = Commit::parse(format, payload) else {
                return;
            };
            assert_eq!(commit.as_bytes(), payload);
            if let Ok(fields) = commit.to_fields() {
                // Imported objects may hold values construction rejects, such as NUL in names.
                let Ok(rebuilt) = Commit::new(fields.clone()) else {
                    return;
                };
                let reparsed = Commit::parse(format, rebuilt.as_bytes()).expect("encoding parses");
                assert_eq!(reparsed.to_fields().as_ref(), Ok(&fields));
            }
        }
        1 => {
            let Ok(tree) = Tree::parse(format, payload) else {
                return;
            };
            assert_eq!(tree.encode(), payload);
            // Construction emits canonical mode spellings, so only entries must survive.
            if tree.validate().is_ok() {
                let rebuilt = Tree::new(format, tree.entries().to_vec()).expect("valid tree");
                let reparsed = Tree::parse(format, &rebuilt.encode()).expect("encoding parses");
                assert_eq!(reparsed.entries(), tree.entries());
            }
        }
        _ => {
            let Ok(tag) = Tag::parse(format, payload) else {
                return;
            };
            assert_eq!(tag.as_bytes(), payload);
            if let Ok(fields) = tag.to_fields() {
                // Imported objects may hold values construction rejects, such as NUL in names.
                let Ok(rebuilt) = Tag::new(fields.clone()) else {
                    return;
                };
                let reparsed = Tag::parse(format, rebuilt.as_bytes()).expect("encoding parses");
                assert_eq!(reparsed.to_fields().as_ref(), Ok(&fields));
            }
        }
    }
}

/// Inflates `[format][zlib stream]` as a loose object, then parses its payload by kind.
pub fn loose(data: &[u8]) {
    let mut input = Input(data);
    let format = input.format();
    let Ok(encoded) = crate::loose::decompress(input.rest(), MAX_BYTES + 32) else {
        return;
    };
    let Ok((kind, payload)) = crate::loose::split_header(&encoded, None) else {
        return;
    };
    match kind {
        ObjectKind::Commit => object_payload(format, 0, payload),
        ObjectKind::Tree => object_payload(format, 1, payload),
        ObjectKind::Tag => object_payload(format, 2, payload),
        ObjectKind::Blob => {}
    }
}

/// Applies `[base part][delta program]` with explicit output and work budgets.
pub fn delta(data: &[u8]) {
    let mut input = Input(data);
    let base = input.part();
    let program = input.rest();
    let mut remaining = 4 * MAX_BYTES;
    if let Ok(result) = crate::pack::delta::apply(base, program, MAX_BYTES, &mut remaining) {
        assert!(result.len() <= MAX_BYTES);
    }
}

/// Parses `[format][pack end u32][idx bytes]` as a v1 or v2 pack index and walks its tables.
pub fn pack_index(data: &[u8]) {
    let mut input = Input(data);
    let format = input.format();
    let pack_end = u32::from_be_bytes([input.byte(), input.byte(), input.byte(), input.byte()]);
    let bytes = input.rest();
    let Ok(index) = crate::pack::index::Index::parse(format, bytes, pack_end as usize) else {
        return;
    };
    for position in 0..index.len().min(MAX_VISITS) {
        let entry = index.entry(bytes, position);
        assert_eq!(index.find(bytes, entry.id), Some(position));
        assert_eq!(index.at_offset(entry.offset).ok(), Some(position));
    }
}

/// Opens `[format][idx part][pack bytes]` as an on-disk pack and reads every entry.
///
/// The files live in one directory reused for the life of the process, so each iteration
/// replaces them. Index identities are not checked against payloads, as an index paired with
/// fuzzed pack bytes rarely names its entries truthfully; framing, CRCs and deltas still are.
pub fn pack(data: &[u8]) {
    static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let mut input = Input(data);
    let format = input.format();
    let index = input.part();
    let pack = input.rest();
    let directory = DIRECTORY.get_or_init(|| tempfile::tempdir().expect("fuzz pack directory"));
    let path = directory.path().join("pack-fuzz.idx");
    std::fs::write(&path, index).expect("write fuzz index");
    std::fs::write(path.with_extension("pack"), pack).expect("write fuzz pack");
    let mut limits = crate::PackLimits {
        max_bytes: 8 * MAX_BYTES,
        max_index_bytes: MAX_BYTES,
        ..crate::PackLimits::default()
    };
    let Ok(file) = crate::pack::FilePack::open(format, &path, &mut limits, &NOT_CANCELLED) else {
        return;
    };
    let count = file.identities().take(MAX_VISITS).count();
    for position in 0..count {
        let _ = file.read_unverified(position, read_limits(), &NOT_CANCELLED);
    }
    let _ = file.verify(&NOT_CANCELLED);
}

#[cfg(test)]
mod tests {
    /// Every entry point tolerates empty and short inputs; fuzzing explores the rest.
    #[test]
    fn entry_points_accept_trivial_inputs() {
        let entries: [fn(&[u8]); 5] = [
            super::object,
            super::loose,
            super::delta,
            super::pack_index,
            super::pack,
        ];
        for entry in entries {
            for input in [&b""[..], b"\0", b"\x01\x02", b"\xff\xff\xff\xff\xff\xff"] {
                entry(input);
            }
        }
    }
}
