//! Line-ending and `ident` conversion compared with what `git add` stores and `git checkout-index`
//! writes, across `core.autocrlf` × `core.eol` × attributes × content shapes.
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::collections::BTreeMap;
use std::path::Path;

use girt::filter::{CrlfWarning, Error, Staged};
use girt::workflow::Environment;
use girt::{ObjectId, Repository};
use pack_git::{git, git_output};

const CONTENTS: &[(&str, &[u8])] = &[
    ("lf", b"a\nb\n"),
    ("crlf", b"a\r\nb\r\n"),
    ("mixed", b"a\r\nb\n"),
    ("lonecr", b"a\rb\n"),
    ("trailingcr", b"a\r\nb\r"),
    ("nul", b"a\r\n\0b\n"),
    ("noeol", b"ab"),
    ("empty", b""),
    ("control", b"a\x01\x01b\r\n"),
    ("eofmark", b"a\r\nb\r\n\x1a"),
    ("ident", b"x $Id$\r\ny $Id: old $ $Id: two words $\n"),
];

const ATTRIBUTES: &[&str] = &[
    "",
    "text",
    "-text",
    "text=auto",
    "eol=lf",
    "eol=crlf",
    "text eol=lf",
    "text eol=crlf",
    "text=auto eol=lf",
    "text=auto eol=crlf",
    "binary",
    "ident",
    "text ident eol=crlf",
];

const CONFIGS: &[(Option<&str>, Option<&str>)] = &[
    (None, None),
    (None, Some("lf")),
    (None, Some("crlf")),
    (Some("true"), None),
    (Some("true"), Some("lf")),
    (Some("input"), None),
    (Some("input"), Some("crlf")),
    (Some("false"), Some("crlf")),
];

fn environment() -> Environment {
    Environment::default().with("GIT_ATTR_NOSYSTEM", "1")
}

fn repository(root: &Path, config: (Option<&str>, Option<&str>), attributes: &str) {
    git(root, &["init", "-q", "."], b"");
    git(root, &["config", "core.attributesFile", "absent"], b"");
    if let Some(autocrlf) = config.0 {
        git(root, &["config", "core.autocrlf", autocrlf], b"");
    }
    if let Some(eol) = config.1 {
        git(root, &["config", "core.eol", eol], b"");
    }
    std::fs::write(root.join(".gitattributes"), format!("* {attributes}\n")).unwrap();
}

/// Git's `safecrlf=warn` warnings from `git add`, by path.
fn warnings(stderr: &[u8]) -> BTreeMap<String, CrlfWarning> {
    let mut found = BTreeMap::new();
    for line in String::from_utf8_lossy(stderr).lines() {
        let Some(rest) = line.strip_prefix("warning: in the working copy of '") else {
            continue;
        };
        let (path, message) = rest.split_once("', ").unwrap();
        let warning = if message.starts_with("CRLF will be replaced by LF") {
            CrlfWarning::CrlfToLf
        } else if message.starts_with("LF will be replaced by CRLF") {
            CrlfWarning::LfToCrlf
        } else {
            panic!("unexpected warning {line}");
        };
        found.insert(path.to_owned(), warning);
    }
    found
}

#[test]
fn storing_matches_git_add() {
    let mut failures = Vec::new();
    let (mut converted, mut warned) = (0, 0);
    for &config in CONFIGS {
        for attributes in ATTRIBUTES {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            repository(root, config, attributes);
            for (name, bytes) in CONTENTS {
                std::fs::write(root.join(name), bytes).unwrap();
            }
            let names: Vec<&str> = CONTENTS.iter().map(|(name, _)| *name).collect();
            let mut args = vec!["add", "--"];
            args.extend(&names);
            let output = git_output(root, &args, b"");
            assert!(output.status.success());
            let expected_warnings = warnings(&output.stderr);

            let repo = Repository::open(root).unwrap();
            let mut filters = repo.filters(&environment()).unwrap();
            for (name, bytes) in CONTENTS {
                let stored = git(root, &["cat-file", "blob", &format!(":{name}")], b"");
                let ours = filters
                    .to_repository(name.as_bytes(), bytes, Staged::Absent)
                    .unwrap();
                let case = format!("{config:?} [{attributes}] {name}");
                converted += usize::from(*stored != **bytes);
                warned += usize::from(expected_warnings.contains_key(*name));
                if *ours.bytes != *stored {
                    failures.push(format!("{case}: stored {:?} vs git {stored:?}", ours.bytes));
                }
                if ours.warning != expected_warnings.get(*name).copied() {
                    failures.push(format!(
                        "{case}: warning {:?} vs git {:?}",
                        ours.warning,
                        expected_warnings.get(*name)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        converted > 100 && warned > 100,
        "{converted} converted, {warned} warned"
    );
}

#[test]
fn writing_matches_git_checkout_index() {
    let mut failures = Vec::new();
    let mut converted = 0;
    for &config in CONFIGS {
        for attributes in ATTRIBUTES {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            repository(root, config, attributes);
            let mut ids = Vec::new();
            for (name, bytes) in CONTENTS {
                let id = git(
                    root,
                    &["hash-object", "-w", "--no-filters", "--stdin"],
                    bytes,
                );
                let id = String::from_utf8(id).unwrap().trim().to_owned();
                git(
                    root,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("100644,{id},{name}"),
                    ],
                    b"",
                );
                ids.push(id.parse::<ObjectId>().unwrap());
            }
            git(root, &["checkout-index", "-a", "-f"], b"");

            let repo = Repository::open(root).unwrap();
            let mut filters = repo.filters(&environment()).unwrap();
            for ((name, bytes), id) in CONTENTS.iter().zip(ids) {
                let written = std::fs::read(root.join(name)).unwrap();
                let ours = filters.to_worktree(name.as_bytes(), bytes, id).unwrap();
                converted += usize::from(written != **bytes);
                if *ours != *written {
                    failures.push(format!(
                        "{config:?} [{attributes}] {name}: wrote {ours:?} vs git {written:?}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(converted > 100, "{converted} converted");
}

#[test]
fn staged_cr_keeps_auto_files_unconverted() {
    for (staged, file) in [
        (&b"a\r\nb\r\n"[..], &b"a\r\nb\r\nc\r\n"[..]),
        (b"a\r\nb\n", b"a\r\nb\r\n"),
        (b"a\nb\n", b"a\r\nb\r\n"),
        (b"old\rtext\n", b"new\r\n"),
    ] {
        for attributes in ["text=auto", "", "text"] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            repository(root, (Some("true"), None), attributes);
            let id = git(
                root,
                &["hash-object", "-w", "--no-filters", "--stdin"],
                staged,
            );
            let id = String::from_utf8(id).unwrap().trim().to_owned();
            git(
                root,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{id},f"),
                ],
                b"",
            );
            std::fs::write(root.join("f"), file).unwrap();
            git(root, &["add", "f"], b"");
            let expected = git(root, &["cat-file", "blob", ":f"], b"");

            let repo = Repository::open(root).unwrap();
            let mut filters = repo.filters(&environment()).unwrap();
            let ours = filters
                .to_repository(b"f", file, Staged::Blob(staged))
                .unwrap();
            assert_eq!(*ours.bytes, *expected, "[{attributes}] staged {staged:?}");

            git(root, &["add", "--renormalize", "f"], b"");
            let expected = git(root, &["cat-file", "blob", ":f"], b"");
            let ours = filters
                .to_repository(b"f", file, Staged::Renormalize)
                .unwrap();
            assert_eq!(*ours.bytes, *expected, "renormalize [{attributes}]");
        }
    }
}

#[test]
fn safecrlf_true_refuses_as_git_does() {
    for (attributes, autocrlf, file, refused) in [
        ("", "input", &b"a\r\n"[..], true),
        ("", "true", b"a\n", true),
        ("", "true", b"a\r\n", false),
        ("text eol=lf", "false", b"a\r\nb\n", true),
        ("-text", "input", b"a\r\n", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        repository(root, (Some(autocrlf), None), attributes);
        git(root, &["config", "core.safecrlf", "true"], b"");
        std::fs::write(root.join("f"), file).unwrap();
        let output = git_output(root, &["add", "f"], b"");
        assert_eq!(
            !output.status.success(),
            refused,
            "git [{attributes}] {autocrlf}"
        );

        let repo = Repository::open(root).unwrap();
        let mut filters = repo.filters(&environment()).unwrap();
        let ours = filters.to_repository(b"f", file, Staged::Absent);
        assert_eq!(
            matches!(ours, Err(Error::UnsafeCrlf { .. })),
            refused,
            "girt [{attributes}] {autocrlf}"
        );
        // `--renormalize` neither refuses nor warns.
        assert!(
            git_output(root, &["add", "--renormalize", "f"], b"")
                .status
                .success()
        );
        let ours = filters
            .to_repository(b"f", file, Staged::Renormalize)
            .unwrap();
        assert_eq!(ours.warning, None);
    }
}
