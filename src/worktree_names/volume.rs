//! Volume-specific checkout name equivalence.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use unicode_normalization::UnicodeNormalization as _;

use super::is_hfs_ignorable;
use crate::Repository;
use crate::checkout::Collision;

/// Which names the working tree's volume treats as one file.
///
/// Case folds when `core.ignoreCase` is true or the working tree's root resolves `.GIT` to `.git`.
/// Normalization folds on macOS, whose APFS and HFS+ volumes fold it even when case-sensitive; Git
/// reads `core.precomposeUnicode` only there. Config can only add folding to what the volume
/// shows, so missing or stale config never lets two names that collide on disk both be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamePolicy {
    case: bool,
    normalization: bool,
}

/// Paths as one volume stores them, with the sets of planned paths it merges.
pub(crate) struct Resolved<V> {
    /// Leaves by the path they occupy on the volume.
    pub(crate) disk: BTreeMap<Vec<u8>, V>,
    /// The planned path each leaf on the volume was written from, where the two differ.
    pub(crate) sources: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Sets of planned paths that share a name on the volume.
    pub(crate) collisions: Vec<Collision>,
}

impl NamePolicy {
    /// Byte-exact names, as on Linux volumes.
    #[cfg(test)]
    pub(crate) const EXACT: Self = Self {
        case: false,
        normalization: false,
    };

    /// Reads the policy for `repo`'s working tree.
    ///
    /// Returns the name of the first option whose value is not a boolean.
    pub(crate) fn from_config(repo: &Repository) -> Result<Self, &'static str> {
        let config = repo.config();
        let case = config
            .boolean("core", None, "ignorecase")
            .map_err(|_| "core.ignoreCase")?;
        Ok(Self {
            case: case == Some(true) || probe_case_folding(repo),
            // Git applies core.precomposeUnicode only on macOS, whose volumes fold normalization
            // whatever the setting; elsewhere a volume stores both spellings.
            normalization: cfg!(target_os = "macos"),
        })
    }

    /// The name `component` shares with every name this volume treats as the same file.
    pub(crate) fn key(self, component: &[u8]) -> Vec<u8> {
        if !self.case && !self.normalization {
            return component.to_vec();
        }
        let Ok(text) = std::str::from_utf8(component) else {
            // Normalizing volumes reject invalid UTF-8; folding ASCII still finds case aliases.
            return if self.case {
                component.to_ascii_lowercase()
            } else {
                component.to_vec()
            };
        };
        let mut key: String = if self.normalization {
            text.nfd().filter(|c| !is_hfs_ignorable(*c)).collect()
        } else {
            text.to_owned()
        };
        if self.case {
            // Folding through upper case merges characters whose lower cases differ, such as the
            // Kelvin sign and `k`; decomposing again keeps the key canonical.
            key = key
                .chars()
                .flat_map(char::to_uppercase)
                .flat_map(char::to_lowercase)
                .collect();
            if self.normalization {
                key = key.nfd().collect();
            }
        }
        key.into_bytes()
    }

    /// Where each of `leaves` lands on this volume, resolving shared names as Git does.
    ///
    /// Paths are taken in index order. Directories sharing a name take the first spelling, so
    /// later spellings' children land inside it. Leaves sharing a name keep the last, with its
    /// own name and value; the earlier ones are shadowed. Every merge is reported. A leaf sharing
    /// a name with a directory has no single result and is refused, returning that leaf's path.
    pub(crate) fn resolve<V: Copy>(
        self,
        leaves: &BTreeMap<Vec<u8>, V>,
    ) -> Result<Resolved<V>, Vec<u8>> {
        // Both maps are keyed by (parent path on disk, name key). A directory keeps the planned
        // prefix that named it; a leaf keeps the planned path whose value it holds.
        let mut directories: BTreeMap<(Vec<u8>, Vec<u8>), Name<()>> = BTreeMap::new();
        let mut files: BTreeMap<(Vec<u8>, Vec<u8>), Name<V>> = BTreeMap::new();
        let mut collisions = Vec::new();
        for (path, value) in leaves {
            let mut disk = Vec::new();
            let mut end = 0;
            let mut components = path.split(|byte| *byte == b'/').peekable();
            while let Some(component) = components.next() {
                end += component.len();
                let planned = &path[..end];
                let slot = (disk.clone(), self.key(component));
                let on_disk = join(&disk, component);
                if components.peek().is_none() {
                    if directories.contains_key(&slot) {
                        return Err(path.clone());
                    }
                    match files.entry(slot) {
                        Entry::Vacant(vacant) => {
                            vacant.insert(Name::new(planned, on_disk, *value));
                        }
                        // The last spelling wins, with its own name and value.
                        Entry::Occupied(mut occupied) => {
                            let name = occupied.get_mut();
                            let shadowed = std::mem::replace(&mut name.planned, path.clone());
                            name.disk = on_disk;
                            name.value = *value;
                            name.shadow(shadowed, &mut collisions);
                        }
                    }
                    break;
                }
                if files.contains_key(&slot) {
                    return Err(planned.to_vec());
                }
                // The first spelling names the directory; later ones land inside it.
                let name = directories
                    .entry(slot)
                    .or_insert_with(|| Name::new(planned, on_disk, ()));
                if name.planned != planned {
                    name.shadow(planned.to_vec(), &mut collisions);
                }
                disk.clone_from(&name.disk);
                end += 1;
            }
        }
        let mut disk = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for name in files.into_values() {
            if name.planned != name.disk {
                sources.insert(name.disk.clone(), name.planned);
            }
            disk.insert(name.disk, name.value);
        }
        collisions.sort_by(|a: &Collision, b| a.written.cmp(&b.written));
        Ok(Resolved {
            disk,
            sources,
            collisions,
        })
    }
}

/// One name on the volume during [`NamePolicy::resolve`].
struct Name<V> {
    /// The planned path written under this name.
    planned: Vec<u8>,
    disk: Vec<u8>,
    value: V,
    /// Position of this name's entry in the collision list, once another spelling arrives.
    collision: Option<usize>,
}

impl<V> Name<V> {
    fn new(planned: &[u8], disk: Vec<u8>, value: V) -> Self {
        Self {
            planned: planned.to_vec(),
            disk,
            value,
            collision: None,
        }
    }

    /// Records `shadowed` as another planned spelling of this name, now written as `planned`.
    fn shadow(&mut self, shadowed: Vec<u8>, collisions: &mut Vec<Collision>) {
        let index = *self.collision.get_or_insert_with(|| {
            collisions.push(Collision::default());
            collisions.len() - 1
        });
        let collision = &mut collisions[index];
        collision.written.clone_from(&self.planned);
        if !collision.shadowed.contains(&shadowed) {
            collision.shadowed.push(shadowed);
        }
    }
}

/// Whether the working tree's root directory resolves `.GIT` to its `.git` entry. Read-only. When
/// the root has no `.git` entry, or a lookup fails other than by absence, folding is assumed.
///
/// A volume mounted below the root is outside checkout's contract; the `lstat` of each wanted
/// name still refuses an alias that already exists there.
fn probe_case_folding(repo: &Repository) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Some(root) = repo.worktree() else {
        return true;
    };
    match (
        std::fs::symlink_metadata(root.join(".git")),
        std::fs::symlink_metadata(root.join(".GIT")),
    ) {
        (Ok(_), Err(error)) if error.kind() == std::io::ErrorKind::NotFound => false,
        (Ok(actual), Ok(alias)) => actual.dev() == alias.dev() && actual.ino() == alias.ino(),
        _ => true,
    }
}

fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = parent.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rstest::rstest;

    use super::NamePolicy;
    use crate::checkout::Collision;

    const FOLDING: NamePolicy = NamePolicy {
        case: true,
        normalization: true,
    };
    const NORMALIZING: NamePolicy = NamePolicy {
        case: false,
        normalization: true,
    };

    #[rstest]
    #[case::ascii_case(FOLDING, "README", "readme", true)]
    #[case::composed(FOLDING, "caf\u{e9}", "cafe\u{301}", true)]
    #[case::unicode_case(FOLDING, "\u{c9}t\u{e9}", "\u{e9}t\u{e9}", true)]
    #[case::kelvin(FOLDING, "\u{212a}", "k", true)]
    #[case::ignorable(FOLDING, ".g\u{200c}it", ".git", true)]
    #[case::distinct(FOLDING, "a", "b", false)]
    #[case::case_sensitive_apfs(NORMALIZING, "README", "readme", false)]
    #[case::case_sensitive_apfs_composed(NORMALIZING, "caf\u{e9}", "cafe\u{301}", true)]
    #[case::exact_case(NamePolicy::EXACT, "README", "readme", false)]
    #[case::exact_composed(NamePolicy::EXACT, "caf\u{e9}", "cafe\u{301}", false)]
    fn keys_merge_what_the_volume_merges(
        #[case] policy: NamePolicy,
        #[case] a: &str,
        #[case] b: &str,
        #[case] same: bool,
    ) {
        assert_eq!(policy.key(a.as_bytes()) == policy.key(b.as_bytes()), same);
    }

    #[test]
    fn invalid_utf8_folds_ascii_only_when_case_folds() {
        assert_eq!(FOLDING.key(b"A\xff"), FOLDING.key(b"a\xff"));
        assert_ne!(NORMALIZING.key(b"A\xff"), NORMALIZING.key(b"a\xff"));
    }

    fn leaves(paths: &[(&str, u8)]) -> BTreeMap<Vec<u8>, u8> {
        paths
            .iter()
            .map(|(path, value)| (path.as_bytes().to_vec(), *value))
            .collect()
    }

    fn disk(entries: &[(&str, u8)]) -> BTreeMap<Vec<u8>, u8> {
        leaves(entries)
    }

    // Matches what Git 2.56.0 wrote on a case-insensitive APFS volume for the same tree.
    #[test]
    fn resolves_collisions_as_git_does() {
        let resolved = FOLDING
            .resolve(&leaves(&[
                ("Dir/f", 1),
                ("X.h", 2),
                ("caf\u{e9}", 3),
                ("cafe\u{301}", 4),
                ("dir/f", 5),
                ("dir/g", 6),
                ("x.h", 7),
            ]))
            .unwrap();
        assert_eq!(
            resolved.disk,
            disk(&[("Dir/f", 5), ("Dir/g", 6), ("caf\u{e9}", 3), ("x.h", 7)])
        );
        let collision = |written: &str, shadowed: &[&str]| Collision {
            written: written.as_bytes().to_vec(),
            shadowed: shadowed
                .iter()
                .map(|path| path.as_bytes().to_vec())
                .collect(),
        };
        assert_eq!(
            resolved.collisions,
            [
                collision("Dir", &["dir"]),
                collision("caf\u{e9}", &["cafe\u{301}"]),
                collision("dir/f", &["Dir/f"]),
                collision("x.h", &["X.h"]),
            ]
        );
    }

    #[test]
    fn exact_policy_keeps_every_path() {
        let planned = leaves(&[
            ("X.h", 1),
            ("x.h", 2),
            ("xt_CONNMARK.h", 3),
            ("xt_connmark.h", 4),
        ]);
        let resolved = NamePolicy::EXACT.resolve(&planned).unwrap();
        assert_eq!(resolved.disk, planned);
        assert!(resolved.collisions.is_empty());
    }

    #[rstest]
    #[case::file_first(&[("A", 1), ("a/b", 2)], "a")]
    #[case::directory_first(&[("A/b", 1), ("a", 2)], "a")]
    fn refuses_a_file_and_directory_sharing_a_name(#[case] paths: &[(&str, u8)], #[case] at: &str) {
        assert_eq!(
            FOLDING.resolve(&leaves(paths)).err().unwrap(),
            at.as_bytes()
        );
    }
}
