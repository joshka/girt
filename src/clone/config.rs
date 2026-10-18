use std::fs;
use std::io::{self, Write};

use super::CloneHead;
use crate::fetch::ObjectFilter;
use crate::repository::initial_config;
use crate::{InitKind, ObjectFormat, Repository};

pub(super) fn contents(
    kind: InitKind,
    format: ObjectFormat,
    url: &[u8],
    head: &CloneHead,
    filter: Option<&ObjectFilter>,
) -> Vec<u8> {
    let mut bytes = initial_config(kind, format);
    if filter.is_some() {
        // A partial clone needs version 1 so older Git refuses it rather than misreading it.
        let version = b"repositoryformatversion = 0";
        if let Some(at) = bytes.windows(version.len()).position(|w| w == version) {
            bytes[at + version.len() - 1] = b'1';
        }
    }
    bytes.extend_from_slice(b"[remote \"origin\"]\n\turl = ");
    quoted(&mut bytes, url);
    bytes.extend_from_slice(
        b"\n\tfetch = refs/heads/*:refs/remotes/origin/*\n\tfetch = refs/tags/*:refs/tags/*\n",
    );
    if let Some(filter) = filter {
        bytes.extend_from_slice(b"\tpromisor = true\n\tpartialclonefilter = ");
        let spec = filter.to_string();
        // Specs are usually plain, as Git writes them; quote anything a reader could misparse.
        if spec
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"\\#;".contains(&b))
        {
            bytes.extend_from_slice(spec.as_bytes());
        } else {
            quoted(&mut bytes, spec.as_bytes());
        }
        bytes.push(b'\n');
    }
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
    let _lock_operation = crate::locks::operation();
    let lock = std::path::absolute(repo.git_dir().join("config.lock"))?;
    let destination = repo.git_dir().join("config");
    let file = crate::locks::lock_options().open(&lock)?;
    let identity = crate::locks::capture_identity(&lock, &file)?;
    let mut file = crate::locks::SharedFile::from(file);
    let cleanup = crate::locks::Guard::new(&lock, &file, identity);
    let result = (|| {
        if fs::read(&destination)? != initial_config(kind, repo.object_format()) {
            return Err(io::Error::other(
                "clone initialization configuration changed",
            ));
        }
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        cleanup.check_path()?;
        fs::rename(&lock, &destination)
    })();
    if result.is_err() && cleanup.check_path().is_ok() {
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
        let bytes = contents(InitKind::Worktree, ObjectFormat::Sha1, url, &head, None);
        let config = Config::parse(&bytes).unwrap();
        let remote = Remote::find(&config, b"origin").unwrap().unwrap();
        assert_eq!(remote.fetch_url(), Some(url.as_slice()));
        assert_eq!(remote.fetch_refspecs().specs().len(), 2);
    }

    #[test]
    fn filter_records_a_promisor_remote() {
        let head = CloneHead::Unborn(RefName::new("refs/heads/main").unwrap());
        let filter = ObjectFilter::BlobNone;
        let bytes = contents(
            InitKind::Worktree,
            ObjectFormat::Sha1,
            b"/repo",
            &head,
            Some(&filter),
        );
        let config = Config::parse(&bytes).unwrap();
        assert_eq!(
            config.integer("core", None, "repositoryformatversion"),
            Ok(Some(1))
        );
        assert_eq!(
            config.boolean("remote", Some(b"origin"), "promisor"),
            Ok(Some(true))
        );
        assert_eq!(
            config.string("remote", Some(b"origin"), "partialclonefilter"),
            Some(b"blob:none".as_slice())
        );
    }

    #[test]
    fn preserves_other_writers_config_and_releases_owned_lock() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            InitKind::Bare,
        )
        .unwrap();
        fs::write(repo.git_dir().join("config"), b"keep").unwrap();
        assert!(publish(&repo, InitKind::Bare, b"replace").is_err());
        assert_eq!(fs::read(repo.git_dir().join("config")).unwrap(), b"keep");
        assert!(!repo.git_dir().join("config.lock").exists());
    }
}
