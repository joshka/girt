//! Opting out of fetch's optional reference packing preserves transferred reference values.
use std::fs;

use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};
use girt::{ObjectFormat, Repository};
use rstest::rstest;

#[path = "support/maintenance_git.rs"]
mod oracle;

#[rstest]
#[case::default_sha1(ObjectFormat::Sha1, None)]
#[case::default_sha256(ObjectFormat::Sha256, None)]
#[case::enabled(ObjectFormat::Sha1, Some(true))]
#[case::disabled_sha1(ObjectFormat::Sha1, Some(false))]
#[case::disabled_sha256(ObjectFormat::Sha256, Some(false))]
fn fetch_reference_packing_is_optional(
    #[case] format: ObjectFormat,
    #[case] packing: Option<bool>,
) {
    let root = tempfile::tempdir().unwrap();
    for name in ["origin.git", "local.git"] {
        oracle::git(
            root.path(),
            &[
                "init",
                "--bare",
                "--template=",
                "--initial-branch=main",
                &format!("--object-format={format}"),
                name,
            ],
            b"",
        );
    }
    let origin = root.path().join("origin.git");
    let local = root.path().join("local.git");
    let tree = oracle::git(&origin, &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let commit = oracle::git(&origin, &["commit-tree", tree], b"one\n");
    let commit = std::str::from_utf8(&commit).unwrap().trim();
    // Sixteen is the minimum auto pack-refs threshold with no existing packed-refs file.
    let updates: String = (0..16)
        .map(|index| format!("create refs/heads/topic-{index:02} {commit}\n"))
        .collect();
    oracle::git(&origin, &["update-ref", "--stdin"], updates.as_bytes());
    oracle::git(
        &local,
        &["remote", "add", "origin", origin.to_str().unwrap()],
        b"",
    );
    // Repository::open reads local config only; the empty Environment inherits no variables.
    let repo = Repository::open(&local).unwrap();
    let mut options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
    if let Some(packing) = packing {
        options = options.with_auto_pack_refs(packing);
    }
    let report = workflow::fetch(
        &repo,
        "origin",
        &options,
        &Environment::default(),
        &mut NoCallbacks,
    )
    .unwrap();
    assert_eq!(report.updated.len(), 16);
    let packed = local.join("packed-refs");
    assert_eq!(packed.exists(), packing.unwrap_or(true));
    if packing == Some(false) {
        for index in 0..16 {
            assert_eq!(
                fs::read_to_string(local.join(format!("refs/remotes/origin/topic-{index:02}")))
                    .unwrap()
                    .trim(),
                commit
            );
        }
    }
    for index in 0..16 {
        let name = format!("refs/remotes/origin/topic-{index:02}");
        assert_eq!(
            oracle::git(&local, &["rev-parse", &name], b""),
            format!("{commit}\n").as_bytes()
        );
    }
    oracle::git(&local, &["fsck", "--strict"], b"");
}
