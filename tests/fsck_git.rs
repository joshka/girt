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

#[test]
fn configured_severity_and_skip_list_match_git() {
    use girt::fsck::{ObjectFinding, Severity};
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
    let tree = support::git(
        directory.path(),
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &[b"100644 .git\0".as_slice(), blob.as_bytes()].concat(),
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    support::git(
        directory.path(),
        &["update-ref", "refs/tags/tree", tree],
        b"",
    );
    for (value, severity, failure) in [
        ("error", Some(Severity::Error), true),
        ("warn", Some(Severity::Warning), false),
        ("ignore", None, false),
    ] {
        support::git(directory.path(), &["config", "fsck.hasDotgit", value], b"");
        let git = support::git_output(directory.path(), &["fsck", "--no-dangling"], b"");
        assert_eq!(!git.status.success(), failure);
        let repo = girt::Repository::open(directory.path()).unwrap();
        let report = repo.fsck().unwrap();
        let actual = report.findings.iter().find_map(|finding| match finding {
            ObjectFinding::Payload { finding, .. }
                if finding.message_id == MessageId::HasDotgit =>
            {
                Some(finding.severity)
            }
            _ => None,
        });
        assert_eq!(actual, severity);
        assert_eq!(report.has_errors(), failure);
    }
    support::git(
        directory.path(),
        &["config", "fsck.hasDotgit", "error"],
        b"",
    );
    let skip = directory.path().join("skip-list");
    std::fs::write(
        &skip,
        format!("# known historical object\n\n {tree} # comment\n{tree}\n"),
    )
    .unwrap();
    support::git(
        directory.path(),
        &["config", "fsck.skipList", skip.to_str().unwrap()],
        b"",
    );
    support::git(directory.path(), &["fsck", "--no-dangling"], b"");
    assert!(
        girt::Repository::open(directory.path())
            .unwrap()
            .fsck()
            .unwrap()
            .findings
            .is_empty()
    );
}

#[test]
fn policy_namespaces_unknown_ids_and_atomic_skip_loading() {
    use girt::fsck::{Policy, PolicyError, PolicyLimits, Scope, Severity};
    let directory = tempfile::tempdir().unwrap();
    let config = girt::Config::parse(b"[fsck]\n hasDotgit = ignore\n[fetch \"fsck\"]\n hasDotgit = error\n unknownFutureId = warn\n").unwrap();
    let repo = Policy::from_config(
        &config,
        Scope::Repository,
        ObjectFormat::Sha1,
        directory.path(),
        Default::default(),
    )
    .unwrap();
    let fetch = Policy::from_config(
        &config,
        Scope::Fetch,
        ObjectFormat::Sha1,
        directory.path(),
        Default::default(),
    )
    .unwrap();
    let id = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"x");
    assert_eq!(
        repo.severity(id, MessageId::HasDotgit, true),
        Severity::Ignore
    );
    assert_eq!(
        fetch.severity(id, MessageId::HasDotgit, false),
        Severity::Error
    );
    assert_eq!(fetch.unknown_messages(), [b"unknownFutureId".to_vec()]);
    let mut policy = Policy::default();
    assert!(matches!(
        policy.add_skip_list(
            ObjectFormat::Sha1,
            format!("{id}\nbad\n").as_bytes(),
            PolicyLimits::default()
        ),
        Err(PolicyError::SkipRecord(2))
    ));
    assert_eq!(
        policy.severity(id, MessageId::HasDotgit, false),
        Severity::Warning
    );
    for (key, value) in [("unknownFutureId", "warn"), ("hasDotgit", "info")] {
        let config = girt::Config::parse(format!("[fsck]\n {key} = {value}\n").as_bytes()).unwrap();
        assert!(
            Policy::from_config(
                &config,
                Scope::Repository,
                ObjectFormat::Sha1,
                directory.path(),
                Default::default()
            )
            .is_err()
        );
    }
}

#[test]
fn skip_lists_do_not_suppress_storage_corruption() {
    let directory = tempfile::tempdir().unwrap();
    support::git(
        directory.path(),
        &["init", "--bare", "--template=", "."],
        b"",
    );
    let repo = girt::Repository::open(directory.path()).unwrap();
    let id = repo.loose_objects().write_blob(b"original").unwrap();
    let hex = id.to_string();
    std::fs::write(
        repo.object_dir().join(&hex[..2]).join(&hex[2..]),
        b"broken zlib",
    )
    .unwrap();
    let skip = directory.path().join("skip-list");
    std::fs::write(&skip, format!("{id}\n")).unwrap();
    support::git(
        directory.path(),
        &["config", "fsck.skipList", skip.to_str().unwrap()],
        b"",
    );

    assert!(
        !support::git_output(directory.path(), &["fsck"], b"")
            .status
            .success()
    );
    let report = girt::Repository::open(directory.path())
        .unwrap()
        .fsck()
        .unwrap();
    assert!(report.findings.iter().any(|finding| matches!(finding, girt::fsck::ObjectFinding::Read { id: found, .. } if *found == id)));
}

#[test]
fn configurable_path_length_and_catalogue_defaults() {
    use girt::fsck::{Policy, Scope, Severity};
    let config = girt::Config::parse(b"[fsck]\n largePathname = error:3\n").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let policy = Policy::from_config(
        &config,
        Scope::Repository,
        ObjectFormat::Sha1,
        directory.path(),
        Default::default(),
    )
    .unwrap();
    let blob = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"content");
    let payload = [b"100644 four\0".as_slice(), blob.as_bytes()].concat();
    let id = ObjectFormat::Sha1.hash_object(ObjectKind::Tree, &payload);
    let findings = policy.check_object(id, ObjectKind::Tree, &payload, false);
    assert!(
        findings
            .iter()
            .any(|finding| finding.message_id == MessageId::LargePathname
                && finding.severity == Severity::Error)
    );
    for &message in MessageId::ALL {
        if matches!(message, MessageId::BadCommit | MessageId::BadTagObject) {
            continue;
        }
        assert_eq!(
            MessageId::from_config_name(message.as_str().to_ascii_uppercase().as_bytes()),
            Some(message)
        );
    }
    assert_eq!(MessageId::BadFilemode.default_severity(), Severity::Info);
    assert_eq!(MessageId::HasDotgit.severity(true), Severity::Error);
    assert_eq!(MessageId::BadFilemode.severity(true), Severity::Warning);
    assert_eq!(
        MessageId::UnterminatedHeader.severity(false),
        Severity::Fatal
    );
}

#[test]
fn strict_mode_reports_historical_group_writable_mode() {
    use girt::fsck::{Policy, Severity};
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
    let payload = [b"100664 file\0".as_slice(), blob.as_bytes()].concat();
    let tree = support::git(
        directory.path(),
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &payload,
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let id = girt::ObjectId::from_hex(ObjectFormat::Sha1, tree).unwrap();
    let policy = Policy::default();
    assert!(
        policy
            .check_object(id, ObjectKind::Tree, &payload, false)
            .is_empty()
    );
    let findings = policy.check_object(id, ObjectKind::Tree, &payload, true);
    assert!(
        findings
            .iter()
            .any(|finding| finding.message_id == MessageId::BadFilemode
                && finding.severity == Severity::Warning)
    );
    let output = support::git_output(directory.path(), &["fsck", "--strict", tree], b"");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("badFilemode"));
}

#[test]
fn fatal_messages_accept_error_but_cannot_be_demoted() {
    use girt::fsck::{Policy, Scope};
    let directory = tempfile::tempdir().unwrap();
    support::git(
        directory.path(),
        &["init", "--bare", "--template=", "."],
        b"",
    );
    for (value, accepted) in [("error", true), ("warn", false), ("ignore", false)] {
        let output = support::git_output(
            directory.path(),
            &["-c", &format!("fsck.nulInHeader={value}"), "fsck"],
            b"",
        );
        assert_eq!(output.status.success(), accepted);
        let config =
            girt::Config::parse(format!("[fsck]\n nulInHeader = {value}\n").as_bytes()).unwrap();
        assert_eq!(
            Policy::from_config(
                &config,
                Scope::Repository,
                ObjectFormat::Sha1,
                directory.path(),
                Default::default()
            )
            .is_ok(),
            accepted
        );
    }
}
