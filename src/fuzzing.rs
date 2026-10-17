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
        assert_eq!(index.at_offset(bytes, entry.offset).ok(), Some(position));
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

fn fetch_limits() -> crate::fetch::FetchLimits {
    crate::fetch::FetchLimits {
        max_wire_bytes: 4 * MAX_BYTES,
        max_advertisement_bytes: MAX_BYTES,
        max_refs: MAX_VISITS,
        max_wants: MAX_VISITS,
        max_haves: 32,
        max_shallow_roots: MAX_VISITS,
        max_known_objects: MAX_VISITS,
        max_known_bytes: MAX_BYTES,
        max_known_edges: 4 * MAX_VISITS,
        known_read: read_limits(),
        max_pack_bytes: 2 * MAX_BYTES,
        max_objects: 4 * MAX_VISITS,
        max_object_bytes: MAX_BYTES,
        max_delta_bytes: MAX_BYTES,
        max_decode_bytes: 4 * MAX_BYTES,
        max_delta_depth: 16,
        max_resolution_steps: 100_000,
        max_connectivity_edges: 100_000,
        max_threads: 1,
        source_packs: crate::PackLimits {
            max_bytes: 8 * MAX_BYTES,
            max_index_bytes: MAX_BYTES,
            ..crate::PackLimits::default()
        },
    }
}

/// Decodes a pkt-line stream and checks that re-encoding reproduces the bytes consumed.
pub fn pkt_line(data: &[u8]) {
    let mut reader = data;
    let mut wire = crate::packet::Wire {
        reader: &mut reader,
        remaining: data.len(),
        cancel: &NOT_CANCELLED,
    };
    let mut encoded = Vec::new();
    for _ in 0..MAX_VISITS {
        match wire.packet() {
            Ok(Some(packet)) => crate::packet::packet(&mut encoded, &packet, &NOT_CANCELLED)
                .expect("decoded packet re-encodes"),
            Ok(None) => encoded.extend_from_slice(b"0000"),
            Err(_) => return,
        }
        let consumed = data.get(..encoded.len()).expect("encoding fits the input");
        assert!(consumed.eq_ignore_ascii_case(&encoded));
    }
}

/// Discovers refs from a v0, v1 or v2 upload-pack session.
pub fn fetch_discovery(data: &[u8]) {
    let mut reader = data;
    let mut requests = Vec::new();
    let limits = fetch_limits();
    let _ = crate::fetch::discover_session(&mut reader, &mut requests, limits, &NOT_CANCELLED);
}

/// Receives a v0 fetch response for the first advertised ref, validating any pack it carries.
pub fn fetch_receive(data: &[u8]) {
    let mut reader = data;
    let select = |advertisement: &crate::fetch::Advertisement| {
        advertisement.refs.iter().take(1).map(|r| r.id).collect()
    };
    let _ = crate::fetch::receive(
        &mut reader,
        &mut std::io::sink(),
        select,
        fetch_limits(),
        &NOT_CANCELLED,
        |_| std::ops::ControlFlow::Continue(()),
    );
}

/// Parses `[flags][advertisement part][report]` as a receive-pack exchange.
///
/// Flag bits select SHA-256, `report-status-v2` and side-band status framing.
pub fn push(data: &[u8]) {
    let mut input = Input(data);
    let flags = input.byte();
    let format = if flags & 1 == 0 {
        ObjectFormat::Sha1
    } else {
        ObjectFormat::Sha256
    };
    let advertisement = input.part();
    let limits = crate::push::PushLimits {
        max_advertisement_bytes: MAX_BYTES,
        max_refs: MAX_VISITS,
        max_status_bytes: MAX_BYTES,
        ..crate::push::PushLimits::default()
    };
    let caps = (flags & 2 != 0, flags & 4 != 0);
    crate::push::fuzz_exchange(
        format,
        advertisement,
        input.rest(),
        caps,
        limits,
        &NOT_CANCELLED,
    );
}

/// Parses one configuration file both ways and checks the document retains its exact bytes.
pub fn config(data: &[u8]) {
    let parsed = crate::Config::parse(data);
    let document = crate::config::Document::parse(data);
    assert_eq!(parsed.is_ok(), document.is_ok());
    if let Ok(document) = document {
        assert_eq!(document.as_bytes(), data);
        let _ = document.sections().count();
    }
}

/// Resolves `[root part][a part][b part][c]` as a local config whose includes name `~/a`-`~/c`.
///
/// Files live in one directory reused for the life of the process; it is also the include home
/// and the repository's parent, so `gitdir` conditions can match. Every `/` must follow `~`, so
/// includes stay within that directory: opening a FIFO or device blocks Git and girt alike,
/// which is not a parser defect.
pub fn config_includes(data: &[u8]) {
    static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let confined = data.iter().enumerate().all(|(i, &byte)| {
        byte != b'/' || i.checked_sub(1).and_then(|p| data.get(p)) == Some(&b'~')
    });
    if !confined {
        return;
    }
    let directory = DIRECTORY.get_or_init(|| tempfile::tempdir().expect("fuzz config directory"));
    let home = directory.path();
    let mut input = Input(data);
    for name in ["config", "a", "b"] {
        std::fs::write(home.join(name), input.part()).expect("write fuzz config");
    }
    std::fs::write(home.join("c"), input.rest()).expect("write fuzz config");
    let inputs = crate::config::ConfigInputs {
        files: vec![crate::config::ConfigFile {
            path: home.join("config"),
            scope: crate::config::ConfigScope::Local,
            optional: false,
        }],
        context: crate::config::IncludeContext {
            home: Some(home.to_path_buf()),
            git_dirs: vec![home.join(".git")],
            branch: Some(b"main".to_vec()),
            ..Default::default()
        },
        limits: crate::config::ResolveLimits {
            depth: 4,
            bytes: MAX_BYTES,
            entries: 4096,
            match_cells: 100_000,
        },
        ..Default::default()
    };
    let _ = crate::Config::resolve(&inputs);
}

/// Adds `[case][global part][directory part][rules]` as ignore sources, then checks every
/// line of the directory part as a path.
pub fn ignore(data: &[u8]) {
    use crate::ignore::{Case, Ignore, Limits, Source};
    let mut input = Input(data);
    let case = if input.byte() & 1 == 0 {
        Case::Sensitive
    } else {
        Case::AsciiInsensitive
    };
    let limits = Limits {
        bytes: MAX_BYTES,
        sources: 8,
        patterns: 4096,
        line_bytes: 4096,
        path_bytes: 4096,
        work: 1_000_000,
    };
    let mut ignore = Ignore::new(case, limits);
    let global = input.part();
    let paths = input.part();
    if ignore.add(Source::Global, global).is_err() {
        return;
    }
    if ignore.add(Source::Directory(b"dir"), input.rest()).is_err() {
        return;
    }
    for path in paths.split(|&byte| byte == b'\n').take(MAX_VISITS) {
        let directory = path.ends_with(b"/");
        let path = path.strip_suffix(b"/").unwrap_or(path);
        // A whole-path query adds excluded ancestors to the final path's own rules.
        if let (Ok(full), Ok(Some(_))) = (
            ignore.check(
                path,
                if directory {
                    crate::ignore::InputKind::Directory
                } else {
                    crate::ignore::InputKind::File
                },
            ),
            ignore.check_direct(
                path,
                if directory {
                    crate::ignore::InputKind::Directory
                } else {
                    crate::ignore::InputKind::File
                },
            ),
        ) {
            assert!(full.is_some());
        }
    }
}

/// Parses `[format][index file]` and checks that re-encoding parses to the same index.
pub fn index(data: &[u8]) {
    let mut input = Input(data);
    let format = input.format();
    let limits = crate::index::Limits {
        max_bytes: 4 * MAX_BYTES,
        max_entries: 4096,
        max_path_bytes: 4096,
        max_extensions: 16,
    };
    let Ok(index) = crate::index::Index::parse(format, input.rest(), limits) else {
        return;
    };
    let encoded = index.encode(limits).expect("parsed index re-encodes");
    let reparsed = crate::index::Index::parse(format, &encoded, limits).expect("encoding parses");
    assert_eq!(reparsed.version(), index.version());
    assert_eq!(reparsed.entries(), index.entries());
    assert_eq!(reparsed.extensions(), index.extensions());
}

/// Parses `[format][kind][bytes]` as a loose ref, packed-refs file, reflog or reference name.
pub fn refs(data: &[u8]) {
    let mut input = Input(data);
    let format = input.format();
    let kind = input.byte();
    crate::refs::fuzz_files(format, kind, input.rest());
}

/// Decodes a reftable and checks that re-encoding decodes to the same table.
pub fn reftable(data: &[u8]) {
    use crate::refs::reftable::{Limits, Table};
    let limits = Limits {
        bytes: 4 * MAX_BYTES,
        block_bytes: MAX_BYTES,
        records: 4096,
        string_bytes: 4096,
        decoded_bytes: 4 * MAX_BYTES,
    };
    let Ok(table) = Table::decode(data, limits) else {
        return;
    };
    let Ok(encoded) = table.encode(limits) else {
        return;
    };
    let decoded = Table::decode(&encoded, limits).expect("encoded table decodes");
    assert_eq!(decoded, table);
}

#[cfg(test)]
mod tests {
    /// Every entry point tolerates empty and short inputs; fuzzing explores the rest.
    #[test]
    fn entry_points_accept_trivial_inputs() {
        let entries: [fn(&[u8]); 15] = [
            super::object,
            super::loose,
            super::delta,
            super::pack_index,
            super::pack,
            super::pkt_line,
            super::fetch_discovery,
            super::fetch_receive,
            super::push,
            super::config,
            super::config_includes,
            super::ignore,
            super::index,
            super::refs,
            super::reftable,
        ];
        for entry in entries {
            for input in [&b""[..], b"\0", b"\x01\x02", b"\xff\xff\xff\xff\xff\xff"] {
                entry(input);
            }
        }
    }
}
