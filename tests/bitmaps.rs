//! Bitmap format evidence comes from isolated Git CLI output.
#[path = "support/history_git.rs"]
#[allow(dead_code)]
mod history_git;
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::collections::BTreeSet;
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::bitmap::{BitmapIndex, Error, Limits, ReverseIndex};
use girt::{ObjectFormat, ObjectId, ObjectKind};
use rstest::rstest;

#[rstest]
#[case::sha1_pack(ObjectFormat::Sha1, false)]
#[case::sha256_pack(ObjectFormat::Sha256, false)]
#[case::sha1_midx(ObjectFormat::Sha1, true)]
#[case::sha256_midx(ObjectFormat::Sha256, true)]
fn git_bitmaps_match_types_and_reachability(#[case] format: ObjectFormat, #[case] midx: bool) {
    let fixture = pack_git::Fixture::new(format, true, 8);
    let root = fixture.root.path();
    pack_git::git(
        root,
        &["-c", "pack.writeBitmapLookupTable=true", "repack", "-adb"],
        b"",
    );
    let objects = fixture.repo.objects().unwrap();
    let pack = objects.packs().next().unwrap();
    let ids: Vec<_> = pack.ids().collect();
    let (bytes, anchor, order) = if midx {
        pack_git::git(
            root,
            &[
                "-c",
                "pack.writeBitmapLookupTable=true",
                "multi-pack-index",
                "write",
                "--bitmap",
            ],
            b"",
        );
        let index = fs::read(root.join("objects/pack/multi-pack-index")).unwrap();
        let anchor = trailer(&index, format);
        let path = root.join(format!("objects/pack/multi-pack-index-{anchor}.bitmap"));
        let reverse = chunk(&index, b"RIDX");
        let order = reverse
            .as_chunks::<4>()
            .0
            .iter()
            .map(|row| u32::from_be_bytes(*row))
            .collect();
        (fs::read(path).unwrap(), anchor, order)
    } else {
        let reverse = fs::read(pack.path().with_extension("rev")).unwrap();
        let reverse = ReverseIndex::parse(&reverse, pack.checksum(), ids.len()).unwrap();
        assert_eq!(reverse.anchor(), pack.checksum());
        assert_eq!(reverse.index_position(ids.len()), None);
        (
            fs::read(pack.path().with_extension("bitmap")).unwrap(),
            pack.checksum(),
            reverse.positions().collect::<Vec<_>>(),
        )
    };
    let index = BitmapIndex::parse(&bytes, anchor, ids.len()).unwrap();
    assert_eq!(index.anchor(), anchor);
    assert_eq!(index.object_count(), ids.len());
    assert!(index.entry_positions().len() > 0);
    for kind in [
        ObjectKind::Commit,
        ObjectKind::Tree,
        ObjectKind::Blob,
        ObjectKind::Tag,
    ] {
        let bitmap = index.type_bitmap(kind);
        let actual: BTreeSet<_> = bitmap
            .positions()
            .map(|bit| ids[order[bit] as usize])
            .collect();
        let expected = fixture
            .records
            .iter()
            .filter(|(_, k, _)| *k == kind)
            .map(|(id, _, _)| *id)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(bitmap.bit_len(), ids.len());
        assert_eq!(bitmap.count(), actual.len());
        assert!(!bitmap.contains(ids.len()));
    }
    for position in index.entry_positions() {
        let id = ids[position as usize];
        let reachable = index.reachable(position).unwrap().unwrap();
        let actual: BTreeSet<_> = reachable
            .positions()
            .map(|bit| ids[order[bit] as usize])
            .collect();
        let expected = reachable_ids(root, id);
        assert_eq!(actual, expected);
        let output = pack_git::git(
            root,
            &["rev-list", "--use-bitmap-index", "--count", &id.to_string()],
            b"",
        );
        assert_eq!(std::str::from_utf8(&output).unwrap().trim(), "1");
        assert!(index.name_hash(position as usize).is_some());
    }
    assert!(index.reachable(u32::MAX).unwrap().is_none());
}

fn reachable_ids(root: &std::path::Path, id: ObjectId) -> BTreeSet<ObjectId> {
    let output = pack_git::git(
        root,
        &[
            "rev-list",
            "--use-bitmap-index",
            "--objects",
            &id.to_string(),
        ],
        b"",
    );
    std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
        .collect()
}

fn trailer(bytes: &[u8], format: ObjectFormat) -> ObjectId {
    ObjectId::from_bytes(format, &bytes[bytes.len() - format.digest_len()..]).unwrap()
}

fn chunk<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    for row in bytes[12..].windows(24).step_by(12) {
        if &row[..4] == kind {
            let start = u64::from_be_bytes(row[4..12].try_into().unwrap()) as usize;
            let end = u64::from_be_bytes(row[16..24].try_into().unwrap()) as usize;
            return &bytes[start..end];
        }
    }
    panic!("missing chunk");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn xor_entries_match_git_and_obey_budgets(#[case] format: ObjectFormat) {
    let parents: Vec<_> = (0..36)
        .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
        .collect();
    let history = history_git::History::with_format(&parents, false, format);
    let root = history.root.path();
    pack_git::git(
        root,
        &["-c", "pack.writeBitmapLookupTable=true", "repack", "-adb"],
        b"",
    );
    let repo = girt::Repository::open(root).unwrap();
    let objects = repo.objects().unwrap();
    let pack = objects.packs().next().unwrap();
    let ids: Vec<_> = pack.ids().collect();
    let bytes = fs::read(pack.path().with_extension("bitmap")).unwrap();
    let reverse = ReverseIndex::parse(
        &fs::read(pack.path().with_extension("rev")).unwrap(),
        pack.checksum(),
        ids.len(),
    )
    .unwrap();
    let order: Vec<_> = reverse.positions().collect();
    let index = BitmapIndex::parse(&bytes, pack.checksum(), ids.len()).unwrap();
    assert!(index.entry_positions().len() > 2);
    for position in index.entry_positions() {
        let bitmap = index.reachable(position).unwrap().unwrap();
        let actual = bitmap
            .positions()
            .map(|bit| ids[order[bit] as usize])
            .collect();
        assert_eq!(reachable_ids(root, ids[position as usize]), actual);
    }
    let position = index.entry_positions().next().unwrap();
    let mut limits = Limits::default();
    limits.max_decoded_words = 0;
    assert!(matches!(
        index.reachable_with(position, limits, &AtomicBool::new(false)),
        Err(Error::Limit(_))
    ));
    assert!(matches!(
        index.reachable_with(position, Limits::default(), &AtomicBool::new(true)),
        Err(Error::Cancelled)
    ));
    limits = Limits::default();
    limits.max_bytes = bytes.len() - 1;
    assert!(matches!(
        index.reachable_with(position, limits, &AtomicBool::new(false)),
        Err(Error::Limit("bytes"))
    ));
    assert!(matches!(
        BitmapIndex::parse_with(&bytes, pack.checksum(), ids.len(), limits),
        Err(Error::Limit("bytes"))
    ));
    let mut damaged = bytes.clone();
    damaged[12] ^= 1;
    assert!(matches!(
        BitmapIndex::parse(&damaged, pack.checksum(), ids.len()),
        Err(Error::Checksum)
    ));
    resign(&mut damaged, format);
    assert!(matches!(
        BitmapIndex::parse(&damaged, pack.checksum(), ids.len()),
        Err(Error::Binding)
    ));
    for end in 0..bytes.len() {
        assert!(BitmapIndex::parse(&bytes[..end], pack.checksum(), ids.len()).is_err());
    }
}

fn resign(bytes: &mut [u8], format: ObjectFormat) {
    let end = bytes.len() - format.digest_len();
    use sha1::Digest;
    let hash = match format {
        ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..end]).to_vec(),
        ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..end]).to_vec(),
        _ => panic!("unsupported fixture hash"),
    };
    bytes[end..].copy_from_slice(&hash);
}
