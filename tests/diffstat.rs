//! Byte-for-byte observations of Git's numstat rendering.
#[path = "support/apply_git.rs"]
mod oracle;

use girt::diffstat::{Counts, Diffstat, FileStat, Limits};

#[test]
fn numstat_matches_git_for_text_binary_empty_and_quoted_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    oracle::git(root, &["init", "-q"], b"");
    for (path, data) in [
        ("a", &b"old\n"[..]),
        ("binary", &b"\0old"[..]),
        ("zé", &b"old\n"[..]),
    ] {
        std::fs::write(root.join(path), data).unwrap();
    }
    oracle::git(root, &["add", "."], b"");
    oracle::git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "base"],
        b"",
    );
    for (path, data) in [
        ("a", &b"new\nextra\n"[..]),
        ("binary", &b"\0newxx"[..]),
        ("empty", &b""[..]),
        ("zé", &b"new\n"[..]),
    ] {
        std::fs::write(root.join(path), data).unwrap();
    }
    oracle::git(root, &["add", "."], b"");
    let files = vec![
        file(
            b"a",
            None,
            Counts::Text {
                added: 2,
                removed: 1,
            },
        ),
        file(
            b"binary",
            None,
            Counts::Binary {
                old_bytes: 4,
                new_bytes: 6,
            },
        ),
        file(
            b"empty",
            None,
            Counts::Text {
                added: 0,
                removed: 0,
            },
        ),
        file(
            "zé".as_bytes(),
            None,
            Counts::Text {
                added: 1,
                removed: 1,
            },
        ),
    ];
    let stat = Diffstat::new(files, Limits::default()).unwrap();
    for nul in [false, true] {
        let mut args = vec!["diff", "--cached", "--numstat"];
        if nul {
            args.push("-z");
        }
        assert_eq!(stat.numstat(nul).unwrap(), oracle::git(root, &args, b""));
    }
}

#[test]
fn renamed_paths_match_git() {
    for (old, new) in [
        ("old", "new"),
        ("dir/old", "dir/new"),
        ("old/file", "new/file"),
        ("dir/old/file", "dir/new/file"),
        ("a/x/b", "a/b"),
        ("a/b", "a/x/b"),
        ("a/b/c", "a/c"),
        ("a/b/c/d", "a/c/d"),
        ("old/inner/file", "new/outer/file"),
        ("old space/file", "new space/file"),
        ("old-é/file", "new-é/file"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        oracle::git(root, &["init", "-q"], b"");
        let source = root.join(old);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"unique contents\n").unwrap();
        oracle::git(root, &["add", "."], b"");
        oracle::git(
            root,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "base"],
            b"",
        );
        let destination = root.join(new);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::rename(source, destination).unwrap();
        oracle::git(root, &["add", "-A"], b"");
        let stat = Diffstat::new(
            vec![file(
                new.as_bytes(),
                Some(old.as_bytes()),
                Counts::Text {
                    added: 0,
                    removed: 0,
                },
            )],
            Limits::default(),
        )
        .unwrap();
        for nul in [false, true] {
            let mut args = vec!["diff", "--cached", "-M", "--numstat"];
            if nul {
                args.push("-z");
            }
            assert_eq!(
                stat.numstat(nul).unwrap(),
                oracle::git(root, &args, b""),
                "{old} -> {new}, nul={nul}"
            );
        }
    }
}

#[test]
fn validates_input_and_output_limits() {
    let entry = file(
        b"file",
        None,
        Counts::Text {
            added: u64::MAX,
            removed: u64::MAX,
        },
    );
    for limits in [
        Limits::default().with_max_files(0),
        Limits::default().with_max_path_bytes(3),
    ] {
        assert!(Diffstat::new(vec![entry.clone()], limits).is_err());
    }
    let stat = Diffstat::new(vec![entry], Limits::default().with_max_output_bytes(4)).unwrap();
    assert!(stat.numstat(false).is_err());
    assert!(stat.numstat(true).is_err());
    for invalid in [&b""[..], &b"a\0b"[..]] {
        assert!(
            Diffstat::new(
                vec![file(
                    invalid,
                    None,
                    Counts::Text {
                        added: 0,
                        removed: 0
                    }
                )],
                Limits::default()
            )
            .is_err()
        );
    }
}

fn file(path: &[u8], source: Option<&[u8]>, counts: Counts) -> FileStat {
    FileStat {
        path: path.to_vec(),
        source_path: source.map(<[u8]>::to_vec),
        counts,
    }
}
