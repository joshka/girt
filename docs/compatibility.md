# Git Compatibility Evidence

Loose storage supports SHA-1 blobs, trees, and commits with canonical object headers. The caller
supplies an object directory and its known `ObjectFormat::Sha1` format. The library rejects
`ObjectFormat::Sha256`; it does not read repository configuration. Crate Rustdoc owns the API
examples and complete limitations.

## References and Provenance

The format references are [Git Objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects)
and [Git's hash transition document](https://git-scm.com/docs/hash-function-transition). Blob
identity hashes the uncompressed header and content. Loose files contain a zlib stream, located
under the first two hexadecimal identity digits and a filename containing the remaining digits.

Implementation and tests are original. No Git source, test code, or comments were copied or adapted.
`tests/blobs.rs` generates empty, text, binary, and malformed inputs at runtime. Its fixed
`ABC_LOOSE` byte array was independently generated with Python's `zlib.compress(b"blob 3\0abc")`. A
complete-read case validates that fixture; named cases test all 18 incomplete prefixes. No fixture
files are vendored.

Run `cargo test` with Git available on `PATH`. Interoperability tests create isolated bare SHA-1
repositories using `git init --bare --object-format=sha1 --template=`. They compare identities with
`git hash-object --stdin`, read girt-written objects with `git cat-file blob`, then remove those
loose files and regenerate them using `git hash-object -w --stdin` for girt to read. Tests remove
inherited `GIT_*` environment overrides and disable system/global config for child processes. They
do not modify global configuration or the working checkout. This slice was validated with Git 2.55.0
on macOS; other platforms have not been exercised.

## Independent Unit-Test Identities

- `src/object.rs` checks exact headers at 9/10-byte and 99/100-byte decimal transitions, using
  repeated `x` bytes. A fifth case preserves every byte value from 0 through 255, including NUL and
  non-UTF-8 bytes.
- Each named case compares the complete encoding with a literal header plus the original payload,
  and the identity with a literal expected SHA-1 value. Expected values are not computed by girt.
- The constants were established with Python 3.14.7 `hashlib` and independently cross-checked with
  Git 2.55.0. The empty-blob identity remains covered by the existing doctest.
- Reproduce the constants outside a repository with this script; all temporary state is disposable:

```python
import hashlib
import os
import subprocess
import tempfile

fixtures = [b"x" * size for size in (9, 10, 99, 100)] + [bytes(range(256))]
env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
with tempfile.TemporaryDirectory() as directory:
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.path.join(directory, "absent"))
    for content in fixtures:
        encoded = b"blob " + str(len(content)).encode("ascii") + b"\0" + content
        expected = hashlib.sha1(encoded).hexdigest()
        result = subprocess.run(
            ["git", "hash-object", "--stdin"], input=content, cwd=directory, env=env,
            check=True, capture_output=True,
        )
        assert result.stdout.decode().strip() == expected
        print(len(content), expected)
```

## Validation Boundaries

Tests cover empty and binary content, exact read limits, missing objects, unsupported formats and
object types, malformed headers and lengths, wrong identities, every truncation of a small
compressed object, invalid checksums, trailing bytes, and concatenated streams. Publication tests
cover repeated and concurrent writes, existing corrupt objects, and failed publication with
temporary-file cleanup.

Reads require canonical decimal lengths and complete zlib streams. They reject malformed encodings
rather than emulate Git's tolerance for unusual inputs. Compressed bytes can differ from Git's
output; identity and decoded bytes must agree. This slice does not implement collision detection,
packed objects, alternates, power-loss durability, shared-repository permissions, or protection
against a hostile filesystem owner. File publication requires hard-link support.

Performance commands, cache conditions, results, and limitations are recorded in the
[blob benchmark baseline](benchmarks.md).

## Dependencies

Runtime dependencies are `sha1` for hashing, `flate2` for zlib, `tempfile` for exclusive temporary
files and disposable test directories, and `thiserror` for error derivation. All four declare
`MIT OR Apache-2.0`. Requirements permit compatible releases within their selected release series;
the lockfile records tested versions.

`rstest` provides test parameterization and Criterion provides benchmark sampling and analysis. Both
are development-only dependencies with MIT/Apache-2.0 license alternatives.

The resolved dependency manifests were checked with `cargo metadata --format-version 1`. Resolved
packages offer MIT, Apache-2.0, or Zlib terms, including platform-specific dependencies;
`unicode-ident` also requires Unicode-3.0 terms. `simd-adler32` and `generic-array` use MIT;
`zlib-rs` uses Zlib. License alternatives in transitive packages do not require selecting LGPL. This
records package metadata, not an independent legal audit.

## In-Memory SHA-1 Trees

`Tree` supports the five standard entry modes: regular blob (`100644`), executable blob (`100755`),
symlink (`120000`), subtree (`40000`), and gitlink (`160000`). Payload names are byte strings and
IDs are 20 raw SHA-1 bytes. `Tree::new` rejects empty names, NUL, slash, dot, dot-dot, and
byte-identical duplicates, then sorts entries in Git order. Directories compare as though their name
ends in `/`; other entries, including gitlinks, compare with a NUL terminator.

`Tree::parse` preserves entry order, duplicate names, and invalid path components. It accepts only
the exact mode spellings above; zero-padded and historical permission modes are explicitly
unsupported. Accepted payloads re-encode byte-for-byte, and identity hashes those unchanged bytes
with the canonical tree object header. `Tree::validate` checks names, duplicates, and ordering
without modifying the object. It is not a complete `git fsck` check: `.git`, platform aliases,
all-zero IDs, referenced object existence, and referenced object types are not checked.

The references are [Git Objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects),
[`git mktree`](https://git-scm.com/docs/git-mktree), and the diagnostic categories in
[`git fsck`](https://git-scm.com/docs/git-fsck). Ordering and encoded bytes are independently
checked through Git commands, without consulting or adapting Git implementation code.

`src/tree.rs` tests literal encodings for every supported mode, empty-tree identity, byte names,
file/directory prefixes, nonadjacent duplicate file/tree names, invalid names, malformed and
unsupported modes, missing delimiters, and truncated IDs. `tests/trees.rs` generates original
fixtures at runtime, including whitespace and non-UTF-8 names. No fixture files are vendored.

Both-direction interoperability tests run in disposable bare SHA-1 repositories with inherited
`GIT_*` overrides removed and system/global configuration disabled. `git mktree -z --missing`
constructs trees from independently specified, unsorted listings; `git cat-file tree` supplies
Git-produced payloads for girt to parse and compare byte-for-byte. The tests then remove the Git
object and publish girt's tree with `LooseObjects::write_tree`. `git ls-tree -z` and `git mktree`
reconstruct the same identity. Empty and mixed-mode trees use this complete sequence. Synthetic
referenced IDs intentionally avoid requiring commits or reference resolution in girt. Additional
tests compare preserved unsorted, duplicate-name, and invalid-name identities with
`git hash-object --literally`.

Validated with Git 2.55.0 on macOS arm64. Other platforms have not been exercised. The tree API
performs no filesystem operations and makes no checkout-safety guarantee. It allocates owned entries
and encoded buffers proportional to input size; callers bound input size before parsing. There is no
separate resource-limit, partial-write, cleanup, or concurrency contract in this in-memory slice.
SHA-256 trees, filesystem traversal, the index, checkout, commits, references, and packs remain
outside scope. No dependencies were added.

## Loose SHA-1 Trees

`LooseObjects::read_tree` verifies the canonical object header, payload size, complete zlib stream,
and requested SHA-1 identity before parsing. `write_tree` stores the exact encoded payload,
including supported unsorted entries, duplicate names, and invalid names. Neither operation calls
`Tree::validate` or resolves references. Unparseable payloads return `Error::Tree` with the
underlying cause; wrong object types return `Error::UnsupportedObjectType`.

`tests/trees.rs` reads actual Git-written loose files and compares their identities, entries, and
payloads. After deleting those files, it publishes trees through girt and checks Git's `cat-file`,
`ls-tree`, and `mktree` results. Noncanonical fixtures use
`git hash-object -w --literally -t tree --stdin`; girt reads and republishes their exact bytes,
which Git reads back unchanged. All fixtures are original runtime data in isolated repositories.

Unit tests in `src/loose.rs` cover empty and populated trees, exact and exceeded payload limits,
highly compressed oversized input, malformed framing, identity mismatch, tree parse failures, wrong
types, missing storage, duplicates, concurrent writes, corrupt existing files, and cleanup after
failed publication. The existing blob suite continues to cover every truncated prefix of a zlib
fixture, checksum damage, trailing data, and concatenated streams through the shared decoder.

The executable `examples/loose_tree.rs` writes two blobs, constructs and stores a tree referencing
them, then reads and checks the tree and both blobs. The size limit bounds decompressed payload, not
total heap use: parsing additionally owns entries and names proportional to the payload. Storage
shares blob publication and filesystem assumptions, including hard-link requirements and no
power-loss durability guarantee. Validated with Git 2.55.0 on macOS arm64; other platforms remain
untested. No dependencies were added.

## SHA-1 Commits and Loose Storage

`Commit` supports a tree ID, ordered parent IDs (including root and merge commits), author and
committer byte identities, signed 64-bit Unix seconds, timezone offsets, arbitrary message bytes,
and opaque extra headers with multiline values. Parsing requires the standard order: tree, parents,
author, committer, extra headers, then a blank line. IDs must be 40 hexadecimal SHA-1 digits;
offsets must be signed four-digit `HHMM` with hours below 24 and minutes below 60. Missing,
reordered, or repeated required headers and continuations on required headers are unsupported.
Malformed framing, truncated headers, SHA-256 IDs, and dates outside this grammar return errors. A
shortened message is still a valid payload; only loose framing/identity can detect that corruption.

Parsing retains the entire payload and decoded fields. Encoding and hashing preserve uppercase IDs,
signed or padded seconds, `-0000`, unknown/repeated extra headers, continuation spaces, non-UTF-8
bytes, and messages without a final newline. Continuation values remove exactly one framing space
per line. Construction writes canonical IDs and dates and validates nonempty identity components
without NUL, CR, LF, angle brackets, or surrounding ASCII whitespace. New seconds must be
nonnegative. Extra-header names must be printable non-space ASCII and cannot reuse required keys;
new values reject NUL and CR. `validate` applies these field rules without modifying parsed bytes.
Explicit reconstruction may change identity by normalizing lexical details.

Construction is not full `git fsck`: messages can contain NUL (which Git's default `hash-object`
rejects), parent IDs can repeat or be zero, and referenced objects are not checked. Encoding and
signature headers are opaque; no charset conversion, embedded-tag parsing, or cryptographic
verification occurs. SHA-256, history traversal, annotated tag objects, refs, packs, and transport
remain outside this slice.

The format references are [`git commit-tree`](https://git-scm.com/docs/git-commit-tree) for commit
metadata and date offsets, and [`gitformat-signature`](https://git-scm.com/docs/gitformat-signature)
for multiline extra headers. Implementation and fixtures are original, based on those descriptions
and observed Git behavior; no Git source code or tests were consulted or adapted.

`src/commit.rs` covers literal encoding, a fixed independently calculated SHA-1 identity, root and
ordered merge parents, message and header byte preservation, lexical reconstruction, malformed
framing, invalid identity components, reserved keys, date bounds, and overflow. The fixed identity
uses Python `hashlib.sha1(b"commit " + str(len(payload)).encode() + b"\0" + payload)` and is
cross-checked with Git in `tests/commits.rs`.

Integration tests isolate bare SHA-1 repositories, remove inherited `GIT_*` overrides, disable
system/global configuration and templates, and set explicit author/committer identities and dates.
`git commit-tree` independently creates root, single-parent, and merge commits. Tests compare
fields, complete payloads, and identities, remove Git's loose files, publish girt's constructed
commits, and check `cat-file` and strict `fsck` against real trees and parents. Additional tests
check girt-created binary messages and unknown multiline headers in both storage directions;
`hash-object --literally` permits NUL and noncanonical fixtures. These tests preserve bytes without
claiming that Git considers those fixtures valid. No fixture files or new dependencies are added.

`LooseObjects::read_commit` verifies framing, size, zlib completion, and identity before parsing;
`write_commit` publishes exact bytes without calling `validate`. Commit storage unit tests cover
exact/exceeded payload limits, oversized compressed data, wrong type, malformed framing, identity
mismatch, parser errors with retained causes, missing storage, duplicate/concurrent writes, existing
corruption, and publication cleanup. The shared blob decoder tests cover truncated zlib streams,
invalid checksums, concatenated streams, and trailing compressed bytes. Size limits bound
decompressed payload, not total heap use: parsed commits also own decoded fields and retained bytes.
Publication inherits the documented hard-link and trusted-filesystem assumptions and does not
promise power-loss durability.

`examples/loose_commit.rs` exercises blob-to-tree-to-commit storage and reads the referenced
snapshot back in disposable storage. Validated with Git 2.55.0 on macOS 26.6.2 arm64; other
platforms are untested. The [commit baseline](benchmarks.md#commit-baseline) records performance
evidence.
