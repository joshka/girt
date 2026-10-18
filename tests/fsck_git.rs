//! Original malformed payloads compared with the Git executable under isolated configuration.
#[path = "support/apply_git.rs"]
mod support;

use girt::fsck::{MessageId, check_object};
use girt::{ObjectFormat, ObjectKind};

#[test]
fn tree_diagnostics_match_git() {
    for (prefixes, expected) in [
        (vec![b"100644 .git\0".as_slice()], MessageId::HasDotgit),
        (vec![b"100644 .GIT\0".as_slice()], MessageId::HasDotgit),
        (vec![b"100644 .git.\0".as_slice()], MessageId::HasDotgit),
        (vec![b"100644 .git..\0".as_slice()], MessageId::HasDotgit),
        (vec![b"100644 .git. .\0".as_slice()], MessageId::HasDotgit),
        (
            vec!["100644 .g\u{200c}it\0".as_bytes()],
            MessageId::HasDotgit,
        ),
        (
            vec![b"100644 .git::$INDEX_ALLOCATION\0".as_slice()],
            MessageId::HasDotgit,
        ),
        (vec![b"100644 GIT~1\0".as_slice()], MessageId::HasDotgit),
        (vec![b"100644 ..\0".as_slice()], MessageId::HasDotdot),
        (vec![b"100644 .\0".as_slice()], MessageId::HasDot),
        (vec![b"100644 a/b\0".as_slice()], MessageId::FullPathname),
        (
            vec![b"0100644 a\0".as_slice()],
            MessageId::ZeroPaddedFilemode,
        ),
        (vec![b"100600 a\0".as_slice()], MessageId::BadFilemode),
        (
            vec![b"100644 a\0".as_slice(), b"100644 a\0"],
            MessageId::DuplicateEntries,
        ),
        (
            vec![b"100644 z\0".as_slice(), b"100644 a\0"],
            MessageId::TreeNotSorted,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        support::git(
            directory.path(),
            &["init", "--bare", "--template=", "."],
            b"",
        );
        let blob = support::git(directory.path(), &["hash-object", "-w", "--stdin"], b"blob");
        let blob = girt::ObjectId::from_hex(
            ObjectFormat::Sha1,
            std::str::from_utf8(&blob).unwrap().trim(),
        )
        .unwrap();
        let mut payload = Vec::new();
        for prefix in prefixes {
            payload.extend_from_slice(prefix);
            payload.extend_from_slice(blob.as_bytes());
        }
        let id = support::git(
            directory.path(),
            &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
            &payload,
        );
        let output = support::git_output(
            directory.path(),
            &["fsck", std::str::from_utf8(&id).unwrap().trim()],
            b"",
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected.as_str()), "{expected:?}: {stderr}");
        let findings = check_object(ObjectFormat::Sha1, ObjectKind::Tree, &payload);
        assert!(
            findings
                .iter()
                .any(|finding| finding.message_id == expected),
            "{findings:?}"
        );
    }
}

#[test]
fn repository_reports_missing_and_unreachable_objects() {
    let directory = tempfile::tempdir().unwrap();
    support::git(
        directory.path(),
        &["init", "--bare", "--template=", "."],
        b"",
    );
    let repository = girt::Repository::open(directory.path()).unwrap();
    let orphan = repository.loose_objects().write_blob(b"orphan").unwrap();
    let missing = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    let payload = [b"100644 missing\0".as_slice(), missing.as_bytes()].concat();
    let tree = support::git(
        directory.path(),
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &payload,
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    support::git(
        directory.path(),
        &["update-ref", "refs/tags/tree", tree],
        b"",
    );

    let report = repository.fsck().unwrap();

    assert!(report.unreachable.contains(&orphan));
    assert!(report.findings.iter().any(|finding| matches!(finding,
        girt::fsck::ObjectFinding::Missing { id } if *id == missing)));
}
