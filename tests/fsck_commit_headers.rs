//! Original commit-header fixtures compared with isolated Git fsck message IDs and severity.
#[path = "support/apply_git.rs"]
mod support;

use girt::fsck::{FindingDetail, MessageId, Severity, check_object};
use girt::{ObjectFormat, ObjectKind};
use rstest::rstest;

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn physical_headers_and_required_people_match_git(#[case] format: ObjectFormat) {
    let cases = [
        ("valid", vec![]),
        ("missing-author", vec![MessageId::MissingAuthor]),
        ("missing-committer", vec![MessageId::MissingCommitter]),
        ("multiple-authors", vec![MessageId::MultipleAuthors]),
        ("extra-before-author", vec![MessageId::MissingAuthor]),
        ("extra-before-committer", vec![MessageId::MissingCommitter]),
        ("late-author", vec![MessageId::MissingAuthor]),
        ("late-parent", vec![MessageId::MissingCommitter]),
        ("no-people", vec![MessageId::MissingAuthor]),
        ("bare-author-key", vec![MessageId::MissingAuthor]),
        ("late-duplicate-author", vec![]),
        ("duplicate-committer", vec![]),
        ("header-only", vec![]),
        ("unterminated-person", vec![MessageId::UnterminatedHeader]),
        ("unterminated-extra", vec![MessageId::UnterminatedHeader]),
        ("nul-person", vec![MessageId::NulInHeader]),
        ("nul-extra", vec![MessageId::NulInHeader]),
        ("nul-message", vec![MessageId::NulInCommit]),
    ];
    for (name, expected) in cases {
        let directory = tempfile::tempdir().unwrap();
        let format_arg = format!(
            "--object-format={}",
            if format == ObjectFormat::Sha1 {
                "sha1"
            } else {
                "sha256"
            }
        );
        support::git(
            directory.path(),
            &["init", "--bare", "--template=", &format_arg, "."],
            b"",
        );
        let tree = support::git(
            directory.path(),
            &["hash-object", "-t", "tree", "-w", "--stdin"],
            b"",
        );
        let tree = std::str::from_utf8(&tree).unwrap().trim();
        let t = format!("tree {tree}\n");
        let a = "author Author <a@example.invalid> 1000000000 +0000\n";
        let c = "committer Committer <c@example.invalid> 1000000000 +0000\n";
        let payload = match name {
            "valid" => format!("{t}{a}{c}\nmessage\n"),
            "missing-author" => format!("{t}{c}\nx\n"),
            "missing-committer" => format!("{t}{a}\nx\n"),
            "multiple-authors" => format!("{t}{a}{a}{c}\nx\n"),
            "extra-before-author" => format!("{t}extra value\n{a}{c}\nx\n"),
            "extra-before-committer" => format!("{t}{a}extra value\n{c}\nx\n"),
            "late-author" => format!("{t}{c}{a}\nx\n"),
            "late-parent" => format!("{t}{a}parent {tree}\n{c}\nx\n"),
            "no-people" => format!("{t}\nx\n"),
            "bare-author-key" => format!("{t}author\n{c}\nx\n"),
            "late-duplicate-author" => format!("{t}{a}{c}{a}\nx\n"),
            "duplicate-committer" => format!("{t}{a}{c}{c}\nx\n"),
            "header-only" => format!("{t}{a}{c}"),
            "unterminated-person" => format!("{t}{a}{}", c.trim_end()),
            "unterminated-extra" => format!("{t}{a}{c}extra value"),
            "nul-person" => format!("{t}{}{c}\nx\n", a.replace("Author", "Au\0thor")),
            "nul-extra" => format!("{t}{a}{c}extra x\0y\n\nx\n"),
            "nul-message" => format!("{t}{a}{c}\nx\0y\n"),
            _ => unreachable!(),
        };
        let id = support::git(
            directory.path(),
            &[
                "hash-object",
                "-t",
                "commit",
                "--literally",
                "-w",
                "--stdin",
            ],
            payload.as_bytes(),
        );
        let id = std::str::from_utf8(&id).unwrap().trim();
        let oracle = support::git_output(
            directory.path(),
            &["fsck", "--no-reflogs", "--no-dangling", id],
            b"",
        );
        let stderr = String::from_utf8_lossy(&oracle.stderr);
        let findings = check_object(format, ObjectKind::Commit, payload.as_bytes());
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.message_id)
                .collect::<Vec<_>>(),
            expected,
            "{name}: {stderr}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.contains(" in commit "))
                .count(),
            expected.len(),
            "{name}: {stderr}"
        );
        for finding in findings {
            if finding.message_id == MessageId::NulInHeader {
                assert_eq!(
                    finding.detail,
                    payload
                        .as_bytes()
                        .iter()
                        .position(|byte| *byte == 0)
                        .map(FindingDetail::ByteOffset)
                );
            }
            let prefix = if matches!(finding.severity, Severity::Warning | Severity::Info) {
                "warning"
            } else {
                "error"
            };
            assert!(
                stderr.contains(&format!(
                    "{prefix} in commit {id}: {}:",
                    finding.message_id.as_str()
                )),
                "{name}: {stderr}"
            );
        }
        let errors = expected
            .iter()
            .any(|id| matches!(id.severity(false), Severity::Error | Severity::Fatal));
        assert_eq!(oracle.status.success(), !errors, "{name}: {stderr}");
    }
}
