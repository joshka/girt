//! Disposable original data; Git creates the formats, girt reads them.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{HistoryLimits, ObjectId, PackLimits, ReadLimits, Repository};

pub struct Gate {
    pub started: tokio::sync::oneshot::Sender<()>,
    pub release: std::sync::mpsc::Receiver<()>,
}

pub struct Fixture {
    pub root: tempfile::TempDir,
    tip: ObjectId,
    blob: ObjectId,
}

impl Fixture {
    pub fn new(packed: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &["init", "--bare", "--object-format=sha1", "--template=", "."],
            b"",
        );
        let mut state = 42u32;
        let bytes: Vec<u8> = (0..2 * 1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let blob = ObjectId::for_blob(girt::ObjectFormat::Sha1, &bytes);
        let mut input = format!("blob\nmark :1\ndata {}\n", bytes.len()).into_bytes();
        input.extend(bytes);
        input.extend(b"\n");
        for i in 0..512 {
            input.extend(format!("commit refs/heads/main\ncommitter A <a@example.com> 1700000000 +0000\ndata {}\nnode {i}\n", format!("node {i}").len()).bytes());
            if i == 0 {
                input.extend(b"M 100644 :1 payload\n");
            }
            input.extend(b"\n");
        }
        git(root.path(), &["fast-import", "--quiet"], &input);
        // fast-import writes a pack. Unpack in a separate disposable object directory to obtain
        // genuinely loose storage; then let Git repack only for the packed scenario.
        let pack_dir = root.path().join("objects/pack");
        let pack = std::fs::read_dir(&pack_dir)
            .unwrap()
            .find_map(|e| {
                let path = e.unwrap().path();
                (path.extension().is_some_and(|e| e == "pack")).then_some(path)
            })
            .unwrap();
        let data = std::fs::read(pack).unwrap();
        std::fs::remove_dir_all(&pack_dir).unwrap();
        std::fs::create_dir(&pack_dir).unwrap();
        git(root.path(), &["unpack-objects"], &data);
        if packed {
            git(root.path(), &["repack", "-ad"], b"");
            git(root.path(), &["prune-packed"], b"");
        }
        let tip = String::from_utf8(git(root.path(), &["rev-parse", "refs/heads/main"], b""))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        Self { root, tip, blob }
    }

    pub fn operation(&self, gate: Option<Gate>) {
        let repo = Repository::open(self.root.path()).unwrap();
        let objects = repo.objects(PackLimits::default()).unwrap();
        if let Some(gate) = gate {
            gate.started.send(()).unwrap();
            gate.release.recv().unwrap();
        }
        let blob = objects
            .read(self.blob, ReadLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(blob.data().len(), 2 * 1024 * 1024);
        assert_eq!(
            objects
                .walk(&[self.tip], HistoryLimits::default())
                .unwrap()
                .len(),
            512
        );
    }
}

fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
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
    output.stdout
}
