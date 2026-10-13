//! Private object storage; publication always uses the original validated receive artifact.
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{PushError, PushFailure, PushReport};
use crate::Repository;

pub(super) struct Quarantine {
    directory: tempfile::TempDir,
    identity: DirectoryIdentity,
    pub(super) repository: Repository,
}

impl Quarantine {
    pub(super) fn new(primary: &Repository) -> std::io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("tmp_objdir-incoming-")
            .tempdir_in(primary.object_dir())?;
        fs::create_dir(directory.path().join("info"))?;
        let mut alternate = quoted_path(primary.object_dir())
            .as_encoded_bytes()
            .to_vec();
        alternate.push(b'\n');
        fs::write(directory.path().join("info/alternates"), alternate)?;
        let repository = primary.with_private_object_directory(directory.path());
        let identity = DirectoryIdentity::read(directory.path())?;
        Ok(Self {
            directory,
            identity,
            repository,
        })
    }

    pub(super) fn path(&self) -> &Path {
        self.directory.path()
    }

    /// Only the known private installation topology is accepted; hook-created/changed objects
    /// require a separate validated migration contract, rather than silently dropping them.
    pub(super) fn observe(&self) -> std::io::Result<Vec<FileInput>> {
        if DirectoryIdentity::read(self.path())? != self.identity {
            return Err(std::io::Error::other(
                "receive quarantine directory replaced",
            ));
        }
        let mut inputs = Vec::new();
        let mut pending = vec![self.path().to_owned()];
        while let Some(path) = pending.pop() {
            if inputs.len() >= 16 {
                return Err(std::io::Error::other("unsupported quarantine contents"));
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                for entry in fs::read_dir(&path)? {
                    pending.push(entry?.path());
                }
            } else if !metadata.is_file() {
                return Err(std::io::Error::other("unsupported quarantine file type"));
            }
            inputs.push(FileInput {
                path: path
                    .strip_prefix(self.path())
                    .expect("quarantine descendant")
                    .to_owned(),
                len: metadata.len(),
                modified: metadata.modified()?,
                #[cfg(unix)]
                identity: (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                ),
            });
        }
        inputs.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(inputs)
    }

    pub(super) fn close(self) -> std::io::Result<()> {
        match DirectoryIdentity::read(self.path()) {
            Ok(identity) if identity == self.identity => self.directory.close(),
            _ => {
                // Disarm TempDir's path-based Drop: the replacement is not ours to remove.
                drop(self.directory.keep());
                Err(std::io::Error::other(
                    "receive quarantine replaced or unavailable; cleanup refused",
                ))
            }
        }
    }

    /// Ordinary returns report cleanup errors. Panic/process-exit cleanup remains best effort.
    pub(super) fn finish(
        self,
        result: Result<PushReport, PushError>,
        pending: PushReport,
    ) -> Result<PushReport, PushError> {
        let path = self.path().to_owned();
        match self.close() {
            Ok(()) => result,
            Err(source) => {
                let report = match &result {
                    Ok(report) => report.clone(),
                    Err(PushError::Uncertain { report, .. }) => (**report).clone(),
                    Err(PushError::NotSent(_)) => pending,
                };
                Err(PushError::Uncertain {
                    cause: PushFailure::QuarantineCleanup {
                        path,
                        source,
                        failure: result.err().map(Box::new),
                    },
                    report: Box::new(report),
                })
            }
        }
    }
}

/// Git's quoted alternate-path syntax protects separators, quotes and arbitrary Unix path bytes.
pub(super) fn quoted_path(path: &Path) -> OsString {
    let mut output = String::from("\"");
    for byte in path.as_os_str().as_encoded_bytes() {
        // Octal quoting also avoids alternate-list separators on every supported hook platform.
        output.push_str(&format!("\\{byte:03o}"));
    }
    output.push('"');
    output.into()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FileInput {
    path: PathBuf,
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

#[derive(Eq, PartialEq)]
struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl DirectoryIdentity {
    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::other("quarantine is not a directory"));
        }
        Ok(Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}
