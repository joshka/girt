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
            if !nul {
                assert_eq!(
                    stat.stat(girt::diffstat::StatLayout::default()).unwrap(),
                    oracle::git(root, &["diff", "--cached", "-M", "--stat"], b""),
                    "stat: {old} -> {new}"
                );
            }
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

#[test]
fn human_stat_matches_git_width_scaling() {
    use girt::diffstat::StatLayout;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    oracle::git(root, &["init", "-q"], b"");
    std::fs::create_dir_all(root.join("very-long-directory")).unwrap();
    let path = "very-long-directory/a-change.txt";
    let old = (0..200)
        .map(|line| format!("old{line}\n"))
        .collect::<String>();
    std::fs::write(root.join(path), old.clone()).unwrap();
    let short_old = (0..12).map(|line| format!("s{line}\n")).collect::<String>();
    std::fs::write(root.join("short.txt"), short_old).unwrap();
    let sum_old = (0..100)
        .map(|line| format!("t{line}\n"))
        .collect::<String>();
    std::fs::write(root.join("sum.txt"), sum_old).unwrap();
    oracle::git(root, &["add", "."], b"");
    oracle::git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "base"],
        b"",
    );
    let new = (0..200)
        .map(|line| format!("new{line}\n"))
        .collect::<String>();
    std::fs::write(root.join(path), new).unwrap();
    let short_new = (6..12)
        .map(|line| format!("s{line}\n"))
        .chain((0..6).map(|line| format!("added{line}\n")))
        .collect::<String>();
    std::fs::write(root.join("short.txt"), short_new).unwrap();
    let sum_new = (50..100)
        .map(|line| format!("t{line}\n"))
        .chain((0..60).map(|line| format!("added{line}\n")))
        .collect::<String>();
    std::fs::write(root.join("sum.txt"), sum_new).unwrap();
    oracle::git(root, &["add", "."], b"");
    let stat = Diffstat::new(
        vec![
            file(
                b"short.txt",
                None,
                Counts::Text {
                    added: 6,
                    removed: 6,
                },
            ),
            file(
                b"sum.txt",
                None,
                Counts::Text {
                    added: 60,
                    removed: 50,
                },
            ),
            file(
                path.as_bytes(),
                None,
                Counts::Text {
                    added: 200,
                    removed: 200,
                },
            ),
        ],
        Limits::default(),
    )
    .unwrap();
    for width in [10, 20, 25, 30] {
        let actual = stat.stat(StatLayout { width }).unwrap();
        let expected = oracle::git(root, &["diff", "--cached", &format!("--stat={width}")], b"");
        assert_eq!(
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(&expected),
            "width={width}"
        );
    }
}

#[test]
fn human_stat_aligns_combined_counts_that_are_wider_than_either_side() {
    use girt::diffstat::StatLayout;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    oracle::git(root, &["init", "-q"], b"");
    std::fs::write(root.join("b"), "old\n".repeat(4)).unwrap();
    oracle::git(root, &["add", "."], b"");
    oracle::git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "base"],
        b"",
    );
    std::fs::write(root.join("a"), "new\n".repeat(9)).unwrap();
    std::fs::write(root.join("b"), "new\n".repeat(6)).unwrap();
    oracle::git(root, &["add", "."], b"");
    let stat = Diffstat::new(
        vec![
            file(
                b"a",
                None,
                Counts::Text {
                    added: 9,
                    removed: 0,
                },
            ),
            file(
                b"b",
                None,
                Counts::Text {
                    added: 6,
                    removed: 4,
                },
            ),
        ],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        stat.stat(StatLayout::default()).unwrap(),
        oracle::git(root, &["diff", "--cached", "--stat"], b"")
    );
}

#[test]
fn human_stat_omits_zero_summary_sides_like_git() {
    use girt::diffstat::StatLayout;

    for (path, data, counts) in [
        (
            "added",
            b"one\ntwo\n".as_slice(),
            Counts::Text {
                added: 2,
                removed: 0,
            },
        ),
        (
            "deleted",
            b"".as_slice(),
            Counts::Text {
                added: 0,
                removed: 2,
            },
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        oracle::git(root, &["init", "-q"], b"");
        if path == "deleted" {
            std::fs::write(root.join(path), b"one\ntwo\n").unwrap();
        }
        oracle::git(root, &["add", "."], b"");
        let mut commit = vec!["-c", "commit.gpgsign=false", "commit", "-qm", "base"];
        if path == "added" {
            commit.push("--allow-empty");
        }
        oracle::git(root, &commit, b"");
        if path == "added" {
            std::fs::write(root.join(path), data).unwrap();
        } else {
            std::fs::remove_file(root.join(path)).unwrap();
        }
        oracle::git(root, &["add", "-A"], b"");
        let stat =
            Diffstat::new(vec![file(path.as_bytes(), None, counts)], Limits::default()).unwrap();
        assert_eq!(
            stat.stat(StatLayout::default()).unwrap(),
            oracle::git(root, &["diff", "--cached", "--stat"], b""),
            "{path}"
        );
    }
}

#[test]
fn human_stat_matches_git_binary_rows() {
    use girt::diffstat::StatLayout;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    oracle::git(root, &["init", "-q"], b"");
    std::fs::write(root.join("binary"), b"\0abc").unwrap();
    std::fs::write(root.join("empty"), b"").unwrap();
    oracle::git(root, &["add", "."], b"");
    oracle::git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "base"],
        b"",
    );
    std::fs::write(root.join("binary"), b"\0xyz12").unwrap();
    std::fs::remove_file(root.join("empty")).unwrap();
    std::fs::write(root.join("added"), b"\0newlong").unwrap();
    oracle::git(root, &["add", "-A"], b"");
    let stat = Diffstat::new(
        vec![
            file(
                b"added",
                None,
                Counts::Binary {
                    old_bytes: 0,
                    new_bytes: 8,
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
        ],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        stat.stat(StatLayout::default()).unwrap(),
        oracle::git(root, &["diff", "--cached", "--stat"], b"")
    );
}
