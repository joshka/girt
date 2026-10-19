#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, TryLockError};

use super::store::{check_path, io_error, malformed};
use super::{RefName, ReferenceError};
use crate::ObjectId;

// Keep all records for duplicate and namespace checks. Every parse validates the complete file,
// even for one lookup; `PackedCache` keeps that cost to once per version of the file.
pub(super) type Packed = BTreeMap<RefName, PackedRecord>;

/// One version of `packed-refs`: its bytes and their validated records.
#[derive(Debug, Default)]
pub(super) struct Snapshot {
    pub bytes: Vec<u8>,
    pub records: Packed,
}

impl std::ops::Deref for Snapshot {
    type Target = Packed;

    fn deref(&self) -> &Packed {
        &self.records
    }
}

/// The last `packed-refs` a repository parsed, reused while the file keeps its identity.
///
/// Git and girt replace `packed-refs` by renaming a complete new file over it, never by writing
/// in place, so an unchanged device, inode, size, modification time and change time identify
/// unchanged contents. Git relies on the same assumption. Each load still opens the file and
/// checks its identity, so a replaced file is always read again. Without inode numbers (on
/// Windows), every load parses the file.
#[derive(Debug, Default)]
pub(crate) struct PackedCache(Mutex<Option<(Identity, Arc<Snapshot>)>>);

/// What identifies one version of a file that is only ever replaced, never edited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    format: crate::ObjectFormat,
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Identity {
    #[cfg(unix)]
    fn of(format: crate::ObjectFormat, file: &File) -> io::Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Some(Self {
            format,
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }))
    }

    #[cfg(not(unix))]
    fn of(_format: crate::ObjectFormat, _file: &File) -> io::Result<Option<Self>> {
        Ok(None)
    }
}

impl PackedCache {
    /// Returns the current contents of `path`, parsing them only when the file has changed.
    pub(super) fn load(
        &self,
        format: crate::ObjectFormat,
        path: &Path,
    ) -> Result<Arc<Snapshot>, ReferenceError> {
        self.load_with::<false>(
            format,
            path,
            usize::MAX,
            usize::MAX,
            &AtomicBool::new(false),
        )
    }

    /// Admits retained cache bytes and records as well as newly read input.
    pub(super) fn load_controlled(
        &self,
        format: crate::ObjectFormat,
        path: &Path,
        max_bytes: usize,
        max_refs: usize,
        cancel: &AtomicBool,
    ) -> Result<Arc<Snapshot>, ReferenceError> {
        self.load_with::<true>(format, path, max_bytes, max_refs, cancel)
    }

    fn load_with<const CONTROLLED: bool>(
        &self,
        format: crate::ObjectFormat,
        path: &Path,
        max_bytes: usize,
        max_refs: usize,
        cancel: &AtomicBool,
    ) -> Result<Arc<Snapshot>, ReferenceError> {
        if CONTROLLED {
            super::lock_wait::check_cancelled(cancel)?;
        }
        check_path(path)?;
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Arc::default());
            }
            Err(source) => return Err(io_error(path, source)),
        };
        let identity = Identity::of(format, &file).map_err(|source| io_error(path, source))?;
        // A trusted load may hold the cache lock through an unbounded read. Controlled readers
        // bypass a busy cache rather than waiting for another caller's resource policy.
        let mut cached = if CONTROLLED {
            match self.0.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            }
        } else {
            Some(
                self.0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        };
        if let (Some(identity), Some((known, snapshot))) =
            (identity, cached.as_deref().and_then(Option::as_ref))
            && identity == *known
        {
            if CONTROLLED {
                admit(snapshot, max_bytes, max_refs)?;
                super::lock_wait::check_cancelled(cancel)?;
            }
            return Ok(Arc::clone(snapshot));
        }
        let mut bytes = Vec::new();
        if CONTROLLED {
            if file
                .metadata()
                .map_err(|source| io_error(path, source))?
                .len()
                > max_bytes as u64
            {
                return Err(ReferenceError::Limit("reference bytes"));
            }
            file.take(max_bytes.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|source| io_error(path, source))?;
            if bytes.len() > max_bytes {
                return Err(ReferenceError::Limit("reference bytes"));
            }
            super::lock_wait::check_cancelled(cancel)?;
        } else {
            file.read_to_end(&mut bytes)
                .map_err(|source| io_error(path, source))?;
        }
        let records = parse_with::<CONTROLLED>(format, &bytes, path, max_refs, cancel)?;
        let snapshot = Arc::new(Snapshot { bytes, records });
        if let Some(cached) = cached.as_deref_mut() {
            *cached = identity.map(|identity| (identity, Arc::clone(&snapshot)));
        }
        Ok(snapshot)
    }
}

fn admit(snapshot: &Snapshot, max_bytes: usize, max_refs: usize) -> Result<(), ReferenceError> {
    if snapshot.bytes.len() > max_bytes {
        return Err(ReferenceError::Limit("reference bytes"));
    }
    if snapshot.len() > max_refs {
        return Err(ReferenceError::Limit("reference entries"));
    }
    Ok(())
}

/// Reads and parses `path` without a cache.
pub(super) fn load(
    format: crate::ObjectFormat,
    path: &Path,
) -> Result<Arc<Snapshot>, ReferenceError> {
    PackedCache::default().load(format, path)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PackedRecord {
    pub target: ObjectId,
    pub peeled_hint: Option<ObjectId>,
}

pub(super) fn parse(
    format: crate::ObjectFormat,
    bytes: &[u8],
    path: &Path,
) -> Result<Packed, ReferenceError> {
    parse_with::<false>(format, bytes, path, usize::MAX, &AtomicBool::new(false))
}

fn parse_with<const CONTROLLED: bool>(
    format: crate::ObjectFormat,
    bytes: &[u8],
    path: &Path,
    max_refs: usize,
    cancel: &AtomicBool,
) -> Result<Packed, ReferenceError> {
    let width = format.digest_len() * 2;
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(malformed(path, "unterminated packed record"));
    }
    let mut entries = Packed::new();
    if bytes.is_empty() {
        return Ok(entries);
    }
    let mut previous: Option<RefName> = None;
    let mut can_peel = false;
    let mut sorted = false;
    for (index, line) in bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .split(|b| *b == b'\n')
        .enumerate()
    {
        if CONTROLLED && index % 64 == 0 {
            super::lock_wait::check_cancelled(cancel)?;
        }
        if let Some(header) = line.strip_prefix(b"# pack-refs with:") {
            if index != 0 {
                return Err(malformed(path, "misplaced packed header"));
            }
            for trait_name in header.split(|b| *b == b' ').filter(|v| !v.is_empty()) {
                match trait_name {
                    b"peeled" | b"fully-peeled" => (),
                    b"sorted" => sorted = true,
                    _ => return Err(ReferenceError::Unsupported("packed-refs header trait")),
                }
            }
            continue;
        }
        if let Some(peeled) = line.strip_prefix(b"^") {
            if !can_peel {
                return Err(malformed(path, "orphan or repeated peeled record"));
            }
            let peeled = parse_id(format, peeled, path)?;
            // can_peel is set only after a reference record, which also sets previous.
            let entry = previous
                .as_ref()
                .and_then(|name| entries.get_mut(name))
                .ok_or_else(|| malformed(path, "orphan or repeated peeled record"))?;
            entry.peeled_hint = Some(peeled);
            can_peel = false;
            continue;
        }
        let Some((id, name)) = line
            .split_at_checked(width)
            .and_then(|(id, rest)| Some((id, rest.strip_prefix(b" ")?)))
            .filter(|(_, name)| !name.is_empty())
        else {
            return Err(malformed(path, "expected object ID and reference name"));
        };
        if CONTROLLED && entries.len() >= max_refs {
            return Err(ReferenceError::Limit("reference entries"));
        }
        let id = parse_id(format, id, path)?;
        let name =
            RefName::new(name).map_err(|_| malformed(path, "invalid packed reference name"))?;
        if name.as_bytes() == b"HEAD" || name.per_worktree() {
            return Err(ReferenceError::Unsupported("packed per-worktree reference"));
        }
        if sorted && previous.as_ref().is_some_and(|last| last >= &name) {
            return Err(malformed(path, "packed sorted order violated"));
        }
        if entries
            .insert(
                name.clone(),
                PackedRecord {
                    target: id,
                    peeled_hint: None,
                },
            )
            .is_some()
        {
            return Err(malformed(path, "duplicate packed reference"));
        }
        previous = Some(name);
        can_peel = true;
    }
    if CONTROLLED {
        super::lock_wait::check_cancelled(cancel)?;
    }
    Ok(entries)
}

/// Removes one direct record and its immediately following peel from already validated bytes.
/// Retain unrelated representation exactly, including header spacing and hexadecimal case.
pub(super) fn without_ref(format: crate::ObjectFormat, bytes: &[u8], name: &RefName) -> Vec<u8> {
    let width = format.digest_len() * 2;
    let mut result = Vec::with_capacity(bytes.len());
    let mut removed = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"^") && removed {
            continue;
        }
        removed = line.get(width) == Some(&b' ')
            && line.get(width + 1..line.len() - 1) == Some(name.as_bytes());
        if !removed {
            result.extend_from_slice(line);
        }
    }
    result
}

pub(super) fn parse_id(
    format: crate::ObjectFormat,
    bytes: &[u8],
    path: &Path,
) -> Result<ObjectId, ReferenceError> {
    if bytes.len() != format.digest_len() * 2 {
        return Err(malformed(
            path,
            "expected repository-format reference target",
        ));
    }
    // Decode ASCII pairs directly: reference inventories parse thousands of IDs, and neither
    // UTF-8 validation nor a separate whole-string hexadecimal pass is needed.
    let mut digest = [0; 32];
    for (&[high, low], output) in bytes.as_chunks::<2>().0.iter().zip(digest.iter_mut()) {
        let value = hex_digit(high)
            .zip(hex_digit(low))
            .map(|(high, low)| high << 4 | low)
            .ok_or_else(|| malformed(path, "invalid repository-format reference target"))?;
        *output = value;
    }
    let id = ObjectId::from_bytes(
        format,
        digest
            .get(..format.digest_len())
            .ok_or_else(|| malformed(path, "unsupported reference digest width"))?,
    )
    .map_err(|_| malformed(path, "invalid repository-format reference target"))?;
    if id.is_null() {
        return Err(malformed(path, "zero reference target"));
    }
    Ok(id)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    const ID: &str = "1111111111111111111111111111111111111111";

    #[test]
    fn controlled_load_admits_cold_and_cached_snapshots_against_both_limits() {
        use std::sync::atomic::AtomicBool;

        for format in [crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().canonicalize().unwrap().join("packed-refs");
            let id = "1".repeat(format.digest_len() * 2);
            let bytes =
                format!("# pack-refs with: peeled\n{id} refs/tags/a\n^{id}\n{id} refs/heads/b\n");
            std::fs::write(&path, &bytes).unwrap();
            let cancel = AtomicBool::new(false);
            let cache = PackedCache::default();
            for warm in [false, true] {
                if warm {
                    cache.load(format, &path).unwrap();
                }
                assert!(matches!(
                    cache.load_controlled(format, &path, bytes.len() - 1, 2, &cancel),
                    Err(ReferenceError::Limit("reference bytes"))
                ));
                assert!(matches!(
                    cache.load_controlled(format, &path, bytes.len(), 1, &cancel),
                    Err(ReferenceError::Limit("reference entries"))
                ));
                let snapshot = cache
                    .load_controlled(format, &path, bytes.len(), 2, &cancel)
                    .unwrap();
                assert_eq!(snapshot.bytes, bytes.as_bytes());
                assert_eq!(snapshot.len(), 2);
            }
            // A refused controlled load must not poison ordinary trusted loading.
            assert_eq!(cache.load(format, &path).unwrap().len(), 2);
        }
    }

    #[test]
    fn controlled_load_checks_cancellation_before_opening_or_using_a_cached_snapshot() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("packed-refs");
        let cache = PackedCache::default();
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            cache.load_controlled(crate::ObjectFormat::Sha1, &path, 100, 1, &cancel),
            Err(ReferenceError::Cancelled)
        ));
        std::fs::write(&path, format!("{ID} refs/heads/a\n")).unwrap();
        cache.load(crate::ObjectFormat::Sha1, &path).unwrap();
        assert!(matches!(
            cache.load_controlled(crate::ObjectFormat::Sha1, &path, 100, 1, &cancel),
            Err(ReferenceError::Cancelled)
        ));
        cancel.store(false, Ordering::Relaxed);
        assert_eq!(
            cache
                .load_controlled(crate::ObjectFormat::Sha1, &path, 100, 1, &cancel)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn controlled_load_bypasses_a_cache_held_by_another_reader() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("packed-refs");
        std::fs::write(&path, format!("{ID} refs/heads/a\n")).unwrap();
        let cache = Arc::new(PackedCache::default());
        let guard = cache.0.lock().unwrap();
        let reader = Arc::clone(&cache);
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = reader.load_controlled(
                crate::ObjectFormat::Sha1,
                &path,
                100,
                1,
                &AtomicBool::new(false),
            );
            send.send(result.map(|snapshot| snapshot.len())).unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        // Release before joining even on regression: the test must fail instead of hanging.
        drop(guard);
        worker.join().unwrap();
        assert_eq!(result.unwrap().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cache_reuses_an_unchanged_file_and_rereads_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        // Reference paths refuse symlinks, including the one above macOS temporary directories.
        let directory = root.path().canonicalize().unwrap();
        let path = directory.join("packed-refs");
        let format = crate::ObjectFormat::Sha1;
        std::fs::write(&path, format!("{ID} refs/heads/a\n")).unwrap();
        let cache = PackedCache::default();
        let first = cache.load(format, &path).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.load(format, &path).unwrap()));

        // Same size and, on coarse clocks, the same modification time: only the inode differs.
        let replacement = directory.join("packed-refs.new");
        std::fs::write(&replacement, format!("{ID} refs/heads/b\n")).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let second = cache.load(format, &path).unwrap();
        assert!(second.contains_key(&RefName::new("refs/heads/b").unwrap()));
        assert!(!second.contains_key(&RefName::new("refs/heads/a").unwrap()));

        std::fs::remove_file(&path).unwrap();
        assert!(cache.load(format, &path).unwrap().is_empty());
    }

    #[rstest]
    #[case::sha1_lower(crate::ObjectFormat::Sha1, "0123456789abcdef0123456789abcdef01234567")]
    #[case::sha1_upper(crate::ObjectFormat::Sha1, "0123456789ABCDEF0123456789ABCDEF01234567")]
    #[case::sha256(
        crate::ObjectFormat::Sha256,
        "0123456789ABCDEF0123456789abcdef0123456789ABCDEF0123456789abcdef"
    )]
    fn decodes_reference_ids_in_either_hex_case(
        #[case] format: crate::ObjectFormat,
        #[case] hex: &str,
    ) {
        assert_eq!(
            parse_id(format, hex.as_bytes(), Path::new("packed-refs")).unwrap(),
            ObjectId::from_hex(format, hex).unwrap()
        );
    }

    #[rstest]
    #[case::high(b"g111111111111111111111111111111111111111")]
    #[case::low(b"1g11111111111111111111111111111111111111")]
    #[case::last(b"111111111111111111111111111111111111111g")]
    #[case::non_ascii(b"\xff111111111111111111111111111111111111111")]
    #[case::multibyte(b"\xc3\xa911111111111111111111111111111111111111")]
    fn rejects_non_hex_reference_pairs(#[case] bytes: &[u8]) {
        assert_eq!(bytes.len(), 40);
        assert!(parse_id(crate::ObjectFormat::Sha1, bytes, Path::new("packed-refs")).is_err());
    }

    #[test]
    fn reads_unsorted_records_and_retains_valid_peeled_metadata() {
        let bytes = format!("{ID} refs/tags/z\n^{ID}\n{ID} refs/heads/a\n");
        let entries = parse(
            crate::ObjectFormat::Sha1,
            bytes.as_bytes(),
            Path::new("packed-refs"),
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[&RefName::new(b"refs/tags/z").unwrap()],
            PackedRecord {
                target: ID.parse().unwrap(),
                peeled_hint: Some(ID.parse().unwrap())
            }
        );
    }

    #[rstest]
    #[case::first(
        "refs/tags/z",
        "2222222222222222222222222222222222222222 refs/heads/a\n"
    )]
    #[case::last(
        "refs/heads/a",
        "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD refs/tags/z\n^1111111111111111111111111111111111111111\n"
    )]
    fn deletion_preserves_unsorted_headerless_representation(
        #[case] name: &str,
        #[case] expected: &str,
    ) {
        let bytes = b"ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD refs/tags/z\n^1111111111111111111111111111111111111111\n2222222222222222222222222222222222222222 refs/heads/a\n";
        parse(crate::ObjectFormat::Sha1, bytes, Path::new("packed-refs")).unwrap();
        assert_eq!(
            without_ref(
                crate::ObjectFormat::Sha1,
                bytes,
                &RefName::new(name).unwrap()
            ),
            expected.as_bytes()
        );
    }

    #[test]
    fn removing_final_record_leaves_valid_empty_packed_file() {
        let bytes = format!("{ID} refs/heads/a\n");
        let remaining = without_ref(
            crate::ObjectFormat::Sha1,
            bytes.as_bytes(),
            &RefName::new(b"refs/heads/a").unwrap(),
        );
        assert!(remaining.is_empty());
        assert!(
            parse(
                crate::ObjectFormat::Sha1,
                &remaining,
                Path::new("packed-refs")
            )
            .unwrap()
            .is_empty()
        );
    }

    #[rstest]
    #[case::truncated("1111 refs/heads/a\n")]
    #[case::id_only("1111111111111111111111111111111111111111\n")]
    #[case::empty_name("1111111111111111111111111111111111111111 \n")]
    #[case::tab_separator("1111111111111111111111111111111111111111\trefs/heads/a\n")]
    #[case::missing_newline("1111111111111111111111111111111111111111 refs/heads/a")]
    #[case::orphan("^1111111111111111111111111111111111111111\n")]
    #[case::blank("\n")]
    #[case::comment("# other comment\n")]
    #[case::invalid_name("1111111111111111111111111111111111111111 refs/../x\n")]
    #[case::zero("0000000000000000000000000000000000000000 refs/heads/a\n")]
    fn rejects_malformed(#[case] bytes: &str) {
        assert!(matches!(
            parse(
                crate::ObjectFormat::Sha1,
                bytes.as_bytes(),
                Path::new("packed-refs")
            ),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[rstest]
    #[case::duplicate("refs/heads/a", "refs/heads/a")]
    #[case::out_of_order("refs/heads/z", "refs/heads/a")]
    fn rejects_invalid_sorted_records(#[case] first: &str, #[case] second: &str) {
        let bytes = format!("# pack-refs with: sorted\n{ID} {first}\n{ID} {second}\n");
        assert!(
            parse(
                crate::ObjectFormat::Sha1,
                bytes.as_bytes(),
                Path::new("packed-refs")
            )
            .is_err()
        );
    }
}
