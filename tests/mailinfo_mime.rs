//! Original multipart messages compared with isolated Git CLI artifacts.
#[path = "support/mailinfo_git.rs"]
mod mail_oracle;
#[path = "support/apply_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use girt::mailinfo::{Error, Limits, Options, Subject};
use rstest::rstest;

const HEADER: &[u8] = b"From: A Writer <a@example.com>\nSubject: [PATCH] title\nMessage-ID: <id@example.com>\nMIME-Version: 1.0\n";
fn multipart(kind: &str, body: &[u8]) -> Vec<u8> {
    [
        HEADER,
        format!("Content-Type: multipart/{kind}; boundary=\"x\"\n\n").as_bytes(),
        body,
    ]
    .concat()
}

#[rstest]
#[case::plain(b"preamble\n--x\n\nbody\n--x--\nepilogue\n")]
#[case::mixed_patch(b"--x\nContent-Type: text/plain\n\nbody\n--x\nContent-Type: text/x-patch\n\ndiff --git a/f b/f\npatch\n--x--\n")]
#[case::opaque_patch(b"--x\nContent-Type: text/plain\n\nbody\n--x\nContent-Type: application/octet-stream\nContent-Disposition: attachment; filename=\"p.patch\"\n\ndiff --git a/f b/f\npatch\n--x--\n")]
#[case::html(b"--x\nContent-Type: text/plain\n\nplain body\n---\npatch\n--x\nContent-Type: text/html\n\n<html>html</html>\n--x--\n")]
#[case::binary_leaf(b"--x\nContent-Type: image/png\n\n\0\xffabc\n--x--\n")]
#[case::identity_headers(b"--x\nFrom: Other <b@x>\nSubject: inner\n\nbody\n---\npatch\n--x--\n")]
#[case::nested(b"--x\nContent-Type: multipart/mixed; boundary=y\n\npreamble\n--y\n\nbody\n--y\nContent-Type: text/x-patch\n\n---\npatch\n--y--\ninner epilogue\n--x--\nouter epilogue\n")]
#[case::base64(b"--x\nContent-Transfer-Encoding: base64\n\nYm9keQo=\n--x\nContent-Transfer-Encoding: base64\n\nLS0tCnBhdGNoCg==\n--x--\n")]
#[case::quoted_printable(b"--x\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: quoted-printable\n\nbody =C3=BC=\nnicode\n--x\nContent-Transfer-Encoding: quoted-printable\n\n---\npatch=20line\n--x--\n")]
#[case::truncated(b"--x\n\nbody\n---\npatch\n")]
#[case::closing_eof(b"--x\n\nbody\n---\npatch\n--x--")]
#[case::empty(b"--x--\n")]
#[case::two_logs(b"--x\n\nfirst body\n--x\n\nsecond body\n---\npatch\n--x--\n")]
#[case::scissors(b"--x\n\npreamble\n-- >8 --\nbody\n--x\n\n---\npatch\n--x--\n")]
#[case::no_decoded_lf(b"--x\nContent-Transfer-Encoding: base64\n\nYm9keQ==\n--x\n\ndiff --git a/f b/f\npatch\n--x--\n")]
fn multipart_artifacts_match_git(
    #[case] body: &[u8],
    #[values("mixed", "alternative")] kind: &str,
    #[values(false, true)] scissors: bool,
    #[values(false, true)] message_id: bool,
) {
    mail_oracle::compare(
        &multipart(kind, body),
        Options::default()
            .with_scissors(scissors)
            .with_message_id(message_id),
    );
}

#[rstest]
#[case("x;y", b"--x;y\n\nbody\n---\npatch\n--x;y--\n")]
#[case("x\\\"y", b"--x\"y\n\nbody\n---\npatch\n--x\"y--\n")]
#[case("x", b"--x \t\n\nbody\n---\npatch\n--x-- \t\n")]
fn quoted_and_padded_boundaries_match_git(#[case] boundary: &str, #[case] body: &[u8]) {
    let input = [
        HEADER,
        format!("Content-Type: Multipart/Mixed; ignored=\"a;b\"; boundary=\"{boundary}\"\n\n")
            .as_bytes(),
        body,
    ]
    .concat();
    mail_oracle::compare(&input, Options::default().with_subject(Subject::Keep));
}

#[test]
fn mime_entity_and_depth_limits_have_exact_edges() {
    let input = multipart("mixed", b"--x\n\nbody\n--x\n\n---\npatch\n--x--\n");
    let limits = Limits::default()
        .with_max_mime_parts(3)
        .with_max_mime_depth(1);
    assert!(
        girt::mailinfo::parse_with(&input, Options::default(), limits, &AtomicBool::new(false))
            .is_ok()
    );
    for (limits, name) in [
        (limits.with_max_mime_parts(2), "MIME parts"),
        (limits.with_max_mime_depth(0), "MIME depth"),
    ] {
        assert!(
            matches!(girt::mailinfo::parse_with(&input, Options::default(), limits, &AtomicBool::new(false)), Err(Error::Limit(found)) if found == name)
        );
    }
}
#[test]
fn nested_depth_is_explicit_and_iterative() {
    let input = multipart(
        "mixed",
        b"--x\nContent-Type: multipart/mixed; boundary=y\n\n--y\n\nbody\n--y--\n--x--\n",
    );
    let limits = Limits::default()
        .with_max_mime_depth(2)
        .with_max_mime_parts(3);
    assert!(
        girt::mailinfo::parse_with(&input, Options::default(), limits, &AtomicBool::new(false))
            .is_ok()
    );
    assert!(matches!(
        girt::mailinfo::parse_with(
            &input,
            Options::default(),
            limits.with_max_mime_depth(1),
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("MIME depth"))
    ));
}
#[test]
fn part_headers_share_the_outer_header_budget() {
    let input = multipart("mixed", b"--x\nX-Extra: value\n\nbody\n--x--\n");
    let result = girt::mailinfo::parse_with(
        &input,
        Options::default(),
        Limits::default().with_max_headers(5),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Limit("headers"))));
}
#[test]
fn closing_newline_is_charged_to_decoded_limit() {
    let input = multipart("mixed", b"--x\n\nbody\n--x--\n");
    // A bound on the transfer output itself also includes the synthetic closing LF.
    assert!(matches!(
        girt::mailinfo::parse_with(
            &input,
            Options::default(),
            Limits::default().with_max_decoded_bytes(5),
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("decoded bytes"))
    ));
}
#[rstest]
#[case(b"Content-Type: multipart/mixed\n\nbody\n")]
#[case(b"Content-Type: multipart/mixed; boundary=\"\"\n\nbody\n")]
#[case(b"Content-Type: multipart/mixed; boundary=\"unterminated\n\nbody\n")]
fn missing_or_malformed_boundaries_fail_without_artifacts(#[case] input: &[u8]) {
    assert!(matches!(
        girt::mailinfo::parse(input),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn encapsulated_message_containers_are_explicitly_refused() {
    let input = multipart(
        "mixed",
        b"--x\nContent-Type: message/rfc822\n\nFrom: B <b@x>\nSubject: nested\n\nbody\n--x--\n",
    );
    assert!(matches!(
        girt::mailinfo::parse(&input),
        Err(Error::Unsupported("encapsulated message MIME"))
    ));
}

#[rstest]
#[case::split_body_headers(b"--x\nContent-Transfer-Encoding: base64\n\nRnJvbTogQiA8YkB4Pg==\n--x\n\nSubject: inner\n\nbody\n---\npatch\n--x--\n")]
#[case::truncated_nested(
    b"--x\nContent-Type: multipart/mixed; boundary=y\n\npreamble\n--y\n\nbody\n---\npatch\n--x--\n"
)]
#[case::empty_nested(
    b"--x\n\nbody\n--x\nContent-Type: multipart/mixed; boundary=y\n\n--y--\n--x--\n"
)]
#[case::prefixed_delimiter(b"--x-junk\n\nbody\n---\npatch\n--x--trailing\n")]
fn multipart_boundary_and_header_state_match_git(#[case] body: &[u8]) {
    mail_oracle::compare(&multipart("mixed", body), Options::default());
}

fn nested_message(depth: usize, closed: bool) -> Vec<u8> {
    let mut entity = b"Content-Type: text/plain\n\nbody\n---\npatch\n".to_vec();
    for level in (0..depth).rev() {
        let header = format!(
            "Content-Type: multipart/mixed; boundary=b{level}\n\npreamble {level}\n--b{level}\n"
        );
        let tail = if closed {
            format!("--b{level}--\nepilogue {level}\n")
        } else {
            String::new()
        };
        entity = [header.as_bytes(), &entity, tail.as_bytes()].concat();
    }
    [HEADER, &entity].concat()
}
#[rstest]
fn multiple_container_closings_match_git(
    #[values(1, 2, 4)] depth: usize,
    #[values(false, true)] closed: bool,
) {
    mail_oracle::compare(&nested_message(depth, closed), Options::default());
}
#[test]
fn caller_selected_deep_nesting_uses_bounded_heap_state() {
    let input = nested_message(100, true);
    let limits = Limits::default()
        .with_max_mime_depth(100)
        .with_max_mime_parts(101);
    assert!(
        girt::mailinfo::parse_with(&input, Options::default(), limits, &AtomicBool::new(false))
            .is_ok()
    );
    assert!(matches!(
        girt::mailinfo::parse_with(
            &input,
            Options::default(),
            limits.with_max_mime_depth(99),
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("MIME depth"))
    ));
}
#[test]
fn decoded_body_and_metadata_share_an_exact_budget() {
    let input = b"Subject: hi\nContent-Type: multipart/mixed; boundary=x\n\n--x\n\nbody\n--x--\n";
    let limits = Limits::default().with_max_decoded_bytes(8);
    assert!(
        girt::mailinfo::parse_with(input, Options::default(), limits, &AtomicBool::new(false))
            .is_ok()
    );
    assert!(matches!(
        girt::mailinfo::parse_with(
            input,
            Options::default(),
            limits.with_max_decoded_bytes(7),
            &AtomicBool::new(false)
        ),
        Err(Error::Limit("decoded bytes"))
    ));
}
