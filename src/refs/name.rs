/// An owned, byte-preserving full reference name, or `HEAD`.
///
/// Accepts `refs/…` names following Git's check-ref-format rules, without normalization or
/// UTF-8 validation. `HEAD` is the only accepted name outside `refs/`; revision expressions,
/// shorthand branches, other pseudorefs, and cross-worktree aliases are outside this API.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RefName(Vec<u8>);

impl RefName {
    /// Validates and copies a full name without consulting the filesystem.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRefName`] for invalid Git syntax or names outside the supported namespace.
    /// Filesystem restrictions (including case collisions) are checked only during storage access.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// let branch = RefName::new(b"refs/heads/main")?;
    /// assert_eq!(branch.as_bytes(), b"refs/heads/main");
    /// assert!(RefName::new(b"refs/heads/../main").is_err());
    /// # Ok::<(), girt::refs::InvalidRefName>(())
    /// ```
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, InvalidRefName> {
        let bytes = bytes.as_ref();
        if bytes == b"HEAD" {
            return Ok(Self(bytes.to_vec()));
        }
        if !bytes.starts_with(b"refs/")
            || bytes.ends_with(b".")
            || bytes.windows(2).any(|pair| pair == b".." || pair == b"@{")
            || bytes
                .iter()
                .any(|b| *b <= b' ' || *b == 127 || b"~^:?*[\\".contains(b))
            || bytes
                .split(|b| *b == b'/')
                .any(|part| part.is_empty() || part.starts_with(b".") || part.ends_with(b".lock"))
        {
            return Err(InvalidRefName);
        }
        Ok(Self(bytes.to_vec()))
    }

    /// Borrows the original bytes, without normalization.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(super) fn per_worktree(&self) -> bool {
        self.0 == b"HEAD"
            || [
                b"refs/bisect/".as_slice(),
                b"refs/rewritten/",
                b"refs/worktree/",
            ]
            .iter()
            .any(|prefix| self.0.starts_with(prefix))
    }
}

/// A name violates Git's full-name rules or this API's `refs/…`/`HEAD` boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("expected HEAD or a valid full refs/ reference name")]
pub struct InvalidRefName;

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::head(b"HEAD")]
    #[case::branch(b"refs/heads/main")]
    #[case::bytes(b"refs/tags/\xff")]
    #[case::at(b"refs/heads/@")]
    #[case::dot_component_end(b"refs/heads/a./b")]
    fn accepts_names(#[case] bytes: &[u8]) {
        assert_eq!(RefName::new(bytes).unwrap().as_bytes(), bytes);
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::short(b"main")]
    #[case::pseudo(b"MERGE_HEAD")]
    #[case::alias(b"main-worktree/HEAD")]
    #[case::double_slash(b"refs//a")]
    #[case::trailing_slash(b"refs/a/")]
    #[case::dot(b"refs/.a")]
    #[case::dot_dot(b"refs/a..b")]
    #[case::trailing_dot(b"refs/a.")]
    #[case::lock(b"refs/a.lock/b")]
    #[case::reflog(b"refs/a@{b")]
    #[case::control(b"refs/a\0b")]
    #[case::del(b"refs/a\x7f")]
    #[case::space(b"refs/a b")]
    #[case::tilde(b"refs/a~b")]
    #[case::caret(b"refs/a^b")]
    #[case::colon(b"refs/a:b")]
    #[case::question(b"refs/a?b")]
    #[case::glob(b"refs/a*b")]
    #[case::bracket(b"refs/a[b")]
    #[case::backslash(b"refs/a\\b")]
    fn rejects_names(#[case] bytes: &[u8]) {
        assert_eq!(RefName::new(bytes), Err(InvalidRefName));
    }
}
