//! Independently generated notes, compared against an isolated Git executable.
#[path = "support/history_git.rs"]
#[allow(dead_code)]
mod oracle;
use girt::notes::{Notes, Operation};
use girt::{ObjectFormat, ObjectId, Repository, Signature};

fn signature() -> Signature {
    Signature {
        name: b"Pack Author".to_vec(),
        email: b"pack@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    }
}
fn id(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

#[test]
fn notes_commits_match_git() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let root = tempfile::tempdir().unwrap();
        oracle::git(
            root.path(),
            &[
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
            ],
            b"",
        );
        let a = id(&oracle::git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"a",
        ));
        let b = id(&oracle::git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"b",
        ));
        let repo = Repository::open(root.path()).unwrap();
        let mut notes = Notes::new(format);
        let mut parent = None;
        for operation in [
            Operation::Add,
            Operation::Append,
            Operation::Copy,
            Operation::Remove,
        ] {
            let a_text = a.to_string();
            let b_text = b.to_string();
            let args = match operation {
                Operation::Add => {
                    notes.add(a, b"hello\n").unwrap();
                    vec!["notes", "add", "-m", "hello", &a_text]
                }
                Operation::Append => {
                    notes.append(a, b"more\n").unwrap();
                    vec!["notes", "append", "-m", "more", &a_text]
                }
                Operation::Copy => {
                    notes.copy(a, b).unwrap();
                    vec!["notes", "copy", &a_text, &b_text]
                }
                Operation::Remove => {
                    notes.remove(a).unwrap();
                    vec!["notes", "remove", &a_text]
                }
                _ => unreachable!(),
            };
            let prepared = notes
                .prepare(parent, signature(), signature(), operation)
                .unwrap();
            oracle::git(root.path(), &args, b"");
            let expected = id(&oracle::git(
                root.path(),
                &["rev-parse", "refs/notes/commits"],
                b"",
            ));
            assert_eq!(prepared.commit().id(), expected, "{operation:?} {format}");
            let expected_tree = id(&oracle::git(
                root.path(),
                &["rev-parse", "refs/notes/commits^{tree}"],
                b"",
            ));
            assert_eq!(prepared.tree().id(), expected_tree);
            assert_eq!(prepared.store(&repo.loose_objects()).unwrap(), expected);
            let loaded = Notes::read(&repo.objects().unwrap(), expected).unwrap();
            assert_eq!(
                loaded.iter().collect::<Vec<_>>(),
                notes.iter().collect::<Vec<_>>()
            );
            parent = Some(expected);
        }
    }
}

#[test]
fn arbitrary_fanout_depths_are_readable() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let root = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(format, root.path().join("repo"), girt::InitKind::Bare).unwrap();
        let loose = repo.loose_objects();
        let object = loose.write_blob(b"annotated").unwrap();
        let blob = loose.write_blob(b"reviewed\n").unwrap();
        let hex = object.to_string();
        for depth in [0, 1, 2, 3, hex.len() / 2 - 1] {
            let mut tree = girt::Tree::new(
                format,
                vec![girt::TreeEntry {
                    mode: girt::EntryMode::Blob,
                    name: hex.as_bytes()[depth * 2..].to_vec(),
                    id: blob,
                }],
            )
            .unwrap();
            loose.write_tree(&tree).unwrap();
            for i in (0..depth).rev() {
                tree = girt::Tree::new(
                    format,
                    vec![girt::TreeEntry {
                        mode: girt::EntryMode::Tree,
                        name: hex.as_bytes()[i * 2..i * 2 + 2].to_vec(),
                        id: tree.id(),
                    }],
                )
                .unwrap();
                loose.write_tree(&tree).unwrap();
            }
            let commit = girt::Commit::builder(tree.id(), signature(), b"notes\n")
                .build()
                .unwrap();
            loose.write_commit(&commit).unwrap();
            let notes = Notes::read(&repo.objects().unwrap(), commit.id()).unwrap();
            assert_eq!(notes.get(object), Some(b"reviewed\n".as_slice()));
            if depth > 0 {
                assert!(matches!(
                    notes.prepare(Some(commit.id()), signature(), signature(), Operation::Add),
                    Err(girt::notes::Error::Layout(_))
                ));
            }
            oracle::git(
                repo.git_dir(),
                &["update-ref", "refs/notes/commits", &commit.id().to_string()],
                b"",
            );
            assert_eq!(
                oracle::git(repo.git_dir(), &["notes", "show", &object.to_string()], b""),
                b"reviewed\n"
            );
        }
    }
}

#[test]
fn configured_reference_selection_matches_git() {
    let root = tempfile::tempdir().unwrap();
    oracle::git(root.path(), &["init", "--bare", "--template="], b"");
    oracle::git(
        root.path(),
        &["config", "core.notesRef", "refs/notes/review"],
        b"",
    );
    let object = id(&oracle::git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"object",
    ));
    oracle::git(
        root.path(),
        &["notes", "add", "-m", "review", &object.to_string()],
        b"",
    );
    let config = girt::Config::parse(b"[core]\nnotesRef = refs/notes/review\n[notes]\ndisplayRef = refs/notes/team/*\ndisplayRef = refs/notes/ci\n").unwrap();
    let selection =
        girt::notes::Selection::from_config(&config, &girt::workflow::Environment::default())
            .unwrap();
    assert_eq!(selection.primary.as_bytes(), b"refs/notes/review");
    assert_eq!(
        selection.display_patterns,
        [b"refs/notes/team/*".to_vec(), b"refs/notes/ci".to_vec()]
    );
    oracle::git(
        root.path(),
        &[
            "rev-parse",
            std::str::from_utf8(selection.primary.as_bytes()).unwrap(),
        ],
        b"",
    );
    let environment = girt::workflow::Environment::default().with("GIT_NOTES_REF", "refs/notes/ci");
    assert_eq!(
        girt::notes::Selection::from_config(&config, &environment)
            .unwrap()
            .primary
            .as_bytes(),
        b"refs/notes/ci"
    );
}

#[test]
fn uppercase_import_cannot_be_silently_renamed() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let loose = repo.loose_objects();
    let object = loose.write_blob(b"object").unwrap();
    let note = loose.write_blob(b"note\n").unwrap();
    let tree = girt::Tree::new(
        ObjectFormat::Sha1,
        vec![girt::TreeEntry {
            mode: girt::EntryMode::Blob,
            name: object.to_string().to_uppercase().into_bytes(),
            id: note,
        }],
    )
    .unwrap();
    loose.write_tree(&tree).unwrap();
    let commit = girt::Commit::builder(tree.id(), signature(), b"notes\n")
        .build()
        .unwrap();
    loose.write_commit(&commit).unwrap();
    let notes = Notes::read(&repo.objects().unwrap(), commit.id()).unwrap();
    assert_eq!(notes.get(object), Some(b"note\n".as_slice()));
    assert!(matches!(
        notes.prepare(Some(commit.id()), signature(), signature(), Operation::Add),
        Err(girt::notes::Error::Layout(_))
    ));
    let config = girt::Config::parse(b"").unwrap();
    for value in ["", "review", "refs/notes/../invalid"] {
        let environment = girt::workflow::Environment::default().with("GIT_NOTES_REF", value);
        assert!(matches!(
            girt::notes::Selection::from_config(&config, &environment),
            Err(girt::notes::Error::Reference { .. })
        ));
    }
}
