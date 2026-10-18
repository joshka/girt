//! Git CLI artifact comparisons shared by original mailinfo integration fixtures.
use std::sync::atomic::AtomicBool;

use girt::mailinfo::{Limits, Message, Options, Subject};

use super::oracle as git_oracle;

fn metadata(info: &Message) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in [
        ("Author", &info.author),
        ("Email", &info.email),
        ("Subject", &info.subject),
        ("Date", &info.date),
    ] {
        if let Some(value) = value {
            out.extend_from_slice(format!("{name}: {value}\n").as_bytes());
        }
    }
    out.push(b'\n');
    out
}
pub fn compare(input: &[u8], options: Options) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let mut args = vec!["mailinfo"];
    match options.subject {
        Subject::Keep => args.push("-k"),
        Subject::KeepNonPatch => args.push("-b"),
        _ => {}
    }
    if options.scissors {
        args.push("--scissors");
    }
    if options.message_id {
        args.push("--message-id");
    }
    args.extend_from_slice(&["message", "patch"]);
    let expected = git_oracle::git(root, &args, input);
    // Git's CLI repeats an empty Subject: line for the terminal newline of an in-body
    // subject under -k. The structured API exposes the single unfolded subject value.
    let mut seen_subject = false;
    let expected: Vec<u8> = expected
        .split_inclusive(|&byte| byte == b'\n')
        .filter(|line| {
            if line.starts_with(b"Subject: ") {
                if seen_subject && *line == b"Subject: \n" {
                    return false;
                }
                seen_subject = true;
            }
            true
        })
        .flatten()
        .copied()
        .collect();
    let actual =
        girt::mailinfo::parse_with(input, options, Limits::default(), &AtomicBool::new(false))
            .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&metadata(&actual)),
        String::from_utf8_lossy(&expected)
    );
    assert_eq!(actual.message, std::fs::read(root.join("message")).unwrap());
    assert_eq!(actual.patch, std::fs::read(root.join("patch")).unwrap());
}
