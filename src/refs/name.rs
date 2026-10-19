use std::borrow::Cow;

/// An owned, byte-preserving full reference name, or `HEAD`.
///
/// Accepts `refs/…` names following Git's check-ref-format rules, without normalization or
/// UTF-8 validation. `HEAD` is the only accepted name outside `refs/`; revision expressions,
/// shorthand branches, other pseudorefs, and cross-worktree aliases are outside this API.
///
/// Borrow a validated name through [`AsRef`] when passing it to a byte-oriented consumer, or
/// recover its owned buffer with [`Self::into_bytes`]:
///
/// ```
/// use girt::refs::RefName;
///
/// let name = RefName::new(b"refs/heads/topic")?;
/// // Construction still validates when its input is another validated name.
/// let copied = RefName::new(&name)?;
/// assert_eq!(copied, name);
/// let bytes = name.into_bytes();
/// assert_eq!(bytes, b"refs/heads/topic");
/// # Ok::<(), girt::refs::InvalidRefName>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RefName(Cow<'static, [u8]>);

impl AsRef<[u8]> for RefName {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl RefName {
    /// The `HEAD` reference, which names the current branch or a detached commit.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// assert_eq!(RefName::HEAD, RefName::new("HEAD")?);
    /// # Ok::<(), girt::refs::InvalidRefName>(())
    /// ```
    pub const HEAD: Self = Self(Cow::Borrowed(b"HEAD"));

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
            return Ok(Self::HEAD);
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
        Ok(Self(Cow::Owned(bytes.to_vec())))
    }

    /// Validates `refs/heads/<name>`, the full name of the branch `name`.
    ///
    /// Equivalent to `RefName::new([b"refs/heads/", name].concat())`. `name` is the short branch
    /// name as `git branch` spells it; a `refs/heads/` prefix in `name` is kept, not stripped.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRefName`] when the full name breaks Git's rules, as [`Self::new`] does.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// assert_eq!(RefName::branch("topic")?, RefName::new("refs/heads/topic")?);
    /// assert!(RefName::branch("bad..name").is_err());
    /// # Ok::<(), girt::refs::InvalidRefName>(())
    /// ```
    pub fn branch(name: impl AsRef<[u8]>) -> Result<Self, InvalidRefName> {
        Self::new([b"refs/heads/", name.as_ref()].concat())
    }

    /// Validates `refs/tags/<name>`, the full name of the tag `name`.
    ///
    /// Equivalent to `RefName::new([b"refs/tags/", name].concat())`.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRefName`] when the full name breaks Git's rules, as [`Self::new`] does.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// assert_eq!(RefName::tag("v1.0")?, RefName::new("refs/tags/v1.0")?);
    /// # Ok::<(), girt::refs::InvalidRefName>(())
    /// ```
    pub fn tag(name: impl AsRef<[u8]>) -> Result<Self, InvalidRefName> {
        Self::new([b"refs/tags/", name.as_ref()].concat())
    }

    /// Validates `refs/remotes/<remote>/<branch>`, a remote-tracking branch.
    ///
    /// Equivalent to `RefName::new([b"refs/remotes/", remote, b"/", branch].concat())`. This is
    /// the name the default fetch refspec `+refs/heads/*:refs/remotes/<remote>/*` writes; a
    /// remote with a different configured refspec may track its branches elsewhere.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRefName`] when the full name breaks Git's rules, as [`Self::new`] does,
    /// including an empty `remote` or `branch`.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// let tracking = RefName::remote_branch("origin", "main")?;
    /// assert_eq!(tracking, RefName::new("refs/remotes/origin/main")?);
    /// assert!(RefName::remote_branch("", "main").is_err());
    /// # Ok::<(), girt::refs::InvalidRefName>(())
    /// ```
    pub fn remote_branch(
        remote: impl AsRef<[u8]>,
        branch: impl AsRef<[u8]>,
    ) -> Result<Self, InvalidRefName> {
        Self::new([b"refs/remotes/", remote.as_ref(), b"/", branch.as_ref()].concat())
    }

    /// Returns whether this is a branch, a name under `refs/heads/`.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// assert!(RefName::branch("main")?.is_branch());
    /// assert!(!RefName::HEAD.is_branch());
    /// # Ok::<(), girt::refs::InvalidRefName>(())
    /// ```
    pub fn is_branch(&self) -> bool {
        self.0.starts_with(b"refs/heads/")
    }

    /// Borrows the original bytes, without normalization.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the exact name bytes, consuming the name.
    ///
    /// A name built from input reuses its buffer. [`Self::HEAD`] has no buffer of its own, so its
    /// bytes are copied.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.into_owned()
    }

    /// Wraps bytes that are only used as a lookup key in an ordered collection of names, such as
    /// a ref's parent directory or a range bound; the result is never stored or published.
    pub(super) fn lookup_key(bytes: Vec<u8>) -> Self {
        Self(Cow::Owned(bytes))
    }

    pub(crate) fn per_worktree(&self) -> bool {
        *self.0 == *b"HEAD"
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

    #[test]
    fn consuming_name_preserves_non_utf8_buffer() {
        let name = RefName::new(b"refs/heads/\xff").unwrap();
        let pointer = name.as_bytes().as_ptr();
        let bytes = name.into_bytes();
        assert_eq!(bytes, b"refs/heads/\xff");
        assert_eq!(bytes.as_ptr(), pointer);
    }

    #[rstest]
    #[case::branch(RefName::branch("topic"), b"refs/heads/topic".as_slice())]
    #[case::nested_branch(RefName::branch("a/b"), b"refs/heads/a/b".as_slice())]
    #[case::non_utf8_branch(RefName::branch(b"\xff"), b"refs/heads/\xff".as_slice())]
    #[case::tag(RefName::tag("v1"), b"refs/tags/v1".as_slice())]
    #[case::remote_branch(RefName::remote_branch("origin", "main"), b"refs/remotes/origin/main".as_slice())]
    fn builds_conventional_names(
        #[case] name: Result<RefName, InvalidRefName>,
        #[case] expected: &[u8],
    ) {
        assert_eq!(name.unwrap().as_bytes(), expected);
    }

    #[rstest]
    #[case::empty_branch(RefName::branch(""))]
    #[case::branch_component(RefName::branch("a//b"))]
    #[case::branch_lock(RefName::branch("main.lock"))]
    #[case::tag_dots(RefName::tag("a..b"))]
    #[case::empty_remote(RefName::remote_branch("", "main"))]
    #[case::empty_remote_branch(RefName::remote_branch("origin", ""))]
    fn rejects_invalid_conventional_names(#[case] name: Result<RefName, InvalidRefName>) {
        assert_eq!(name, Err(InvalidRefName));
    }

    #[rstest]
    #[case::branch(b"refs/heads/main", true)]
    #[case::head(b"HEAD", false)]
    #[case::tag(b"refs/tags/main", false)]
    #[case::tracking(b"refs/remotes/origin/main", false)]
    fn recognizes_branches(#[case] bytes: &[u8], #[case] expected: bool) {
        assert_eq!(RefName::new(bytes).unwrap().is_branch(), expected);
    }

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
