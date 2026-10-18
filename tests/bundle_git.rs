//! Original synthetic histories compared with Git bundle commands under isolated configuration.
#[path = "support/apply_git.rs"]
mod support;

use std::sync::atomic::AtomicBool;

use girt::bundle::{Bundle, CreateLimits, Reference, create};
use girt::{ObjectFormat, ObjectId, Repository};

fn history(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let directory = tempfile::tempdir().unwrap();
    support::git(
        directory.path(),
        &[
            "init",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let mut content: Vec<u8> = (0..2048)
        .flat_map(|row| format!("line {row:04}: original synthetic bundle fixture\n").into_bytes())
        .collect();
    let tree_for = |content: &[u8]| {
        let blob = support::git(directory.path(), &["hash-object", "-w", "--stdin"], content);
        let blob = std::str::from_utf8(&blob).unwrap().trim();
        let tree = support::git(
            directory.path(),
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        );
        std::str::from_utf8(&tree).unwrap().trim().to_owned()
    };
    let tree = tree_for(&content);
    let first = support::git(directory.path(), &["commit-tree", &tree], b"first\n");
    let first = std::str::from_utf8(&first).unwrap().trim();
    content[1024] ^= 1;
    let second_tree = tree_for(&content);
    let second = support::git(
        directory.path(),
        &["commit-tree", &second_tree, "-p", first],
        b"second\n",
    );
    let second = std::str::from_utf8(&second).unwrap().trim();
    support::git(
        directory.path(),
        &["update-ref", "refs/heads/main", second],
        b"",
    );
    support::git(
        directory.path(),
        &["symbolic-ref", "HEAD", "refs/heads/main"],
        b"",
    );
    let (first, second) = (
        ObjectId::from_hex(format, first).unwrap(),
        ObjectId::from_hex(format, second).unwrap(),
    );
    let repo = Repository::open(directory.path()).unwrap();
    (directory, repo, first, second)
}

#[test]
fn read_write_and_incremental_bundles_match_git() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (directory, repo, first, second) = history(format);
        for prerequisites in [vec![], vec![first]] {
            let references = [Reference::new(
                girt::refs::RefName::new("refs/heads/main").unwrap(),
                second,
            )];
            let bytes = create(
                &repo.objects().unwrap(),
                &references,
                &prerequisites,
                CreateLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
            let path = directory.path().join("girt.bundle");
            std::fs::write(&path, &bytes).unwrap();

            let listed = support::git(
                directory.path(),
                &["bundle", "list-heads", path.to_str().unwrap()],
                b"",
            );
            assert_eq!(listed, format!("{second} refs/heads/main\n").as_bytes());
            support::git(
                directory.path(),
                &["bundle", "verify", path.to_str().unwrap()],
                b"",
            );
            let bundle = Bundle::parse(&bytes).unwrap();
            assert_eq!(bundle.object_format(), format);
            assert_eq!(bundle.prerequisites(), prerequisites);
            bundle
                .verify(&repo, Default::default(), &AtomicBool::new(false))
                .unwrap();
        }
        let path = directory.path().join("git.bundle");
        support::git(
            directory.path(),
            &[
                "bundle",
                "create",
                if format == ObjectFormat::Sha1 {
                    "--version=2"
                } else {
                    "--version=3"
                },
                path.to_str().unwrap(),
                "--all",
            ],
            b"",
        );
        let bytes = std::fs::read(path).unwrap();
        let bundle = Bundle::parse(&bytes).unwrap();
        let received = bundle
            .receive(
                &Default::default(),
                Default::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(received.object_count(), 6);
    }
}

#[test]
fn refuses_unknown_capabilities_and_header_exhaustion() {
    assert!(matches!(
        Bundle::parse(b"# v3 git bundle\n@unknown=value\n\n"),
        Err(girt::bundle::Error::Unsupported(_))
    ));
    let mut limits = girt::bundle::Limits::default();
    limits.max_header_bytes = 8;
    assert!(matches!(
        Bundle::parse_with(b"# v2 git bundle\n", limits),
        Err(girt::bundle::Error::Limit(_))
    ));
}

#[test]
fn clone_bundle_uses_existing_publication_path() {
    use girt::clone::{BranchSelection, CloneRequest};
    use girt::refs::{RefName, Reflog};
    let (directory, source, _, second) = history(ObjectFormat::Sha1);
    let refs = [Reference::new(
        RefName::new("refs/heads/main").unwrap(),
        second,
    )];
    let bytes = create(
        &source.objects().unwrap(),
        &refs,
        &[],
        CreateLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let bundle = Bundle::parse(&bytes).unwrap();
    let destination = directory.path().join("clone");
    let request = CloneRequest::prepare_tracking(
        &destination,
        girt::InitKind::Bare,
        b"source.bundle",
        BranchSelection::Branch(RefName::new("refs/heads/main").unwrap()),
        Reflog::Preserve,
    )
    .unwrap();

    request
        .receive_bundle(&bundle, Default::default(), &AtomicBool::new(false))
        .unwrap()
        .finish(Default::default(), &AtomicBool::new(false))
        .unwrap();

    let cloned = Repository::open(destination).unwrap();
    assert_eq!(cloned.head().unwrap().id, Some(second));
}

#[test]
fn incremental_fetch_requires_prerequisite_history() {
    use girt::clone::{BranchSelection, CloneRequest};
    use girt::fetch::{FetchRequest, KnownHistory};
    use girt::refs::{RefName, Reflog};
    use girt::remote::Remote;
    let (directory, source, first, second) = history(ObjectFormat::Sha1);
    let objects = source.objects().unwrap();
    let refs = |id| [Reference::new(RefName::new("refs/heads/main").unwrap(), id)];
    let initial = create(
        &objects,
        &refs(first),
        &[],
        CreateLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let initial = Bundle::parse(&initial).unwrap();
    let destination = directory.path().join("incremental");
    let request = CloneRequest::prepare_tracking(
        &destination,
        girt::InitKind::Bare,
        b"source.bundle",
        BranchSelection::Branch(RefName::new("refs/heads/main").unwrap()),
        Reflog::Preserve,
    )
    .unwrap();
    request
        .receive_bundle(&initial, Default::default(), &AtomicBool::new(false))
        .unwrap()
        .finish(Default::default(), &AtomicBool::new(false))
        .unwrap();
    let cloned = Repository::open(destination).unwrap();
    let incremental = create(
        &objects,
        &refs(second),
        &[first],
        CreateLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        Bundle::parse(&incremental).unwrap().prerequisites(),
        [first]
    );
    // Receive Git's incremental pack, including its external-base delta form when selected.
    let path = directory.path().join("git-incremental.bundle");
    support::git(
        directory.path(),
        &[
            "bundle",
            "create",
            path.to_str().unwrap(),
            &format!("{first}..refs/heads/main"),
        ],
        b"",
    );
    let git_incremental = std::fs::read(path).unwrap();
    let incremental = Bundle::parse(&git_incremental).unwrap();
    assert!(
        matches!(incremental.receive(&KnownHistory::default(), Default::default(), &AtomicBool::new(false)), Err(girt::bundle::Error::MissingPrerequisite(id)) if id == first)
    );
    let known = KnownHistory::new(
        &cloned.objects().unwrap(),
        &[first],
        Default::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let remote = Remote::find(cloned.config(), b"origin").unwrap().unwrap();
    let request = FetchRequest::prepare(
        cloned.clone(),
        remote.fetch_refspecs().clone(),
        Default::default(),
        Reflog::Preserve,
    )
    .unwrap();

    request
        .receive_bundle(
            &incremental,
            &known,
            Default::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .finish(Default::default(), &AtomicBool::new(false))
        .unwrap();

    let tip = cloned
        .references()
        .unwrap()
        .resolve(&RefName::new("refs/remotes/origin/main").unwrap())
        .unwrap();
    assert_eq!(tip.id, Some(second));
}

#[test]
fn damaged_packs_and_filtered_installation_are_refused() {
    let (_directory, repo, _, tip) = history(ObjectFormat::Sha1);
    let refs = [Reference::new(
        girt::refs::RefName::new("refs/heads/main").unwrap(),
        tip,
    )];
    let mut bytes = create(
        &repo.objects().unwrap(),
        &refs,
        &[],
        CreateLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    assert!(
        Bundle::parse(&bytes)
            .unwrap()
            .receive(
                &Default::default(),
                Default::default(),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    let filtered = format!(
        "# v3 git bundle\n@object-format=sha1\n@filter=blob:none\n{tip} refs/heads/main\n\n"
    );
    let bundle = Bundle::parse(filtered.as_bytes()).unwrap();
    assert_eq!(bundle.filter(), Some(b"blob:none".as_slice()));
    assert!(matches!(
        bundle.receive(
            &Default::default(),
            Default::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::bundle::Error::Filtered)
    ));
}
