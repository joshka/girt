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
    for (version, token) in [(1u32, 123u64.to_be_bytes().to_vec()), (2, b"opaque\0".to_vec())] {
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
    let cache = UntrackedCache::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    assert!(cache.directories.is_empty());
    assert_eq!(cache.flags, 6);
    assert_eq!(cache.encode(Limits::default()).unwrap(), bytes);
    for end in 0..bytes.len() {
        assert!(UntrackedCache::parse(crate::ObjectFormat::Sha1, &bytes[..end], Limits::default()).is_err());
    }
}
