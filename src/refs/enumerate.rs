use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::store::{io_error, malformed, parse_loose, read_optional};
use super::{RefName, ReferenceError, References, Target};

/// A full reference name and its stored target, without symbolic resolution or tag peeling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    /// Exact reference-name bytes; enumeration excludes pseudorefs such as HEAD.
    pub name: RefName,
    /// Effective loose-over-packed value. Symbolic targets may be missing or cyclic.
    pub target: Target,
}

impl References<'_> {
    /// Lists all `refs/` names in bytewise name order, without resolving symbolic refs.
    ///
    /// Includes branches, tags, other shared namespaces, and the current worktree's private
    /// refs. Excludes HEAD, other pseudorefs, other worktrees' private refs, dot-prefixed entries
    /// and `.lock` entries. Empty directories contribute no refs. Loose targets shadow packed
    /// targets, even if symbolic; a malformed loose target fails instead of falling back.
    ///
    /// Reads and validates packed-refs once, then walks loose files without locks. The owned
    /// result is stable after return but is not a point-in-time snapshot: concurrent creation or
    /// deletion may be included or missed, and values can come from different times. A loose
    /// file that disappears during traversal is ignored; a previously read packed value can
    /// remain in the result. Callers must use an expected value when acting on these observations.
    /// Memory use is proportional to packed data, discovered refs, and pending directories;
    /// there is no configurable resource limit.
    ///
    /// # Errors
    ///
    /// Reports I/O errors, invalid names or targets, conflicting effective names, filesystem
    /// symlinks and non-regular files, or malformed/unsupported packed data. Returns no partial
    /// list on error and does not modify storage.
    pub fn list(&self) -> Result<Vec<Reference>, ReferenceError> {
        self.enumerate(None)
    }

    /// Lists a full name and its descendants, using `/` as the namespace boundary.
    ///
    /// Use `refs/heads` for branches and `refs/tags` for tags. `refs/heads/topic` includes that
    /// exact ref or its descendants, but not `refs/heads/topic-two`. Missing namespaces return
    /// an empty list. Only selected loose paths are read; packed-refs is always fully validated.
    /// Ordering, symbolic behavior, private namespaces and live-read guarantees match
    /// [`Self::list`].
    ///
    /// # Errors
    ///
    /// Reports errors from [`Self::list`] within the selected loose namespace or anywhere in
    /// packed-refs. HEAD is unsupported here; use [`Self::read`] for pseudorefs.
    ///
    /// ```
    /// # #[cfg(unix)] {
    /// use girt::refs::{Expected, RefName, Target};
    /// use girt::{InitKind, ObjectId, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(directory.path().join("repo"), InitKind::Bare)?;
    /// let refs = repo.references()?;
    /// let tag = RefName::new(b"refs/tags/example")?;
    /// let value = Target::Direct(ObjectId::Sha1([1; 20]));
    /// // Reference storage does not require the target object to exist.
    /// refs.update_without_reflog(&tag, value.clone(), Expected::Absent)?;
    /// let tags = refs.list_namespace(&RefName::new(b"refs/tags")?)?;
    /// assert_eq!(tags[0].name, tag);
    /// refs.delete_without_reflog(&tag, Expected::Value(tags[0].target.clone()))?;
    /// assert!(refs.list()?.is_empty());
    /// # }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_namespace(&self, namespace: &RefName) -> Result<Vec<Reference>, ReferenceError> {
        if namespace.as_bytes() == b"HEAD" {
            return Err(ReferenceError::Unsupported("enumerating pseudorefs"));
        }
        self.enumerate(Some(namespace))
    }

    fn enumerate(&self, namespace: Option<&RefName>) -> Result<Vec<Reference>, ReferenceError> {
        let mut entries: BTreeMap<_, _> = self
            .packed()?
            .into_iter()
            .filter(|(name, _)| selected(name.as_bytes(), namespace))
            .map(|(name, id)| (name, Target::Direct(id)))
            .collect();
        let separate = self.repository.git_dir() != self.repository.common_dir();
        collect_loose(
            self.repository.common_dir(),
            namespace,
            if separate {
                LooseScope::Shared
            } else {
                LooseScope::All
            },
            &mut entries,
        )?;
        if separate {
            collect_loose(
                self.repository.git_dir(),
                namespace,
                LooseScope::Private,
                &mut entries,
            )?;
        }
        // Prefix conflicts are not necessarily adjacent in byte order (a, a.b, a/c).
        for name in entries.keys() {
            for (index, byte) in name.as_bytes().iter().enumerate().skip(5) {
                if *byte == b'/'
                    && RefName::new(&name.as_bytes()[..index])
                        .is_ok_and(|parent| entries.contains_key(&parent))
                {
                    return Err(ReferenceError::Conflict(self.path(name)?));
                }
            }
        }
        Ok(entries
            .into_iter()
            .map(|(name, target)| Reference { name, target })
            .collect())
    }
}

fn selected(name: &[u8], namespace: Option<&RefName>) -> bool {
    namespace.is_none_or(|namespace| within(name, namespace.as_bytes()))
}

fn within(name: &[u8], namespace: &[u8]) -> bool {
    name == namespace
        || name
            .strip_prefix(namespace)
            .is_some_and(|rest| rest.starts_with(b"/"))
}

fn private(name: &[u8]) -> bool {
    let namespaces = [
        b"refs/bisect".as_slice(),
        b"refs/rewritten",
        b"refs/worktree",
    ];
    namespaces.iter().any(|prefix| within(name, prefix))
}

#[derive(Clone, Copy)]
enum LooseScope {
    All,
    Shared,
    Private,
}

fn collect_loose(
    root: &Path,
    namespace: Option<&RefName>,
    scope: LooseScope,
    entries: &mut BTreeMap<RefName, Target>,
) -> Result<(), ReferenceError> {
    let mut pending = vec![(root.join("refs"), b"refs".to_vec())];
    while let Some((path, bytes)) = pending.pop() {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        let is_private = if metadata.is_dir() || metadata.file_type().is_symlink() {
            private(&bytes)
        } else {
            RefName::new(&bytes).is_ok_and(|name| name.per_worktree())
        };
        let excluded = match scope {
            LooseScope::All => false,
            LooseScope::Shared => is_private,
            LooseScope::Private => !is_private,
        };
        if bytes != b"refs" && excluded {
            continue;
        }
        if metadata.file_type().is_symlink() {
            return Err(ReferenceError::Unsupported(
                "filesystem symlink in reference path",
            ));
        }
        if metadata.is_dir() {
            let children = match fs::read_dir(&path) {
                Ok(children) => children,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(&path, source)),
            };
            for child in children {
                let child = child.map_err(|source| io_error(&path, source))?;
                let component = child.file_name();
                // References::new rejects non-Unix storage; this branch preserves Unix bytes.
                #[cfg(unix)]
                let component = {
                    use std::os::unix::ffi::OsStrExt;
                    component.as_bytes()
                };
                #[cfg(not(unix))]
                let component = component
                    .to_str()
                    .ok_or(ReferenceError::Unsupported("non-UTF-8 filesystem name"))?
                    .as_bytes();
                if component.starts_with(b".") || component.ends_with(b".lock") {
                    continue;
                }
                let mut name = bytes.clone();
                name.push(b'/');
                name.extend_from_slice(component);
                if namespace.is_some_and(|ns| {
                    !within(&name, ns.as_bytes()) && !within(ns.as_bytes(), &name)
                }) {
                    continue;
                }
                pending.push((child.path(), name));
            }
        } else if selected(&bytes, namespace) {
            let name = RefName::new(bytes)
                .map_err(|_| malformed(&path, "invalid loose reference name"))?;
            if let Some(bytes) = read_optional(&path)? {
                entries.insert(name, parse_loose(&bytes, &path)?);
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{InitKind, ObjectId, Repository};

    fn fixture() -> (tempfile::TempDir, Repository) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
        fs::create_dir_all(repo.git_dir().join("refs/heads")).unwrap();
        fs::write(
            repo.git_dir().join("refs/heads/a"),
            b"ref: refs/heads/missing\n",
        )
        .unwrap();
        fs::write(repo.git_dir().join("packed-refs"), b"1111111111111111111111111111111111111111 refs/heads/a\n2222222222222222222222222222222222222222 refs/tags/z\n").unwrap();
        (root, repo)
    }

    #[test]
    fn lists_shadowing_symbols_and_packed_tags_in_byte_order() {
        let (_root, repo) = fixture();
        let entries = repo.references().unwrap().list().unwrap();
        assert_eq!(
            entries,
            vec![
                Reference {
                    name: RefName::new(b"refs/heads/a").unwrap(),
                    target: Target::Symbolic(RefName::new(b"refs/heads/missing").unwrap())
                },
                Reference {
                    name: RefName::new(b"refs/tags/z").unwrap(),
                    target: Target::Direct(ObjectId::Sha1([0x22; 20]))
                },
            ]
        );
    }

    #[rstest]
    #[case::branch_namespace(b"refs/heads", 1)]
    #[case::exact(b"refs/heads/a", 1)]
    #[case::not_a_string_prefix(b"refs/head", 0)]
    #[case::missing(b"refs/other", 0)]
    fn filters_at_namespace_boundaries(#[case] namespace: &[u8], #[case] count: usize) {
        let (_root, repo) = fixture();
        assert_eq!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(namespace).unwrap())
                .unwrap()
                .len(),
            count
        );
    }

    #[test]
    fn skips_locks_dot_entries_and_empty_directories() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("refs/heads/a.lock"), b"writer").unwrap();
        fs::write(repo.git_dir().join("refs/heads/.hidden"), b"unrelated").unwrap();
        fs::create_dir(repo.git_dir().join("refs/heads/empty")).unwrap();
        assert_eq!(repo.references().unwrap().list().unwrap().len(), 2);
    }

    #[test]
    fn broken_loose_does_not_fall_back_but_unselected_loose_is_not_read() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("refs/heads/a"), b"broken").unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Malformed { .. })
        ));
        assert_eq!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(b"refs/tags").unwrap())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            fs::read(repo.git_dir().join("refs/heads/a")).unwrap(),
            b"broken"
        );
    }

    #[test]
    fn filtered_list_still_validates_all_packed_data() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("packed-refs"), b"broken").unwrap();
        assert!(matches!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(b"refs/heads").unwrap()),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_symlink_directory_without_traversing_it() {
        let (_root, repo) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.git_dir().join("refs/link")).unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Unsupported(_))
        ));
    }

    #[test]
    fn private_namespace_symlink_is_rejected_in_its_own_worktree() {
        let (_root, repo) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.git_dir().join("refs/worktree")).unwrap();
        let mut entries = BTreeMap::new();
        assert!(matches!(
            collect_loose(repo.git_dir(), None, LooseScope::Private, &mut entries),
            Err(ReferenceError::Unsupported(_))
        ));
    }

    #[test]
    fn rejects_invalid_loose_name() {
        let (_root, repo) = fixture();
        fs::write(
            repo.git_dir().join("refs/heads/bad name"),
            b"1111111111111111111111111111111111111111\n",
        )
        .unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_nonadjacent_namespace_conflicts() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("packed-refs"), b"1111111111111111111111111111111111111111 refs/a\n1111111111111111111111111111111111111111 refs/a.b\n1111111111111111111111111111111111111111 refs/a/c\n").unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Conflict(_))
        ));
    }

    #[test]
    fn preserves_packed_non_utf8_names() {
        let (_root, repo) = fixture();
        fs::write(
            repo.git_dir().join("packed-refs"),
            b"1111111111111111111111111111111111111111 refs/tags/\xff\n",
        )
        .unwrap();
        let tags = repo
            .references()
            .unwrap()
            .list_namespace(&RefName::new(b"refs/tags").unwrap())
            .unwrap();
        assert_eq!(tags[0].name.as_bytes(), b"refs/tags/\xff");
    }

    #[test]
    fn head_requires_single_name_read() {
        let (_root, repo) = fixture();
        assert!(matches!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(b"HEAD").unwrap()),
            Err(ReferenceError::Unsupported(_))
        ));
    }
}
