//! Commit message cleanup compared with `git stripspace` and `git commit` on an original corpus.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::message::{Cleanup, CommentPrefix, cleanup};

/// Messages written for these tests: blank-line runs, comments, whitespace kinds and scissors.
const CORPUS: &[&[u8]] = &[
    b"",
    b"\n",
    b"subject",
    b"subject\n",
    b"\n\n  subject  \n\n\n\tbody\t\n\n",
    b"subject\r\n\r\nbody \r\n",
    b"a\x0b\x0c \nb\xc2\xa0\n",
    b"a\0b  \n",
    b"# only a comment\n#\n",
    b"subject\n# comment\n\n# comment\nbody\n",
    b" #indented\n#\ttab\n##double\nlast",
    b"// slash\n//\n/x\n; semi\n",
    b"subject\n\n\n\n# ------------------------ >8 ------------------------\ndiff\n",
    b"subject\n#  ------------------------ >8 ------------------------\nkept\n",
    b"subject\n# ------------------------ >8 ------------------------ x\nkept\n",
    b"subject\n// ------------------------ >8 ------------------------\ncut\n",
    b"subject\n# ------------------------ >8 ------------------------ \ncut\n",
    b"subject\n# ------------------------ >8 ------------------------\t\r\ncut\n",
    b"subject\n  #\nb # ------------------------ >8 ------------------------\n",
];

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Message Author")
        .env("GIT_AUTHOR_EMAIL", "message@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Message Author")
        .env("GIT_COMMITTER_EMAIL", "message@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git required for message compatibility tests");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn whitespace_and_strip_match_stripspace() {
    let directory = tempfile::tempdir().unwrap();
    for prefix in [&b"#"[..], b"//", b";"] {
        let setting = format!(
            "core.commentString={}",
            std::str::from_utf8(prefix).unwrap()
        );
        let comment = CommentPrefix::new(prefix).unwrap();
        for message in CORPUS {
            let whitespace = git(directory.path(), &["-c", &setting, "stripspace"], message);
            let strip = git(
                directory.path(),
                &["-c", &setting, "stripspace", "-s"],
                message,
            );
            assert_eq!(
                cleanup(message, Cleanup::Whitespace, &comment),
                whitespace,
                "{message:?}"
            );
            assert_eq!(
                cleanup(message, Cleanup::Strip, &comment),
                strip,
                "{prefix:?} {message:?}"
            );
        }
    }
}

/// `git commit -e -F` with `commit.cleanup=scissors` stores what [`Cleanup::Scissors`] returns.
#[test]
fn scissors_matches_commit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q", "--template=", "."], b"");
    for prefix in ["#", "//"] {
        let comment = CommentPrefix::new(prefix.as_bytes()).unwrap();
        let setting = format!("core.commentString={prefix}");
        for message in CORPUS {
            let expected = cleanup(message, Cleanup::Scissors, &comment);
            // Git aborts an empty commit and refuses NUL bytes in a message.
            if expected.is_empty() || message.contains(&0) {
                continue;
            }
            std::fs::write(root.join("message"), message).unwrap();
            git(
                root,
                &[
                    "-c",
                    "commit.cleanup=scissors",
                    "-c",
                    &setting,
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-e",
                    "-F",
                    "message",
                ],
                b"",
            );
            let stored = git(root, &["cat-file", "commit", "HEAD"], b"");
            let body = stored
                .windows(2)
                .position(|pair| pair == b"\n\n")
                .map(|at| &stored[at + 2..])
                .unwrap();
            assert_eq!(body, expected, "{prefix} {message:?}");
        }
    }
}

/// Messages written for trailer tests: block detection, continuations, comments and dividers.
const TRAILER_CORPUS: &[&[u8]] = &[
    b"",
    b"subject",
    b"subject\n",
    b"subject\n\n",
    b"subject\n\n\n",
    b"subject\nbody",
    b"Key: value\n",
    b"subject\nKey: value\n",
    b"subject\n\nbody\n\nA: b\nC-d: e\n",
    b"subject\n\nA: b\n  continued\n\tagain\nB: c\n",
    b"subject\n\nA: b\n  continued\nC :e\nnot a trailer\n",
    b"subject\n\nA: 1\nx\ny\nz\n",
    b"subject\n\nSigned-off-by: b\nx\ny\nz\n",
    b"subject\n\nSigned-off-by: b\nx\ny\nz\nw\n",
    b"subject\n\nSigned-off-by: 1\n  c\n  d\n  e\nx\ny\nz\n",
    b"subject\n\nSigned-off-by: 1\n  c\n  d\n  e\nx\ny\nz\nw\n",
    b"subject\n\nsigned-off-BY: 1\nx\ny\nz\n",
    b"subject\n\nSigned-off-by : 1\nx\ny\nz\n",
    b"subject\n\nSigned-off-by:1\nx\ny\nz\n",
    b"subject\n\n(cherry picked from commit abc)\nx\ny\n",
    b"subject\n\n(cherry picked from commit abc)\nx\ny\nz\nw\n",
    b"subject\n\n(cherry picked from commit\nx\ny\nz\n",
    b"subject\n\nA: 1\n(cherry picked from commit x)\n",
    b"subject\n\nA: 1\n# comment\n#d\n",
    b"subject\n\nA: 1\n\n# comment\n",
    b"subject\n\nbody\n# comment\n",
    b"subject\n\nA: 1\n# comment\nx\n",
    b"subject\n\nA: 1\n#c\nB: 2\n",
    b"subject\n\nA: 1\n  \nB: 2\n",
    b"subject\n\nA: 1\n# ------------------------ >8 ------------------------\nB: 2\n",
    b"subject\n\nA: 1\n---\nfoo\n",
    b"subject\n\nA: 1\n--- \nB: 3\n",
    b"subject\n\nA: 1\n---x\nB: 3\n",
    b"subject\n\nA: 1",
    b"subject\n\nA: 1\n\n\n",
    b"\n\nsubject\n\nA: 1\n",
    b"# c\nsubject\n\nA: 1\n",
    b"A: 1\n\nB: 2\n",
    b"subject\n\n A: b\nB: c\n",
    b"subject\n\nA_b: c\n",
    b"subject\n\nA b: c\n",
    b"subject\n\n1x: c\n-x: c\nx-: c\n",
    b"subject\n\nA:\n",
    b"subject\n\nA: 1\n:x\n",
    b"subject\n\nhttp://example.com\n",
    b"subject\n\nA= 3\nB# c\n",
    b"subject\n\nA: 1\nB: 2\nA: 3\nC: 4\n",
    b"subject\n\nSigned-off-by: a\nfoo\n",
    b"subject\n\nx: 1\nxy :2\nFo: 3\n",
    b"subject\n\nA: one\n two\n",
    b"subject\n\nA:\n two\n three\n",
    b"subject\n\nA:  \n\ttwo\n",
    b"subject\n\nA: one\n# c\n two\nB: 2\n",
];

/// Configurations for trailer tests, as `-c` arguments.
const TRAILER_CONFIGS: &[&[&str]] = &[
    &[],
    &["trailer.separators==#"],
    &["trailer.separators=%:"],
    &["trailer.x.key=Y", "trailer.f.key=Foo"],
    &["trailer.sob.key=Signed-off-by", "trailer.sob.where=start"],
    &["trailer.a.key=Acked-by #", "trailer.separators=:#"],
    &["trailer.where=start"],
    &["trailer.where=after", "trailer.ifexists=replace"],
    &["trailer.where=before", "trailer.ifexists=replace"],
    &["trailer.where=end", "trailer.ifexists=replace"],
    &["trailer.ifexists=addIfDifferent"],
    &["trailer.ifexists=add", "trailer.where=after"],
    &["trailer.ifexists=doNothing", "trailer.ifmissing=doNothing"],
    &[
        "trailer.where=after",
        "trailer.a.where=before",
        "trailer.a.ifExists=Add",
    ],
    &["trailer.where=BOGUS", "trailer.ifexists=Replace"],
    &["core.commentChar=;"],
    &["trailer.x.key=Foo: ", "trailer.y.key=Bar :"],
];

/// `--trailer` argument sets applied to every corpus message.
const TRAILER_ARGUMENTS: &[&[&str]] = &[
    &[],
    &["K: v"],
    &["a: 9", "Z: z"],
    &["A: 1", "a: 1", "A: 3"],
    &["Signed-off-by: a", "Signed-off-by: a", "sig: b"],
    &["sob: me", "so: y", "Signed-off-by-me: x"],
    &["K=v", "L% w", "M# x", "N"],
    &["x: 1", "Fo: 4", "A : 3 "],
    &["A: one", "A: one two", "y: b"],
];

fn config_arguments(config: &[&str]) -> Vec<String> {
    config
        .iter()
        .flat_map(|setting| ["-c".to_owned(), (*setting).to_owned()])
        .collect()
}

fn girt_config(config: &[&str]) -> girt::Config {
    let mut text = String::new();
    for setting in config {
        let (name, value) = setting.split_once('=').unwrap();
        let (section, variable) = name.rsplit_once('.').unwrap();
        match section.split_once('.') {
            Some((section, subsection)) => {
                text.push_str(&format!("[{section} \"{subsection}\"]\n"))
            }
            None => text.push_str(&format!("[{section}]\n")),
        }
        text.push_str(&format!("\t{variable} = \"{value}\"\n"));
    }
    girt::Config::parse(text.as_bytes()).unwrap()
}

#[test]
fn trailers_match_interpret_trailers() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    // Each configuration runs a few hundred Git processes; run configurations side by side.
    std::thread::scope(|scope| {
        for config in TRAILER_CONFIGS {
            scope.spawn(move || check_trailers(root, config));
        }
    });
}

/// Messages whose trailers Git 2.56 reads differently from earlier versions: before 2.56 a
/// `key://…` line, such as a URL, counts as a trailer. girt follows 2.56; `src/message/tests.rs`
/// pins that behaviour, and older Git skips these comparisons.
const CHANGED_IN_2_56: &[&[u8]] = &[b"subject\n\nhttp://example.com\n"];

/// The `(major, minor)` version of the Git under test.
fn git_version(root: &Path) -> (u32, u32) {
    let output = git(root, &["--version"], b"");
    let text = String::from_utf8(output).unwrap();
    let version = text.trim().trim_start_matches("git version ");
    let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

fn check_trailers(root: &Path, config: &[&str]) {
    use girt::message::{Trailer, TrailerConfig, add_trailers, trailers};

    let before_2_56 = git_version(root) < (2, 56);
    {
        let settings = config_arguments(config);
        let girt_config = TrailerConfig::from_config(&girt_config(config)).unwrap();
        for message in TRAILER_CORPUS {
            if before_2_56 && CHANGED_IN_2_56.contains(message) {
                continue;
            }
            let mut args: Vec<&str> = settings.iter().map(String::as_str).collect();
            args.extend(["interpret-trailers", "--parse"]);
            let expected = git(root, &args, message);
            let mut parsed = Vec::new();
            for trailer in trailers(message, &girt_config) {
                let mut line = trailer.key.clone();
                let separator = girt_config_separator(config);
                if !trailer
                    .key
                    .trim_ascii_end()
                    .last()
                    .is_some_and(|last| separator.contains(last))
                {
                    line.push(separator[0]);
                    line.push(b' ');
                }
                line.extend_from_slice(&trailer.value);
                line.push(b'\n');
                parsed.extend(line);
            }
            assert_eq!(
                String::from_utf8_lossy(&parsed),
                String::from_utf8_lossy(&expected),
                "--parse {config:?} {:?}",
                String::from_utf8_lossy(message)
            );

            for arguments in TRAILER_ARGUMENTS {
                let mut args: Vec<&str> = settings.iter().map(String::as_str).collect();
                args.push("interpret-trailers");
                for argument in *arguments {
                    args.extend(["--trailer", argument]);
                }
                let expected = git(root, &args, message);
                let new: Vec<_> = arguments
                    .iter()
                    .map(|argument| {
                        Trailer::parse_argument(argument.as_bytes(), &girt_config).unwrap()
                    })
                    .collect();
                assert_eq!(
                    String::from_utf8_lossy(&add_trailers(message, &new, &girt_config)),
                    String::from_utf8_lossy(&expected),
                    "{config:?} {arguments:?} {:?}",
                    String::from_utf8_lossy(message)
                );
            }
        }
    }
}

fn girt_config_separator(config: &[&str]) -> Vec<u8> {
    config
        .iter()
        .rev()
        .find_map(|setting| setting.strip_prefix("trailer.separators="))
        .unwrap_or(":")
        .as_bytes()
        .to_vec()
}
