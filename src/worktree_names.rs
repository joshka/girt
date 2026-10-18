//! Which working-tree names a checkout may write.
//!
//! Git refuses tree entries that a filesystem could resolve to the repository's own `.git`
//! directory. `.git` in any ASCII case is always refused. `core.protectNTFS` adds the spellings
//! NTFS resolves to `.git`, and `core.protectHFS` adds those HFS+ resolves to it. These rules
//! were observed from the git CLI; see `docs/compatibility.md`.

use crate::Repository;

/// Which metadata-name rules apply, read from `core.protectNTFS` and `core.protectHFS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Protection {
    ntfs: bool,
    hfs: bool,
}

impl Protection {
    /// Reads both options, using Git's defaults when unset: NTFS protection everywhere, HFS+
    /// protection on macOS.
    ///
    /// Returns the name of the first option whose value is not a boolean.
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
    pub(crate) fn refuses(self, path: &[u8]) -> bool {
        path.split(|byte| *byte == b'/')
            .any(|component| self.refuses_component(component))
    }

    fn refuses_component(self, component: &[u8]) -> bool {
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

/// `name` without the code points HFS+ ignores when comparing names.
fn without_hfs_ignorables(name: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(name) else {
        return name.to_vec();
    };
    text.chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
            )
        })
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
