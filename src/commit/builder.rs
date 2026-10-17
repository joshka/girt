//! Canonical commit construction with optional external signing.
use super::{Commit, CommitError, CommitFields, CommitHeader, Signature};
use crate::ObjectId;

/// Collects fields for [`Commit::new`]. The supplied author is also the default committer.
///
/// Use [`Self::from_config`] to choose the committer through [`Signature::committer`]. Parents and
/// headers retain insertion order; message bytes are unchanged. Building does not store objects
/// or update references. Signing receives the exact unsigned payload, without an object header.
///
/// ```
/// use girt::commit::CommitBuilder;
/// use girt::workflow::Environment;
/// use girt::{Config, ObjectFormat, Tree};
/// let config = Config::parse(b"[user]\nname = Writer\nemail = writer@example.com\n")?;
/// let environment = Environment::default()
///     .with("GIT_AUTHOR_DATE", "@1700000000 +0000")
///     .with("GIT_COMMITTER_DATE", "@1700000001 +0000");
/// let tree = Tree::new(ObjectFormat::Sha1, vec![])?.id();
/// let commit = CommitBuilder::from_config(tree, b"Snapshot\n", &config, &environment)?
///     .header(b"reviewed-by", b"Maintainer")
///     .build()?;
/// assert_eq!(commit.subject(), b"Snapshot");
/// assert_eq!(commit.to_fields()?.committer.seconds, 1700000001);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct CommitBuilder {
    fields: CommitFields,
}
impl CommitBuilder {
    /// Starts a root commit, defaulting the committer to the supplied author.
    pub fn new(tree: ObjectId, author: Signature, message: impl AsRef<[u8]>) -> Self {
        Self {
            fields: CommitFields {
                tree,
                parents: Vec::new(),
                committer: author.clone(),
                author,
                extra_headers: Vec::new(),
                message: message.as_ref().to_vec(),
            },
        }
    }
    /// Resolves author and default committer using the Signature constructors.
    ///
    /// # Errors
    ///
    /// Returns missing/invalid identity, clock or timezone errors from those constructors.
    pub fn from_config(
        tree: ObjectId,
        message: impl AsRef<[u8]>,
        config: &crate::Config,
        environment: &crate::workflow::Environment,
    ) -> Result<Self, CommitError> {
        let author = Signature::author(config, environment)?;
        let committer = Signature::committer(config, environment)?;
        Ok(Self::new(tree, author, message).committer(committer))
    }
    /// Appends a parent; order is meaningful for merges.
    pub fn parent(mut self, parent: ObjectId) -> Self {
        self.fields.parents.push(parent);
        self
    }
    /// Appends parents in iteration order, including an optional parent directly.
    pub fn parents(mut self, parents: impl IntoIterator<Item = ObjectId>) -> Self {
        self.fields.parents.extend(parents);
        self
    }
    /// Overrides the default committer.
    pub fn committer(mut self, committer: Signature) -> Self {
        self.fields.committer = committer;
        self
    }
    /// Appends an opaque header. [`Self::build`] validates its name and framing.
    pub fn header(mut self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        self.fields.extra_headers.push(CommitHeader {
            name: name.as_ref().to_vec(),
            value: value.as_ref().to_vec(),
        });
        self
    }
    /// Validates and constructs a commit through [`Commit::new`].
    ///
    /// # Errors
    ///
    /// Returns invalid fields or mixed object-format errors from `Commit::new`.
    pub fn build(self) -> Result<Commit, CommitError> {
        Commit::new(self.fields)
    }

    /// Signs the exact unsigned payload and adds the returned bytes as a format-specific signature
    /// header.
    ///
    /// The caller owns the signing mechanism and key policy. No subprocess is started. Existing
    /// `gpgsig` and `gpgsig-sha256` headers are rejected before the closure runs. The closure runs
    /// once, only after unsigned fields validate. Exactly one final LF is removed, matching
    /// Git's armor framing; remaining bytes are validated by [`Commit::new`].
    ///
    /// # Errors
    ///
    /// Returns field errors, duplicate signature errors or the closure's original error.
    pub fn build_signed<E>(
        mut self,
        sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, E>,
    ) -> Result<Commit, SignError<E>> {
        if self
            .fields
            .extra_headers
            .iter()
            .any(|header| matches!(header.name.as_slice(), b"gpgsig" | b"gpgsig-sha256"))
        {
            return Err(SignError::ExistingSignature);
        }
        let unsigned = Commit::new(self.fields.clone()).map_err(SignError::Commit)?;
        let signature = sign(unsigned.as_bytes()).map_err(SignError::Signer)?;
        let name: &[u8] = match self.fields.tree.format() {
            crate::ObjectFormat::Sha1 => b"gpgsig",
            crate::ObjectFormat::Sha256 => b"gpgsig-sha256",
        };
        self.fields.extra_headers.push(CommitHeader {
            name: name.to_vec(),
            value: signature.strip_suffix(b"\n").unwrap_or(&signature).to_vec(),
        });
        Commit::new(self.fields).map_err(SignError::Commit)
    }
}

/// Commit construction or caller-owned signing failed; nothing has been stored.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignError<E> {
    /// Invalid commit fields or returned signature framing.
    #[error(transparent)]
    Commit(CommitError),
    /// The signing closure failed.
    #[error("signing commit")]
    Signer(#[source] E),
    /// A `gpgsig` or `gpgsig-sha256` header was already supplied.
    #[error("commit already contains a signature header")]
    ExistingSignature,
}
