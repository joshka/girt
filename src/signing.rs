//! Signed commit and tag payloads.
//!
//! A signed commit stores its signature in a `gpgsig` header (SHA-1 repositories) or a
//! `gpgsig-sha256` header (SHA-256 repositories), folded with one leading space per continuation
//! line. A signed tag appends the signature armor to its message. In both cases the signature
//! covers the object with the signature removed; [`SignedPayload`] recovers that split byte for
//! byte so it can be handed to a verifier.
//!
//! A merge commit can also embed the tags it merged in `mergetag` headers. [`Commit::merge_tags`]
//! returns them as [`Tag`] values whose own signatures are available through
//! [`Tag::signed_payload`].
//!
//! [`SshSigner`] makes SSH signatures and [`SshVerifier`] checks them against an allowed signers
//! file, both by running `ssh-keygen -Y` as Git does. Running programs is opt-in through
//! [`Programs`]. Other signers plug in through [`Signer`] or a closure. A check returns a
//! [`Verification`] whose [`Status`] matches Git's `%G?` letters.
//!
//! # Split a signed tag
//!
//! ```
//! use girt::signing::SignatureFormat;
//! use girt::{ObjectFormat, Tag};
//!
//! let bytes = b"object 0000000000000000000000000000000000000001\n\
//! type commit\n\
//! tag v1\n\
//! \n\
//! Release\n\
//! -----BEGIN SSH SIGNATURE-----\n\
//! U1NIU0lH\n\
//! -----END SSH SIGNATURE-----\n";
//! let tag = Tag::parse(ObjectFormat::Sha1, bytes)?;
//! let signed = tag.signed_payload().ok_or("unsigned")?;
//!
//! assert_eq!(signed.format(), Some(SignatureFormat::Ssh));
//! assert!(signed.payload().ends_with(b"\nRelease\n"));
//! assert!(
//!     signed
//!         .signature()
//!         .starts_with(b"-----BEGIN SSH SIGNATURE-----\n")
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::ffi::{OsStr, OsString};
use std::process::{ExitStatus, Output};

use crate::{Commit, CommitPayload, ObjectFormat, Tag, TagError, TagFields};

mod ssh;

pub use ssh::{SshKey, SshSigner, SshVerifier};

/// Header holding a SHA-1 object's signature.
const SHA1_HEADER: &[u8] = b"gpgsig";
/// Header holding a SHA-256 object's signature.
const SHA256_HEADER: &[u8] = b"gpgsig-sha256";

/// Armor lines that start a signature, with the format each one belongs to.
const ARMOR: [(&[u8], SignatureFormat); 4] = [
    (b"-----BEGIN PGP SIGNATURE-----", SignatureFormat::OpenPgp),
    (b"-----BEGIN PGP MESSAGE-----", SignatureFormat::OpenPgp),
    (b"-----BEGIN SIGNED MESSAGE-----", SignatureFormat::X509),
    (b"-----BEGIN SSH SIGNATURE-----", SignatureFormat::Ssh),
];

/// The kind of signature, which decides the program that creates or checks it.
///
/// This matches Git's `gpg.format` values: `openpgp`, `x509` and `ssh`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum SignatureFormat {
    /// An OpenPGP signature, made and checked with `gpg`.
    OpenPgp,
    /// An X.509 (CMS) signature, made and checked with `gpgsm`.
    X509,
    /// An SSH signature, made and checked with `ssh-keygen -Y`.
    Ssh,
}

impl SignatureFormat {
    /// Recognizes a signature by its first armor line.
    ///
    /// The signature must start with `-----BEGIN PGP SIGNATURE-----`,
    /// `-----BEGIN PGP MESSAGE-----`, `-----BEGIN SIGNED MESSAGE-----` or
    /// `-----BEGIN SSH SIGNATURE-----`. Bytes after the marker on the same line are allowed, as
    /// they are by Git. Returns `None` for anything else.
    ///
    /// ```
    /// use girt::signing::SignatureFormat;
    ///
    /// assert_eq!(
    ///     SignatureFormat::detect(b"-----BEGIN SIGNED MESSAGE-----\n"),
    ///     Some(SignatureFormat::X509)
    /// );
    /// assert_eq!(SignatureFormat::detect(b"hello\n"), None);
    /// ```
    pub fn detect(signature: &[u8]) -> Option<Self> {
        ARMOR
            .iter()
            .find(|(marker, _)| signature.starts_with(marker))
            .map(|&(_, format)| format)
    }

    /// The `gpg.format` configuration value naming this format.
    ///
    /// ```
    /// use girt::signing::SignatureFormat;
    ///
    /// assert_eq!(SignatureFormat::OpenPgp.as_str(), "openpgp");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenPgp => "openpgp",
            Self::X509 => "x509",
            Self::Ssh => "ssh",
        }
    }
}

/// A signature and the exact bytes it signs, split out of a commit or tag.
///
/// Get one from [`Commit::signed_payload`] or [`Tag::signed_payload`]. The payload is what a
/// signing program received when the object was signed; the signature is the armor as stored,
/// with commit header folding removed. Neither is checked here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedPayload {
    payload: Vec<u8>,
    signature: Vec<u8>,
    signed_at: Option<i64>,
}

impl SignedPayload {
    /// The bytes the signature covers.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The signature armor, ending with LF when the stored signature did.
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// The signature's format, or `None` when it does not start with a known armor line.
    ///
    /// Git refuses to verify a commit whose signature has no known armor line.
    pub fn format(&self) -> Option<SignatureFormat> {
        SignatureFormat::detect(&self.signature)
    }

    /// The committer or tagger date in Unix seconds, when it can be read.
    ///
    /// Verifiers check key validity periods at this time, as Git does.
    pub fn signed_at(&self) -> Option<i64> {
        self.signed_at
    }

    /// Splits into the payload and the signature.
    pub fn into_parts(self) -> (Vec<u8>, Vec<u8>) {
        (self.payload, self.signature)
    }
}

impl Commit {
    /// Splits out this commit's signature and the payload it signs.
    ///
    /// The signature header for the commit's format is `gpgsig` for SHA-1 and `gpgsig-sha256` for
    /// SHA-256. When that header repeats, the unfolded values are joined, each ending with LF
    /// unless it is the unterminated last line of the commit. The
    /// payload is the commit with every `gpgsig` and `gpgsig-sha256` header removed, so a
    /// signature for the other hash format is not part of it. Returns `None` when the commit has
    /// no signature header for its own format.
    ///
    /// ```
    /// use girt::{Commit, ObjectFormat};
    ///
    /// let bytes = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
    /// author A <a@example.com> 1700000000 +0000\n\
    /// committer A <a@example.com> 1700000000 +0000\n\
    /// gpgsig -----BEGIN SSH SIGNATURE-----\n \
    /// U1NIU0lH\n \
    /// -----END SSH SIGNATURE-----\n\
    /// \n\
    /// Message\n";
    /// let commit = Commit::parse(ObjectFormat::Sha1, bytes)?;
    /// let signed = commit.signed_payload().ok_or("unsigned")?;
    ///
    /// assert_eq!(
    ///     signed.signature(),
    ///     b"-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----\n"
    /// );
    /// assert!(!signed.payload().windows(6).any(|w| w == b"gpgsig"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn signed_payload(&self) -> Option<SignedPayload> {
        let bytes = self.as_bytes();
        let view = CommitPayload::from_bytes(bytes);
        let native = native_header(self.object_format());
        let mut signature = Vec::new();
        let mut found = false;
        let mut removed = Vec::new();
        for (index, header) in view.headers().enumerate() {
            if !header.has_value_separator {
                continue;
            }
            if header.name == native {
                found = true;
                signature.extend_from_slice(&header.unfolded_value());
                if is_terminated(bytes, header.value) {
                    signature.push(b'\n');
                }
            }
            if header.name == SHA1_HEADER || header.name == SHA256_HEADER {
                removed.push(index);
            }
        }
        if !found {
            return None;
        }
        let payload = view.without_headers(&removed)?;
        let signed_at = self
            .committer()
            .ok()
            .flatten()
            .and_then(|identity| identity.date().ok())
            .map(|date| date.seconds);
        Some(SignedPayload {
            payload,
            signature,
            signed_at,
        })
    }

    /// Tags embedded in `mergetag` headers, in header order.
    ///
    /// `git merge` of a signed tag records the whole tag object in the merge commit so that its
    /// signature can be checked later. Each header value is unfolded and parsed with this commit's
    /// object format; its ID matches the original tag object.
    ///
    /// # Errors
    ///
    /// Each item fails independently with the [`TagError`] from [`Tag::parse`].
    ///
    /// ```
    /// use girt::{Commit, ObjectFormat};
    ///
    /// let bytes = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
    /// author A <a@example.com> 1700000000 +0000\n\
    /// committer A <a@example.com> 1700000000 +0000\n\
    /// mergetag object 0000000000000000000000000000000000000001\n \
    /// type commit\n \
    /// tag v1\n \
    /// \n \
    /// Release\n\
    /// \n\
    /// Merge tag 'v1'\n";
    /// let commit = Commit::parse(ObjectFormat::Sha1, bytes)?;
    /// let tags = commit.merge_tags().collect::<Result<Vec<_>, _>>()?;
    ///
    /// assert_eq!(tags[0].name(), b"v1");
    /// assert_eq!(tags[0].message(), b"Release\n");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn merge_tags(&self) -> impl Iterator<Item = Result<Tag, TagError>> + '_ {
        let format = self.object_format();
        let commit = self.as_bytes();
        CommitPayload::from_bytes(commit)
            .headers()
            .filter(|header| header.has_value_separator && header.name == b"mergetag")
            .map(|header| {
                let mut bytes = header.unfolded_value();
                if is_terminated(commit, header.value) {
                    bytes.push(b'\n');
                }
                bytes
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(move |bytes| Tag::parse(format, &bytes))
    }
}

impl Tag {
    /// Splits out this tag's signature and the payload it signs.
    ///
    /// The signature starts at the last line that begins with a known armor line (see
    /// [`SignatureFormat::detect`]) and runs to the end of the tag. The payload is everything
    /// before it, minus `gpgsig` and `gpgsig-sha256` headers, which carry a signature for the
    /// other hash format. Matching Git, only the last header of each run of adjacent signature
    /// headers is removed; the others stay in the payload. Returns `None` when no line starts with
    /// a known armor line.
    ///
    /// The search covers the whole tag, as Git's does, so armor placed among the headers still
    /// counts.
    ///
    /// ```
    /// use girt::{ObjectFormat, Tag};
    ///
    /// let bytes = b"object 0000000000000000000000000000000000000001\n\
    /// type commit\n\
    /// tag v1\n\
    /// \n\
    /// Release\n";
    /// assert!(
    ///     Tag::parse(ObjectFormat::Sha1, bytes)?
    ///         .signed_payload()
    ///         .is_none()
    /// );
    /// # Ok::<(), girt::TagError>(())
    /// ```
    pub fn signed_payload(&self) -> Option<SignedPayload> {
        let bytes = self.as_bytes();
        let start = line_starts(bytes)
            .filter(|&start| {
                bytes
                    .get(start..)
                    .is_some_and(|line| SignatureFormat::detect(line).is_some())
            })
            .last()?;
        let (signed, signature) = bytes.split_at_checked(start)?;
        let view = CommitPayload::from_bytes(signed);
        let is_signature: Vec<bool> = view
            .headers()
            .map(|header| {
                header.has_value_separator
                    && (header.name == SHA1_HEADER || header.name == SHA256_HEADER)
            })
            .collect();
        // Git removes only the last header of each run of adjacent signature headers.
        let removed: Vec<usize> = (0..is_signature.len())
            .filter(|&index| {
                is_signature.get(index) == Some(&true) && is_signature.get(index + 1) != Some(&true)
            })
            .collect();
        let signed_at = self
            .tagger()
            .ok()
            .flatten()
            .and_then(|identity| identity.date().ok())
            .map(|date| date.seconds);
        Some(SignedPayload {
            payload: view.without_headers(&removed)?,
            signature: signature.to_vec(),
            signed_at,
        })
    }

    /// Builds a tag and appends the signature `sign` returns for it, as `git tag -s` does.
    ///
    /// `sign` receives the exact unsigned tag, which is also the signed payload of the result.
    /// The message must be empty or end with LF so the signature starts on its own line.
    ///
    /// # Errors
    ///
    /// Returns [`SignTagError::Tag`] for invalid fields, [`SignTagError::MessageNewline`] for a
    /// message without a final LF, [`SignTagError::ExistingSignature`] when the message already
    /// contains signature armor or a header is named `gpgsig` or `gpgsig-sha256`, and the
    /// signer's error.
    ///
    /// ```
    /// use girt::{ObjectFormat, ObjectId, ObjectKind, Tag, TagFields};
    ///
    /// let fields = TagFields {
    ///     target: ObjectId::for_blob(ObjectFormat::Sha1, b"release"),
    ///     target_kind: ObjectKind::Blob,
    ///     name: b"v1".to_vec(),
    ///     tagger: None,
    ///     extra_headers: vec![],
    ///     message: b"Release\n".to_vec(),
    /// };
    /// let tag = Tag::new_signed(fields, |_payload| {
    ///     Ok::<_, std::io::Error>(
    ///         b"-----BEGIN SSH SIGNATURE-----\nU1NI\n-----END SSH SIGNATURE-----\n".to_vec(),
    ///     )
    /// })?;
    /// let signed = tag.signed_payload().ok_or("unsigned")?;
    /// assert!(signed.payload().ends_with(b"\n\nRelease\n"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new_signed<E>(
        mut fields: TagFields,
        sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, E>,
    ) -> Result<Self, SignTagError<E>> {
        if !fields.message.is_empty() && !fields.message.ends_with(b"\n") {
            return Err(SignTagError::MessageNewline);
        }
        let signature_header = fields
            .extra_headers
            .iter()
            .filter_map(|line| crate::bytes::split_once(line, b' '))
            .any(|(name, _)| name == SHA1_HEADER || name == SHA256_HEADER);
        let unsigned = Self::new(fields.clone()).map_err(SignTagError::Tag)?;
        if signature_header || unsigned.signed_payload().is_some() {
            return Err(SignTagError::ExistingSignature);
        }
        let signature = sign(unsigned.as_bytes()).map_err(SignTagError::Signer)?;
        fields.message.extend_from_slice(&signature);
        Self::new(fields).map_err(SignTagError::Tag)
    }
}

/// Tag construction or signing failed; nothing has been stored.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignTagError<E> {
    /// Invalid tag fields.
    #[error(transparent)]
    Tag(TagError),
    /// The message does not end with LF, so a signature would not start its own line.
    #[error("tag message must end with a newline before it is signed")]
    MessageNewline,
    /// The message already contains signature armor, or a header carries a signature.
    #[error("tag already contains a signature")]
    ExistingSignature,
    /// The signer failed.
    #[error("signing tag")]
    Signer(#[source] E),
}

/// Whether girt may run the programs that make and check signatures.
///
/// Signing and verifying run `gpg`, `gpgsm` or `ssh-keygen` (or the program configured in
/// their place) with the permissions of the current process, and may reach the user's keyring,
/// agent or hardware keys. Git runs them whenever it signs or shows a signature. girt refuses
/// until the caller opts in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Programs {
    /// Return [`Error::Refused`] instead of running a program.
    #[default]
    Refuse,
    /// Run programs as Git does.
    Run,
}

impl Programs {
    fn check(self, program: &OsStr) -> Result<(), Error> {
        match self {
            Self::Refuse => Err(Error::Refused {
                program: program.to_owned(),
            }),
            Self::Run => Ok(()),
        }
    }
}

/// Something that turns a payload into signature armor.
///
/// [`SshSigner`] is the built-in implementation. Pass a signer to
/// [`crate::commit::CommitBuilder::build_signed`] or [`Tag::new_signed`] as
/// `|payload| signer.sign(payload)`.
pub trait Signer {
    /// The format of the signatures this signer makes.
    fn format(&self) -> SignatureFormat;

    /// Signs `payload` and returns the armored signature.
    ///
    /// # Errors
    ///
    /// Returns why the signature could not be made.
    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, Error>;
}

/// The outcome of checking a signature, matching what Git reports with `%G?`, `%GS`, `%GF` and
/// `%GT`.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Verification {
    /// Whether the signature is good, and how far its key is trusted.
    pub status: Status,
    /// Who signed: the matching SSH principal, or the OpenPGP user ID.
    pub signer: Option<String>,
    /// The signing key's fingerprint, such as `SHA256:…` for SSH keys.
    pub fingerprint: Option<String>,
    /// How far the key is trusted.
    pub trust: Trust,
    /// The verifying program's output, as `git verify-commit --raw` prints it.
    pub output: Vec<u8>,
}

impl Verification {
    /// Whether Git would accept the signature: `verify-commit` and `verify-tag` exit with 0.
    pub fn is_good(&self) -> bool {
        self.status == Status::Good
    }
}

/// The verdict on one signature, one per `%G?` letter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Status {
    /// `G`: a good signature from a trusted key.
    Good,
    /// `B`: the signature does not match the payload, or the key is not allowed to make it.
    Bad,
    /// `U`: a good signature from a key with unknown validity, such as an SSH key no allowed
    /// signer owns.
    UnknownValidity,
    /// `X`: a good signature that has expired.
    ExpiredSignature,
    /// `Y`: a good signature from an expired key.
    ExpiredKey,
    /// `R`: a good signature from a revoked key.
    RevokedKey,
    /// `E`: the signature cannot be checked, for example because the key is missing.
    CannotCheck,
}

impl Status {
    /// The letter `git log --format=%G?` prints.
    ///
    /// ```
    /// use girt::signing::Status;
    ///
    /// assert_eq!(Status::UnknownValidity.letter(), 'U');
    /// ```
    pub fn letter(self) -> char {
        match self {
            Self::Good => 'G',
            Self::Bad => 'B',
            Self::UnknownValidity => 'U',
            Self::ExpiredSignature => 'X',
            Self::ExpiredKey => 'Y',
            Self::RevokedKey => 'R',
            Self::CannotCheck => 'E',
        }
    }
}

/// How far a signing key is trusted, as `%GT` prints it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum Trust {
    /// `undefined`.
    Undefined,
    /// `never`.
    Never,
    /// `marginal`.
    Marginal,
    /// `fully`.
    Fully,
    /// `ultimate`.
    Ultimate,
}

impl Trust {
    /// The word `git log --format=%GT` prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Never => "never",
            Self::Marginal => "marginal",
            Self::Fully => "fully",
            Self::Ultimate => "ultimate",
        }
    }
}

/// Signing or verifying could not run to a verdict.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A program would run under [`Programs::Refuse`].
    #[error("running {program:?} is not allowed")]
    Refused {
        /// The program or command that would have run.
        program: OsString,
    },
    /// Neither `user.signingKey` nor `gpg.ssh.defaultKeyCommand` gives a key.
    #[error("either user.signingKey or gpg.ssh.defaultKeyCommand needs to be configured")]
    NoSigningKey,
    /// `gpg.ssh.allowedSignersFile` is not configured, so SSH signatures cannot be checked.
    #[error("gpg.ssh.allowedSignersFile needs to be configured for ssh signature verification")]
    NoAllowedSigners,
    /// The signature is not an SSH signature.
    #[error("signature is not an ssh signature")]
    NotSsh,
    /// A configured path or program cannot be used.
    #[error("unusable path: {0}")]
    Path(&'static str),
    /// A program could not be started or talked to.
    #[error("running {program:?}")]
    Spawn {
        /// The program.
        program: OsString,
        /// Why it failed.
        #[source]
        source: std::io::Error,
    },
    /// A signing program exited unsuccessfully.
    #[error("{program:?} failed with {status}: {stderr}")]
    Failed {
        /// The program.
        program: OsString,
        /// Its exit status.
        status: ExitStatus,
        /// Its standard error, lossily decoded.
        stderr: String,
    },
    /// A temporary key or signature file could not be written.
    #[error("writing a temporary file")]
    Temporary(#[source] std::io::Error),
}

impl Error {
    fn failed(program: &OsStr, output: &Output) -> Self {
        Self::Failed {
            program: program.to_owned(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_owned(),
        }
    }
}

/// The commit header that holds a signature for `format`.
fn native_header(format: ObjectFormat) -> &'static [u8] {
    match format {
        ObjectFormat::Sha1 => SHA1_HEADER,
        ObjectFormat::Sha256 => SHA256_HEADER,
    }
}

/// Whether `value`, a subslice of `bytes`, is followed by the LF that ends its header.
///
/// The final header of a commit without a message separator can end without one.
fn is_terminated(bytes: &[u8], value: &[u8]) -> bool {
    let end = (value.as_ptr() as usize - bytes.as_ptr() as usize) + value.len();
    bytes.get(end) == Some(&b'\n')
}

/// Offsets of every line start in `bytes`: zero and each position after an LF.
fn line_starts(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(
        bytes
            .iter()
            .enumerate()
            .filter(|&(_, &byte)| byte == b'\n')
            .map(|(index, _)| index + 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(format: ObjectFormat, rest: &str) -> Tag {
        let id = "1".repeat(format.digest_len() * 2);
        let bytes = format!("object {id}\ntype commit\ntag t\n{rest}");
        Tag::parse(format, bytes.as_bytes()).unwrap()
    }

    #[test]
    fn last_armor_line_starts_the_tag_signature() {
        let tag = tag(
            ObjectFormat::Sha1,
            "\nhi\n-----BEGIN SSH SIGNATURE-----\nA\n-----END SSH SIGNATURE-----\nmid\n-----BEGIN PGP SIGNATURE-----\nB\n",
        );
        let signed = tag.signed_payload().unwrap();
        assert!(
            signed
                .payload()
                .ends_with(b"-----END SSH SIGNATURE-----\nmid\n")
        );
        assert_eq!(signed.signature(), b"-----BEGIN PGP SIGNATURE-----\nB\n");
        assert_eq!(signed.format(), Some(SignatureFormat::OpenPgp));
    }

    #[test]
    fn armor_must_start_a_line_and_match_whole_marker() {
        for message in [
            "\nhi x-----BEGIN SSH SIGNATURE-----\n",
            "\n-----BEGIN SSH SIGNATUREX-----\n",
        ] {
            assert_eq!(tag(ObjectFormat::Sha1, message).signed_payload(), None);
        }
        let trailing = tag(ObjectFormat::Sha1, "\n-----BEGIN PGP SIGNATURE-----  x\n");
        assert_eq!(
            trailing.signed_payload().unwrap().payload().last(),
            Some(&b'\n')
        );
    }

    #[test]
    fn tag_payload_drops_last_signature_header_of_each_run() {
        for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
            let tag = tag(
                format,
                "gpgsig A\n a\ngpgsig-sha256 B\nx-mid m\ngpgsig C\n\nmsg\ngpgsig body\n-----BEGIN PGP SIGNATURE-----\n",
            );
            let payload = tag.signed_payload().unwrap().into_parts().0;
            assert!(payload.ends_with(b"tag t\ngpgsig A\n a\nx-mid m\n\nmsg\ngpgsig body\n"));
        }
    }

    #[test]
    fn repeated_commit_signature_headers_are_joined() {
        let bytes = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
gpgsig-sha256 other\ngpgsig -----BEGIN PGP SIGNATURE-----\n one\nx keep\ngpgsig two\n\nmsg\n";
        let commit = Commit::parse(ObjectFormat::Sha1, bytes).unwrap();
        let signed = commit.signed_payload().unwrap();
        assert_eq!(
            signed.signature(),
            b"-----BEGIN PGP SIGNATURE-----\none\ntwo\n"
        );
        assert_eq!(
            signed.payload(),
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nx keep\n\nmsg\n"
        );
    }

    #[test]
    fn unterminated_final_headers_gain_no_line_feed() {
        let bytes = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
gpgsig -----BEGIN PGP SIGNATURE-----\n A";
        let commit = Commit::parse(ObjectFormat::Sha1, bytes).unwrap();
        assert_eq!(
            commit.signed_payload().unwrap().signature(),
            b"-----BEGIN PGP SIGNATURE-----\nA"
        );

        let tag = format!(
            "object {}\n type commit\n tag v1\n \n Release",
            "1".repeat(40)
        );
        let bytes = format!("tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nmergetag {tag}");
        let commit = Commit::parse(ObjectFormat::Sha1, bytes.as_bytes()).unwrap();
        let merged = commit.merge_tags().next().unwrap().unwrap();
        assert_eq!(merged.message(), b"Release");
    }

    #[test]
    fn signing_refuses_tags_with_signature_headers() {
        for header in [&b"gpgsig old"[..], b"gpgsig-sha256 old"] {
            let fields = TagFields {
                target: "1".repeat(40).parse().unwrap(),
                target_kind: crate::ObjectKind::Commit,
                name: b"t".to_vec(),
                tagger: None,
                extra_headers: vec![header.to_vec()],
                message: b"msg\n".to_vec(),
            };
            let signed = Tag::new_signed(fields, |_| Ok::<_, std::io::Error>(Vec::new()));
            assert!(matches!(signed, Err(SignTagError::ExistingSignature)));
        }
    }

    #[test]
    fn other_format_header_alone_is_unsigned() {
        let bytes = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ngpgsig-sha256 s\n\nmsg\n";
        let commit = Commit::parse(ObjectFormat::Sha1, bytes).unwrap();
        assert_eq!(commit.signed_payload(), None);
    }

    #[test]
    fn unknown_armor_has_no_format() {
        let bytes = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ngpgsig hello\n\nmsg\n";
        let commit = Commit::parse(ObjectFormat::Sha1, bytes).unwrap();
        assert_eq!(commit.signed_payload().unwrap().format(), None);
    }
}
