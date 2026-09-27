//! Original fixtures compared with the installed Git executable; no upstream test sources used.
use std::process::Command;

use girt::Config;
use rstest::rstest;

#[rstest]
#[case::blank_line(b"first  \\\n\n", b"first  ")]
#[case::trailing_space(b"first  \\\n  \n", b"first  ")]
#[case::comment(b"first\t \\\n# comment\n", b"first\t ")]
#[case::ordinary_trailing_space(b"first  \n", b"first")]
#[case::quoted(b"\"first  \"\\\n\n", b"first  ")]
#[case::empty_quote_suffix(b"first  \\\n\"\"\n", b"first  ")]
#[case::empty(b"  \\\n\n", b"")]
#[case::empty_quote_prefix(b"\"\"  \\\n\n", b"")]
#[case::continued_text(b"first  \\\nsecond  \n", b"first  second")]
#[case::crlf(b"first  \\\r\n\r\n", b"first  ")]
fn continuation_whitespace_matches_git(#[case] source: &[u8], #[case] expected: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    let bytes = [b"[push]\n pushOption = ".as_slice(), source].concat();
    std::fs::write(&path, &bytes).unwrap();
    let observed = Command::new("git")
        .args(["config", "--file"])
        .arg(&path)
        .args(["--null", "--get-all", "push.pushOption"])
        .output()
        .unwrap();
    assert!(observed.status.success(), "{:?}", observed.stderr);
    assert_eq!(observed.stdout, [expected, b"\0"].concat());
    let config = Config::parse(&bytes).unwrap();
    assert_eq!(
        config.value("push", None, "pushOption"),
        Some(Some(expected))
    );
}

#[test]
fn continued_empty_values_remain_distinct_reset_entries() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    let bytes = b"[push]\n pushOption = first  \\\n\n pushOption = \\\n\n pushOption = \"\"\\\n\n pushOption = last\n";
    std::fs::write(&path, bytes).unwrap();
    let observed = Command::new("git")
        .args(["config", "--file"])
        .arg(&path)
        .args(["--null", "--get-all", "push.pushOption"])
        .output()
        .unwrap();
    assert!(observed.status.success(), "{:?}", observed.stderr);
    assert_eq!(observed.stdout, b"first  \0\0\0last\0");
    let config = Config::parse(bytes).unwrap();
    assert_eq!(
        config
            .values("push", None, "pushOption")
            .collect::<Vec<_>>(),
        [
            Some(b"first  ".as_slice()),
            Some(b"".as_slice()),
            Some(b"".as_slice()),
            Some(b"last".as_slice())
        ]
    );
}
