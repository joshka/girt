//! Which working-tree names a checkout may write, and which of them one volume treats as the same
//! file.
//!
//! Git refuses tree entries that a filesystem could resolve to the repository's own `.git`
//! directory. `.git` in any ASCII case is always refused. `core.protectNTFS` adds the spellings
//! NTFS resolves to `.git`, and `core.protectHFS` adds those HFS+ resolves to it. These rules
//! were observed from the git CLI; see `docs/compatibility.md`.
//!
//! Volume-specific checkout name equivalence lives in the platform-specific volume module.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::Repository;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod volume;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use volume::NamePolicy;

/// Which metadata-name rules apply, read from `core.protectNTFS` and `core.protectHFS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Protection {
    ntfs: bool,
    hfs: bool,
}

impl Protection {
    /// Metadata aliases forbidden by fsck on every platform.
    pub(crate) const ALL: Self = Self {
        ntfs: true,
        hfs: true,
    };

    /// Reads both options, using Git's defaults when unset: NTFS protection everywhere, HFS+
    /// protection on macOS.
    ///
    /// Returns the name of the first option whose value is not a boolean.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn from_config(repo: &Repository) -> Result<Self, &'static str> {
        let config = repo.config();
        let ntfs = config
            .boolean("core", None, "protectntfs")
            .map_err(|_| "core.protectNTFS")?;
        let hfs = config
            .boolean("core", None, "protecthfs")
            .map_err(|_| "core.protectHFS")?;
        Ok(Self {
            ntfs: ntfs.unwrap_or(true),
            hfs: hfs.unwrap_or(cfg!(target_os = "macos")),
        })
    }

    /// Whether any component of `path` is a name these rules refuse.
    #[cfg(any(test, target_os = "linux", target_os = "macos"))]
    pub(crate) fn refuses(self, path: &[u8]) -> bool {
        path.split(|byte| *byte == b'/')
            .any(|component| self.refuses_component(component))
    }

    pub(crate) fn refuses_component(self, component: &[u8]) -> bool {
        if component.eq_ignore_ascii_case(b".git") {
            return true;
        }
        if self.ntfs {
            // NTFS ignores trailing dots and spaces, and `name:stream` names a stream of `name`.
            let base = component
                .split(|byte| *byte == b':')
                .next()
                .unwrap_or(component);
            let end = base
                .iter()
                .rposition(|byte| !matches!(byte, b'.' | b' '))
                .map_or(0, |last| last + 1);
            let base = &base[..end];
            if base.eq_ignore_ascii_case(b".git") || base.eq_ignore_ascii_case(b"git~1") {
                return true;
            }
        }
        self.hfs && without_hfs_ignorables(component).eq_ignore_ascii_case(b".git")
    }
}

fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
    )
}

/// `name` without the code points HFS+ ignores when comparing names.
fn without_hfs_ignorables(name: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(name) else {
        return name.to_vec();
    };
    text.chars()
        .filter(|c| !is_hfs_ignorable(*c))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::Protection;
    const ALL: Protection = Protection {
        ntfs: true,
        hfs: true,
    };
    const NONE: Protection = Protection {
        ntfs: false,
        hfs: false,
    };

    #[rstest]
    #[case::exact(".git", true, true, true)]
    #[case::ascii_case("a/.GIT/b", true, true, true)]
    #[case::short_name("git~1", true, false, true)]
    #[case::short_name_case("GIT~1", true, false, true)]
    #[case::trailing_space(".git ", true, false, true)]
    #[case::trailing_dots(".git..", true, false, true)]
    #[case::stream(".git::$INDEX_ALLOCATION", true, false, true)]
    #[case::hfs_ignorable(".g\u{200c}it", true, true, false)]
    #[case::bom("\u{feff}.git", true, true, false)]
    #[case::colon("c:d", false, false, false)]
    #[case::backslash("a\\b", false, false, false)]
    #[case::device("aux", false, false, false)]
    #[case::gitignore(".gitignore", false, false, false)]
    #[case::git_dot_x(".git.x", false, false, false)]
    fn matches_git_metadata_rules(
        #[case] path: &str,
        #[case] all: bool,
        #[case] hfs_only: bool,
        #[case] ntfs_only: bool,
    ) {
        let path = path.as_bytes();
        assert_eq!(ALL.refuses(path), all);
        assert_eq!(
            Protection {
                ntfs: false,
                hfs: true
            }
            .refuses(path),
            hfs_only
        );
        assert_eq!(
            Protection {
                ntfs: true,
                hfs: false
            }
            .refuses(path),
            ntfs_only
        );
        // Only `.git` in some ASCII case is refused under every rule set.
        assert_eq!(NONE.refuses(path), all && hfs_only && ntfs_only);
    }
}
