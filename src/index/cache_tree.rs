//! Claims from the optional `TREE` (cache-tree) extension, verified before any reuse.
//!
//! Each record names a directory relative to its parent (the root is empty), the number of index
//! entries it covers or -1 when invalid, its subtree count, and, when valid, the directory's tree
//! identity. A parent's subtrees follow it. Records are treated only as claims: callers recompute
//! every identity from the entries, so a stale or malformed extension can cost time but never
//! change a result.
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository input (#103)"
)]
use super::{Entry, Mode, Stage};
use crate::ObjectId;

/// A valid record whose entry count matches the contiguous entries under its directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Claim {
    /// First covered entry.
    pub start: usize,
    /// One past the last covered entry.
    pub end: usize,
    /// Claimed tree identity of the directory.
    pub id: ObjectId,
}

struct Record<'a> {
    name: &'a [u8],
    entries: Option<usize>,
    id: Option<ObjectId>,
    subtrees: usize,
}

/// Returns the outermost claims, sorted and disjoint, or none for an absent or malformed payload.
///
/// A claim covers only normal-stage, non-intent-to-add file entries, so every identity under it
/// is derived from those entries alone. Parsing allocates at most one record per index entry plus
/// the root.
pub(super) fn claims(format: crate::ObjectFormat, data: &[u8], entries: &[Entry]) -> Vec<Claim> {
    let Some(records) = parse(format, data, entries.len() + 1) else {
        return Vec::new();
    };
    let children = children(&records);
    let mut claims = Vec::new();
    // Each pending item is a record position, its directory prefix and its entry range.
    let mut pending = vec![(0, Vec::new(), 0, entries.len())];
    while let Some((position, prefix, start, end)) = pending.pop() {
        let (Some(record), Some(within)) = (records.get(position), entries.get(start..end)) else {
            continue;
        };
        if let (Some(count), Some(id)) = (record.entries, record.id)
            && count == within.len()
            && count != 0
            && within.iter().all(plain)
        {
            claims.push(Claim { start, end, id });
            continue;
        }
        for &child in children.get(position).into_iter().flatten() {
            let Some(child_record) = records.get(child) else {
                continue;
            };
            let mut directory = prefix.clone();
            directory.extend_from_slice(child_record.name);
            directory.push(b'/');
            // Paths under a directory are contiguous in the sorted entries.
            let first = within.partition_point(|e| e.path < directory);
            let last =
                within.partition_point(|e| e.path < directory || e.path.starts_with(&directory));
            if first < last {
                pending.push((child, directory, start + first, start + last));
            }
        }
    }
    claims.sort_by_key(|claim| claim.start);
    claims
}

fn plain(entry: &Entry) -> bool {
    entry.stage == Stage::Normal && !entry.intent_to_add && entry.mode != Mode::SparseDirectory
}

// Records in pre-order; each subtree count says how many direct children follow.
fn parse(format: crate::ObjectFormat, data: &[u8], max_records: usize) -> Option<Vec<Record<'_>>> {
    let mut records = Vec::new();
    let mut rest = data;
    // Children still expected by each open ancestor.
    let mut open: Vec<usize> = Vec::new();
    loop {
        if records.len() == max_records {
            return None;
        }
        let (name, after) = split_at_byte(rest, 0)?;
        if records.is_empty() != name.is_empty() || name.contains(&b'/') {
            return None;
        }
        let (count, after) = split_at_byte(after, b' ')?;
        let entries = decimal(count, true)?;
        let (subtrees, after) = split_at_byte(after, b'\n')?;
        let subtrees = usize::try_from(decimal(subtrees, false)?).ok()?;
        rest = after;
        let (entries, id) = if entries < 0 {
            (None, None)
        } else {
            let (digest, after) = rest.split_at_checked(format.digest_len())?;
            rest = after;
            let id = ObjectId::from_bytes(format, digest).ok()?;
            (Some(usize::try_from(entries).ok()?), Some(id))
        };
        records.push(Record {
            name,
            entries,
            id,
            subtrees,
        });
        if let Some(remaining) = open.last_mut() {
            *remaining = remaining.checked_sub(1)?;
        }
        open.push(subtrees);
        while open.last() == Some(&0) {
            open.pop();
        }
        if open.is_empty() {
            return rest.is_empty().then_some(records);
        }
    }
}

// The bytes before the first `delimiter` and those after it.
fn split_at_byte(bytes: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let position = bytes.iter().position(|b| *b == delimiter)?;
    let (before, after) = bytes.split_at_checked(position)?;
    Some((before, after.get(1..)?))
}

// Direct children of every record, from the pre-order subtree counts.
fn children(records: &[Record<'_>]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); records.len()];
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (position, record) in records.iter().enumerate() {
        if let Some((parent, remaining)) = open.last_mut() {
            if let Some(list) = children.get_mut(*parent) {
                list.push(position);
            }
            *remaining = remaining.saturating_sub(1);
        }
        open.push((position, record.subtrees));
        while open.last().is_some_and(|(_, remaining)| *remaining == 0) {
            open.pop();
        }
    }
    children
}

// Git writes plain ASCII decimals; only the entry count may be -1.
fn decimal(bytes: &[u8], signed: bool) -> Option<i64> {
    if signed && bytes == b"-1" {
        return Some(-1);
    }
    if bytes.is_empty() || bytes.len() > 18 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    const FORMAT: ObjectFormat = ObjectFormat::Sha1;

    fn id(byte: u8) -> ObjectId {
        ObjectId::from_bytes(FORMAT, &[byte; 20]).unwrap()
    }

    fn entry(path: &str) -> Entry {
        Entry::new(path.as_bytes().to_vec(), Mode::Regular, id(0xee))
    }

    fn record(name: &str, entries: i64, subtrees: usize, tree: u8) -> Vec<u8> {
        let mut bytes = format!("{name}\0{entries} {subtrees}\n").into_bytes();
        if entries >= 0 {
            bytes.extend_from_slice(&[tree; 20]);
        }
        bytes
    }

    fn entries() -> Vec<Entry> {
        ["a-x/1", "a.txt", "a/2", "a/z/1", "ab/1", "b/1"]
            .into_iter()
            .map(entry)
            .collect()
    }

    // Children appear in Git's length-first order, not index order; ranges come from names.
    #[test]
    fn invalid_root_yields_its_valid_subtrees() {
        let data = [
            record("", -1, 4, 0),
            record("a", 2, 1, 1),
            record("z", 1, 0, 2),
            record("b", 1, 0, 3),
            record("ab", -1, 0, 0),
            record("a-x", 1, 0, 4),
        ]
        .concat();
        assert_eq!(
            claims(FORMAT, &data, &entries()),
            [
                Claim {
                    start: 0,
                    end: 1,
                    id: id(4)
                },
                Claim {
                    start: 2,
                    end: 4,
                    id: id(1)
                },
                Claim {
                    start: 5,
                    end: 6,
                    id: id(3)
                },
            ]
        );
    }

    #[test]
    fn valid_root_claims_every_entry() {
        let data = [record("", 6, 0, 9)].concat();
        assert_eq!(
            claims(FORMAT, &data, &entries()),
            [Claim {
                start: 0,
                end: 6,
                id: id(9)
            }]
        );
    }

    #[rstest]
    #[case::count_mismatch([record("", 5, 0, 9)].concat())]
    #[case::truncated_digest(b"\x006 0\n\x01\x02".to_vec())]
    #[case::trailing_bytes([record("", 6, 0, 9), b"x".to_vec()].concat())]
    #[case::missing_children([record("", -1, 2, 0), record("a", 2, 0, 1)].concat())]
    #[case::named_root([record("r", 6, 0, 9)].concat())]
    #[case::slash_in_name([record("", -1, 1, 0), record("a/z", 1, 0, 2)].concat())]
    #[case::sign([record("", -1, 1, 0), b"a\0+2 0\n".to_vec(), vec![1; 20]].concat())]
    fn unusable_records_claim_nothing(#[case] data: Vec<u8>) {
        assert_eq!(claims(FORMAT, &data, &entries()), []);
    }

    #[test]
    fn conflicted_or_intent_to_add_ranges_are_not_claimed() {
        let mut entries = entries();
        entries[2].intent_to_add = true;
        let data = [record("", 6, 1, 9), record("a", 2, 0, 1)].concat();
        assert_eq!(claims(FORMAT, &data, &entries), []);
    }
}
