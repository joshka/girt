//! Independent discovery of stored reflog names.
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, io};

use crate::refs::{Backend, RefName, ReferenceError, References};

impl References<'_> {
    /// Enumerates stored histories even when their references have been deleted.
    ///
    /// The limit applies to all visited directory entries and decoded names. The result is a
    /// live observation, not an atomic snapshot with reference or object reads. A caller planning
    /// deletion must exclude writers and repeat discovery after acquiring its execution boundary.
    ///
    /// # Errors
    ///
    /// Reports malformed names, symlinks, non-file entries, I/O, cancellation, or an exhausted
    /// entry or reftable stack budget. No partial list is returned on failure.
    pub fn imported_reflog_names(
        &self,
        max_entries: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<RefName>, ReferenceError> {
        if self.repository.reference_backend() == Backend::Reftable {
            return crate::refs::reftable::backend::imported_reflog_names(
                self,
                max_entries,
                cancel,
            );
        }
        let mut names = BTreeSet::new();
        let mut visited = 0;
        let mut roots = BTreeSet::new();
        roots.insert(self.repository.common_dir());
        roots.insert(self.repository.git_dir());
        for root in roots {
            let directory = root.join("logs");
            let mut pending = vec![directory.clone()];
            while let Some(path) = pending.pop() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(ReferenceError::Cancelled);
                }
                let entries = match fs::read_dir(&path) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == io::ErrorKind::NotFound && path == directory => {
                        continue;
                    }
                    Err(source) => return Err(ReferenceError::Io { path, source }),
                };
                for entry in entries {
                    visited += 1;
                    if visited > max_entries {
                        return Err(ReferenceError::Limit("reflog discovery entries"));
                    }
                    let entry = entry.map_err(|source| ReferenceError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    let path = entry.path();
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|source| ReferenceError::Io {
                            path: path.clone(),
                            source,
                        })?;
                    if metadata.is_dir() {
                        pending.push(path);
                    } else if metadata.is_file() {
                        let relative = path
                            .strip_prefix(&directory)
                            .expect("discovered descendant");
                        let bytes = path_bytes(relative)?;
                        let name = RefName::new(bytes).map_err(|_| ReferenceError::Malformed {
                            path: path.clone(),
                            reason: "invalid reflog name",
                        })?;
                        names.insert(name);
                    } else {
                        return Err(ReferenceError::Malformed {
                            path,
                            reason: "non-regular reflog entry",
                        });
                    }
                }
            }
        }
        Ok(names.into_iter().collect())
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Result<Vec<u8>, ReferenceError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Result<Vec<u8>, ReferenceError> {
    path.to_str()
        .map(|text| text.replace('\\', "/").into_bytes())
        .ok_or(ReferenceError::Unsupported("non-UTF-8 reflog path"))
}
