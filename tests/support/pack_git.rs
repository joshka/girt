//! Independent Git CLI workload shared by pack interoperability tests and Criterion.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use girt::{ObjectId, ObjectKind, Repository};

pub fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
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
        .env("GIT_AUTHOR_NAME", "Pack Author")
        .env("GIT_AUTHOR_EMAIL", "pack@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Pack Author")
        .env("GIT_COMMITTER_EMAIL", "pack@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git required for pack compatibility fixtures");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn id(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

pub struct Fixture {
    pub root: tempfile::TempDir,
    pub repo: Repository,
    pub records: Vec<(ObjectId, ObjectKind, Vec<u8>)>,
    pub ordinary: ObjectId,
    pub delta: ObjectId,
    pub index_path: PathBuf,
}

impl Fixture {
    /// Builds varied but similar blobs plus a tree, commit, and tag. Git chooses actual deltas;
    /// verify-pack offsets let the fixture assert that the requested encoding was generated.
    pub fn new(format: girt::ObjectFormat, ofs: bool, count: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &[
                "init",
                "--bare",
                &format!("--object-format={format}"),
                "--template=",
                "--initial-branch=main",
                ".",
            ],
            b"",
        );
        let mut records = Vec::new();
        for variant in 0..count {
            let payload = (0..1024)
                .map(|line| {
                    format!(
                        "original pack fixture line {line:04}: {}\n",
                        if line == variant { variant } else { 0 }
                    )
                })
                .collect::<String>()
                .into_bytes();
            let oid = id(&git(
                root.path(),
                &["hash-object", "-w", "--stdin"],
                &payload,
            ));
            records.push((oid, ObjectKind::Blob, payload));
        }
        let tree_input = records
            .iter()
            .enumerate()
            .map(|(i, (oid, _, _))| format!("100644 blob {oid}\tfile-{i:04}\n"))
            .collect::<String>();
        let tree = id(&git(root.path(), &["mktree"], tree_input.as_bytes()));
        let commit = id(&git(
            root.path(),
            &["commit-tree", &tree.to_string()],
            b"Original pack fixture\n",
        ));
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit.to_string()],
            b"",
        );
        let tag_data = format!(
            "object {commit}\ntype commit\ntag packed\ntagger Pack Author <pack@example.com> 1700000000 +0000\n\nPacked fixture\n"
        );
        let tag = id(&git(root.path(), &["mktag"], tag_data.as_bytes()));
        git(
            root.path(),
            &["update-ref", "refs/tags/packed", &tag.to_string()],
            b"",
        );
        for (oid, kind) in [
            (tree, ObjectKind::Tree),
            (commit, ObjectKind::Commit),
            (tag, ObjectKind::Tag),
        ] {
            records.push((
                oid,
                kind,
                git(
                    root.path(),
                    &["cat-file", kind.as_str(), &oid.to_string()],
                    b"",
                ),
            ));
        }
        let input = records
            .iter()
            .map(|(oid, _, _)| format!("{oid}\n"))
            .collect::<String>();
        let mut args = vec![
            "pack-objects",
            "--stdout",
            "--no-reuse-delta",
            "--no-reuse-object",
            "--window=64",
            "--depth=32",
        ];
        if ofs {
            args.push("--delta-base-offset");
        }
        let pack = git(root.path(), &args, input.as_bytes());
        let checksum = pack[pack.len() - format.digest_len()..]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let pack_path = root
            .path()
            .join(format!("objects/pack/pack-{checksum}.pack"));
        fs::write(&pack_path, &pack).unwrap();
        git(
            root.path(),
            &[
                "index-pack",
                "--index-version=2",
                pack_path.to_str().unwrap(),
            ],
            b"",
        );
        let index_path = pack_path.with_extension("idx");
        let report = git(
            root.path(),
            &["verify-pack", "-v", index_path.to_str().unwrap()],
            b"",
        );
        let report = std::str::from_utf8(&report).unwrap();
        let mut ordinary = None;
        let mut delta = None;
        for line in report.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() == 7 {
                let offset: usize = fields[4].parse().unwrap();
                assert_eq!((pack[offset] >> 4) & 7, if ofs { 6 } else { 7 });
                delta = Some(fields[0].parse().unwrap());
            } else if fields.len() == 5 && fields[1] == "blob" {
                ordinary = Some(fields[0].parse().unwrap());
            }
        }
        git(root.path(), &["prune-packed"], b"");
        let repo = Repository::open(root.path()).unwrap();
        Self {
            root,
            repo,
            records,
            ordinary: ordinary.unwrap(),
            delta: delta.expect("fixture must exercise actual deltas"),
            index_path,
        }
    }
}
