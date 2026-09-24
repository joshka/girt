use std::fs::{self, OpenOptions};
use std::io::{self, Write};

use super::CloneHead;
use crate::{InitKind, Repository};

pub(super) fn initial(kind: InitKind) -> Vec<u8> {
    format!(
        "[core]\n\trepositoryformatversion = 0\n\tbare = {}\n",
        kind == InitKind::Bare
    )
    .into_bytes()
}

pub(super) fn contents(kind: InitKind, url: &[u8], head: &CloneHead) -> Vec<u8> {
    let mut bytes = initial(kind);
    bytes.extend_from_slice(b"[remote \"origin\"]\n\turl = ");
    quoted(&mut bytes, url);
    bytes.extend_from_slice(
        b"\n\tfetch = refs/heads/*:refs/remotes/origin/*\n\tfetch = refs/tags/*:refs/tags/*\n",
    );
    if let CloneHead::Branch { name, .. } | CloneHead::Unborn(name) = head {
        bytes.extend_from_slice(b"[branch ");
        quoted(&mut bytes, &name.as_bytes()[b"refs/heads/".len()..]);
        bytes.extend_from_slice(b"]\n\tremote = origin\n\tmerge = ");
        quoted(&mut bytes, name.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

fn quoted(output: &mut Vec<u8>, bytes: &[u8]) {
    output.push(b'"');
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => {
                output.push(b'\\');
                output.push(byte);
            }
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\t' => output.extend_from_slice(b"\\t"),
            b'\x08' => output.extend_from_slice(b"\\b"),
            _ => output.push(byte),
        }
    }
    output.push(b'"');
}

// This deliberately only replaces our initializer's exact configuration under config.lock.
// It is not a general editor and never merges another writer's changes.
pub(super) fn publish(repo: &Repository, kind: InitKind, bytes: &[u8]) -> io::Result<()> {
    let lock = repo.git_dir().join("config.lock");
    let destination = repo.git_dir().join("config");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;
    let result = (|| {
        if fs::read(&destination)? != initial(kind) {
            return Err(io::Error::other(
                "clone initialization configuration changed",
            ));
        }
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        fs::rename(&lock, &destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(lock);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::refs::RefName;
    use crate::remote::Remote;

    #[test]
    fn preserves_quoted_url_bytes_and_branch_configuration() {
        let url = b" /repo/\"quote\\tab\tline\n\xff ";
        let head = CloneHead::Unborn(RefName::new("refs/heads/topic").unwrap());
        let bytes = contents(InitKind::Worktree, url, &head);
        let config = Config::parse(&bytes).unwrap();
        let remote = Remote::find(&config, b"origin").unwrap().unwrap();
        assert_eq!(remote.fetch_url(), Some(url.as_slice()));
        assert_eq!(remote.fetch_refspecs().specs().len(), 2);
    }

    #[test]
    fn preserves_other_writers_config_and_releases_owned_lock() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
        fs::write(repo.git_dir().join("config"), b"keep").unwrap();
        assert!(publish(&repo, InitKind::Bare, b"replace").is_err());
        assert_eq!(fs::read(repo.git_dir().join("config")).unwrap(), b"keep");
        assert!(!repo.git_dir().join("config.lock").exists());
    }
}
