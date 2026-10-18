//! Original identity and date fixtures compared with isolated Git fsck.
#[path = "support/apply_git.rs"]
mod support;
use girt::fsck::{MessageId, check_object};
use girt::{ObjectFormat, ObjectKind};
use rstest::rstest;

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn required_identity_syntax_matches_git(#[case] format: ObjectFormat) {
    let cases = [
        ("valid", "A <a@example.invalid> 1000000000 +0000", None),
        (
            "no-space-before-email",
            "A<a> 1000000000 +0000",
            Some(MessageId::MissingSpaceBeforeEmail),
        ),
        (
            "no-name",
            "<a> 1000000000 +0000",
            Some(MessageId::MissingNameBeforeEmail),
        ),
        ("empty-name", " <a> 1000000000 +0000", None),
        ("empty-email", "A <> 1000000000 +0000", None),
        (
            "no-email",
            "A a 1000000000 +0000",
            Some(MessageId::MissingEmail),
        ),
        (
            "no-close-email",
            "A <a 1000000000 +0000",
            Some(MessageId::BadEmail),
        ),
        (
            "no-open-email",
            "A a> 1000000000 +0000",
            Some(MessageId::BadName),
        ),
        (
            "nested-email",
            "A <a<b> 1000000000 +0000",
            Some(MessageId::BadEmail),
        ),
        (
            "extra-after-email",
            "A <a>x 1000000000 +0000",
            Some(MessageId::MissingSpaceBeforeDate),
        ),
        (
            "no-space-date",
            "A <a>1000000000 +0000",
            Some(MessageId::MissingSpaceBeforeDate),
        ),
        ("no-date", "A <a>", Some(MessageId::MissingSpaceBeforeDate)),
        ("negative-date", "A <a> -1 +0000", Some(MessageId::BadDate)),
        ("positive-sign", "A <a> +1 +0000", Some(MessageId::BadDate)),
        (
            "zero-padded-date",
            "A <a> 0123 +0000",
            Some(MessageId::ZeroPaddedDate),
        ),
        (
            "overflow-date",
            "A <a> 18446744073709551616 +0000",
            Some(MessageId::BadDateOverflow),
        ),
        (
            "signed-overflow",
            "A <a> 9223372036854775808 +0000",
            Some(MessageId::BadDateOverflow),
        ),
        ("bad-timezone", "A <a> 1000000000 +9999", None),
        (
            "missing-timezone",
            "A <a> 1000000000",
            Some(MessageId::BadDate),
        ),
        (
            "short-timezone",
            "A <a> 1000000000 +00",
            Some(MessageId::BadTimezone),
        ),
        (
            "zone-no-sign",
            "A <a> 1000000000 0000",
            Some(MessageId::BadTimezone),
        ),
        (
            "trailing-date",
            "A <a> 1000000000 +0000 x",
            Some(MessageId::BadTimezone),
        ),
        ("double-space-date", "A <a>  1000000000 +0000", None),
        ("bad-date", "A <a> bad +0000", Some(MessageId::BadDate)),
        (
            "tab-before-email",
            "A\t<a> 1000000000 +0000",
            Some(MessageId::MissingSpaceBeforeEmail),
        ),
        (
            "tab-before-date",
            "A <a>\t1000000000 +0000",
            Some(MessageId::MissingSpaceBeforeDate),
        ),
        ("tabs-before-date", "A <a> \t1000000000 +0000", None),
        (
            "double-space-zone",
            "A <a> 1000000000  +0000",
            Some(MessageId::BadTimezone),
        ),
        (
            "tab-before-zone",
            "A <a> 1000000000\t+0000",
            Some(MessageId::BadDate),
        ),
        ("empty-date", "A <a>  +0000", Some(MessageId::BadDate)),
        ("zero", "A <a> 0 +0000", None),
        (
            "zero-padded-zero",
            "A <a> 00 +0000",
            Some(MessageId::ZeroPaddedDate),
        ),
        ("email-space", "A <a b> 1000000000 +0000", None),
        (
            "zone-letter",
            "A <a> 1000000000 +0x00",
            Some(MessageId::BadTimezone),
        ),
        ("newline-date-empty", "A <a> ", Some(MessageId::BadDate)),
        (
            "date-no-zone-space",
            "A <a> 1000000000 ",
            Some(MessageId::BadTimezone),
        ),
        (
            "zone-trailing-space",
            "A <a> 1000000000 +0000 ",
            Some(MessageId::BadTimezone),
        ),
        (
            "name-more-close",
            "A > B <a> 1000000000 +0000",
            Some(MessageId::BadName),
        ),
        (
            "extra-email-close",
            "A <a>> 1000000000 +0000",
            Some(MessageId::MissingSpaceBeforeDate),
        ),
    ];
    for (name, person, expected) in cases {
        for role in ["author", "committer"] {
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
            let ordinary = "Ordinary <ordinary@example.invalid> 1000000000 +0000";
            let (author, committer) = if role == "author" {
                (person, ordinary)
            } else {
                (ordinary, person)
            };
            let payload =
                format!("tree {tree}\nauthor {author}\ncommitter {committer}\n\nmessage\n");
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
                expected.into_iter().collect::<Vec<_>>(),
                "{name}/{role}: {stderr}"
            );
            if let Some(message) = expected {
                assert!(
                    stderr.contains(&format!(": {}:", message.as_str())),
                    "{name}/{role}: {stderr}"
                );
            }
            assert_eq!(
                oracle.status.success(),
                expected.is_none(),
                "{name}/{role}: {stderr}"
            );
        }
    }
}
