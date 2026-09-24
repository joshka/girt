use std::collections::BTreeMap;
use std::path::Path;

use super::store::malformed;
use super::{RefName, ReferenceError};
use crate::ObjectId;

// Keep all records for duplicate and namespace checks. The first implementation deliberately
// validates the complete file, even for one lookup; it has no cached snapshot to invalidate.
pub(super) type Packed = BTreeMap<RefName, ObjectId>;

pub(super) fn parse(bytes: &[u8], path: &Path) -> Result<Packed, ReferenceError> {
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(malformed(path, "unterminated packed record"));
    }
    let mut entries = BTreeMap::new();
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
            parse_id(peeled, path)?;
            can_peel = false;
            continue;
        }
        if line.len() < 42 || line[40] != b' ' {
            return Err(malformed(path, "expected object ID and reference name"));
        }
        let id = parse_id(&line[..40], path)?;
        let name = RefName::new(&line[41..])
            .map_err(|_| malformed(path, "invalid packed reference name"))?;
        if name.as_bytes() == b"HEAD" || name.per_worktree() {
            return Err(ReferenceError::Unsupported("packed per-worktree reference"));
        }
        if sorted && previous.as_ref().is_some_and(|last| last >= &name) {
            return Err(malformed(path, "packed sorted order violated"));
        }
        if entries.insert(name.clone(), id).is_some() {
            return Err(malformed(path, "duplicate packed reference"));
        }
        previous = Some(name);
        can_peel = true;
    }
    Ok(entries)
}

/// Removes one direct record and its immediately following peel from already validated bytes.
/// Retain unrelated representation exactly, including header spacing and hexadecimal case.
pub(super) fn without_ref(bytes: &[u8], name: &RefName) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut removed = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"^") && removed {
            continue;
        }
        removed =
            line.get(40) == Some(&b' ') && line.get(41..line.len() - 1) == Some(name.as_bytes());
        if !removed {
            result.extend_from_slice(line);
        }
    }
    result
}

pub(super) fn parse_id(bytes: &[u8], path: &Path) -> Result<ObjectId, ReferenceError> {
    let id = std::str::from_utf8(bytes)
        .ok()
        .and_then(|v| v.parse::<ObjectId>().ok())
        .ok_or_else(|| malformed(path, "invalid SHA-1 reference target"))?;
    if id.as_bytes() == &[0; 20] {
        return Err(malformed(path, "zero reference target"));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    const ID: &str = "1111111111111111111111111111111111111111";

    #[test]
    fn reads_unsorted_records_and_discards_valid_peeled_metadata() {
        let bytes = format!("{ID} refs/tags/z\n^{ID}\n{ID} refs/heads/a\n");
        let entries = parse(bytes.as_bytes(), Path::new("packed-refs")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[&RefName::new(b"refs/tags/z").unwrap()],
            ID.parse().unwrap()
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
        parse(bytes, Path::new("packed-refs")).unwrap();
        assert_eq!(
            without_ref(bytes, &RefName::new(name).unwrap()),
            expected.as_bytes()
        );
    }

    #[test]
    fn removing_final_record_leaves_valid_empty_packed_file() {
        let bytes = format!("{ID} refs/heads/a\n");
        let remaining = without_ref(bytes.as_bytes(), &RefName::new(b"refs/heads/a").unwrap());
        assert!(remaining.is_empty());
        assert!(
            parse(&remaining, Path::new("packed-refs"))
                .unwrap()
                .is_empty()
        );
    }

    #[rstest]
    #[case::truncated("1111 refs/heads/a\n")]
    #[case::missing_newline("1111111111111111111111111111111111111111 refs/heads/a")]
    #[case::orphan("^1111111111111111111111111111111111111111\n")]
    #[case::blank("\n")]
    #[case::comment("# other comment\n")]
    #[case::invalid_name("1111111111111111111111111111111111111111 refs/../x\n")]
    #[case::zero("0000000000000000000000000000000000000000 refs/heads/a\n")]
    fn rejects_malformed(#[case] bytes: &str) {
        assert!(matches!(
            parse(bytes.as_bytes(), Path::new("packed-refs")),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[rstest]
    #[case::duplicate("refs/heads/a", "refs/heads/a")]
    #[case::out_of_order("refs/heads/z", "refs/heads/a")]
    fn rejects_invalid_sorted_records(#[case] first: &str, #[case] second: &str) {
        let bytes = format!("# pack-refs with: sorted\n{ID} {first}\n{ID} {second}\n");
        assert!(parse(bytes.as_bytes(), Path::new("packed-refs")).is_err());
    }
}
