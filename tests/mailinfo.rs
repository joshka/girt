//! Original mail fixtures compared with isolated Git's mailinfo metadata and both output files.
#[path = "support/mailinfo_git.rs"]
mod mail_oracle;
#[path = "support/apply_git.rs"]
mod oracle;
use std::sync::atomic::AtomicBool;

use girt::mailinfo::{Error, Limits, Options, Subject};
use mail_oracle::compare;
use rstest::rstest;

const HEADERS: &[u8] = b"From: A. Writer <a@example.com>\nDate: Mon, 17 Sep 2001 00:00:00 +0000\nSubject: [PATCH v2 1/2] [topic] title\nMessage-ID: <id@example.com>\n\n";

#[rstest]
#[case::plain(b"body\n\n---\nstat\n\ndiff --git a/f b/f\npatch\n-- \nsignature\n")]
#[case::no_patch(b"\n\nbody\n\n-- \nsignature\n")]
#[case::bare_patch(b"diff --git a/f b/f\npatch\n")]
#[case::dash_prose(b"body\n--- prose\nmore\n")]
#[case::dash_word(b"body\n---word\nmore\n")]
#[case::index(b"body\nIndex: file\n===\n")]
#[case::unified(b"body\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n")]
#[case::escaped_from(b">From body\n>>From body\n")]
#[case::body_headers(b"From: Other <other@example.com>\nSubject: Other subject\nDate: Tue, 18 Sep 2001 00:00:00 +0000\n\nbody\n---\npatch\n")]
#[case::scissors(
    b"preamble\n# ------------------------ >8 ------------------------\nbody\n---\npatch\n"
)]
#[case::short_scissors(b"preamble\n-- >8 --\nbody\n---\npatch\n")]
#[case::scissors_after_patch(b"body\n---\npatch\n-- >8 --\nsignature\n")]
#[case::no_final_newline(b"body\n---\npatch")]
fn body_artifacts_match_git(
    #[case] body: &[u8],
    #[values(Subject::Clean, Subject::Keep, Subject::KeepNonPatch)] subject: Subject,
    #[values(false, true)] scissors: bool,
    #[values(false, true)] message_id: bool,
) {
    compare(
        &[HEADERS, body].concat(),
        Options::default()
            .with_subject(subject)
            .with_scissors(scissors)
            .with_message_id(message_id),
    );
}

#[rstest]
#[case::q(b"From: =?UTF-8?Q?J=C3=B6hn_Writer?= <a@example.com>\nSubject: [PATCH] =?UTF-8?Q?R=C3=A9sum=C3=A9?=\n\nbody\n---\npatch\n")]
#[case::b(b"From: =?UTF-8?B?SsO2aG4gV3JpdGVy?= <a@example.com>\nSubject: [PATCH] =?UTF-8?B?UsOpc3Vtw6k=?=\n\nbody\n")]
#[case::adjacent(b"From: A <a@x>\nSubject: [PATCH] =?UTF-8?Q?one_?=  =?UTF-8?Q?two?=\n\nbody\n")]
#[case::folded(b"From: A <a@x>\nSubject: Re: [PATCH]\n folded subject\n\nbody\n")]
#[case::latin1(
    b"From: =?ISO-8859-1?Q?J=F6hn?= <a@x>\nSubject: [PATCH] =?ISO-8859-1?Q?caf=E9?=\n\nbody\n"
)]
#[case::brackets(b"From: A <a@x>\nSubject: Re: [topic] [RFC PATCH] title  with   spaces\n\nbody\n")]
#[case::quoted(b"From: \"A. Writer\" <a@x>\nSubject: [PATCH] title\n\nbody\n")]
#[case::comment(b"From: a@x (A Writer)\nSubject: [PATCH] title\n\nbody\n")]
#[case::bare_email(b"From: a@example.com\nSubject: [PATCH] title\n\nbody\n")]
#[case::qp_body(b"From: A <a@x>\nSubject: [PATCH] title\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: quoted-printable\n\nbody =C3=BC=\nnicode\n---\npatch\n")]
#[case::b_body(b"From: A <a@x>\nSubject: [PATCH] title\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: base64\n\nYm9keQotLS0KcGF0Y2gK\n")]
#[case::lower_patch(b"From: A <a@x>\nSubject: [patch] lower\n\nbody\n")]
#[case::duplicate(b"From: A <a@x>\nSubject: [PATCH] first\nSubject: second\n\nbody\n")]
fn header_and_transfer_decoding_match_git(
    #[case] input: &[u8],
    #[values(Subject::Clean, Subject::Keep, Subject::KeepNonPatch)] subject: Subject,
) {
    compare(input, Options::default().with_subject(subject));
}

#[test]
fn generated_utf8_git_mail_matches() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    oracle::git(root, &["init", "-q"], b"");
    std::fs::write(root.join("file"), b"before\n").unwrap();
    oracle::git(root, &["add", "file"], b"");
    oracle::git(root, &["commit", "-qm", "Base"], b"");
    std::fs::write(root.join("file"), b"after\n").unwrap();
    oracle::git(root, &["add", "file"], b"");
    oracle::git(
        root,
        &[
            "commit",
            "-qm",
            "Résumé\n\nBody ünicode.",
            "--author",
            "Jöhn Writer <a@example.com>",
        ],
        b"",
    );
    let mail = oracle::git(root, &["format-patch", "--stdout", "-1"], b"");
    compare(&mail, Options::default());
    compare(&mail, Options::default().with_subject(Subject::Keep));
}

#[rstest]
#[case::input(Limits::default().with_max_input_bytes(0), "input bytes")]
#[case::header(Limits::default().with_max_header_bytes(0), "header bytes")]
#[case::fields(Limits::default().with_max_headers(0), "headers")]
#[case::decoded(Limits::default().with_max_decoded_bytes(0), "decoded bytes")]
#[case::message(Limits::default().with_max_message_bytes(0), "message bytes")]
#[case::patch(Limits::default().with_max_patch_bytes(0), "patch bytes")]
fn limit_failures_return_no_artifacts(#[case] limits: Limits, #[case] name: &str) {
    let result = girt::mailinfo::parse_with(
        &[HEADERS, b"body\n---\npatch\n"].concat(),
        Options::default(),
        limits,
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Limit(found)) if found == name));
}
#[test]
fn cancellation_precedes_admission() {
    assert!(matches!(
        girt::mailinfo::parse_with(
            b"",
            Options::default(),
            Limits::default(),
            &AtomicBool::new(true)
        ),
        Err(Error::Cancelled)
    ));
}
#[rstest]
#[case(b"Subject: =?US-ASCII?Q?=C3=A9?=\n\nbody\n")]
#[case(b"Subject: =?UTF-8?B?!!!!?=\n\nbody\n")]
#[case(b"Subject: =?UTF-8?Q?inject=0Aheader?=\n\nbody\n")]
fn malformed_metadata_is_refused(#[case] input: &[u8]) {
    assert!(matches!(
        girt::mailinfo::parse(input),
        Err(Error::Malformed(_))
    ));
}
#[rstest]
#[case(b"Subject: =?X-UNKNOWN?Q?word?=\n\nbody\n")]
#[case(b"Content-Type: text/plain; charset=shift-jis\n\nbody\n")]
fn unsupported_mime_and_charsets_are_explicit(#[case] input: &[u8]) {
    assert!(matches!(
        girt::mailinfo::parse(input),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn exact_artifact_and_decoded_bounds() {
    let header = b"Subject: [PATCH] hi\n";
    let body = b"body\n---\npatch\n";
    let input = [header.as_slice(), b"\n", body].concat();
    let exact = Limits::default()
        .with_max_input_bytes(input.len())
        .with_max_header_bytes(header.len())
        .with_max_headers(1)
        .with_max_decoded_bytes(body.len() + b"[PATCH] hi".len())
        .with_max_message_bytes(b"body\n".len())
        .with_max_patch_bytes(b"---\npatch\n".len());
    assert!(
        girt::mailinfo::parse_with(&input, Options::default(), exact, &AtomicBool::new(false))
            .is_ok()
    );
    for (limits, name) in [
        (
            exact.with_max_header_bytes(header.len() - 1),
            "header bytes",
        ),
        (
            exact.with_max_decoded_bytes(body.len() + b"[PATCH] hi".len() - 1),
            "decoded bytes",
        ),
        (
            exact.with_max_message_bytes(b"body\n".len() - 1),
            "message bytes",
        ),
        (
            exact.with_max_patch_bytes(b"---\npatch\n".len() - 1),
            "patch bytes",
        ),
    ] {
        assert!(
            matches!(girt::mailinfo::parse_with(&input, Options::default(), limits, &AtomicBool::new(false)), Err(Error::Limit(found)) if found == name)
        );
    }
}

#[test]
fn message_id_trailer_is_in_the_log_budget() {
    let input = [HEADERS, b"body\n---\npatch\n"].concat();
    let options = Options::default().with_message_id(true);
    let info =
        girt::mailinfo::parse_with(&input, options, Limits::default(), &AtomicBool::new(false))
            .unwrap();
    let limits = Limits::default().with_max_message_bytes(info.message.len());
    assert!(girt::mailinfo::parse_with(&input, options, limits, &AtomicBool::new(false)).is_ok());
    assert!(matches!(
        girt::mailinfo::parse_with(
            &input,
            options,
            limits.with_max_message_bytes(info.message.len() - 1),
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("message bytes"))
    ));
}

#[rstest]
#[case(b"-- %< --")]
#[case(b"---->8----")]
#[case(b"-- >% --")]
#[case(b"-- 8< --")]
fn standard_scissors_forms_match_git(#[case] line: &[u8]) {
    let input = [HEADERS, b"preamble\n", line, b"\nbody\n---\npatch\n"].concat();
    compare(&input, Options::default().with_scissors(true));
}

#[rstest]
#[case(b"text -- >8 --")]
#[case(b"- >8 ---")]
#[case(b"--   >8 --")]
fn broader_scissors_forms_match_git(#[case] line: &[u8]) {
    let input = [HEADERS, b"preamble\n", line, b"\nbody\n---\npatch\n"].concat();
    compare(&input, Options::default().with_scissors(true));
}

#[rstest]
#[case(b"From: A <a@x>\nSubject: =?UTF-8?Q?x=C3?=\n =?UTF-8?Q?=A9y?=\n\nbody\n")]
#[case(b"From: A <a@x>\nSubject: =?utf-8?b?w6k?=\n\nbody\n")]
#[case(b"From: A <a@x>\nSubject: =?utf-8?q?bad=ZZ?=\n\nbody\n")]
#[case(b"From: =?utf-8?q?A_=3Cx=3E?= <a@b>\nSubject: title\n\nbody\n")]
#[case(b"From: A <a@x>\nSubject: title\nContent-Transfer-Encoding: base64\n\nYm9keQotLS0KcGF0Y2g")]
#[case(b"From: A <a@x>\nSubject: title\nContent-Transfer-Encoding: quoted-printable\n\nlog =ZZ and = x\n---\npatch=\n")]
fn review_encoding_regressions_match_git(
    #[case] input: &[u8],
    #[values(Subject::Clean, Subject::Keep)] subject: Subject,
) {
    compare(input, Options::default().with_subject(subject));
}

#[test]
fn repeated_unterminated_markers_fail_without_partial_artifacts() {
    let subject = "=?utf-8?q?".repeat(10_000);
    let input = format!("Subject: {subject}\n\nbody\n");
    assert!(matches!(
        girt::mailinfo::parse(input.as_bytes()),
        Err(Error::Malformed("encoded word"))
    ));
}

#[rstest]
#[case(b"x =? y".as_slice())]
#[case(b"=?utf-8?q?a".as_slice())]
fn unfinished_encoded_words_match_git_failure_status(#[case] subject: &[u8]) {
    let input = [b"From: A <a@x>\nSubject: ", subject, b"\n\nbody\n"].concat();
    let temporary = tempfile::tempdir().unwrap();
    let output = oracle::git_output(temporary.path(), &["mailinfo", "message", "patch"], &input);
    assert!(!output.status.success());
    assert!(
        std::fs::read(temporary.path().join("message"))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        girt::mailinfo::parse(&input),
        Err(Error::Malformed("encoded word"))
    ));
}

#[rstest]
#[case(b">8")]
#[case(b"- >8 -")]
#[case(b">8 ----")]
#[case(b">8 ------")]
#[case(b"---- >8")]
#[case(b"------ >8")]
#[case(b"-  >8  -")]
#[case(b"===== >8 =====")]
#[case(b"--text>8text--")]
#[case(b"--abcdefgh>8abcdefgh--")]
#[case(b"-- >8 -- trailing")]
#[case(b"-- >8 --                    ")]
#[case(b"-- >8 --\x0b\x0b\x0b\x0b\x0b\x0b")]
#[case(b"-- >8 --\x0c\x0c\x0c\x0c\x0c\x0c")]
#[case(b"\x0b\x0b\x0b\x0b\x0b\x0b-- >8 --")]
#[case(b"\x0c\x0c\x0c\x0c\x0c\x0c-- >8 --")]
#[case(b"                    -- >8 --")]
#[case(b"-- >8 -- >8 --")]
#[case(b"--   >8 >8   --")]
#[case(b"--\t>8\t--")]
#[case("ééé -- >8 -- 界界".as_bytes())]
#[case(b"--\x0b>8\x0c--")]
fn scissors_acceptance_and_retained_body_match_git(#[case] line: &[u8]) {
    let input = [HEADERS, b"preamble\n", line, b"\nbody\n---\npatch\n"].concat();
    compare(&input, Options::default().with_scissors(true));
}

#[rstest]
fn generated_scissors_shapes_match_git(
    #[values(0, 1, 2, 4)] dashes: usize,
    #[values(0, 1, 3, 6)] gap: usize,
    #[values(">8", "8<", ">%", "%<")] marker: &str,
    #[values(0, 8, 20)] text: usize,
) {
    let line = format!(
        "{}{}{}{marker}{}{}",
        "x".repeat(text),
        "-".repeat(dashes),
        " ".repeat(gap),
        "y".repeat(gap),
        "-".repeat(dashes)
    );
    let input = [
        HEADERS,
        b"preamble\n",
        line.as_bytes(),
        b"\nbody\n---\npatch\n",
    ]
    .concat();
    compare(&input, Options::default().with_scissors(true));
}

#[rstest]
#[case(b"--abcdefg>8abcdefg--")]
#[case(b"-- >8 --")]
#[case(b"-- >8 --\x0b\x0b\x0b\x0b\x0b\x0b")]
#[case(b"-- >8 --\x0c\x0c\x0c\x0c\x0c\x0c")]
fn scissors_at_eof_matches_git(#[case] line: &[u8], #[values(false, true)] newline: bool) {
    let ending: &[u8] = if newline { b"\n" } else { b"" };
    let input = [HEADERS, b"preamble\n", line, ending].concat();
    compare(&input, Options::default().with_scissors(true));
}
