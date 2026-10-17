//! Original fixtures generated with Git's CLI; no upstream implementation or test inputs.
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use girt::{
    InitKind, ObjectFormat, ObjectId, ObjectIdPrefix, PackLimits, Repository, ResolvePrefixError,
};

fn git(path: &std::path::Path, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-global-config"))
        .arg("--git-dir")
        .arg(path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn resolves_ambiguous_loose_packed_and_alternate_ids_like_git() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo");
        let repo = Repository::init(format, &path, InitKind::Bare).unwrap();
        let mut seen = HashMap::new();
        let (a, b) = (0..10000)
            .find_map(|i| {
                let bytes = format!("collision fixture {i}");
                let id = ObjectId::for_blob(format, bytes.as_bytes());
                let key = id.to_string()[..4].to_owned();
                seen.insert(key, (id, bytes.clone()))
                    .map(|old| (old, (id, bytes)))
            })
            .unwrap();
        for (id, bytes) in [&a, &b] {
            assert_eq!(
                git(&path, &["hash-object", "-w", "--stdin"], bytes.as_bytes()),
                id.to_string()
            );
        }
        let prefix = ObjectIdPrefix::from_hex(format, &a.0.to_string()[..4]).unwrap();
        let mut expected = vec![a.0, b.0];
        expected.sort();
        for packed in [0, 2, 1] {
            if packed == 2 {
                let input = format!("{}\n{}\n", a.0, b.0);
                let pack = path.join("objects/pack/pack");
                git(
                    &path,
                    &["pack-objects", pack.to_str().unwrap()],
                    input.as_bytes(),
                );
                for id in &expected {
                    let text = id.to_string();
                    std::fs::remove_file(path.join("objects").join(&text[..2]).join(&text[2..]))
                        .unwrap();
                }
            }
            if packed == 1 {
                let pack = std::fs::read_dir(path.join("objects/pack"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().is_some_and(|e| e == "pack"))
                    .unwrap();
                git(
                    &path,
                    &[
                        "index-pack",
                        "--index-version=1",
                        "-o",
                        pack.with_extension("idx").to_str().unwrap(),
                        pack.to_str().unwrap(),
                    ],
                    b"",
                );
            }
            let objects = repo.objects(PackLimits::default()).unwrap();
            assert_eq!(objects.prefix_matches(prefix).unwrap(), expected);
            match objects.resolve_prefix(prefix).unwrap_err() {
                ResolvePrefixError::Ambiguous { candidates, .. } => {
                    assert_eq!(candidates, expected)
                }
                error => panic!("{error}"),
            }
            assert_eq!(
                git(
                    &path,
                    &[
                        "--no-replace-objects",
                        "rev-parse",
                        &format!("--disambiguate={prefix}")
                    ],
                    b""
                ),
                expected
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            for id in &expected {
                for min in [4, 5, 7, 12, format.digest_len() * 2] {
                    let short = objects.abbreviate(*id, min).unwrap();
                    assert_eq!(
                        short.to_string(),
                        git(
                            &path,
                            &["rev-parse", &format!("--short={min}"), &id.to_string()],
                            b""
                        )
                    );
                    assert_eq!(objects.resolve_prefix(short).unwrap(), *id);
                }
            }
        }
        let borrower_path = dir.path().join("borrower");
        let borrower = Repository::init(format, &borrower_path, InitKind::Bare).unwrap();
        std::fs::write(
            borrower_path.join("objects/info/alternates"),
            format!("{}\n", path.join("objects").display()),
        )
        .unwrap();
        // Duplicate loose/alternate copies must not manufacture ambiguity.
        borrower.loose_objects().write_blob(a.1.as_bytes()).unwrap();
        let objects = borrower.objects(PackLimits::default()).unwrap();
        assert_eq!(objects.prefix_matches(prefix).unwrap(), expected);
        assert_eq!(
            objects
                .resolve_prefix(objects.abbreviate(a.0, 5).unwrap())
                .unwrap(),
            a.0
        );
        let absent =
            ObjectIdPrefix::from_hex(format, "0000000000000000000000000000000000000000").unwrap();
        assert!(matches!(
            objects.resolve_prefix(absent),
            Err(ResolvePrefixError::Missing(_))
        ));
        let other = if format == ObjectFormat::Sha1 {
            ObjectFormat::Sha256
        } else {
            ObjectFormat::Sha1
        };
        assert!(
            objects
                .prefix_matches(ObjectIdPrefix::from_hex(other, "abcd").unwrap())
                .is_err()
        );
    }
}

#[path = "support/short_ids_git.rs"]
mod short_ids_git;

#[test]
fn auto_width_tracks_git_packed_object_count_boundaries() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for count in [16383, 16384, 65535, 65536] {
            let dir = tempfile::tempdir().unwrap();
            let repo = Repository::init(format, dir.path().join("repo"), InitKind::Bare).unwrap();
            short_ids_git::populate(repo.git_dir(), count);
            let objects = repo.objects(PackLimits::default()).unwrap();
            let id = ObjectId::for_blob(format, &0u32.to_le_bytes());
            let git_short = git(
                repo.git_dir(),
                &["rev-parse", "--short", &id.to_string()],
                b"",
            );
            assert_eq!(
                objects
                    .abbreviate(id, objects.auto_abbrev_len())
                    .unwrap()
                    .to_string(),
                git_short,
                "{format} {count}"
            );
            assert_eq!(
                objects.auto_abbrev_len(),
                if count < 16384 {
                    7
                } else if count < 65536 {
                    8
                } else {
                    9
                }
            );
        }
    }
}
