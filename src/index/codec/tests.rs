use rstest::rstest;

use super::*;

fn entry(path: &[u8], stage: Stage) -> Entry {
    Entry {
        stage,
        ..Entry::new(
            path.to_vec(),
            Mode::Regular,
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"content"),
        )
    }
}
fn encoded() -> Vec<u8> {
    Index::new(
        crate::ObjectFormat::Sha1,
        vec![entry(b"a", Stage::Normal)],
        Limits::default(),
    )
    .unwrap()
    .encode(Limits::default())
    .unwrap()
}
fn resign(bytes: &mut Vec<u8>) {
    bytes.truncate(bytes.len() - 20);
    let hash = Sha1::digest(&*bytes);
    bytes.extend_from_slice(&hash);
}
fn extension(signature: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = encoded();
    bytes.truncate(bytes.len() - 20);
    bytes.extend_from_slice(signature);
    put_word(&mut bytes, data.len() as u32);
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(&[0; 20]);
    resign(&mut bytes);
    bytes
}

#[rstest]
#[case::regular(Mode::Regular)]
#[case::executable(Mode::Executable)]
#[case::symlink(Mode::Symlink)]
#[case::gitlink(Mode::Gitlink)]
fn retains_modes_flags_and_every_stat_word(#[case] mode: Mode) {
    let original = Entry {
        mode,
        assume_valid: true,
        stat: Stat {
            ctime: Timestamp {
                seconds: u32::MAX,
                nanoseconds: 2,
            },
            mtime: Timestamp {
                seconds: 3,
                nanoseconds: u32::MAX,
            },
            device: 5,
            inode: 6,
            uid: 7,
            gid: 8,
            size: 9,
        },
        ..entry(b"dir/a", Stage::Ours)
    };
    let index = Index::new(
        crate::ObjectFormat::Sha1,
        vec![original.clone()],
        Limits::default(),
    )
    .unwrap();
    let bytes = index.encode(Limits::default()).unwrap();
    let parsed = Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    assert_eq!(parsed.entries(), &[original]);
    assert_eq!(parsed.encode(Limits::default()).unwrap(), bytes);
}

#[rstest]
#[case::non_utf8(b"dir/\xff".to_vec())]
#[case::long(vec![b'x'; 10_000])]
#[case::length_boundary(vec![b'x'; 4095])]
#[case::below_boundary(vec![b'x'; 4094])]
#[case::padding_one(vec![b'x'; 9])]
#[case::padding_eight(vec![b'x'; 2])]
#[case::platform_unsafe(b"C:\\a/.GIT/con".to_vec())]
fn retains_path_bytes_without_checkout_validation(#[case] path: Vec<u8>) {
    let index = Index::new(
        crate::ObjectFormat::Sha1,
        vec![entry(&path, Stage::Normal)],
        Limits::default(),
    )
    .unwrap();
    let bytes = index.encode(Limits::default()).unwrap();
    assert_eq!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default())
            .unwrap()
            .entries()[0]
            .path,
        path
    );
}

#[rstest]
#[case::empty(b"")]
#[case::absolute(b"/a")]
#[case::trailing(b"a/")]
#[case::double_slash(b"a//b")]
#[case::dot(b"a/./b")]
#[case::parent(b"../b")]
#[case::git(b"a/.git/b")]
#[case::nul(b"a\0b")]
fn rejects_invalid_paths(#[case] path: &[u8]) {
    assert!(matches!(
        Index::new(
            crate::ObjectFormat::Sha1,
            vec![entry(path, Stage::Normal)],
            Limits::default()
        ),
        Err(Error::Entry { .. })
    ));
}

#[test]
fn empty_has_independent_header_and_checksum() {
    let bytes = Index::empty(crate::ObjectFormat::Sha1)
        .encode(Limits::default())
        .unwrap();
    assert_eq!(&bytes[..12], b"DIRC\0\0\0\x02\0\0\0\0");
    // Independently obtained using Git read-tree --empty in an isolated repository.
    assert_eq!(
        &bytes[12..],
        &[
            0x39, 0xd8, 0x90, 0x13, 0x9e, 0xe5, 0x35, 0x6c, 0x7e, 0xf5, 0x72, 0x21, 0x6c, 0xeb,
            0xcd, 0x27, 0xaa, 0x41, 0xf9, 0xdf
        ]
    );
    assert!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default())
            .unwrap()
            .entries()
            .is_empty()
    );
}

#[rstest]
#[case::duplicate(Stage::Normal, Stage::Normal)]
#[case::normal_conflict(Stage::Normal, Stage::Ours)]
#[case::duplicate_conflict(Stage::Theirs, Stage::Theirs)]
fn rejects_stage_relationships(#[case] a: Stage, #[case] b: Stage) {
    assert!(matches!(
        Index::new(
            crate::ObjectFormat::Sha1,
            vec![entry(b"a", a), entry(b"a", b)],
            Limits::default()
        ),
        Err(Error::Entry { .. })
    ));
}
#[test]
fn sorts_partial_conflict_stages() {
    let index = Index::new(
        crate::ObjectFormat::Sha1,
        vec![entry(b"a", Stage::Theirs), entry(b"a", Stage::Base)],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        index.entries().iter().map(|e| e.stage).collect::<Vec<_>>(),
        [Stage::Base, Stage::Theirs]
    );
    assert_eq!(
        Index::parse(
            crate::ObjectFormat::Sha1,
            &index.encode(Limits::default()).unwrap(),
            Limits::default()
        )
        .unwrap(),
        index
    );
}
#[rstest]
#[case::same_stage(Stage::Ours, Stage::Ours, false)]
#[case::normal_parent(Stage::Normal, Stage::Theirs, false)]
#[case::normal_child(Stage::Base, Stage::Normal, false)]
#[case::directory_file_conflict(Stage::Ours, Stage::Theirs, true)]
fn validates_prefix_relationships(
    #[case] parent: Stage,
    #[case] child: Stage,
    #[case] valid: bool,
) {
    let result = Index::new(
        crate::ObjectFormat::Sha1,
        vec![
            entry(b"a", parent),
            entry(b"a-b", Stage::Normal),
            entry(b"a/b", child),
        ],
        Limits::default(),
    );
    assert_eq!(result.is_ok(), valid);
}

#[rstest]
#[case::signature(0, b'X')]
#[case::name_length(73, 2)]
#[case::padding(75, 1)]
#[case::extended_flags(72, 0x40)]
#[case::mode(39, 0)]
#[case::path(74, b'/')]
#[case::count(11, 2)]
fn rejects_resigned_corruption(#[case] offset: usize, #[case] value: u8) {
    let mut bytes = encoded();
    bytes[offset] = value;
    resign(&mut bytes);
    assert!(Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).is_err());
}
#[rstest]
#[case::v1(1)]
#[case::unknown(99)]
fn rejects_versions(#[case] version: u32) {
    let mut bytes = encoded();
    bytes[4..8].copy_from_slice(&version.to_be_bytes());
    resign(&mut bytes);
    assert_eq!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Version(version))
    );
}
#[rstest]
#[case::empty(0)]
#[case::header(11)]
#[case::checksum(31)]
#[case::entry(63)]
#[case::one_byte(95)]
fn rejects_truncated_input(#[case] length: usize) {
    let mut bytes = encoded();
    bytes.truncate(length);
    assert!(Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).is_err());
}
#[test]
fn rejects_checksum_damage() {
    let mut bytes = encoded();
    bytes[12] ^= 1;
    assert_eq!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Checksum)
    );
}
#[rstest]
#[case::split(b"link")]
#[case::sparse(b"sdir")]
#[case::unknown(b"abcd")]
fn rejects_mandatory_extensions(#[case] signature: &[u8; 4]) {
    assert_eq!(
        Index::parse(
            crate::ObjectFormat::Sha1,
            &extension(signature, b"opaque"),
            Limits::default()
        ),
        Err(Error::MandatoryExtension(*signature))
    );
}
#[rstest]
#[case::unknown(b"ABCD")]
fn opaque_extensions_roundtrip_and_block_edits(#[case] signature: &[u8; 4]) {
    let bytes = extension(signature, b"opaque\0\xff");
    let mut index = Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    let original = index.clone();
    index
        .replace_entries(index.entries().to_vec(), Limits::default())
        .unwrap();
    assert_eq!(index.encode(Limits::default()).unwrap(), bytes);
    assert_eq!(index.extensions()[0].data(), b"opaque\0\xff");
    assert_eq!(
        index.replace_entries(Vec::new(), Limits::default()),
        Err(Error::ExtensionPreventsEdit(*signature))
    );
    assert_eq!(index, original);
}
#[test]
fn changed_entries_invalidate_tree_cache() {
    let mut index = Index::parse(
        crate::ObjectFormat::Sha1,
        &extension(b"TREE", b"opaque"),
        Limits::default(),
    )
    .unwrap();
    index
        .replace_entries(Vec::new(), Limits::default())
        .unwrap();
    assert!(index.extensions().is_empty());
}
#[rstest]
#[case::bytes(Limits { max_bytes: 95, ..Limits::default() })]
#[case::entries(Limits { max_entries: 0, ..Limits::default() })]
#[case::path(Limits { max_path_bytes: 0, ..Limits::default() })]
fn bounded_parse_and_encode(#[case] limits: Limits) {
    let bytes = encoded();
    let index = Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, limits),
        Err(Error::Limit(_))
    ));
    assert!(matches!(index.encode(limits), Err(Error::Limit(_))));
}
#[test]
fn exact_limits_succeed() {
    let limits = Limits {
        max_bytes: 96,
        max_entries: 1,
        max_path_bytes: 1,
        max_extensions: 0,
    };
    let bytes = encoded();
    assert_eq!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, limits)
            .unwrap()
            .encode(limits)
            .unwrap(),
        bytes
    );
}
#[test]
fn extension_count_is_bounded() {
    let limits = Limits {
        max_extensions: 0,
        ..Limits::default()
    };
    assert_eq!(
        Index::parse(crate::ObjectFormat::Sha1, &extension(b"TREE", b""), limits),
        Err(Error::Limit("extensions"))
    );
}
#[test]
fn failed_edit_preserves_entries_and_extensions() {
    let mut index = Index::parse(
        crate::ObjectFormat::Sha1,
        &extension(b"TREE", b"opaque"),
        Limits::default(),
    )
    .unwrap();
    let before = index.clone();
    assert!(
        index
            .replace_entries(vec![entry(b"..", Stage::Normal)], Limits::default())
            .is_err()
    );
    assert_eq!(index, before);
}

#[rstest]
#[case::short_entry(63)]
#[case::short_padding(64)]
fn rejects_resigned_truncated_entry(#[case] payload_length: usize) {
    let mut bytes = encoded();
    bytes.truncate(payload_length);
    bytes.extend_from_slice(&[0; 20]);
    resign(&mut bytes);
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Malformed { .. })
    ));
}

#[rstest]
#[case::header(4)]
#[case::payload(9)]
fn rejects_resigned_truncated_extension(#[case] extension_length: usize) {
    let mut bytes = extension(b"ABCD", b"data");
    bytes.truncate(76 + extension_length);
    bytes.extend_from_slice(&[0; 20]);
    resign(&mut bytes);
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Malformed { .. })
    ));
}

#[rstest]
#[case::unsorted(b'b', b'a', 0)]
#[case::duplicate(b'a', b'a', 0)]
#[case::normal_conflict(b'a', b'a', 0x20)]
fn parsing_never_repairs_order_or_stages(#[case] first: u8, #[case] second: u8, #[case] flags: u8) {
    let index = Index::new(
        crate::ObjectFormat::Sha1,
        vec![entry(b"a", Stage::Normal), entry(b"b", Stage::Normal)],
        Limits::default(),
    )
    .unwrap();
    let mut bytes = index.encode(Limits::default()).unwrap();
    bytes[74] = first;
    bytes[138] = second;
    bytes[136] = flags;
    resign(&mut bytes);
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Entry { entry: 1, .. })
    ));
}

#[test]
fn huge_declared_entry_count_fails_before_allocation() {
    let mut bytes = encoded();
    bytes[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    resign(&mut bytes);
    let limits = Limits {
        max_entries: usize::MAX,
        ..Limits::default()
    };
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, limits),
        Err(Error::Malformed { offset: 8, .. })
    ));
}

#[test]
fn huge_declared_extension_length_fails_before_allocation() {
    let mut bytes = extension(b"ABCD", b"");
    bytes[80..84].copy_from_slice(&u32::MAX.to_be_bytes());
    resign(&mut bytes);
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Malformed { .. })
    ));
}

#[test]
fn optional_extension_order_is_preserved() {
    let mut bytes = extension(b"TREE", b"a");
    bytes.truncate(bytes.len() - 20);
    bytes.extend_from_slice(b"REUC\0\0\0\x01b");
    bytes.extend_from_slice(&[0; 20]);
    resign(&mut bytes);
    let index = Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    assert_eq!(
        index
            .extensions()
            .iter()
            .map(Extension::signature)
            .collect::<Vec<_>>(),
        [*b"TREE", *b"REUC"]
    );
    assert_eq!(index.encode(Limits::default()).unwrap(), bytes);
}

#[rstest]
#[case::tree(b"TREE", false)]
#[case::untracked(b"UNTR", false)]
#[case::fsmonitor(b"FSMN", false)]
#[case::end_offsets(b"EOIE", false)]
#[case::entry_offsets(b"IEOT", false)]
#[case::resolve_undo(b"REUC", true)]
fn edits_apply_known_extension_policy(#[case] signature: &[u8; 4], #[case] retain: bool) {
    let bytes = extension(signature, b"opaque\0\xff");
    let mut index = Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    index.replace_entries(vec![], Limits::default()).unwrap();
    assert_eq!(index.extensions().len(), usize::from(retain));
    let encoded = index.encode(Limits::default()).unwrap();
    let parsed = Index::parse(crate::ObjectFormat::Sha1, &encoded, Limits::default()).unwrap();
    assert_eq!(parsed, index);
}

#[rstest]
#[case::v3_sha1(crate::ObjectFormat::Sha1, Version::V3)]
#[case::v3_sha256(crate::ObjectFormat::Sha256, Version::V3)]
#[case::v4_sha1(crate::ObjectFormat::Sha1, Version::V4)]
#[case::v4_sha256(crate::ObjectFormat::Sha256, Version::V4)]
fn extended_flags_conflicts_and_byte_paths(
    #[case] format: crate::ObjectFormat,
    #[case] version: Version,
) {
    let mut entry = Entry::new(
        b"directory/\xff".to_vec(),
        Mode::Executable,
        ObjectId::null(format),
    );
    entry.intent_to_add = true;
    entry.skip_worktree = true;
    entry.assume_valid = true;
    entry.stage = Stage::Theirs;
    entry.stat.size = u32::MAX;
    let mut index = Index::new(format, vec![entry.clone()], Limits::default()).unwrap();
    index.set_version(version, Limits::default()).unwrap();
    let bytes = index.encode(Limits::default()).unwrap();
    let mut parsed = Index::parse(format, &bytes, Limits::default()).unwrap();
    assert_eq!(parsed.entries(), &[entry]);
    assert_eq!(parsed.version(), version);
    assert_eq!(parsed.encode(Limits::default()).unwrap(), bytes);
    assert!(parsed.set_version(Version::V2, Limits::default()).is_err());
    assert_eq!(parsed.encode(Limits::default()).unwrap(), bytes);
}

#[rstest]
#[case::v3(Version::V3)]
#[case::v4(Version::V4)]
fn rejects_unknown_extended_flag_bits(#[case] version: Version) {
    let mut entry = entry(b"a", Stage::Normal);
    entry.intent_to_add = true;
    let mut index = Index::new(crate::ObjectFormat::Sha1, vec![entry], Limits::default()).unwrap();
    index.set_version(version, Limits::default()).unwrap();
    let mut bytes = index.encode(Limits::default()).unwrap();
    bytes[74] |= 0x80;
    resign(&mut bytes);
    assert!(matches!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()),
        Err(Error::Entry { .. })
    ));
}

#[rstest]
#[case::remove_too_much(&[1, b'a', 0])]
#[case::truncated_integer(&[128])]
#[case::overflow(&[255; 16])]
#[case::unterminated_suffix(&[0, b'a'])]
fn rejects_malformed_v4_paths(#[case] path: &[u8]) {
    let mut bytes = encoded();
    bytes[4..8].copy_from_slice(&4u32.to_be_bytes());
    bytes.truncate(74);
    bytes.extend_from_slice(path);
    bytes.extend_from_slice(&[0; 20]);
    resign(&mut bytes);
    assert!(Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).is_err());
}

#[test]
fn preserves_nonmaximal_v4_compression_until_edit() {
    let mut bytes = encoded();
    bytes[4..8].copy_from_slice(&4u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&2u32.to_be_bytes());
    bytes.truncate(74);
    bytes.extend_from_slice(&[0, b'a', 0]);
    let fixed = bytes[12..74].to_vec();
    bytes.extend_from_slice(&fixed);
    // Remove the whole previous name rather than sharing its 'a'.
    bytes[137..139].copy_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&[1, b'a', b'b', 0]);
    bytes.extend_from_slice(&[0; 20]);
    resign(&mut bytes);
    let mut index = Index::parse(crate::ObjectFormat::Sha1, &bytes, Limits::default()).unwrap();
    assert_eq!(index.entries()[1].path, b"ab");
    assert_eq!(index.encode(Limits::default()).unwrap(), bytes);
    index
        .replace_entries(index.entries().to_vec(), Limits::default())
        .unwrap();
    assert_eq!(index.encode(Limits::default()).unwrap(), bytes);
    let short = Limits {
        max_path_bytes: 1,
        ..Limits::default()
    };
    assert_eq!(
        Index::parse(crate::ObjectFormat::Sha1, &bytes, short),
        Err(Error::Limit("path bytes"))
    );
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn compressed_edit_limits_use_actual_version(#[case] format: crate::ObjectFormat) {
    let prefix = vec![b'a'; 200];
    let mut first = prefix.clone();
    first.push(b'1');
    let mut second = prefix;
    second.push(b'2');
    let entries = vec![
        Entry::new(first, Mode::Regular, ObjectId::null(format)),
        Entry::new(second, Mode::Regular, ObjectId::null(format)),
    ];
    let mut index = Index::new(format, entries, Limits::default()).unwrap();
    let padded_size = index.encode(Limits::default()).unwrap().len();
    index.set_version(Version::V4, Limits::default()).unwrap();
    let compressed_size = index.encode(Limits::default()).unwrap().len();
    assert!(compressed_size < padded_size);
    let limits = Limits {
        max_bytes: compressed_size,
        ..Limits::default()
    };
    index
        .replace_entries(index.entries().to_vec(), limits)
        .unwrap();
    let mut entries = index.entries().to_vec();
    entries[0].assume_valid = true;
    index.replace_entries(entries, limits).unwrap();
    assert_eq!(index.encode(limits).unwrap().len(), compressed_size);
    let before = index.clone();
    assert_eq!(
        index.set_version(Version::V2, limits),
        Err(Error::Limit("bytes"))
    );
    assert_eq!(index, before);
}
