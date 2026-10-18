//! Independent format vectors; Git-generated integration fixtures exercise actual payloads.
use super::*;

fn bitmap(bits: u32, value: u64) -> Vec<u8> {
    let mut bytes = bits.to_be_bytes().to_vec();
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&(1u64 << 33).to_be_bytes());
    bytes.extend_from_slice(&value.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes
}

#[test]
fn fsmonitor_versions_preserve_tokens_and_dirty_positions() {
    for (version, token) in [
        (1u32, 123u64.to_be_bytes().to_vec()),
        (2, b"opaque\0".to_vec()),
    ] {
        let mut bytes = version.to_be_bytes().to_vec();
        bytes.extend_from_slice(&token);
        let dirty = bitmap(3, 5);
        bytes.extend_from_slice(&(dirty.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&dirty);
        let parsed = FsmonitorCache::parse(&bytes, 3, Limits::default()).unwrap();
        assert_eq!(parsed.dirty, [0, 2]);
        assert_eq!(parsed.encode(Limits::default()).unwrap(), bytes);
        for end in 0..bytes.len() {
            assert!(FsmonitorCache::parse(&bytes[..end], 3, Limits::default()).is_err());
        }
        assert!(FsmonitorCache::parse(&bytes, 2, Limits::default()).is_err());
    }
}

#[test]
fn empty_untracked_cache_has_no_directory_bitmaps() {
    let mut bytes = vec![0]; // No environment identity strings.
    bytes.extend_from_slice(&[0; 72]); // Two nine-word stat records.
    bytes.extend_from_slice(&6u32.to_be_bytes());
    bytes.extend_from_slice(&[0; 40]);
    bytes.extend_from_slice(b".gitignore\0\0\0");
    let cache =
        UntrackedCache::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    assert!(cache.directories.is_empty());
    assert_eq!(cache.flags, 6);
    assert_eq!(cache.encode(Limits::default()).unwrap(), bytes);
    for end in 0..bytes.len() {
        assert!(
            UntrackedCache::parse(crate::ObjectFormat::Sha1, &bytes[..end], Limits::default())
                .is_err()
        );
    }
}

#[test]
fn rejects_cache_resource_exhaustion_before_expansion() {
    let mut bytes = 2u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"token\0");
    let dirty = bitmap(65, 1);
    bytes.extend_from_slice(&(dirty.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&dirty);
    assert!(FsmonitorCache::parse(&bytes, 65, Limits::default().with_max_bytes(8)).is_err());
    assert!(FsmonitorCache::parse(&bytes, 65, Limits::default().with_max_entries(64)).is_err());
    assert!(FsmonitorCache::parse(&bytes, 65, Limits::default().with_max_path_bytes(2)).is_err());
}

#[test]
fn malformed_optional_payloads_remain_opaque_until_requested() {
    let mut index = Index::empty(ObjectFormat::Sha1);
    index.extensions.push(super::super::Extension {
        signature: *b"UNTR",
        data: vec![255],
    });
    assert!(index.untracked_cache(Limits::default()).is_err());
    assert!(index.fsmonitor_cache(Limits::default()).unwrap().is_none());
    index.extensions.push(super::super::Extension {
        signature: *b"UNTR",
        data: vec![255],
    });
    assert!(index.untracked_cache(Limits::default()).is_err());
}

#[test]
fn untracked_directory_bitmaps_select_independent_stat_and_hash_arrays() {
    let mut bytes = vec![0];
    bytes.extend_from_slice(&[0; 72]);
    bytes.extend_from_slice(&6u32.to_be_bytes());
    bytes.extend_from_slice(&[0; 40]);
    bytes.extend_from_slice(b".gitignore\0\x02\x01\x01\0root-new\0\x00\x00child\0");
    bytes.extend_from_slice(&bitmap(2, 1)); // Root listing valid, child invalid.
    bytes.extend_from_slice(&bitmap(2, 2)); // Child check-only.
    bytes.extend_from_slice(&bitmap(2, 2)); // Only child exclude hash valid.
    bytes.extend_from_slice(&[0; 36]);
    bytes.extend_from_slice(&[3; 20]);
    bytes.push(0);

    let cache = UntrackedCache::parse(ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    let root = &cache.directories[0];
    assert!(root.valid);
    assert!(root.stat.is_some());
    assert!(!root.check_only);
    assert!(root.exclude.is_none());
    let child = &cache.directories[1];
    assert!(!child.valid);
    assert!(child.stat.is_none());
    assert!(child.check_only);
    assert_eq!(
        child.exclude,
        Some(ObjectId::from_bytes(ObjectFormat::Sha1, &[3; 20]).unwrap())
    );
}
