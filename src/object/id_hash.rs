//! Hash tables keyed by object ids.
//!
//! Two kinds of table exist. [`IdMap`] and [`IdSet`] hold ids girt computed by hashing object
//! content or read from storage it already trusts, so a peer can only influence them by finding
//! digests with chosen bits. [`PeerIdMap`] and [`PeerIdSet`] hold ids a peer merely names before
//! anything verifies them, such as delta bases, tree entry targets and advertised roots; a peer
//! can choose those freely, so they use std's SipHash with random keys, which is designed to resist
//! hash flooding. The faster hasher below is a performance choice for the first kind only.
//!
//! An object id is already a cryptographic digest, so its bytes are uniformly distributed and need
//! no further hashing to spread across buckets. SipHash, std's default, costs more than the table
//! work itself on graph walks that look up every edge. But a table can also hold identities a peer
//! merely names, such as delta bases it never sends, so the hash must not be predictable. Raw
//! digest bits would let a peer choose ids sharing a table's bucket bits, and hashing only a prefix
//! would let it choose ids that collide for every key. Each eight-byte word of the whole digest is
//! absorbed through a folded multiply with process-random keys, then finished through another, so
//! lookups stay cheap while placement depends on every byte and on keys a peer cannot see.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

use super::ObjectId;

/// A map keyed by object ids.
pub(crate) type IdMap<V> = HashMap<ObjectId, V, IdHashBuilder>;

/// A set of object ids.
pub(crate) type IdSet = HashSet<ObjectId, IdHashBuilder>;

/// A map keyed by object ids a peer names before anything verifies them.
pub(crate) type PeerIdMap<V> = HashMap<ObjectId, V>;

/// A set of object ids a peer names before anything verifies them.
pub(crate) type PeerIdSet = HashSet<ObjectId>;

/// Builds hashers sharing one set of process-random keys.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IdHashBuilder {
    /// Shared rather than copied, so every id-keyed table stays one pointer larger than its map.
    keys: &'static [u64; 4],
}

impl Default for IdHashBuilder {
    fn default() -> Self {
        static KEYS: OnceLock<[u64; 4]> = OnceLock::new();
        let keys = KEYS.get_or_init(|| {
            let random = std::hash::RandomState::new();
            odd_multipliers(std::array::from_fn(|n| random.hash_one(n)))
        });
        Self { keys }
    }
}

/// Odd multipliers keep every input bit in each product.
fn odd_multipliers(mut keys: [u64; 4]) -> [u64; 4] {
    keys[1] |= 1;
    keys[3] |= 1;
    keys
}

impl BuildHasher for IdHashBuilder {
    type Hasher = IdHasher;

    fn build_hasher(&self) -> IdHasher {
        IdHasher {
            keys: *self.keys,
            state: 0,
        }
    }
}

/// Absorbs every written byte, eight at a time, under the builder's keys.
///
/// `ObjectId`'s `Hash` writes only its digest bytes; other writes are absorbed the same way.
#[derive(Debug)]
pub(crate) struct IdHasher {
    keys: [u64; 4],
    state: u64,
}

impl IdHasher {
    fn absorb(&mut self, word: u64) {
        let [k0, k1, ..] = self.keys;
        self.state = folded_multiply(self.state ^ word ^ k0, k1);
    }
}

/// The 128-bit product's halves, combined so every input bit can reach every output bit.
fn folded_multiply(left: u64, right: u64) -> u64 {
    let product = u128::from(left) * u128::from(right);
    (product as u64) ^ ((product >> 64) as u64)
}

impl Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            self.absorb(u64::from_le_bytes(*word));
        }
        // The tail carries its length, so writes differing only in trailing zeros differ.
        let mut word = [0; 8];
        for (slot, byte) in word.iter_mut().zip(tail) {
            *slot = *byte;
        }
        self.absorb(u64::from_le_bytes(word) ^ ((tail.len() as u64) << 56));
    }

    fn write_u8(&mut self, value: u8) {
        self.absorb(u64::from(value));
    }

    fn write_usize(&mut self, value: usize) {
        self.absorb(value as u64);
    }

    fn finish(&self) -> u64 {
        let [.., k2, k3] = self.keys;
        folded_multiply(self.state ^ k2, k3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_ids_hash_equally_and_formats_stay_distinct_keys() {
        let builder = IdHashBuilder::default();
        let sha1 = ObjectId::Sha1([7; 20]);
        let sha256 = ObjectId::Sha256([7; 32]);
        assert_eq!(
            builder.hash_one(sha1),
            IdHashBuilder::default().hash_one(sha1)
        );
        let mut set = IdSet::default();
        assert!(set.insert(sha1));
        assert!(set.insert(sha256));
        assert!(!set.insert(ObjectId::Sha1([7; 20])));
        assert_eq!(set.len(), 2);
    }

    fn id(len: usize, at: usize, n: u32) -> ObjectId {
        let mut digest = [0xa5; 32];
        digest[at..at + 4].copy_from_slice(&n.to_be_bytes());
        ObjectId::from_bytes(
            if len == 20 {
                crate::ObjectFormat::Sha1
            } else {
                crate::ObjectFormat::Sha256
            },
            &digest[..len],
        )
        .unwrap()
    }

    /// Ids that differ in only a few bits anywhere in the digest, including ids sharing their whole
    /// leading word, spread across both the bucket bits and the high fingerprint bits of a table,
    /// for any keys. A prefix-only hash would put same-prefix ids in one bucket for every key.
    #[test]
    fn ids_differing_anywhere_spread_across_buckets_and_fingerprints() {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..50 {
            let keys = odd_multipliers(std::array::from_fn(|_| next()));
            let builder = IdHashBuilder {
                keys: Box::leak(Box::new(keys)),
            };
            for len in [20, 32] {
                for at in (0..=len - 4).step_by(2) {
                    let mut buckets = HashSet::new();
                    let mut fingerprints = HashSet::new();
                    for n in 0..1024_u32 {
                        let hash = builder.hash_one(id(len, at, n));
                        buckets.insert(hash & 0xfff);
                        fingerprints.insert(hash >> 57);
                    }
                    // 1,024 random values fill about 906 of 4,096 buckets and all 128 fingerprints.
                    assert!(buckets.len() > 800, "{len}/{at}: {} buckets", buckets.len());
                    assert!(
                        fingerprints.len() > 120,
                        "{len}/{at}: {}",
                        fingerprints.len()
                    );
                }
            }
        }
    }

    /// Ids sharing their leading eight bytes, as delta-base names a peer can fabricate freely,
    /// insert in linear time.
    #[test]
    fn same_prefix_ids_do_not_collide() {
        let builder = IdHashBuilder::default();
        let hashes: HashSet<_> = (0..4096).map(|n| builder.hash_one(id(20, 16, n))).collect();
        assert_eq!(hashes.len(), 4096);
    }

    #[test]
    fn short_writes_include_their_length() {
        let builder = IdHashBuilder::default();
        let mut short = builder.build_hasher();
        short.write(&[0]);
        let mut empty = builder.build_hasher();
        empty.write(&[]);
        assert_ne!(short.finish(), empty.finish());
    }
}
