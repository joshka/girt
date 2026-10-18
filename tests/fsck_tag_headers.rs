//! Annotated tag conventions compared with isolated Git fsck.
#[path = "support/apply_git.rs"]
mod support;
use girt::fsck::{FindingDetail, MessageId, Severity, check_object};
use girt::{ObjectFormat, ObjectKind};
use rstest::rstest;

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn tag_headers_and_names_match_git(#[case] format: ObjectFormat) {
    let cases = [
        "valid",
        "missing-tagger",
        "misplaced-tagger",
        "nul-header",
        "nul-body",
        "unterminated-tagger",
        "header-only",
        "duplicate-tagger",
        "space-name",
        "empty-name",
        "dotdot-name",
        "dot-name",
        "lock-name",
        "slash-name",
        "at-name",
        "dash-name",
    ];
    for name in cases {
        let directory = tempfile::tempdir().unwrap();
        let format_arg = if format == ObjectFormat::Sha1 {
            "--object-format=sha1"
        } else {
            "--object-format=sha256"
        };
        support::git(
            directory.path(),
            &["init", "--bare", "--template=", format_arg, "."],
            b"",
        );
        let tree = support::git(
            directory.path(),
            &["hash-object", "-t", "tree", "-w", "--stdin"],
            b"",
        );
        let tree = std::str::from_utf8(&tree).unwrap().trim();
        let tag_name = match name {
            "space-name" => "v 1",
            "empty-name" => "",
            "dotdot-name" => "a..b",
            "dot-name" => ".v1",
            "lock-name" => "v1.lock",
            "slash-name" => "a/b",
            "at-name" => "@",
            "dash-name" => "-v1",
            _ => "v1",
        };
        let prefix = format!("object {tree}\ntype tree\ntag {tag_name}\n");
        let person = "tagger A <a@example.invalid> 1000000000 +0000\n";
        let (payload, expected) = match name {
            "missing-tagger" => (
                format!("{prefix}\nx\n"),
                Some(MessageId::MissingTaggerEntry),
            ),
            "misplaced-tagger" => (
                format!("{prefix}extra value\n{person}\nx\n"),
                Some(MessageId::MissingTaggerEntry),
            ),
            "nul-header" => (
                format!("{prefix}{}\nx\n", person.replace("A", "A\0")),
                Some(MessageId::NulInHeader),
            ),
            "nul-body" => (format!("{prefix}{person}\nx\0y\n"), None),
            "unterminated-tagger" => (
                format!("{prefix}{}", person.trim_end()),
                Some(MessageId::UnterminatedHeader),
            ),
            "header-only" => (format!("{prefix}{person}"), None),
            "duplicate-tagger" => (format!("{prefix}{person}tagger invalid\n\nx\n"), None),
            "space-name" | "empty-name" | "dotdot-name" | "dot-name" | "lock-name" => (
                format!("{prefix}{person}\nx\n"),
                Some(MessageId::BadTagName),
            ),
            _ => (format!("{prefix}{person}\nx\n"), None),
        };
        let id = support::git(
            directory.path(),
            &["hash-object", "-t", "tag", "--literally", "-w", "--stdin"],
            payload.as_bytes(),
        );
        let id = std::str::from_utf8(&id).unwrap().trim();
        let oracle = support::git_output(
            directory.path(),
            &["fsck", "--no-reflogs", "--no-dangling", id],
            b"",
        );
        let stderr = String::from_utf8_lossy(&oracle.stderr);
        let findings = check_object(format, ObjectKind::Tag, payload.as_bytes());
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.message_id)
                .collect::<Vec<_>>(),
            expected.into_iter().collect::<Vec<_>>(),
            "{name}: {stderr}"
        );
        for finding in findings {
            let prefix = if matches!(finding.severity, Severity::Warning | Severity::Info) {
                "warning"
            } else {
                "error"
            };
            assert!(
                stderr.contains(&format!(
                    "{prefix} in tag {id}: {}:",
                    finding.message_id.as_str()
                )),
                "{name}: {stderr}"
            );
            if finding.message_id == MessageId::BadTagName {
                assert_eq!(
                    finding.detail,
                    Some(FindingDetail::TagName(tag_name.as_bytes().into()))
                );
            }
        }
        let errors = expected
            .is_some_and(|id| matches!(id.severity(false), Severity::Error | Severity::Fatal));
        assert_eq!(oracle.status.success(), !errors, "{name}: {stderr}");
    }
}
