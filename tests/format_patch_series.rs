//! Complete series/cover bytes compared with original isolated Git fixtures.
#[path = "support/apply_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use girt::format_patch::{CoverLetter, Numbering, Options, SeriesLimits, SeriesOptions};
use girt::{InitKind, ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

struct Fixture {
    _temporary: tempfile::TempDir,
    repo: Repository,
    ids: Vec<ObjectId>,
}

impl Fixture {
    fn new(format: ObjectFormat, count: usize) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
        let root = repo.worktree().unwrap();
        std::fs::write(root.join("file"), b"base\n").unwrap();
        oracle::git(root, &["add", "file"], b"");
        oracle::git(root, &["commit", "-qm", "Base"], b"");
        let mut ids = Vec::new();
        for number in 1..=count {
            std::fs::write(root.join("file"), format!("revision {number}\n")).unwrap();
            oracle::git(root, &["add", "file"], b"");
            oracle::git(
                root,
                &[
                    "commit",
                    "-qm",
                    &format!("Change {number}\n\nBody {number}"),
                    "--author",
                    fixture_author(number),
                ],
                b"",
            );
            let id = oracle::git(root, &["rev-parse", "HEAD"], b"");
            ids.push(std::str::from_utf8(&id).unwrap().trim().parse().unwrap());
        }
        Self {
            _temporary: temporary,
            repo,
            ids,
        }
    }

    fn check_latest(&self, options: SeriesOptions<'_>, flags: &[&str]) {
        let root = self.repo.worktree().unwrap();
        let id = oracle::git(root, &["rev-parse", "HEAD"], b"");
        let id = std::str::from_utf8(&id).unwrap().trim().parse().unwrap();
        let actual = self
            .repo
            .objects()
            .unwrap()
            .format_patch_with(
                &[id],
                options,
                None,
                SeriesLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap()
            .into_mbox();
        let mut args = vec![
            "-c",
            "core.compression=6",
            "format-patch",
            "--stdout",
            "--signature=Test Signature",
            "-1",
        ];
        args.extend_from_slice(flags);
        let expected = oracle::git(root, &args, b"");
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            String::from_utf8(expected).unwrap()
        );
    }

    fn cover_author(&self) -> Signature {
        self.repo
            .objects()
            .unwrap()
            .read_commit(*self.ids.last().unwrap())
            .unwrap()
            .to_fields()
            .unwrap()
            .committer
    }

    fn options(&self, numbering: Numbering) -> SeriesOptions<'static> {
        SeriesOptions::default()
            .with_mail(Options::default().with_signature(Some(b"Test Signature")))
            .with_numbering(numbering)
    }

    fn expected(&self, flags: &[&str], count: usize) -> Vec<u8> {
        let range = format!("HEAD~{count}..HEAD");
        let mut args = vec!["format-patch", "--stdout", "--signature=Test Signature"];
        args.extend_from_slice(flags);
        args.push(&range);
        oracle::git(self.repo.worktree().unwrap(), &args, b"")
    }
}

#[rstest]
#[case::auto(Numbering::Auto, &[])]
#[case::always(Numbering::Always, &["--numbered"])]
#[case::never(Numbering::Never, &["--no-numbered"])]
fn series_bytes_match_git(
    #[case] numbering: Numbering,
    #[case] flags: &[&str],
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(1, 2, 10)] count: usize,
) {
    let fixture = Fixture::new(format, count);
    let actual = fixture
        .repo
        .objects()
        .unwrap()
        .format_patch_with(
            &fixture.ids,
            fixture.options(numbering),
            None,
            SeriesLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_mbox();
    let expected = fixture.expected(flags, count);
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

#[rstest]
#[case::auto(Numbering::Auto, &[])]
#[case::always(Numbering::Always, &["--numbered"])]
#[case::never(Numbering::Never, &["--no-numbered"])]
fn cover_bytes_match_git(
    #[case] numbering: Numbering,
    #[case] flags: &[&str],
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(1, 2, 10)] count: usize,
) {
    let fixture = Fixture::new(format, count);
    let author = fixture.cover_author();
    let options = fixture.options(numbering).with_cover(Some(CoverLetter::new(
        &author,
        b"*** SUBJECT HERE ***\n\n*** BLURB HERE ***",
    )));
    let actual = fixture
        .repo
        .objects()
        .unwrap()
        .format_patch_with(
            &fixture.ids,
            options,
            None,
            SeriesLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_mbox();
    let mut flags = flags.to_vec();
    flags.push("--cover-letter");
    let expected = fixture.expected(&flags, count);
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn attributed_binary_counts_and_payload_match_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, 1);
    let root = fixture.repo.worktree().unwrap();
    std::fs::write(root.join(".gitattributes"), b"file binary\n").unwrap();
    let mut attributes = fixture
        .repo
        .diff_attributes(&girt::workflow::Environment::default())
        .unwrap();
    let options = fixture.options(Numbering::Auto);
    let actual = fixture
        .repo
        .objects()
        .unwrap()
        .format_patch_with(
            &fixture.ids,
            options,
            Some(&mut attributes),
            SeriesLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_mbox();
    let expected = oracle::git(
        root,
        &[
            "-c",
            "core.compression=6",
            "format-patch",
            "--stdout",
            "--signature=Test Signature",
            "-1",
        ],
        b"",
    );
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

#[rstest]
#[case::plain(false, &[])]
#[case::cover(true, &["--cover-letter"])]
fn base_is_emitted_only_on_the_first_artifact(
    #[case] with_cover: bool,
    #[case] cover_flags: &[&str],
) {
    let fixture = Fixture::new(ObjectFormat::Sha1, 2);
    let objects = fixture.repo.objects().unwrap();
    let first = objects.read_commit(fixture.ids[0]).unwrap();
    let author = fixture.cover_author();
    let cover = CoverLetter::new(&author, b"*** SUBJECT HERE ***\n\n*** BLURB HERE ***");
    let options = fixture.options(Numbering::Auto).with_mail(
        Options::default()
            .with_signature(Some(b"Test Signature"))
            .with_base(Some(first.parents()[0])),
    );
    let options = options.with_cover(with_cover.then_some(cover));
    let actual = objects
        .format_patch_with(
            &fixture.ids,
            options,
            None,
            SeriesLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_mbox();
    let base = format!("--base={}", first.parents()[0]);
    let mut flags = vec![base.as_str()];
    flags.extend_from_slice(cover_flags);
    let expected = fixture.expected(&flags, 2);
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

fn fixture_author(number: usize) -> &'static str {
    if number.is_multiple_of(2) {
        "Alpha <alpha@example.com>"
    } else {
        "Zeta <zeta@example.com>"
    }
}

#[test]
fn reversed_selection_is_refused() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 2);
    let ids = [fixture.ids[1], fixture.ids[0]];
    let result = fixture.repo.objects().unwrap().format_patch(&ids);
    assert!(matches!(
        result,
        Err(girt::format_patch::SeriesError::Nonlinear(_))
    ));
}

#[rstest]
#[case::count(SeriesLimits::default().with_max_commits(0))]
#[case::metadata(SeriesLimits::default().with_max_metadata_bytes(0))]
#[case::output(SeriesLimits::default().with_max_output_bytes(1))]
#[case::paths(SeriesLimits::default().with_stat(girt::diffstat::Limits::default().with_max_path_bytes(0)))]
fn aggregate_limits_return_no_series(#[case] limits: SeriesLimits) {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let result = fixture.repo.objects().unwrap().format_patch_with(
        &fixture.ids,
        fixture.options(Numbering::Auto),
        None,
        limits,
        &AtomicBool::new(false),
    );
    assert!(result.is_err());
}

#[test]
fn cancellation_precedes_reading_unknown_commits() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let unknown = ObjectId::for_blob(ObjectFormat::Sha1, b"not a stored commit");
    let result = fixture.repo.objects().unwrap().format_patch_with(
        &[unknown],
        fixture.options(Numbering::Auto),
        None,
        SeriesLimits::default(),
        &AtomicBool::new(true),
    );
    assert!(matches!(
        result,
        Err(girt::format_patch::SeriesError::Mail(
            girt::format_patch::Error::Cancelled
        ))
    ));
}

#[rstest]
#[case::plain(false, &[])]
#[case::cover(true, &["--cover-letter"])]
fn root_and_unsigned_mbox_match_git(#[case] with_cover: bool, #[case] flags: &[&str]) {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let objects = fixture.repo.objects().unwrap();
    let first = objects.read_commit(fixture.ids[0]).unwrap();
    let ids = [first.parents()[0], fixture.ids[0]];
    let author = fixture.cover_author();
    let cover = CoverLetter::new(&author, b"*** SUBJECT HERE ***\n\n*** BLURB HERE ***");
    let options = fixture
        .options(Numbering::Auto)
        .with_mail(Options::default().with_signature(None))
        .with_cover(with_cover.then_some(cover));
    let actual = objects
        .format_patch_with(
            &ids,
            options,
            None,
            SeriesLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_mbox();
    let mut args = vec!["format-patch", "--stdout", "--root", "--no-signature", "-2"];
    args.extend_from_slice(flags);
    let expected = oracle::git(fixture.repo.worktree().unwrap(), &args, b"");
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

#[test]
fn distant_base_is_explicitly_refused() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 2);
    let objects = fixture.repo.objects().unwrap();
    let first = objects.read_commit(fixture.ids[0]).unwrap();
    let options = fixture
        .options(Numbering::Auto)
        .with_mail(Options::default().with_base(Some(first.parents()[0])));
    let result = objects.format_patch_with(
        &fixture.ids[1..],
        options,
        None,
        SeriesLimits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(girt::format_patch::SeriesError::BasePrerequisites)
    ));
}

#[test]
fn merge_selection_is_explicitly_refused() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 2);
    let objects = fixture.repo.objects().unwrap();
    let last = objects.read_commit(fixture.ids[1]).unwrap();
    let merge = oracle::git(
        fixture.repo.worktree().unwrap(),
        &[
            "commit-tree",
            &last.tree().to_string(),
            "-p",
            &fixture.ids[0].to_string(),
            "-p",
            &fixture.ids[1].to_string(),
        ],
        b"Merge\n",
    );
    let merge = std::str::from_utf8(&merge).unwrap().trim().parse().unwrap();
    let result = objects.format_patch(&[merge]);
    assert!(matches!(
        result,
        Err(girt::format_patch::SeriesError::Merge(_))
    ));
}

#[test]
fn late_metadata_failure_returns_no_series_or_stored_objects() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let objects = fixture.repo.objects().unwrap();
    let first = objects.read_commit(fixture.ids[0]).unwrap();
    let invalid = girt::Commit::builder(first.tree(), fixture.cover_author(), b"invalid \xff\n")
        .parent(fixture.ids[0])
        .build()
        .unwrap();
    let id = fixture.repo.loose_objects().write_commit(&invalid).unwrap();
    let root = fixture.repo.worktree().unwrap();
    let before = oracle::git(root, &["count-objects", "-v"], b"");

    let result = objects.format_patch(&[fixture.ids[0], id]);
    assert!(matches!(
        result,
        Err(girt::format_patch::SeriesError::Mail(
            girt::format_patch::Error::Metadata("message UTF-8")
        ))
    ));
    assert_eq!(oracle::git(root, &["count-objects", "-v"], b""), before);
}

#[test]
fn deletion_summary_matches_git() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let root = fixture.repo.worktree().unwrap();
    oracle::git(root, &["rm", "file"], b"");
    oracle::git(root, &["commit", "-qm", "Delete file"], b"");
    fixture.check_latest(fixture.options(Numbering::Auto), &[]);
}

#[test]
fn executable_mode_summary_matches_git() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let root = fixture.repo.worktree().unwrap();
    oracle::git(root, &["update-index", "--chmod=+x", "file"], b"");
    oracle::git(root, &["commit", "-qm", "Change mode"], b"");
    fixture.check_latest(fixture.options(Numbering::Auto), &[]);
}

#[test]
fn rename_summary_matches_git() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let root = fixture.repo.worktree().unwrap();
    oracle::git(root, &["mv", "file", "renamed"], b"");
    oracle::git(root, &["commit", "-qm", "Rename file"], b"");
    fixture.check_latest(fixture.options(Numbering::Auto), &[]);
}

#[test]
fn copy_summary_matches_git() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let root = fixture.repo.worktree().unwrap();
    std::fs::copy(root.join("file"), root.join("copied")).unwrap();
    std::fs::write(root.join("file"), b"edited source\n").unwrap();
    oracle::git(root, &["add", "file", "copied"], b"");
    oracle::git(root, &["commit", "-qm", "Copy file"], b"");
    let rewrites = girt::rewrites::Options::default().with_copies(girt::rewrites::Copies::Modified);
    fixture.check_latest(
        fixture.options(Numbering::Auto).with_rewrites(rewrites),
        &["-C"],
    );
}

#[test]
fn symlink_type_summary_matches_git_without_symlink_checkout() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let root = fixture.repo.worktree().unwrap();
    let blob = oracle::git(root, &["hash-object", "-w", "--stdin"], b"target");
    let blob = std::str::from_utf8(&blob).unwrap().trim();
    oracle::git(
        root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("120000,{blob},file"),
        ],
        b"",
    );
    oracle::git(root, &["commit", "-qm", "Change type"], b"");
    fixture.check_latest(fixture.options(Numbering::Auto), &[]);
}

#[test]
fn gitlink_to_binary_stat_retains_both_sizes() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let root = fixture.repo.worktree().unwrap();
    oracle::git(
        root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{},file", fixture.ids[0]),
        ],
        b"",
    );
    oracle::git(root, &["commit", "-qm", "Use gitlink"], b"");
    let blob = oracle::git(root, &["hash-object", "-w", "--stdin"], b"\0new\n");
    let blob = std::str::from_utf8(&blob).unwrap().trim();
    oracle::git(
        root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},file"),
        ],
        b"",
    );
    oracle::git(root, &["commit", "-qm", "Use binary blob"], b"");
    fixture.check_latest(fixture.options(Numbering::Auto), &[]);
}

#[rstest]
#[case(
    "this is a deliberately very long subject line that exceeds seventy two columns easily by Amy"
)]
#[case(
    "界界界界界界界界界界界界界界界界界界界界界界界界界界界界界界 words beyond the first line and more words"
)]
#[case(
    "résumé résumé résumé résumé résumé résumé résumé résumé résumé résumé résumé résumé résumé"
)]
fn long_cover_shortlog_matches_git(#[case] subject: &str) {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    oracle::git(
        fixture.repo.worktree().unwrap(),
        &["commit", "--amend", "-qm", subject],
        b"",
    );
    let author = fixture.cover_author();
    let options = fixture
        .options(Numbering::Auto)
        .with_cover(Some(CoverLetter::new(
            &author,
            b"*** SUBJECT HERE ***\n\n*** BLURB HERE ***",
        )));
    fixture.check_latest(options, &["--cover-letter"]);
}

#[test]
fn explicit_base_for_root_is_refused() {
    let fixture = Fixture::new(ObjectFormat::Sha1, 1);
    let objects = fixture.repo.objects().unwrap();
    let parent = objects.read_commit(fixture.ids[0]).unwrap().parents()[0];
    let options = fixture
        .options(Numbering::Auto)
        .with_mail(Options::default().with_base(Some(parent)));
    let result = objects.format_patch_with(
        &[parent],
        options,
        None,
        SeriesLimits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(girt::format_patch::SeriesError::RootBase)
    ));
}
