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

#[rstest]
#[case::sha1_walk(ObjectFormat::Sha1, false, false)]
#[case::sha1_pack(ObjectFormat::Sha1, true, false)]
#[case::sha1_midx(ObjectFormat::Sha1, true, true)]
#[case::sha256_walk(ObjectFormat::Sha256, false, false)]
#[case::sha256_pack(ObjectFormat::Sha256, true, false)]
#[case::sha256_midx(ObjectFormat::Sha256, true, true)]
fn reachable_difference_matches_git(
    #[case] format: ObjectFormat,
    #[case] packed: bool,
    #[case] midx: bool,
) {
    let parents = [vec![], vec![0], vec![0], vec![1, 2], vec![2], vec![3]];
    let history = history_git::History::with_format(&parents, false, format);
    let root = history.root.path();
    if packed {
        pack_git::git(root, &["repack", "-adb"], b"");
    }
    if midx {
        pack_git::git(root, &["multi-pack-index", "write", "--bitmap"], b"");
    }
    let repo = girt::Repository::open(root).unwrap();
    let objects = repo.objects().unwrap();
    let include = [history.ids[5], history.ids[4]];
    let exclude = [history.ids[1]];
    let expected = difference_ids(root, &include, &exclude);
    let result = objects.reachable_difference(&include, &exclude).unwrap();
    assert_eq!(result.ids().collect::<BTreeSet<_>>(), expected);
    assert_eq!(result.len(), expected.len());
    assert!(!result.is_empty());
    assert_eq!(result.count(ObjectKind::Commit), expected.len()); // Shared empty tree excluded.
    assert_eq!(result.count(ObjectKind::Tree), 0);
    for id in result.ids() {
        assert!(result.contains(id));
        assert!(result.objects().read(id).unwrap().is_some());
    }
    let walked = objects
        .clone()
        .without_bitmaps()
        .reachable_difference(&include, &exclude)
        .unwrap();
    assert_eq!(
        result.ids().collect::<Vec<_>>(),
        walked.ids().collect::<Vec<_>>()
    );
    assert!(
        objects
            .reachable_difference(&include, &include)
            .unwrap()
            .is_empty()
    );
    // Zero metadata work falls back to walking, without changing the set.
    let mut limits = girt::bitmap::QueryLimits::default();
    limits.bitmap.max_decoded_words = 0;
    let fallback = objects
        .reachable_difference_with(&include, &exclude, limits, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(fallback.ids().collect::<BTreeSet<_>>(), expected);
}

fn difference_ids(
    root: &std::path::Path,
    include: &[ObjectId],
    exclude: &[ObjectId],
) -> BTreeSet<ObjectId> {
    let arguments: Vec<_> = include
        .iter()
        .map(ToString::to_string)
        .chain(exclude.iter().map(|id| format!("^{id}")))
        .collect();
    let mut args = vec!["rev-list", "--use-bitmap-index", "--objects"];
    args.extend(arguments.iter().map(String::as_str));
    let output = pack_git::git(root, &args, b"");
    std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
        .collect()
}

#[test]
fn query_falls_back_from_unusable_sidecars_and_reports_missing_objects() {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 4);
    let root = fixture.root.path();
    pack_git::git(root, &["repack", "-adb"], b"");
    let objects = fixture.repo.objects().unwrap();
    let commit = fixture
        .records
        .iter()
        .find(|(_, kind, _)| *kind == ObjectKind::Commit)
        .unwrap()
        .0;
    let expected = objects.reachable_difference(&[commit], &[]).unwrap();
    assert_eq!(expected.count(ObjectKind::Commit), 1);
    assert_eq!(expected.count(ObjectKind::Tree), 1);
    assert_eq!(expected.count(ObjectKind::Blob), 4);
    let mut limits = girt::bitmap::QueryLimits::default();
    limits.max_walk_bytes = 0;
    assert_eq!(
        objects
            .reachable_difference_with(&[commit], &[], limits, &AtomicBool::new(false))
            .unwrap()
            .len(),
        expected.len()
    );
    assert!(matches!(
        objects.clone().without_bitmaps().reachable_difference_with(
            &[commit],
            &[],
            limits,
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("walk bytes"))
    ));
    let pack = objects.packs().next().unwrap();
    for extension in ["bitmap", "rev"] {
        let path = pack.path().with_extension(extension);
        let original = fs::read(&path).unwrap();
        replace_sidecar(&path, b"damaged");
        let actual = objects.reachable_difference(&[commit], &[]).unwrap();
        assert_eq!(
            actual.ids().collect::<Vec<_>>(),
            expected.ids().collect::<Vec<_>>()
        );
        replace_sidecar(&path, &original);
    }
    let blob = fixture.records[0].0;
    let result = objects.reachable_difference(&[blob], &[]).unwrap();
    assert_eq!(result.ids().collect::<Vec<_>>(), [blob]);
    assert_eq!(result.count(ObjectKind::Blob), 1);
    let missing = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"missing");
    assert!(
        matches!(objects.reachable_difference(&[commit], &[missing]), Err(Error::MissingObject(id)) if id == missing)
    );
    assert!(matches!(
        objects.reachable_difference_with(
            &[commit],
            &[],
            girt::bitmap::QueryLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(Error::Cancelled)
    ));
}

#[test]
fn query_limits_cover_both_closures_and_shallow_boundaries() {
    let history = history_git::History::new(&[vec![], vec![0], vec![1]], false);
    let root = history.root.path();
    pack_git::git(root, &["repack", "-adb"], b"");
    let repo = girt::Repository::open(root).unwrap();
    let objects = repo.objects().unwrap().without_bitmaps();
    let mut limits = girt::bitmap::QueryLimits::default();
    limits.max_objects = 4; // Include closure is four objects; exclusion requires another visit.
    assert!(matches!(
        objects.reachable_difference_with(
            &[history.ids[2]],
            &[history.ids[0]],
            limits,
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("query objects"))
    ));
    limits = girt::bitmap::QueryLimits::default();
    limits.max_edges = 0;
    assert!(matches!(
        objects.reachable_difference_with(&[history.ids[2]], &[], limits, &AtomicBool::new(false)),
        Err(Error::Limit("query edges"))
    ));
    limits = girt::bitmap::QueryLimits::default();
    limits.max_walk_bytes = 0;
    assert!(matches!(
        objects.reachable_difference_with(&[history.ids[2]], &[], limits, &AtomicBool::new(false)),
        Err(Error::Limit("walk bytes"))
    ));
    fs::write(root.join("shallow"), format!("{}\n", history.ids[2])).unwrap();
    let shallow = girt::Repository::open(root).unwrap().objects().unwrap();
    let result = shallow
        .reachable_difference(&[history.ids[2]], &[])
        .unwrap();
    assert_eq!(result.count(ObjectKind::Commit), 1);
    assert_eq!(result.count(ObjectKind::Tree), 1);
}

fn replace_sidecar(path: &std::path::Path, bytes: &[u8]) {
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }
    fs::remove_file(path).unwrap();
    fs::write(path, bytes).unwrap();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn midx_query_binds_multiple_packs_and_uncovered_loose_tip(#[case] format: ObjectFormat) {
    let parents: Vec<_> = (0..8)
        .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
        .collect();
    let history = history_git::History::with_format(&parents, false, format);
    let root = history.root.path();
    let prefix = root.join("objects/pack/pack");
    let tree = format.hash_object(ObjectKind::Tree, b"");
    for (part, ids) in history.ids.chunks(4).enumerate() {
        let mut input = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
        if part == 0 {
            input.push_str(&format!("{tree}\n"));
        }
        pack_git::git(
            root,
            &["pack-objects", prefix.to_str().unwrap()],
            input.as_bytes(),
        );
    }
    pack_git::git(root, &["multi-pack-index", "write", "--bitmap"], b"");
    let objects = girt::Repository::open(root).unwrap().objects().unwrap();
    assert_eq!(objects.packs().count(), 2);
    let tip = history.ids[7];
    let mut limits = girt::bitmap::QueryLimits::default();
    limits.max_walk_bytes = 0;
    let result = objects
        .reachable_difference_with(&[tip], &[], limits, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        result.ids().collect::<BTreeSet<_>>(),
        reachable_ids(root, tip)
    );
    assert_eq!(result.count(ObjectKind::Commit), 8);
    let output = pack_git::git(
        root,
        &["commit-tree", &tree.to_string(), "-p", &tip.to_string()],
        b"uncovered loose tip\n",
    );
    let new_tip = std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let result = objects
        .reachable_difference(&[new_tip], &[history.ids[3]])
        .unwrap();
    assert_eq!(
        result.ids().collect::<BTreeSet<_>>(),
        difference_ids(root, &[new_tip], &[history.ids[3]])
    );
    // Disable MIDX acceleration; these packs have no individual bitmaps.
    pack_git::git(root, &["config", "core.multiPackIndex", "false"], b"");
    let disabled = girt::Repository::open(root).unwrap().objects().unwrap();
    assert!(matches!(
        disabled.reachable_difference_with(&[tip], &[], limits, &AtomicBool::new(false)),
        Err(Error::Limit("walk bytes"))
    ));
    assert_eq!(
        disabled
            .reachable_difference(&[tip], &[])
            .unwrap()
            .ids()
            .collect::<BTreeSet<_>>(),
        reachable_ids(root, tip)
    );
}

#[test]
fn roots_include_tags_trees_and_gitlinks_without_following_submodules() {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 3);
    let objects = fixture.repo.objects().unwrap();
    let tag = fixture
        .records
        .iter()
        .find(|(_, kind, _)| *kind == ObjectKind::Tag)
        .unwrap()
        .0;
    let result = objects.reachable_difference(&[tag], &[]).unwrap();
    assert_eq!(result.count(ObjectKind::Tag), 1);
    assert_eq!(result.count(ObjectKind::Commit), 1);
    assert_eq!(result.count(ObjectKind::Tree), 1);
    assert_eq!(result.count(ObjectKind::Blob), 3);
    let missing = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"not stored");
    let input = format!("160000 commit {missing}\tsubmodule\n");
    let tree = pack_git::git(
        fixture.root.path(),
        &["mktree", "--missing"],
        input.as_bytes(),
    );
    let tree: ObjectId = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
    assert_eq!(
        objects
            .reachable_difference(&[tree], &[])
            .unwrap()
            .ids()
            .collect::<Vec<_>>(),
        [tree]
    );
}

#[test]
fn stale_midx_bitmap_cannot_hide_a_surviving_pack_copy() {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 4);
    let root = fixture.root.path();
    let input = fixture
        .records
        .iter()
        .map(|(id, _, _)| format!("{id}\n"))
        .collect::<String>();
    pack_git::git(
        root,
        &[
            "pack-objects",
            "--no-reuse-object",
            "--window=0",
            "objects/pack/pack",
        ],
        input.as_bytes(),
    );
    pack_git::git(root, &["multi-pack-index", "write", "--bitmap"], b"");
    let directory = root.join("objects/pack");
    let bytes = fs::read(directory.join("multi-pack-index")).unwrap();
    let index = girt::multi_pack_index::MultiPackIndex::parse(&bytes, ObjectFormat::Sha1).unwrap();
    let selected = index.find(fixture.ordinary).unwrap();
    let path = directory.join(index.pack_names()[selected.pack as usize]);
    // Published pack artifacts are read-only on some platforms.
    replace_sidecar(&path, b"removed");
    fs::remove_file(&path).unwrap();
    let pack_path = path.with_extension("pack");
    replace_sidecar(&pack_path, b"removed");
    fs::remove_file(pack_path).unwrap();
    let objects = fixture.repo.objects().unwrap();
    let commit = fixture
        .records
        .iter()
        .find(|(_, kind, _)| *kind == ObjectKind::Commit)
        .unwrap()
        .0;
    let expected = objects
        .clone()
        .without_bitmaps()
        .reachable_difference(&[commit], &[])
        .unwrap();
    assert_eq!(
        objects
            .reachable_difference(&[commit], &[])
            .unwrap()
            .ids()
            .collect::<Vec<_>>(),
        expected.ids().collect::<Vec<_>>()
    );
    let mut limits = girt::bitmap::QueryLimits::default();
    limits.max_walk_bytes = 0;
    assert!(matches!(
        objects.reachable_difference_with(&[commit], &[], limits, &AtomicBool::new(false)),
        Err(Error::Limit("walk bytes"))
    ));
}

#[test]
fn malformed_objects_and_wrong_edge_kinds_preserve_query_errors() {
    let history = history_git::History::new(&[vec![]], false);
    let root = history.root.path();
    let malformed = pack_git::git(
        root,
        &[
            "hash-object",
            "-t",
            "commit",
            "--literally",
            "-w",
            "--stdin",
        ],
        b"invalid commit\n",
    );
    let malformed = std::str::from_utf8(&malformed)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let objects = girt::Repository::open(root).unwrap().objects().unwrap();
    assert!(
        matches!(objects.reachable_difference(&[malformed], &[]), Err(Error::Object { id, .. }) if id == malformed)
    );
    let mut tree_input = b"100644 wrong-kind\0".to_vec();
    tree_input.extend_from_slice(history.ids[0].as_bytes());
    let tree = pack_git::git(
        root,
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &tree_input,
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
    assert!(
        matches!(objects.reachable_difference(&[tree], &[]), Err(Error::Kind(id)) if id == history.ids[0])
    );
}

#[rstest]
#[case::sha1_pack(ObjectFormat::Sha1, false)]
#[case::sha256_pack(ObjectFormat::Sha256, false)]
#[case::sha1_midx(ObjectFormat::Sha1, true)]
#[case::sha256_midx(ObjectFormat::Sha256, true)]
fn encoded_bitmaps_pass_git_graph_verification(#[case] format: ObjectFormat, #[case] midx: bool) {
    let fixture = pack_git::Fixture::new(format, true, 8);
    let root = fixture.root.path();
    let objects = fixture.repo.objects().unwrap();
    let pack = objects.packs().next().unwrap();
    let commit = fixture
        .records
        .iter()
        .find(|(_, kind, _)| *kind == ObjectKind::Commit)
        .unwrap()
        .0;
    let options = girt::bitmap::WriteOptions::default();
    let limits = girt::bitmap::WriteLimits::default();
    let written = if midx {
        let artifacts = objects
            .encode_multi_pack_bitmap(Default::default(), options, limits, &AtomicBool::new(false))
            .unwrap();
        fs::write(root.join("objects/pack/multi-pack-index"), &artifacts.index).unwrap();
        let index =
            girt::multi_pack_index::MultiPackIndex::parse(&artifacts.index, format).unwrap();
        index.verify(&objects).unwrap();
        pack_git::git(root, &["multi-pack-index", "verify"], b"");
        let base = root.join(format!(
            "objects/pack/multi-pack-index-{}",
            artifacts.bitmap.anchor
        ));
        fs::write(base.with_extension("bitmap"), &artifacts.bitmap.bitmap).unwrap();
        fs::write(base.with_extension("rev"), &artifacts.bitmap.reverse_index).unwrap();
        artifacts.bitmap
    } else {
        let written = objects
            .encode_pack_bitmap(pack.checksum(), options, limits, &AtomicBool::new(false))
            .unwrap();
        fs::write(pack.path().with_extension("bitmap"), &written.bitmap).unwrap();
        let reverse_path = pack.path().with_extension("rev");
        if reverse_path.exists() {
            replace_sidecar(&reverse_path, &written.reverse_index);
        } else {
            fs::write(reverse_path, &written.reverse_index).unwrap();
        }
        written
    };
    assert_eq!(written.objects, fixture.records.len());
    assert_eq!(written.commits, 1);
    let index = BitmapIndex::parse(&written.bitmap, written.anchor, written.objects).unwrap();
    let reverse =
        ReverseIndex::parse(&written.reverse_index, written.anchor, written.objects).unwrap();
    let ids: Vec<_> = pack.ids().collect();
    let order: Vec<_> = reverse.positions().collect();
    let position = ids.binary_search(&commit).unwrap() as u32;
    let actual: BTreeSet<_> = index
        .reachable(position)
        .unwrap()
        .unwrap()
        .positions()
        .map(|bit| ids[order[bit] as usize])
        .collect();
    assert_eq!(actual, reachable_ids(root, commit));
    let verified = pack_git::git_output(
        root,
        &["rev-list", "--test-bitmap", &commit.to_string()],
        b"",
    );
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        String::from_utf8_lossy(&verified.stderr).contains("OK!"),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    pack_git::git(
        root,
        &["verify-pack", pack.index_path().to_str().unwrap()],
        b"",
    );
    let reopened = girt::Repository::open(root).unwrap().objects().unwrap();
    let mut query = girt::bitmap::QueryLimits::default();
    query.max_walk_bytes = 0;
    assert_eq!(
        reopened
            .reachable_difference_with(&[commit], &[], query, &AtomicBool::new(false))
            .unwrap()
            .ids()
            .collect::<BTreeSet<_>>(),
        actual
    );
}

#[test]
fn writer_refuses_nonclosed_graphs_and_bounds_selection_work_output() {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 3);
    let root = fixture.root.path();
    let objects = fixture.repo.objects().unwrap();
    let checksum = objects.packs().next().unwrap().checksum();
    let commit = fixture
        .records
        .iter()
        .find(|(_, kind, _)| *kind == ObjectKind::Commit)
        .unwrap()
        .0;
    let mut options = girt::bitmap::WriteOptions::default();
    options.commits = vec![commit, commit];
    options.max_commits = 1;
    let written = objects
        .encode_pack_bitmap(
            checksum,
            options.clone(),
            Default::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(written.commits, 1);
    options.commits = vec![fixture.ordinary];
    assert!(
        matches!(objects.encode_pack_bitmap(checksum, options, Default::default(), &AtomicBool::new(false)), Err(girt::bitmap::WriteError::Commit(id)) if id == fixture.ordinary)
    );
    for field in 0..5 {
        let mut limits = girt::bitmap::WriteLimits::default();
        match field {
            0 => limits.max_objects = 0,
            1 => limits.max_edges = 0,
            2 => limits.max_input_bytes = 0,
            3 => limits.max_visits = 0,
            _ => limits.max_bytes = 0,
        }
        assert!(matches!(
            objects.encode_pack_bitmap(
                checksum,
                Default::default(),
                limits,
                &AtomicBool::new(false)
            ),
            Err(girt::bitmap::WriteError::Limit(_))
        ));
    }
    assert!(matches!(
        objects.encode_pack_bitmap(
            checksum,
            Default::default(),
            Default::default(),
            &AtomicBool::new(true)
        ),
        Err(girt::bitmap::WriteError::Cancelled)
    ));
    // A commit-only pack cannot claim full closure just because its tree exists elsewhere.
    let output = pack_git::git(
        root,
        &["pack-objects", "objects/pack/pack"],
        format!("{commit}\n").as_bytes(),
    );
    let checksum: ObjectId = std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let refreshed = fixture.repo.objects().unwrap();
    assert!(matches!(
        refreshed.encode_pack_bitmap(
            checksum,
            Default::default(),
            Default::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::bitmap::WriteError::Closure(_))
    ));
    fs::write(root.join("shallow"), format!("{commit}\n")).unwrap();
    let shallow = girt::Repository::open(root).unwrap().objects().unwrap();
    assert!(matches!(
        shallow.encode_pack_bitmap(
            checksum,
            Default::default(),
            Default::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::bitmap::WriteError::Shallow)
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn writer_selects_bounded_commits_across_duplicate_midx_packs(#[case] format: ObjectFormat) {
    let parents: Vec<_> = (0..12)
        .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
        .collect();
    let history = history_git::History::with_format(&parents, false, format);
    let root = history.root.path();
    let tree = format.hash_object(ObjectKind::Tree, b"");
    let prefix = root.join("objects/pack/pack");
    for ids in [&history.ids[..8], &history.ids[4..]] {
        let input = ids
            .iter()
            .chain(std::iter::once(&tree))
            .map(|id| format!("{id}\n"))
            .collect::<String>();
        pack_git::git(
            root,
            &["pack-objects", prefix.to_str().unwrap()],
            input.as_bytes(),
        );
    }
    let objects = girt::Repository::open(root).unwrap().objects().unwrap();
    let mut options = girt::bitmap::WriteOptions::default();
    options.max_commits = 3;
    let written = objects
        .encode_multi_pack_bitmap(
            Default::default(),
            options,
            Default::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(written.bitmap.commits, 3);
    assert_eq!(written.bitmap.objects, 13);
    let midx = girt::multi_pack_index::MultiPackIndex::parse(&written.index, format).unwrap();
    midx.verify(&objects).unwrap();
    let ids: Vec<_> = history
        .ids
        .iter()
        .copied()
        .chain(std::iter::once(tree))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let parsed = BitmapIndex::parse(&written.bitmap.bitmap, written.bitmap.anchor, 13).unwrap();
    let reverse =
        ReverseIndex::parse(&written.bitmap.reverse_index, written.bitmap.anchor, 13).unwrap();
    let order: Vec<_> = reverse.positions().collect();
    fs::write(root.join("objects/pack/multi-pack-index"), &written.index).unwrap();
    let base = root.join(format!(
        "objects/pack/multi-pack-index-{}",
        written.bitmap.anchor
    ));
    fs::write(base.with_extension("bitmap"), &written.bitmap.bitmap).unwrap();
    fs::write(base.with_extension("rev"), &written.bitmap.reverse_index).unwrap();
    for position in parsed.entry_positions() {
        let commit = ids[position as usize];
        let bitmap = parsed.reachable(position).unwrap().unwrap();
        let actual: BTreeSet<_> = bitmap
            .positions()
            .map(|bit| ids[order[bit] as usize])
            .collect();
        assert_eq!(actual, reachable_ids(root, commit));
        pack_git::git(
            root,
            &["rev-list", "--test-bitmap", &commit.to_string()],
            b"",
        );
    }
}
