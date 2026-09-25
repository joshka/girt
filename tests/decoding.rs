//! Original executable-Git differential fixtures for tolerant object reading.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;

use girt::{
    Commit, CommitError, CommitPayload, EntryMode, HistoryLimits, InitKind, ObjectFormat, ObjectId,
    ObjectKind, PackLimits, PeelFailure, PeelLimits, Repository, Tag, TagFields,
};
use rstest::rstest;

fn git(path: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn fixture(format: ObjectFormat, kind: &str, name: &str) -> Vec<u8> {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../docs/evidence/r07-git-observations.json")).unwrap();
    let case = corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["format"] == format.to_string() && c["kind"] == kind && c["name"] == name)
        .unwrap();
    unhex(case["payload"].as_str().unwrap())
}
fn repo(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, dir.path().join("repo"), InitKind::Bare).unwrap();
    (dir, repo)
}
fn store(repo: &Repository, kind: &str, bytes: &[u8]) -> ObjectId {
    let out = git(
        repo.git_dir(),
        &["hash-object", "-w", "--literally", "-t", kind, "--stdin"],
        bytes,
    );
    assert!(out.status.success());
    std::str::from_utf8(&out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[rstest]
#[case::canonical("canonical")]
#[case::missing_author("missing_author")]
#[case::missing_committer("missing_committer")]
#[case::reordered_people("reordered_people")]
#[case::duplicate_tree("duplicate_tree")]
#[case::duplicate_author("duplicate_author")]
#[case::duplicate_committer("duplicate_committer")]
#[case::folded_tree("folded_tree")]
#[case::folded_author("folded_author")]
#[case::missing_space("missing_space")]
#[case::tab("tab")]
#[case::orphan("orphan")]
#[case::no_separator("no_separator")]
#[case::truncated_person("truncated_person")]
#[case::short_parent("short_parent")]
#[case::truncated_parent("truncated_parent")]
#[case::late_parent("late_parent")]
#[case::missing_date("date_missing")]
#[case::overflow_date("date_overflow")]
#[case::bad_date("date_bad")]
#[case::noncanonical_hours("date_hours")]
#[case::noncanonical_minutes("date_minutes")]
#[case::short_zone("date_short")]
#[case::unsigned_zone("date_unsigned")]
#[case::negative_date("date_negative")]
#[case::identity_whitespace("date_space")]
#[case::trailing_date("date_trailing")]
#[case::tab_date("date_tab_date")]
#[case::no_date_space("date_nodate_space")]
fn commit_graph_and_storage_match_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
) {
    let bytes = fixture(format, "commit", name);
    let (_dir, repo) = repo(format);
    let id = store(&repo, "commit", &bytes);
    let decoded = repo.loose_objects().read_commit(id, bytes.len()).unwrap();
    let graph = git(
        repo.git_dir(),
        &["rev-list", "--parents", &id.to_string()],
        &[],
    );
    assert!(
        graph.status.success(),
        "{}",
        String::from_utf8_lossy(&graph.stderr)
    );
    assert_eq!(graph.stdout, format!("{id}\n").as_bytes());
    assert_eq!(decoded.id(), id);
    assert_eq!(decoded.encode(), bytes);
    assert_eq!(
        repo.objects(PackLimits::default())
            .unwrap()
            .walk(&[id], HistoryLimits::default())
            .unwrap(),
        [id]
    );
}

#[rstest]
#[case::missing("date_missing")]
#[case::bad("date_bad")]
#[case::overflow("date_overflow")]
#[case::unsigned("date_unsigned")]
#[case::seconds_suffix("date_badseconds")]
fn date_errors_preserve_usable_identity(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
) {
    let bytes = fixture(format, "commit", name);
    let commit = Commit::parse(format, &bytes).unwrap();
    let person = commit.author().unwrap().unwrap();
    assert_eq!(
        (person.name, person.email),
        (b"A".as_slice(), b"a".as_slice())
    );
    assert_eq!(person.date(), Err(CommitError::InvalidDate));
    assert_eq!(commit.to_fields(), Err(CommitError::InvalidDate));
    assert_eq!(commit.encode(), bytes);
}

#[rstest]
#[case::hours("date_hours", 1, 1440)]
#[case::minutes("date_minutes", 1, -60)]
#[case::short("date_short", 1, 1)]
#[case::suffix("date_suffix_zone", 1, 1)]
#[case::adjacent("date_adjacent_zone", 1, 60)]
#[case::negative("date_negative", -1, 0)]
#[case::tab("date_tab_date", 1, 0)]
#[case::no_space("date_nodate_space", 1, 0)]
#[case::duplicate("duplicate_author", 3, 60)]
fn dates_and_last_author_have_explicit_interpretation(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
    #[case] seconds: i64,
    #[case] offset: i16,
) {
    let bytes = fixture(format, "commit", name);
    let commit = Commit::parse(format, &bytes).unwrap();
    let date = commit.author().unwrap().unwrap().date().unwrap();
    assert_eq!((date.seconds, date.offset_minutes), (seconds, offset));
}

#[rstest]
#[case::mode("100664", EntryMode::Blob, "100644 blob")]
#[case::permissions("100600", EntryMode::Blob, "100644 blob")]
#[case::owner_execute("100744", EntryMode::Executable, "100755 blob")]
#[case::padded("0100644", EntryMode::Blob, "100644 blob")]
#[case::tree("040000", EntryMode::Tree, "040000 tree")]
#[case::tree_permissions("40001", EntryMode::Tree, "040000 tree")]
#[case::symlink("120777", EntryMode::Symlink, "120000 blob")]
#[case::gitlink("160001", EntryMode::Gitlink, "160000 commit")]
#[case::overflow("100000000000", EntryMode::Gitlink, "160000 commit")]
#[case::huge("777777777777777777777777", EntryMode::Gitlink, "160000 commit")]
#[case::padding("0000000000000000000000100644", EntryMode::Blob, "100644 blob")]
#[case::zero("0", EntryMode::Gitlink, "160000 commit")]
#[case::unknown("777777", EntryMode::Gitlink, "160000 commit")]
fn historical_tree_modes_match_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
    #[case] mode: EntryMode,
    #[case] display: &str,
) {
    let bytes = fixture(format, "tree", name);
    let (_dir, repo) = repo(format);
    let id = store(&repo, "tree", &bytes);
    let tree = repo.loose_objects().read_tree(id, bytes.len()).unwrap();
    assert_eq!(tree.entries()[0].mode, mode);
    assert_eq!(tree.encode(), bytes);
    assert_eq!(tree.id(), id);
    let read = git(repo.git_dir(), &["ls-tree", &id.to_string()], &[]);
    assert!(read.status.success());
    assert!(read.stdout.starts_with(display.as_bytes()));
    assert!(
        !git(
            repo.git_dir(),
            &["hash-object", "-t", "tree", "--stdin"],
            &bytes
        )
        .status
        .success()
    );
}

fn tag(repo: &Repository, target: ObjectId, kind: ObjectKind) -> ObjectId {
    let tag = Tag::new(TagFields {
        target,
        target_kind: kind,
        name: b"v".to_vec(),
        tagger: None,
        extra_headers: vec![],
        message: b"signed bytes\xff".to_vec(),
    })
    .unwrap();
    repo.loose_objects().write_tag(&tag).unwrap()
}

#[rstest]
#[case::blob(ObjectKind::Blob, "terminal")]
#[case::tree(ObjectKind::Tree, "")]
#[case::commit(ObjectKind::Commit, "tree {tree}\nauthor A <a> now +0000\n\n")]
fn nested_peeling_preserves_identity_and_matches_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] kind: ObjectKind,
    #[case] template: &str,
) {
    let (_dir, repo) = repo(format);
    let bytes = template
        .replace("{tree}", &ObjectId::null(format).to_string())
        .into_bytes();
    let terminal = store(&repo, kind.as_str(), &bytes);
    let inner = tag(&repo, terminal, kind);
    let outer = tag(&repo, inner, ObjectKind::Tag);
    let objects = repo.objects(PackLimits::default()).unwrap();
    let result = objects
        .peel(outer, PeelLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(result.original, outer);
    assert_eq!(result.tags, [outer, inner]);
    assert_eq!((result.target, result.kind), (terminal, kind));
    let git = git(
        repo.git_dir(),
        &["rev-parse", &format!("{outer}^{{}}")],
        &[],
    );
    assert!(git.status.success());
    assert_eq!(git.stdout, format!("{terminal}\n").as_bytes());
}

#[rstest]
fn peeling_failures_identify_the_referring_tag(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_dir, repo) = repo(format);
    let missing = ObjectId::for_blob(format, b"absent");
    let root = tag(&repo, missing, ObjectKind::Blob);
    let objects = repo.objects(PackLimits::default()).unwrap();
    let error = objects
        .peel(root, PeelLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(
        (error.original, error.id, error.from, error.depth),
        (root, missing, Some(root), 1)
    );
    assert!(matches!(*error.source, PeelFailure::Missing));
    let blob = store(&repo, "blob", b"exists");
    let wrong = tag(&repo, blob, ObjectKind::Tree);
    assert!(matches!(
        *objects
            .peel(wrong, PeelLimits::default(), &AtomicBool::new(false))
            .unwrap_err()
            .source,
        PeelFailure::Kind {
            expected: ObjectKind::Tree,
            actual: ObjectKind::Blob
        }
    ));
    assert!(matches!(
        *objects
            .peel(
                root,
                PeelLimits {
                    max_tags: 0,
                    ..PeelLimits::default()
                },
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .source,
        PeelFailure::Depth
    ));
    assert!(matches!(
        *objects
            .peel(root, PeelLimits::default(), &AtomicBool::new(true))
            .unwrap_err()
            .source,
        PeelFailure::Cancelled
    ));
}

#[rstest]
fn legacy_signature_removal_is_exact(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let prefix = format!(
        "tree {}\nauthor A <a> now +0000\nopaque\n",
        ObjectId::null(format)
    );
    let bytes = [
        prefix.as_bytes(),
        b"gpgsig one\n folded\ngpgsig-sha256 two\n last",
    ]
    .concat();
    let payload = CommitPayload::from_bytes(&bytes);
    assert_eq!(
        payload.headers().nth(3).unwrap().unfolded_value(),
        b"one\nfolded"
    );
    assert_eq!(
        payload.without_headers(&[3]).unwrap(),
        [prefix.as_bytes(), b"gpgsig-sha256 two\n last"].concat()
    );
    assert_eq!(payload.without_headers(&[3, 4]).unwrap(), prefix.as_bytes());
    assert_eq!(payload.as_bytes(), bytes);
    assert_eq!(payload.without_headers(&[5]), None);
}

#[rstest]
#[case::canonical("canonical", true)]
#[case::legacy("no_separator", true)]
#[case::truncated("no_lf", false)]
#[case::missing_name("missing_name", false)]
#[case::bad_tagger("bad_tagger", true)]
#[case::duplicate("duplicate_type", true)]
#[case::reordered("reordered", false)]
#[case::folded("folded", true)]
#[case::folded_object("folded_object", false)]
#[case::folded_type("folded_type", false)]
#[case::missing_type("missing_type", false)]
#[case::missing_object("missing_object", false)]
#[case::opaque("opaque", true)]
#[case::truncated_extra("truncated_extra", true)]
#[case::duplicate_tagger("duplicate_tagger", true)]
fn tag_reading_matches_git_peeling(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
    #[case] accepted: bool,
) {
    let (_dir, repo) = repo(format);
    store(&repo, "blob", b"x");
    let bytes = fixture(format, "tag", name);
    let id = store(&repo, "tag", &bytes);
    let result = repo.objects(PackLimits::default()).unwrap().peel(
        id,
        PeelLimits::default(),
        &AtomicBool::new(false),
    );
    assert_eq!(result.is_ok(), accepted);
    let git = git(
        repo.git_dir(),
        &["rev-parse", "--verify", &format!("{id}^{{}}")],
        &[],
    );
    assert_eq!(git.status.success(), accepted);
}

#[rstest]
fn transfer_graph_accepts_legacy_metadata_and_unsorted_trees(
    #[values(ObjectFormat::Sha1)] format: ObjectFormat,
) {
    // SHA-256 negotiation is deliberately still owned by R26; the codecs and history are
    // dual-format.
    let (_dir, repo) = repo(format);
    let blob = store(&repo, "blob", b"x");
    let tree_bytes = [
        b"100664 z\0".as_slice(),
        blob.as_bytes(),
        b"100600 a\0",
        blob.as_bytes(),
    ]
    .concat();
    let tree = store(&repo, "tree", &tree_bytes);
    let bytes = format!("tree {tree}\nauthor A <a> nonsense\n\n").into_bytes();
    let commit = store(&repo, "commit", &bytes);
    let root = tag(&repo, commit, ObjectKind::Commit);
    let objects = repo.objects(PackLimits::default()).unwrap();
    let known = girt::fetch::KnownHistory::new(
        &objects,
        &[root],
        girt::fetch::FetchLimits::default(),
        &AtomicBool::new(false),
    );
    assert!(known.is_ok());
    let read = git(repo.git_dir(), &["ls-tree", &tree.to_string()], &[]);
    assert!(read.status.success());
    assert!(
        read.stdout
            .starts_with(format!("100644 blob {blob}\tz\n").as_bytes())
    );
    assert!(
        girt::Tree::parse(format, &tree_bytes)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[rstest]
fn peeling_checks_terminal_syntax_and_exact_limits(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_dir, repo) = repo(format);
    let blob = store(&repo, "blob", b"x");
    let root = tag(&repo, blob, ObjectKind::Blob);
    let length = repo
        .loose_objects()
        .read_tag(root, 4096)
        .unwrap()
        .as_bytes()
        .len()
        + 1;
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(
        objects
            .peel(
                root,
                PeelLimits {
                    max_tags: 1,
                    max_bytes: length,
                    ..PeelLimits::default()
                },
                &AtomicBool::new(false)
            )
            .is_ok()
    );
    assert!(
        objects
            .peel(
                root,
                PeelLimits {
                    max_tags: 1,
                    max_bytes: length - 1,
                    ..PeelLimits::default()
                },
                &AtomicBool::new(false)
            )
            .is_err()
    );
    let broken = store(&repo, "commit", b"invalid");
    let tag = tag(&repo, broken, ObjectKind::Commit);
    let error = objects
        .peel(tag, PeelLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!((error.id, error.from), (broken, Some(tag)));
    assert!(matches!(*error.source, PeelFailure::Commit(_)));
    let path = repo
        .object_dir()
        .join(&blob.to_string()[..2])
        .join(&blob.to_string()[2..]);
    replace_with_corrupt(&path);
    let error = objects
        .peel(root, PeelLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!((error.id, error.from), (blob, Some(root)));
    assert!(matches!(*error.source, PeelFailure::Read(_)));
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1, b"gpgsig")]
#[case::sha256(ObjectFormat::Sha256, b"gpgsig-sha256")]
fn signature_bytes_match_captured_git_provider_input(
    #[case] format: ObjectFormat,
    #[case] key: &[u8],
    #[values("both", "repeated", "legacy")] name: &str,
) {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../docs/evidence/r07-git-observations.json")).unwrap();
    let probe = &corpus["signatures"][format.to_string()][name];
    let bytes = unhex(probe["payload"].as_str().unwrap());
    let view = CommitPayload::from_bytes(&bytes);
    let indices: Vec<_> = view
        .headers()
        .enumerate()
        .filter(|(_, h)| h.has_value_separator && matches!(h.name, b"gpgsig" | b"gpgsig-sha256"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        view.without_headers(&indices).unwrap(),
        unhex(probe["captured"]["payload"].as_str().unwrap())
    );
    let signatures: Vec<_> = view
        .headers()
        .filter(|h| h.has_value_separator && h.name == key)
        .flat_map(|h| {
            let mut value = h.unfolded_value();
            value.push(b'\n');
            value
        })
        .collect();
    assert_eq!(
        signatures,
        unhex(probe["captured"]["signature"].as_str().unwrap())
    );
    assert_eq!(view.as_bytes(), bytes);
}

#[rstest]
fn metadata_absence_and_raw_identity_are_independent_of_graph_decoding(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let bytes = format!("tree {}\n\n", ObjectId::null(format));
    let commit = Commit::parse(format, bytes.as_bytes()).unwrap();
    assert_eq!(commit.author(), Ok(None));
    assert_eq!(commit.committer(), Ok(None));
    assert_eq!(commit.to_fields(), Err(CommitError::RequiredHeader));
    let raw = [
        format!("tree {}\n", ObjectId::null(format)).as_bytes(),
        b"author  A\xff \t<a\xfe> now\ncommitter not-an-identity\n\n",
    ]
    .concat();
    let commit = Commit::parse(format, &raw).unwrap();
    let author = commit.author().unwrap().unwrap();
    assert_eq!(
        (author.name, author.email, author.date_bytes),
        (
            b" A\xff".as_slice(),
            b"a\xfe".as_slice(),
            b" now".as_slice()
        )
    );
    assert_eq!(author.date(), Err(CommitError::InvalidDate));
    assert_eq!(commit.committer(), Err(CommitError::InvalidSignature));
    assert_eq!(commit.encode(), raw);
}

#[rstest]
fn peeling_leaves_terminal_tree_records_for_the_tree_reader(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_dir, repo) = repo(format);
    let tree = store(&repo, "tree", b"invalid");
    let root = tag(&repo, tree, ObjectKind::Tree);
    let peeled = repo
        .objects(PackLimits::default())
        .unwrap()
        .peel(root, PeelLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!((peeled.target, peeled.kind), (tree, ObjectKind::Tree));
    let observed = git(
        repo.git_dir(),
        &["rev-parse", "--verify", &format!("{root}^{{}}")],
        &[],
    );
    assert!(observed.status.success());
    assert_eq!(observed.stdout, format!("{tree}\n").as_bytes());
    assert!(repo.loose_objects().read_tree(tree, 1024).is_err());
}

fn replace_with_corrupt(path: &Path) {
    #[cfg(windows)]
    {
        // Git can create read-only objects. Use a fresh writable file's Windows permissions
        // for this deliberately corrupt fixture without broadening Unix mode bits.
        let writable = tempfile::tempfile().unwrap();
        let permissions = writable.metadata().unwrap().permissions();
        std::fs::set_permissions(path, permissions).unwrap();
    }
    std::fs::remove_file(path).unwrap();
    std::fs::write(path, b"corrupt").unwrap();
}

#[test]
fn bare_signature_spelling_is_not_a_framed_signature() {
    let bytes = b"gpgsig\ngpgsig \n continuation";
    let view = CommitPayload::from_bytes(bytes);
    let bare = view.headers().next().unwrap();
    let framed = view.headers().nth(1).unwrap();
    assert_eq!(bare.name, b"gpgsig");
    assert!(!bare.has_value_separator);
    assert!(framed.has_value_separator);
    assert_eq!(framed.unfolded_value(), b"\ncontinuation");
    assert_eq!(view.without_headers(&[1]).unwrap(), b"gpgsig\n");
}

#[rstest]
fn unsorted_tree_reading_preserves_record_order_in_both_formats(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_dir, repo) = repo(format);
    let id = store(&repo, "blob", b"x");
    let bytes = [
        b"100644 z\0".as_slice(),
        id.as_bytes(),
        b"100644 a\0",
        id.as_bytes(),
    ]
    .concat();
    let tree_id = store(&repo, "tree", &bytes);
    let tree = repo
        .loose_objects()
        .read_tree(tree_id, bytes.len())
        .unwrap();
    assert_eq!(tree.encode(), bytes);
    assert_eq!(tree.entries()[0].name, b"z");
    assert_eq!(tree.validate(), Err(girt::TreeError::Unsorted));
    let read = git(repo.git_dir(), &["ls-tree", &tree_id.to_string()], &[]);
    assert!(read.status.success());
    assert_eq!(
        read.stdout,
        format!("100644 blob {id}\tz\n100644 blob {id}\ta\n").as_bytes()
    );
    let fsck = git(
        repo.git_dir(),
        &["fsck", "--strict", &tree_id.to_string()],
        &[],
    );
    assert!(!fsck.status.success());
    assert!(String::from_utf8_lossy(&fsck.stderr).contains("treeNotSorted"));
}

#[rstest]
#[case::duplicate("duplicate_tagger")]
#[case::misplaced("misplaced_tagger")]
fn first_tagger_matches_git_metadata(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
) {
    let (_dir, repo) = repo(format);
    let bytes = fixture(format, "tag", name);
    let id = store(&repo, "tag", &bytes);
    let tag = Tag::parse(format, &bytes).unwrap();
    let tagger = tag.tagger().unwrap().unwrap();
    assert_eq!(
        (tagger.name, tagger.email, tagger.date().unwrap().seconds),
        (b"A".as_slice(), b"a".as_slice(), 1)
    );
    assert!(
        git(
            repo.git_dir(),
            &["update-ref", "refs/tags/probe", &id.to_string()],
            &[]
        )
        .status
        .success()
    );
    let out = git(
        repo.git_dir(),
        &[
            "for-each-ref",
            "--format=%(taggername)|%(taggeremail)|%(taggerdate:raw)",
            "refs/tags/probe",
        ],
        &[],
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"A|<a>|1 +0000\n");
    assert_eq!(tag.encode(), bytes);
}

#[rstest]
#[case::only_tree("tree_only")]
#[case::only_parent("parent_only")]
#[case::unterminated_tree("tree_no_lf")]
#[case::misplaced_tree("reordered_tree")]
fn unreadable_graph_records_fail_independently_of_raw_storage(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] name: &str,
) {
    let (_dir, repo) = repo(format);
    let bytes = fixture(format, "commit", name);
    let id = store(&repo, "commit", &bytes);
    assert!(Commit::parse(format, &bytes).is_err());
    assert!(repo.loose_objects().read_commit(id, bytes.len()).is_err());
    assert!(
        !git(repo.git_dir(), &["rev-list", &id.to_string()], &[])
            .status
            .success()
    );
    assert_eq!(
        git(
            repo.git_dir(),
            &["cat-file", "commit", &id.to_string()],
            &[]
        )
        .stdout,
        bytes
    );
}
