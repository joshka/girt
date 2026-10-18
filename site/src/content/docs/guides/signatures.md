---
title: Sign and verify commits
description: Split signatures out of commits and tags, sign with gpg, gpgsm or ssh-keygen, and check signatures as Git does.
---

A signed commit carries a signature in a `gpgsig` header; a signed tag has the signature appended to
its message. The signature covers the object with the signature removed. `girt::signing` finds that
split, makes signatures with a `Signer`, and checks them with a verifier that gives a typed
`Verification` instead of program output.

Git signs with one of three programs, chosen by `gpg.format`: `gpg` for `openpgp` (the default),
`gpgsm` for `x509`, and `ssh-keygen` for `ssh`. girt runs the same programs with the same arguments:
`SshSigner` and `GpgSigner` sign, `SshVerifier` and `GpgVerifier` verify. `Settings::from_config`
reads `gpg.format`, `commit.gpgSign` and `tag.gpgSign`, and `Settings::signer` builds the configured
signer. `Verifier` picks the verifier from the signature's armor, as `git verify-commit` does. Any
other signer can be plugged in through the `Signer` trait or a closure.

## Running signing programs is opt-in

Signing and verifying run a program with your process's permissions, and that program can reach your
SSH agent, keyring or hardware key. Git runs it whenever it signs or shows a signature. girt refuses
until you pass `Programs::Run`; with the default `Programs::Refuse`, signing and verifying return
`Error::Refused` before anything runs.

## Find the signature and what it signs

`Commit::signed_payload` and `Tag::signed_payload` return a `SignedPayload`: the signature armor,
the exact bytes it covers, and the committer or tagger date. These are the bytes Git hands to its
signing program when verifying, including for SHA-256 repositories and objects that carry a second
signature for the other hash format. `Commit::merge_tags` returns the signed tags a merge recorded
in `mergetag` headers, so their signatures can be checked too.

## Sign and verify with SSH

`SshSigner::from_config` reads `user.signingKey` (a key path, or a literal `key::ssh-…` public key
whose private key is in the agent), `gpg.ssh.defaultKeyCommand` and `gpg.ssh.program`.
`SshVerifier::from_config` reads `gpg.ssh.allowedSignersFile`, `gpg.ssh.revocationFile` and
`gpg.ssh.program`. Both can also be built directly, as here with a throwaway key:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::signing::{Programs, Signer, SshKey, SshSigner, SshVerifier, Status};
    use girt::workflow::Environment;
    use girt::{Commit, ObjectId, Signature};

    let directory = tempfile::tempdir()?;
    let key = directory.path().join("key");
    let generated = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "me", "-f"])
        .arg(&key)
        .status();
    if !generated.is_ok_and(|status| status.success()) {
        return Ok(()); // ssh-keygen is not installed.
    }
    let public = std::fs::read_to_string(directory.path().join("key.pub"))?;
    let allowed = directory.path().join("allowed_signers");
    std::fs::write(&allowed, format!("me@example.com {public}"))?;

    // Sign with the key file only: keep ssh-keygen away from any SSH agent.
    let environment = Environment::from_process().without("SSH_AUTH_SOCK");
    let signer = SshSigner::new(SshKey::Path(key))
        .with_programs(Programs::Run)
        .with_environment(environment.clone());
    let author = Signature::parse(b"Me <me@example.com> 1700000000 +0000")?;
    let tree: ObjectId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904".parse()?; // The empty tree.
    let commit: Commit =
        Commit::builder(tree, author, "Signed\n").build_signed(|payload| signer.sign(payload))?;

    let verifier = SshVerifier::new(allowed)
        .with_programs(Programs::Run)
        .with_environment(environment);
    let signed = commit.signed_payload().ok_or("unsigned")?;
    let verification = verifier.verify(&signed)?;
    assert_eq!(verification.status, Status::Good);
    assert_eq!(verification.signer.as_deref(), Some("me@example.com"));
    Ok(())
}
```

Programs inherit your process's environment unless `with_environment` replaces it.

Store the commit with `LooseObjects` or `Objects` as usual; building it writes nothing. For tags,
`Tag::new_signed` signs the unsigned tag and appends the signature to its message.

## Read the verdict

`Verification::status` has one variant per letter that `git log --format=%G?` prints, and
`Status::letter` returns it. `signer`, `key`, `fingerprint`, `primary_fingerprint` and `trust` match
`%GS`, `%GK`, `%GF`, `%GP` and `%GT`, and `output` holds what `git verify-commit --raw` would print.
`Verification::is_good` is true exactly when `git verify-commit` would succeed, which depends on the
format and on `gpg.minTrustLevel` as well as the letter.

For SSH signatures, as in Git:

- **Good (`G`)**: a principal in the allowed signers file owns the key, the key is valid at the
  commit's date (`valid-after`, `valid-before`), may sign in the `git` namespace and is not revoked.
- **Unknown validity (`U`)**: the signature is correct but no principal owns the key at that date.
  `signer` is empty; `fingerprint` names the key.
- **Bad (`B`)**: the payload does not match, or the owning principal's entry rejects the key for its
  namespace or because it is revoked.

A key an allowed signer owns has `fully` trust, so `gpg.minTrustLevel=ultimate` rejects every SSH
signature, as in Git.

For OpenPGP and X.509 signatures, `gpg` or `gpgsm` decides, using the keyring `GNUPGHOME` names:

- **Good (`G`)** needs a correct signature from a key of `marginal` trust or better; a correct
  signature from a key of `undefined` or `never` trust is **unknown validity (`U`)**.
- **Expired signature (`X`)**, **expired key (`Y`)** and **revoked key (`R`)** are correct
  signatures with a time or revocation problem; **cannot check (`E`)** means the key is missing.
- `is_good` accepts `G`, `U` and `Y` when trust is at least `gpg.minTrustLevel` (default
  `undefined`, so `never` is rejected), and rejects the rest. This is what Git does.

Without `gpg.ssh.allowedSignersFile`, `verify` returns `Error::NoAllowedSigners`, where Git prints
an error and reports `N`. Unlike Git, girt passes the date to `ssh-keygen` in UTC, so the result
does not depend on the machine's time zone.
