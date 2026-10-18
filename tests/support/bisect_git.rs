//! Generated histories for bisect compatibility tests, written through Git.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Runs Git with isolated configuration, fixed identities and dates.
pub fn run(directory: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Bisect Author")
        .env("GIT_AUTHOR_EMAIL", "bisect@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Bisect Author")
        .env("GIT_COMMITTER_EMAIL", "bisect@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("Git required for bisect compatibility tests")
}

/// Runs Git and returns trimmed standard output, failing on a nonzero exit.
pub fn git(directory: &Path, args: &[&str]) -> String {
    let output = run(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A small deterministic generator (xorshift) so graphs need no extra dependency.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// A repository whose commits `c0..cN` form a random graph, with `main` at the last commit
/// checked out. Each commit edits one of five files; dates are equal when `same_dates` is set
/// so ties exercise discovery order.
pub struct Graph {
    pub root: tempfile::TempDir,
}

impl Graph {
    pub fn random(seed: u64, count: usize, same_dates: bool) -> Self {
        let mut rng = Rng::new(seed);
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &["init", "-q", "-b", "main", "--template=", "."],
        );

        let mut script = String::new();
        for i in 0..count {
            let date = if same_dates {
                1_700_000_000
            } else {
                1_700_000_000 + i * 10 + rng.below(30)
            };
            let message = format!("c{i}\n");
            script.push_str(&format!(
                "commit refs/heads/c{i}\nmark :{}\ncommitter A <a@e> {date} +0000\ndata {}\n{message}",
                i + 1,
                message.len()
            ));
            if i > 0 {
                let first = if rng.chance(80) {
                    i - 1 - rng.below(i.min(4))
                } else {
                    rng.below(i)
                };
                script.push_str(&format!("from :{}\n", first + 1));
                if i > 1 && rng.chance(25) {
                    let other = rng.below(i);
                    if other != first {
                        script.push_str(&format!("merge :{}\n", other + 1));
                    }
                }
            }
            let content = format!("{i}\n");
            script.push_str(&format!(
                "M 644 inline f{}\ndata {}\n{content}\n",
                rng.below(5),
                content.len()
            ));
        }

        let mut child = Command::new("git")
            .current_dir(root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());

        let last = format!("c{}", count - 1);
        git(root.path(), &["branch", "-f", "main", &last]);
        git(root.path(), &["checkout", "-q", "-f", "main"]);
        Self { root }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    pub fn id(&self, revision: &str) -> girt::ObjectId {
        git(self.path(), &["rev-parse", revision]).parse().unwrap()
    }
}
