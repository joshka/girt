//! `girt archive` must write what `git archive` writes.
//!
//! Tar is compared byte for byte. Git's zlib and girt's zlib-rs encode deflate streams
//! differently, so tar.gz is compared by its gzip header and decompressed bytes, and zip by every
//! header field and the inflated content of each entry.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(command: &mut Command, home: &Path, directory: &Path) -> Output {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TZ", "UTC")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 -0800")
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 -0800")
        .output()
        .unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = run(Command::new("git").args(args), &self.home, &self.root);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn write(&self, path: &str, content: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Stages a symbolic link to `target` without creating it in the working tree.
    fn symlink(&self, path: &str, target: &[u8]) {
        self.write("target.tmp", target);
        let id = self.git(&["hash-object", "-w", "target.tmp"]);
        fs::remove_file(self.root.join("target.tmp")).unwrap();
        let id = String::from_utf8(id).unwrap();
        self.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("120000,{},{path}", id.trim()),
        ]);
    }

    /// The archives both tools write from `directory` (relative to the root).
    fn archives(&self, directory: &str, args: &[&str]) -> (Output, Output) {
        let directory = self.root.join(directory);
        let git = run(
            Command::new("git").arg("archive").args(args),
            &self.home,
            &directory,
        );
        let girt = run(
            Command::new(env!("CARGO_BIN_EXE_girt"))
                .arg("archive")
                .args(args),
            &self.home,
            &directory,
        );
        assert!(
            git.status.success(),
            "git archive {args:?}: {}",
            String::from_utf8_lossy(&git.stderr)
        );
        assert!(
            girt.status.success(),
            "girt archive {args:?}: {}",
            String::from_utf8_lossy(&girt.stderr)
        );
        (git, girt)
    }
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();
    let fixture = Fixture {
        _temp: temp,
        home,
        root,
    };
    fixture.git(&["init", "-q", "-b", "main"]);
    fixture.write("a.txt", b"hello\n");
    fixture.write("empty", b"");
    fixture.write("bin.dat", b"\0\x01\x02binary\n");
    fixture.write("big.txt", "compressible line\n".repeat(400).as_bytes());
    fixture.write("d/x.sh", b"#!/bin/sh\necho hi\n");
    fixture.write("crlf.txt", b"one\ntwo\n");
    fixture.write("id.txt", b"id $Id$\n");
    fixture.write(
        "version.txt",
        b"$Format:%H %h %T %t %P %p%n%s|%an <%ae> %ad|%ci|%ct %%$ and $Format:%cn$ %H\n",
    );
    fixture.write("ignored/one.log", b"ignored\n");
    fixture.write("ignored-dir/two", b"ignored\n");
    fixture.write("\u{fc}n\u{ef}.txt", b"unicode\n");
    let long = format!("{}/{}", "p".repeat(90), "q".repeat(60));
    fixture.write(&long, b"split into prefix and name\n");
    fixture.write(&"r".repeat(120), b"pax path\n");
    fixture.write(&format!("{}/x", "s".repeat(155)), b"prefix too long\n");
    fixture.write(
        ".gitattributes",
        b"crlf.txt text eol=crlf\nid.txt ident\nversion.txt export-subst\n*.log export-ignore\nignored-dir export-ignore\n",
    );
    fixture.git(&["add", "."]);
    fixture.git(&["update-index", "--chmod=+x", "d/x.sh"]);
    fixture.symlink("link", b"a.txt");
    fixture.symlink("long-link", "t".repeat(150).as_bytes());
    fixture.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000,1111111111111111111111111111111111111111,sub",
    ]);
    fixture.git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "First line\n\nBody.",
    ]);
    fixture.write("a.txt", b"second\n");
    fixture.git(&["-c", "commit.gpgsign=false", "commit", "-qam", "Second"]);
    fixture.git(&[
        "-c",
        "tag.gpgsign=false",
        "tag",
        "-a",
        "v1",
        "-m",
        "Release",
    ]);
    fixture.write("extra.txt", b"added\n");
    fixture
}

const TAR: &[(&str, &[&str])] = &[
    ("", &["HEAD"]),
    ("", &["v1"]),
    ("", &["--mtime=@1234567890", "HEAD^{tree}"]),
    ("", &["--prefix=project/", "HEAD"]),
    ("", &["--prefix=project", "HEAD", "a.txt"]),
    ("", &["HEAD", "d"]),
    ("", &["HEAD", "d/x.sh", "a.txt"]),
    ("", &["HEAD", "*.txt"]),
    ("", &["HEAD:d"]),
    ("d", &["HEAD"]),
    ("", &["--worktree-attributes", "HEAD"]),
    (
        "",
        &[
            "--prefix=a/",
            "--add-file=extra.txt",
            "--prefix=b/",
            "--add-virtual-file=v/w.txt:virtual",
            "HEAD",
            "a.txt",
        ],
    ),
];

#[test]
fn tar_matches_git_byte_for_byte() {
    let fixture = fixture();
    let mut failures = Vec::new();
    for (directory, args) in TAR {
        let (git, girt) = fixture.archives(directory, args);
        if git.stdout != girt.stdout {
            failures.push(format!("{args:?} in {directory:?}"));
        }
    }
    let prefix = format!("--prefix={}/", "l".repeat(101));
    let virtual_file = format!("--add-virtual-file={}:hi", "v".repeat(101));
    let extra = format!("--add-file={}", fixture.root.join("extra.txt").display());
    for args in [
        vec![prefix.as_str(), "HEAD", "a.txt"],
        vec![virtual_file.as_str(), extra.as_str(), "HEAD", "a.txt"],
    ] {
        let (git, girt) = fixture.archives("", &args);
        if git.stdout != girt.stdout {
            failures.push(format!("{args:?}"));
        }
    }
    fixture.git(&["config", "tar.umask", "022"]);
    let (git, girt) = fixture.archives("", &["HEAD"]);
    if git.stdout != girt.stdout {
        failures.push("tar.umask=022".to_owned());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn tar_gz_matches_git_after_decompression() {
    let fixture = fixture();
    for args in [
        &["--format=tar.gz", "HEAD"][..],
        &["--format=tgz", "-9", "HEAD"],
    ] {
        let (git, girt) = fixture.archives("", args);
        assert_eq!(git.stdout.get(..10), girt.stdout.get(..10), "{args:?}");
        assert_eq!(gunzip(&git.stdout), gunzip(&girt.stdout), "{args:?}");
    }
}

#[test]
fn zip_matches_git_except_compressed_bytes() {
    let fixture = fixture();
    for args in [
        &["--format=zip", "HEAD"][..],
        &["--format=zip", "-0", "HEAD"],
        &["--format=zip", "--prefix=project/", "v1"],
        &["--format=zip", "--mtime=@1234567890", "HEAD^{tree}", "d"],
    ] {
        let (git, girt) = fixture.archives("", args);
        assert_eq!(
            zip_entries(&git.stdout),
            zip_entries(&girt.stdout),
            "{args:?}"
        );
    }
}

/// `-o` chooses the format from the file name.
#[test]
fn output_file_chooses_format() {
    let fixture = fixture();
    fixture.archives("", &["-o", "../out-git.zip", "HEAD"]);
    let git_file = fs::read(fixture.root.join("../out-git.zip")).unwrap();
    fixture.archives("", &["-o", "../out-girt.zip", "HEAD"]);
    let girt_file = fs::read(fixture.root.join("../out-girt.zip")).unwrap();
    assert_eq!(zip_entries(&git_file), zip_entries(&girt_file));
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Every field of each entry except compressed sizes and offsets, with inflated content, and the
/// archive comment.
fn zip_entries(zip: &[u8]) -> (Vec<String>, Vec<u8>) {
    let end = zip
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .unwrap();
    let count = usize::from(u16_at(zip, end + 10));
    let mut at = u32_at(zip, end + 16) as usize;
    let comment = zip[end + 22..].to_vec();
    let mut entries = Vec::new();
    for _ in 0..count {
        assert_eq!(&zip[at..at + 4], b"PK\x01\x02");
        let name_len = usize::from(u16_at(zip, at + 28));
        let extra_len = usize::from(u16_at(zip, at + 30));
        let comment_len = usize::from(u16_at(zip, at + 32));
        let name = String::from_utf8_lossy(&zip[at + 46..at + 46 + name_len]).into_owned();
        let extra = zip[at + 46 + name_len..at + 46 + name_len + extra_len].to_vec();
        let method = u16_at(zip, at + 10);
        let local = u32_at(zip, at + 42) as usize;
        let local_name = usize::from(u16_at(zip, local + 26));
        let local_extra = usize::from(u16_at(zip, local + 28));
        let start = local + 30 + local_name + local_extra;
        let stored = &zip[start..start + u32_at(zip, at + 20) as usize];
        let content = if method == 8 {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(stored)
                .read_to_end(&mut out)
                .unwrap();
            out
        } else {
            stored.to_vec()
        };
        entries.push(format!(
            "{name} made_by={:#x} needed={} flags={:#x} method={method} time={:#x} date={:#x} \
             crc={:#x} size={} internal={} external={:#x} extra={extra:?} local={:?} \
             content={content:?}",
            u16_at(zip, at + 4),
            u16_at(zip, at + 6),
            u16_at(zip, at + 8),
            u16_at(zip, at + 12),
            u16_at(zip, at + 14),
            u32_at(zip, at + 16),
            u32_at(zip, at + 24),
            u16_at(zip, at + 36),
            u32_at(zip, at + 38),
            // The local header repeats version, flags, method, time, date, CRC and size.
            (
                u16_at(zip, local + 4),
                u16_at(zip, local + 6),
                u16_at(zip, local + 8),
                u16_at(zip, local + 10),
                u16_at(zip, local + 12),
                u32_at(zip, local + 14),
                u32_at(zip, local + 22),
                &zip[local + 30 + local_name..start],
            ),
        ));
        at += 46 + name_len + extra_len + comment_len;
    }
    (entries, comment)
}
