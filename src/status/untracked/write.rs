//! Bounded UNTR encoding of observations captured by the descriptor-relative status walk.
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt as _;

use super::{Capture, Record};
use crate::{ObjectId, Repository, index};

impl Capture {
    pub(in crate::status) fn encode(
        &self,
        repo: &Repository,
        excludes: [ObjectId; 2],
        exclude_stats: [index::Stat; 2],
        listed: &[crate::status::UntrackedPath],
        started: u32,
        limits: index::Limits,
    ) -> Result<index::UntrackedCache, index::Error> {
        let mut records = self
            .directories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Queued child workers may finish after their parent's directory observation. Merge
        // the completed report so collapsed directories and nested repositories are never lost.
        for entry in listed {
            let boundary = entry.path.iter().rposition(|byte| *byte == b'/');
            let parent = boundary.map_or(&b""[..], |position| &entry.path[..position]);
            let name = boundary.map_or(entry.path.as_slice(), |position| {
                &entry.path[position + 1..]
            });
            if let Some(record) = records.get_mut(parent) {
                let mut name = name.to_vec();
                if entry.kind != crate::status::UntrackedKind::File {
                    name.push(b'/');
                }
                record.untracked.push(name);
            }
        }
        for record in records.values_mut() {
            record.untracked.sort_unstable();
            record.untracked.dedup();
        }
        let mut children: BTreeMap<&[u8], Vec<&[u8]>> = BTreeMap::new();
        for path in records.keys().filter(|path| !path.is_empty()) {
            let parent = path
                .iter()
                .rposition(|byte| *byte == b'/')
                .map_or(&b""[..], |position| &path[..position]);
            children.entry(parent).or_default().push(path);
        }
        let mut stack = vec![&b""[..]];
        let mut ordered = Vec::new();
        while let Some(path) = stack.pop() {
            let record = records.get(path).ok_or(index::Error::Malformed {
                offset: 0,
                reason: "missing captured directory",
            })?;
            ordered.push((path, record));
            if let Some(children) = children.get(path) {
                stack.extend(children.iter().rev().copied());
            }
        }
        if ordered.len() > limits.max_entries {
            return Err(index::Error::Limit("untracked directories"));
        }
        let root = repo.worktree().ok_or(index::Error::Malformed {
            offset: 0,
            reason: "missing working tree",
        })?;
        let root = root.canonicalize().map_err(|_| index::Error::Malformed {
            offset: 0,
            reason: "working-tree identity unavailable",
        })?;
        let mut identity = b"Location ".to_vec();
        identity.extend_from_slice(root.as_os_str().as_bytes());
        identity.extend_from_slice(if cfg!(target_os = "macos") {
            b", system Darwin\0"
        } else {
            b", system Linux\0"
        });
        let mut output = Output {
            bytes: Vec::new(),
            limits,
        };
        output.number(identity.len())?;
        output.add(&identity)?;
        for stat in exclude_stats {
            output.stat(stat)?;
        }
        output.add(&6u32.to_be_bytes())?;
        for id in excludes {
            output.add(id.as_bytes())?;
        }
        output.add(b".gitignore\0")?;
        output.number(ordered.len())?;
        for (path, record) in &ordered {
            output.number(record.untracked.len())?;
            output.number(children.get(path).map_or(0, Vec::len))?;
            let name = path.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
            output.string(name)?;
            for name in &record.untracked {
                output.string(name)?;
            }
        }
        let valid = |record: &Record| {
            record.valid
                && record.stat.mtime.seconds < started
                && record.stat.ctime.seconds < started
        };
        output.bitmap(
            &ordered
                .iter()
                .map(|(_, record)| valid(record))
                .collect::<Vec<_>>(),
        )?;
        output.bitmap(
            &ordered
                .iter()
                .map(|(_, record)| record.check_only)
                .collect::<Vec<_>>(),
        )?;
        output.bitmap(
            &ordered
                .iter()
                .map(|(_, record)| record.exclude.is_some())
                .collect::<Vec<_>>(),
        )?;
        for (_, record) in &ordered {
            if valid(record) {
                output.stat(record.stat)?;
            }
        }
        for (_, record) in &ordered {
            if let Some(id) = record.exclude {
                output.add(id.as_bytes())?;
            }
        }
        output.add(&[0])?;
        index::UntrackedCache::parse(repo.object_format(), &output.bytes, limits)
    }
}

struct Output {
    bytes: Vec<u8>,
    limits: index::Limits,
}
impl Output {
    fn add(&mut self, bytes: &[u8]) -> Result<(), index::Error> {
        if bytes.len() > self.limits.max_bytes.saturating_sub(self.bytes.len()) {
            return Err(index::Error::Limit("untracked cache bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn string(&mut self, bytes: &[u8]) -> Result<(), index::Error> {
        if bytes.len() > self.limits.max_path_bytes || bytes.contains(&0) {
            return Err(index::Error::Limit("untracked cache string"));
        }
        self.add(bytes)?;
        self.add(&[0])
    }
    fn number(&mut self, mut value: usize) -> Result<(), index::Error> {
        let mut bytes = vec![(value & 127) as u8];
        value >>= 7;
        while value != 0 {
            value -= 1;
            bytes.push(128 | (value & 127) as u8);
            value >>= 7;
        }
        bytes.reverse();
        self.add(&bytes)
    }
    fn bitmap(&mut self, bits: &[bool]) -> Result<(), index::Error> {
        let count =
            u32::try_from(bits.len()).map_err(|_| index::Error::Limit("untracked bitmap"))?;
        let words = bits.len().div_ceil(64);
        self.add(&count.to_be_bytes())?;
        self.add(
            &u32::try_from(words + 1)
                .map_err(|_| index::Error::Limit("untracked bitmap"))?
                .to_be_bytes(),
        )?;
        self.add(&((words as u64) << 33).to_be_bytes())?;
        for chunk in bits.chunks(64) {
            let word = chunk
                .iter()
                .enumerate()
                .fold(0u64, |word, (bit, set)| word | (u64::from(*set) << bit));
            self.add(&word.to_be_bytes())?;
        }
        self.add(&0u32.to_be_bytes())
    }
    fn stat(&mut self, stat: index::Stat) -> Result<(), index::Error> {
        for word in [
            stat.ctime.seconds,
            stat.ctime.nanoseconds,
            stat.mtime.seconds,
            stat.mtime.nanoseconds,
            stat.device,
            stat.inode,
            stat.uid,
            stat.gid,
            stat.size,
        ] {
            self.add(&word.to_be_bytes())?;
        }
        Ok(())
    }
}

#[cfg(test)]
use crate::layout_git as oracle;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_reuses_encoded_directory_listings() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            temporary.path().join("r"),
            crate::InitKind::Worktree,
        )
        .unwrap();
        let root = repo.worktree().unwrap();
        for (key, value) in [
            ("core.ignoreCase", "false"),
            ("core.precomposeUnicode", "false"),
            ("core.untrackedCache", "false"),
            ("core.excludesFile", "/girt-absent-excludes"),
        ] {
            oracle::git(root, &["config", key, value], b"");
        }
        std::fs::write(root.join("tracked"), b"tracked\n").unwrap();
        // Cross both the 64-bit bitmap boundary and the offset-varint's first byte.
        for position in 0..130 {
            let directory = root.join(format!("d{position:03}"));
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("new"), b"new\n").unwrap();
        }

        std::fs::create_dir_all(root.join("pure/nested")).unwrap();
        std::fs::write(root.join("pure/nested/new"), b"new\n").unwrap();
        std::fs::write(root.join(".gitignore"), b"*.hidden\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(root.join(".gitignore"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))
            .unwrap();
        oracle::git(root, &["add", "tracked", ".gitignore"], b"");
        let expected = oracle::git(root, &["status", "--porcelain"], b"");
        oracle::git(root, &["config", "core.untrackedCache", "true"], b"");
        let repo = Repository::open(root).unwrap();
        let rules = repo
            .ignore_rules(&crate::workflow::Environment::default())
            .unwrap();
        let capture = std::sync::Arc::new(Capture::default());
        let mut scan = crate::status::Scan::new(
            &repo,
            crate::status::Baseline::Head,
            crate::status::UntrackedFiles::Collapsed,
            &rules,
        );
        scan.capture = Some(capture.clone());
        let report = scan.run().unwrap();
        // A synthetic future cutoff makes every captured directory non-racy without a
        // clock-dependent sleep. Production refresh uses its lock's actual timestamp.
        let cache = capture
            .encode(
                &repo,
                rules.untracked_excludes(repo.object_format()),
                [index::Stat::default(); 2],
                &report.untracked,
                u32::MAX,
                index::Limits::default(),
            )
            .unwrap();
        let mut edit = repo.edit_index(index::Limits::default()).unwrap();
        edit.set_untracked_cache(&cache).unwrap();
        edit.commit().unwrap();
        std::fs::File::options()
            .write(true)
            .open(repo.git_dir().join("index"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
            .unwrap();
        let output = oracle::attempt_with_env(
            root,
            &["status", "--porcelain"],
            b"",
            &[("GIT_TRACE2_PERF", std::ffi::OsStr::new("1"))],
        );
        assert!(output.status.success());
        assert_eq!(output.stdout, expected);
        let trace = String::from_utf8_lossy(&output.stderr);
        assert!(trace.contains("opendir:0"), "{trace}");
    }
}
