//! Original mailbox bytes compared with Git's isolated mailsplit executable.
#[path = "support/apply_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use girt::mailbox::{Error, Limits, Options, split_with};
use rstest::rstest;

const ENVELOPE: &[u8] = b"From writer Mon Sep 17 00:00:00 2001\n";

fn git_split(input: &[u8], options: Options) -> Result<Vec<Vec<u8>>, ()> {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    std::fs::create_dir(root.join("out")).unwrap();
    std::fs::write(root.join("input"), input).unwrap();
    let mut args = vec!["mailsplit", "-oout"];
    if options.allow_bare {
        args.push("-b");
    }
    if options.keep_cr {
        args.push("--keep-cr");
    }
    args.push("input");
    let output = oracle::git_output(root, &args, b"");
    if !output.status.success() {
        return Err(());
    }
    let count: usize = std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    Ok((1..=count)
        .map(|number| std::fs::read(root.join(format!("out/{number:04}"))).unwrap())
        .collect())
}

#[rstest]
#[case::single(b"From writer Mon Sep 17 00:00:00 2001\nSubject: a\n\nbody\n")]
#[case::crlf(b"From writer Mon Sep 17 00:00:00 2001\r\nSubject: a\r\n\r\nbody\r\n")]
#[case::no_final_newline(b"From writer Mon Sep 17 00:00:00 2001\nbody")]
#[case::initial_whitespace(b" \t\r\nFrom writer Mon Sep 17 00:00:00 2001\nbody\n")]
#[case::escaped_body(
    b"From writer Mon Sep 17 00:00:00 2001\n\nFrom ordinary words\n>From escaped\nbody\rtest\n"
)]
#[case::two(b"From a Mon Sep 17 00:00:00 2001\nbody\n\nFrom b Mon Sep 17 00:00:00 2001\nsecond\n")]
#[case::bare(b"Subject: a\n\nbody\nFrom a Mon Sep 17 00:00:00 2001\nbody\n")]
#[case::bare_whitespace(b" \t\nSubject: a\n\nbody\n")]
#[case::binary_body(b"From writer Mon Sep 17 00:00:00 2001\n\n\0\xff\r\n")]
#[case::empty(b"")]
#[case::whitespace_only(b" \t\r\n")]
#[case::form_feed(b"\x0cFrom a Mon Sep 17 00:00:00 2001\nbody\n")]
#[case::vertical_tab(b"\x0bFrom a Mon Sep 17 00:00:00 2001\nbody\n")]
#[case::double_space_clock(b"From a Mon Sep 17  00:00:00 2001\nbody\n")]
fn split_bytes_match_git(
    #[case] input: &[u8],
    #[values(false, true)] bare: bool,
    #[values(false, true)] keep_cr: bool,
) {
    let options = Options::default()
        .with_allow_bare(bare)
        .with_keep_cr(keep_cr);
    let actual = split_with(input, options, Limits::default(), &AtomicBool::new(false));
    let expected = git_split(input, options);
    match expected {
        Ok(messages) => assert_eq!(actual.unwrap(), messages),
        Err(()) => assert!(actual.is_err()),
    }
}

#[rstest]
#[case("Mon Sep 17 00:00:00 2001")]
#[case("Xxx Xxx 99 99:99:99 2001")]
#[case("Mon Sep 17 00:00 2001")]
#[case("Mon Sep 17 0:00:00 2001")]
#[case("Mon Sep x7 12:34 2001")]
#[case("Mon Sep 123 12:34 2001")]
#[case("Mon Sep 17 00:00:00 0099")]
#[case("Mon Sep 17 00:00:00 90")]
#[case("Mon Sep 17 00:00:00 91")]
#[case("Mon Sep 17 00:00:00 91x")]
#[case("Mon Sep 17 00:00:00 2001 trailing")]
#[case("Mon Sep 17 00:00:00 20010")]
#[case("Mon Sep 17 00:00:00 +0000 2001")]
#[case("Mon Sep 17 00:0:00 2001")]
#[case("Mon Sep 17 00:00:0 2001")]
#[case("Mon Sep 17 00:00:xx 2001")]
#[case("abc abc abc 12:34 2001")]
#[case("12 12:34 2001")]
#[case("not a date")]
#[case("1 00:00 91")]
#[case("Mon Sep 17 000:00 2001")]
#[case("Mon Sep 17 12:34 +2001")]
#[case("Mon Sep 17 12:34 -2001")]
#[case("Mon Sep 17 12:34:56:78 2001")]
#[case("Mon Sep 17\t12:34 2001")]
#[case("Mon Sep 17  00:00:00 2001")]
#[case("Mon Sep 17 0:00 2001")]
#[case("Mon Sep 17x12:34 2001")]
#[case("Mon Sep 17 12x34:56 2001")]
#[case("Mon Sep 17 12:34:56 2001 remote foo:bar")]
#[case("Mon Sep 17 12:34:56+2001")]
#[case("Mon Sep 17  12:34 2001")]
#[case("Mon Sep 17 12:34  2001")]
#[case("Mon Sep  17 12:34 2001")]
#[case("Mon Sep 17\x0c12:34 2001")]
#[case("Mon Sep 17\x0b12:34 2001")]
#[case("Mon Sep 17 12:34\t2001")]
fn envelope_boundaries_match_git(#[case] date: &str) {
    let mut input = ENVELOPE.to_vec();
    input.extend_from_slice(format!("\nbody\nFrom a {date}\nnext\n").as_bytes());
    assert_eq!(
        girt::mailbox::split(&input).unwrap(),
        git_split(&input, Options::default()).unwrap()
    );
}

#[test]
fn exact_normalized_limits_and_one_byte_failures() {
    let input = [ENVELOPE, b"body\r\n"].concat();
    let size = input.len() - 1;
    let exact = Limits::default()
        .with_max_input_bytes(input.len())
        .with_max_message_bytes(size)
        .with_max_output_bytes(size)
        .with_max_messages(1);
    assert!(split_with(&input, Options::default(), exact, &AtomicBool::new(false)).is_ok());
    for (limits, name) in [
        (exact.with_max_input_bytes(input.len() - 1), "input bytes"),
        (exact.with_max_message_bytes(size - 1), "message bytes"),
        (exact.with_max_output_bytes(size - 1), "output bytes"),
        (exact.with_max_messages(0), "messages"),
    ] {
        let result = split_with(&input, Options::default(), limits, &AtomicBool::new(false));
        assert!(matches!(result, Err(Error::Limit(found)) if found == name));
    }
}

#[test]
fn later_failure_discards_the_series() {
    let input = [ENVELOPE, b"body\n", ENVELOPE, b"second body\n"].concat();
    let result = split_with(
        &input,
        Options::default(),
        Limits::default().with_max_messages(1),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Limit("messages"))));
    let result = split_with(
        &input,
        Options::default(),
        Limits::default().with_max_output_bytes(input.len() - 1),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Limit("output bytes"))));
}

#[test]
fn cancellation_precedes_admission() {
    let result = split_with(
        b"",
        Options::default(),
        Limits::default(),
        &AtomicBool::new(true),
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[test]
fn generated_git_patch_series_splits_without_changing_messages() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    oracle::git(root, &["init", "-q"], b"");
    oracle::git(root, &["commit", "--allow-empty", "-qm", "First"], b"");
    oracle::git(root, &["commit", "--allow-empty", "-qm", "Second"], b"");
    let input = oracle::git(
        root,
        &["format-patch", "--stdout", "--always", "--root", "-2"],
        b"",
    );
    let actual = girt::mailbox::split(&input).unwrap();
    assert_eq!(actual.len(), 2);
    assert_eq!(actual, git_split(&input, Options::default()).unwrap());
}
