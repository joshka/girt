# Git Compatibility Evidence

Loose storage supports SHA-1 blobs, trees, commits, and tags with canonical object headers. The
caller can supply an object directory and its known `ObjectFormat::Sha1` format, or use `Repository`
to open an explicit repository path and detect the format from local configuration. SHA-256 storage
is recognized and rejected. Crate Rustdoc owns the API examples and complete limitations.

## Current Capabilities and Evidence

The current API supports SHA-1 loose objects, pack/index v2, complete-history queries, object-only
fetch, conditional branch/tag push, and explicit reference updates without reflogs. HTTP and SSH
downloads share owned validation state. Installation takes explicit destination snapshot limits.
Read and operation limits remain per phase; no process-wide heap or hard CPU-latency guarantee is
implied.

| Platform       | Current evidence boundary                                   |
| -------------- | ----------------------------------------------------------- |
| macOS arm64    | Latest transport and ownership tests run locally.           |
| Linux x86_64   | Earlier runtime evidence; current revision awaits CI.       |
| Windows x86_64 | Earlier portable tests; repository support remains limited. |

The configured CI checks core-only, HTTP-only, and SSH-only library compilation independently.
Configuration is not evidence of a successful run. Historical run IDs, counts and environments below
apply only to their stated revisions.

## Platform and Git-Version Validation

### Historical Cross-Platform Runs

The repository capabilities preceding transport interruption have runtime evidence on macOS arm64
and Linux x86_64. The [owned transport change](#owned-transport-interruption) records its evidence
separately. Windows reference storage remains unsupported. Earlier capability sections below retain
their original baseline environments; this section records the later cross-platform validation.

| Target                    | Runtime evidence                 | Boundary                 |
| ------------------------- | -------------------------------- | ------------------------ |
| macOS arm64               | Full suite and transfer examples | Trusted local filesystem |
| Ubuntu 22.04/24.04 x86_64 | Full suite and transfer examples | Trusted local filesystem |
| Windows 2022 x86_64       | 537 units, 11 doctests           | References unsupported   |

Native checks used macOS 26.6.2 arm64 and rustc 1.98.1 with Git 2.55.0 and Apple Git 2.54.0.
Commands were `just check`, `PATH=/usr/bin:$PATH cargo test --locked`,
`RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --document-private-items`, and
`cargo run --locked --example fetch_local` / `cargo run --locked --example push_local`. The final
native suite contains 788 tests, including doctests. The final malformed-config regression also
passed separately with Apple Git using
`PATH=/usr/bin:$PATH cargo test --locked --test repositories malformed_syntax_is_rejected_by_git_and_girt`.

[GitHub Actions run 35944395649](https://github.com/joshka/girt/actions/runs/35944395649) passed at
revision `ca01ec99e01bc6c257c76e26876b90a7f25e04d4`. Both Ubuntu hosts ran 790 tests, including the
Linux-only non-UTF-8 path/ref cases; macOS 14 arm64 ran 788. All three passed all-target Clippy,
private Rustdoc and disposable local fetch/push examples. All used rustc 1.98.1 and Git 2.55.0.
Windows 2022 MSVC passed all-target compilation with rustc 1.98.1 and Git 2.55.0.windows.5.
[Run 35944525644](https://github.com/joshka/girt/actions/runs/35944525644), revision
`92856caf9dc8bc4e15480a291b2da58a938b456a`, also passed 537 Windows library-unit tests and 11
doctests using `cargo +stable test --locked --lib` and `cargo +stable test --locked --doc`. An
unused Unix-only test import warning was subsequently fixed; the Windows job now also rejects
all-target Clippy warnings. These checks do not establish Windows repository support: reference
storage explicitly rejects non-Unix platforms, and several integration tests require it. Local
server process/environment and filesystem semantics still need Windows integration coverage before
expanding that contract.

[Platform CI](../.github/workflows/validation.yml) records toolchain and Git versions and repeats
those checks. Native Linux VM attempts on the development Mac failed to boot/connect, so the Linux
runtime evidence comes from CI. Library cross-checks also passed using
`cargo check --locked --lib --target <target>` for `x86_64-unknown-linux-gnu`,
`x86_64-pc-windows-gnu`, and `x86_64-pc-windows-msvc`. Windows all-target cross-checking on the Mac
was blocked by missing MinGW for Criterion's `alloca` dependency; the native Windows CI compile
succeeded. Cross-compilation alone does not establish runtime behavior. Crash durability, network
filesystems, hostile path mutation and multi-gigabyte workloads remain outside this evidence.

Git 2.54.0 and 2.55.0 were exercised, not a minimum supported version. Fixture commands rely on
`init --object-format=sha1`, `--initial-branch`, `mktag --no-strict`, pack/index commands and local
upload-pack/receive-pack v0. CI's installed Git versions must be retained with results; runner
labels alone do not establish compatibility with an older Git release.

The independent review traced object framing/identity and pack/delta bounds, ref locks and
publication, repository/config interpretation, ancestry queries, and transfer validation/status
paths. It found and fixed two config parser discrepancies: leading unquoted whitespace after empty
quotes or a continued line was retained, and a backslash followed by CR CR LF was incorrectly
treated as a continuation. New unit and Git-comparison cases cover both; the unit cases failed
before the fix. Existing corruption, concurrency, namespace, graph and partial-push tests were
rerun. This is a correctness review within the documented capabilities, not proof of complete Git
fsck equivalence or an exhaustive security audit. No performance claim or processing-path redesign
was introduced.

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
verification occurs. SHA-256, history traversal, refs, packs, and transport remain outside this
slice.

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

## SHA-1 Annotated Tags and Loose Storage

`Tag` supports a target ID and declared `ObjectKind` (blob, tree, commit, or tag), byte name,
optional `Signature` tagger, opaque extra header lines, and arbitrary message bytes. Creating or
storing an object does not create a tag reference, resolve the target, check its actual type, or
peel nested tags. Embedded signature armor remains part of the message without verification.

The format reference is [`git mktag`](https://git-scm.com/docs/git-mktag). Independent command-line
probes and `tests/tags.rs` verify tags with absent taggers, opaque extra lines, and empty messages
without a blank separator. Git's strict `mktag` promotes missing-tagger diagnostics to errors and
rejects extra headers; those fixtures explicitly set `fsck.missingTaggerEntry=ignore` and
`fsck.extraHeaderEntry=ignore`. This documents preservation rather than a claim that every supported
payload passes default strict validation.

Parsing requires ordered `object`, `type`, and `tag` headers, followed by an optional `tagger` and
extra lines. Headers end in LF; a blank line introduces arbitrary message bytes. Without a blank
separator, every header must still end in LF and the message is empty. Known headers cannot repeat
or appear among extra lines. Targets require exactly 40 hexadecimal digits; unknown types, SHA-256
IDs, truncated headers, and unreadable tagger dates fail explicitly. Taggers reuse the commit
identity/date grammar. Parsing retains lexical details and does not apply construction validation.
Extra lines retain their full bytes and order, including bare keys, repeated unknown keys, and
leading spaces; they are not decoded as commit continuations.

Construction rejects empty names, ASCII whitespace or NUL in names, and extra lines containing NUL,
CR, or LF. Taggers apply the existing identity/date validation. Absent taggers are allowed. This
does not validate tag reference paths or promise full fsck validity. Construction emits a blank
separator even for empty messages, lowercase IDs, and canonical dates; reconstructing parsed fields
can change identity. Encoding, hashing, and storage use the retained payload unchanged.

Original runtime fixtures in `tests/tags.rs` use isolated bare SHA-1 repositories with inherited Git
overrides removed and global/system configuration disabled. `git tag -a` independently creates tags
of each of the four object types. Tests compare exact fields, payloads, and identities, remove the
Git-written loose object, publish through girt, and check `cat-file`, `mktag`, and strict `fsck`.
Additional fixtures check absent taggers, omitted separators, unknown lines, and opaque signature
text through relaxed `mktag`. Noncanonical dates, uppercase IDs, binary content, and invalid
construction fields use `hash-object --literally`; they promise byte preservation only. No Git
implementation code or fixture content was copied or adapted.

The literal unit identity `1316e0263ce4c0bc7afc85d20286be352811d4cf` was independently obtained with
Git for a tag of the empty blob named `v1`, tagger `A <a> 1 +0000`, and no message separator. Unit
tests also exercise all target types, framing errors, unsupported formats, byte names, reserved
headers, opaque PGP/SSH signature text, and validation failures without losing original bytes.
Storage tests cover exact limits, oversized decompression, wrong types, missing files, corruption,
parser errors after identity verification, duplicate/concurrent publication, and cleanup while
preserving existing corrupt files. Existing shared decoder tests cover zlib truncation and trailing
data. The example `loose_tag` demonstrates storage without tag refs.

Validated with Git 2.55.0 on macOS arm64; other platforms are untested. No dependencies were added.
Storage retains the existing trusted-directory, hard-link, synchronous I/O, and durability limits.
The read limit bounds decompressed payload bytes, not the additional owned fields and payload
copies. SHA-256, tag refs, peeling/traversal, signature verification, discovery, packs, and
transport remain outside scope. [Performance evidence](benchmarks.md#tag-baseline) records the tag
baseline.

## Repository Opening and Configuration

`Repository::open` accepts exactly a worktree root, Git directory, or indirection file. Ordinary,
bare, separate Git directories and linked worktrees are covered. It returns canonical Git/common
paths, the shared object directory, and the associated worktree. No upward search, initialization,
reference resolution, packed-object reads, graph traversal or transport is performed.

The original implementation follows the published
[repository layout](https://git-scm.com/docs/gitrepository-layout),
[configuration syntax](https://git-scm.com/docs/git-config#_syntax) and
[repository version](https://git-scm.com/docs/repository-version) specifications. Independent
runtime fixtures in `tests/repositories.rs` use Git 2.55.0 on macOS arm64. They create repositories
with `git init --template=`, separate metadata with `--separate-git-dir`, and linked worktrees with
`git worktree add`. Git-generated loose blobs are read through the opened repository. No dependency
was added, and no Git implementation or upstream fixture was copied.

### Configuration Boundary

- `Config::parse` consumes caller-supplied bytes from one source. Repository opening supplies only
  the common directory's `config`; a missing file uses layout defaults, repository version 0 and
  SHA-1. No system/global config, command-line settings, `GIT_*` override, home directory lookup or
  precedence merge is consulted. The current directory only resolves relative input paths.
- Section/variable names use ASCII case-insensitive lookup; quoted subsections preserve exact case
  and bytes. Ordered repeated values, implicit booleans and explicit empty values remain distinct.
  Scalar lookup selects the last occurrence. Includes remain inert entries in standalone parsing;
  opening rejects every include/includeIf entry, even an apparently inactive condition.
- Supported syntax includes comments, mixed quoted/unquoted values, documented escapes, physical
  line continuations, LF/CRLF and an initial UTF-8 BOM. Arbitrary value bytes are preserved. NUL,
  invalid escapes, multiline quoted strings, unsectioned variables and malformed headers are
  rejected. Deprecated dotted section syntax is explicitly rejected rather than misinterpreted as a
  modern quoted subsection.
- Opening interprets core.repositoryFormatVersion, core.bare, core.worktree and
  extensions.objectFormat. Integer settings support signs, decimal/octal/hexadecimal notation and
  binary k/m/g suffixes within the implementation's integer range. Boolean words are
  case-insensitive; empty values are false, implicit values true, and numeric zero is false.
- Versions 0 and 1 with SHA-1 are supported. SHA-256, unknown object formats, every other extension
  (including refStorage and worktreeConfig), and objectFormat under version 0 are rejected.
  Rejecting extensions under version 0 is intentionally stricter than historical Git behavior.
  Ordinary unrelated settings remain queryable without affecting opening.
- `config.worktree` is ignored when its extension is absent, as in Git. Enabling the extension is
  unsupported, including on ordinary repositories; this prevents accidentally missing overrides.
  Shared core.worktree is rejected for linked layouts. Linked worktrees use the verified absolute
  `gitdir` backlink; relative backlinks are explicitly unsupported.
- Relative core.worktree paths resolve against the Git directory. Bare/worktree conflicts and empty
  worktree values fail. Tilde and `%(...)` path interpolation is unsupported. A directly supplied,
  non-bare metadata directory needs an explicit worktree relationship; no current-directory worktree
  is guessed. Unix path conversion preserves bytes. A non-UTF-8 filename fixture is Linux-only
  because the exercised macOS filesystem rejects such filenames; byte conversion itself is tested on
  macOS. Other platforms require UTF-8 metadata paths and remain untested.

### Failure and Compatibility Evidence

Named Git comparisons cover comments, whitespace, escaping, continuation, CRLF/BOM, byte values,
Git-written quoted subsections, repeated keys, boolean forms, numeric versions and relative
core.worktree. Repository fixtures cover root, Git-directory and gitfile inputs, shared linked
objects, SHA-256 rejection, unsupported sources/extensions/storage, malformed metadata, missing
paths, and no parent discovery. The runnable example is exercised with hostile Git environment and
global configuration settings in its child process; those settings do not affect opening.

Opening checks HEAD's marker shape without resolving its reference and requires object and refs
directories. It rejects shallow markers and alternates files, including empty ones, rather than
pretending that local storage is complete. The repository opener does not inspect packs.
`Repository::objects` opens bounded pack snapshots; `Repository::loose_objects` continues to search
only loose storage.

File-content snapshots before and after successful and rejected opening establish absence of file
creation, deletion or content changes. Filesystem access times are not covered by that guarantee.
Missing repositories, malformed metadata, configuration syntax failures, filesystem errors and
unsupported features have distinct error variants with source paths. Opening performs synchronous
reads and allocations without a configurable resource limit. Concurrent metadata replacement is not
a consistent snapshot, and canonicalization is not an ownership or security check. Symlinks are
resolved; callers must supply a trusted repository path and handle later storage changes.

## Files References

### Contract and Supported Boundary

`Repository::references` provides a borrowed files-backend store. `RefName` owns exact bytes and
accepts full `refs/…` names following Git's name rules, plus `HEAD`; it does not normalize or expand
shorthand. Other pseudorefs (including multi-record FETCH_HEAD), revision expressions,
cross-worktree aliases such as `main-worktree/HEAD`, and reftable are excluded. Repository opening
continues to reject every refStorage extension, including an explicit `files` value; repositories
using the implicit default files backend are supported. Unix storage is enabled, with macOS arm64
exercised; non-Unix storage returns an explicit unsupported error.

Loose refs take precedence over packed refs. A malformed loose value reports an error even when a
valid packed value exists. Direct targets are nonzero SHA-1 IDs; symbolic records begin with `ref:`
and a supported full name. Trailing ASCII whitespace is accepted, and writes produce one LF. The
reader rejects zero IDs, SHA-256 IDs, invalid symbolic names and extra non-whitespace data. It
supports regular files, rejecting filesystem symlinks (including legacy symlink HEAD) and other
non-regular files. Parent symlinks are also rejected. Names remain bytes through parsing and Unix
path conversion, but filesystem restrictions still apply: APFS rejects non-UTF-8 loose filenames.
Packed byte-name reads are tested on macOS; loose byte-name writes have a Linux-only test that has
not been exercised here. Case/Unicode aliases inherit filesystem behavior; no normalization or
portable alias detection is provided.

`HEAD`, `refs/bisect/`, `refs/rewritten/`, and `refs/worktree/` are local to the opened worktree;
other names and packed-refs use its common directory. Resolution returns the terminal name and an
optional ID. Missing initial refs and unborn/dangling symbolic chains return an absent ID; missing
objects are not detected. Cycles are distinct errors, and callers set the maximum symbolic hops for
reads. Updates through symbolic chains allow at most 32 hops. Resolution follows live reads rather
than a snapshot and never peels tags or loads objects.

### Packed Grammar

An empty or absent packed-refs file is accepted. Nonempty files use LF-terminated records,
optionally starting with `# pack-refs with:` and space-separated `peeled`, `fully-peeled`, and/or
`sorted` traits. Unknown traits are unsupported. Each direct record contains 40 hexadecimal digits,
one space and a validated shared reference name. A single `^` record with a nonzero 40-digit ID may
immediately follow a direct record. Peeled IDs are checked syntactically and discarded; the traits
do not prove object type, existence, or correctness of the peel. Resolution returns the direct
tag-object ID.

Both headerless/unsorted records and Git-generated sorted/peeled files are supported. Claimed sorted
order is checked bytewise. Duplicate names, misplaced headers, orphan/repeated peel lines, blank
lines, unknown comments, invalid names/IDs, missing final LF and packed per-worktree names fail.
Whenever packed fallback is needed, the entire file is read and validated, including unrelated
records; a loose hit does not open it. Every update validates packed-refs. Reads allocate in
proportion to file size without a configurable limit. There is no packed cache or indexing yet.

### Conditional Publication and Reflogs

`update_without_reflog` replaces the named reference itself, including a symbolic reference.
`update_resolved_without_reflog` locks and preserves each symbolic hop and writes only its terminal
name. `Expected::Absent` requires no loose or packed value; `Value` compares the stored target;
`Any` accepts any valid value or absence. Conditions are checked under locks. For resolved updates,
the terminal value is direct or absent, so an expected symbolic value cannot match. Symbolic writes
may create dangling names or cycles, but symbolic HEAD must point into `refs/`; resolution reports
those states separately.

Every write acquires common `packed-refs.lock` by exclusive creation, followed by the destination
lock (or all visited symbolic-chain locks). Holding the packed lock stabilizes packed expectations
and namespace checks against cooperating Git writers and serializes even unrelated girt updates.
Existing locks cause an immediate error, with no retry or lock stealing. Ancestor/descendant
conflicts in packed refs are checked before publication; filesystem conflicts reject loose
namespaces, including empty directories. A successful write renames its complete owned lock over the
loose destination. This shadows an existing packed value without changing packed-refs or unrelated
reference data. Deletion is deferred because removing only a loose file would expose an older packed
value.

These methods deliberately omit all reflog writes regardless of `core.logAllRefUpdates` and leave
existing logs unchanged. Git normally creates/appends relevant logs and records old/new IDs and
identity information. girt's API names make opting out visible at the call site. Prior tips gain no
new reflog-based retention or recovery record, old unreachable objects may become eligible for
pruning, and history shown by `git reflog show` omits girt updates. The test confirms that Git's
`main@{0}` can still return the current tip despite the unchanged log; this selector alone cannot
verify logging. No hooks run, and there is no object existence/type or fast-forward check. This is
low-level reference publication, not full Git update-ref behavior or a checkout operation.

The filesystem must provide exclusive file creation and atomic rename; callers must use a trusted
repository and cooperating lock-protocol writers. Configuration, layout, symlink and lock ownership
must not be changed adversarially during an operation. Reads across refs/hops are not transactional.
Failures before rename preserve old values, but empty directories may remain. Owned locks are
removed on ordinary errors/unwind; cleanup I/O failures or process termination can leave stale
locks. No fsync is issued, so successful visibility does not guarantee crash/power-loss durability
for either refs or objects. Network filesystems and crash recovery have not been validated.

### Evidence and Provenance

Original `tests/references.rs` fixtures invoke Git 2.55.0 in isolated temporary repositories with
ambient Git configuration and environment removed. Git `update-ref`, `symbolic-ref`, annotated
`tag`, `pack-refs --all`, and `worktree add --detach` produce input. Git `check-ref-format`,
`rev-parse`, `show-ref`, `symbolic-ref` and `reflog show` check results. No Git source or upstream
test fixtures were used, and no dependencies were added. References consulted were
[`git-check-ref-format`](https://git-scm.com/docs/git-check-ref-format),
[`gitrepository-layout`](https://git-scm.com/docs/gitrepository-layout),
[`git-pack-refs`](https://git-scm.com/docs/git-pack-refs) and
[`git-update-ref`](https://git-scm.com/docs/git-update-ref).

Tests cover both loose and packed input, annotated-tag peel records, loose shadowing without packed
mutation, byte names, detached/unborn HEAD, dangling symbolic and object targets, exact conditional
updates, and symbolic replacement versus terminal updates. Linked worktree tests compare each
private namespace and shared branches with Git. Rejection/failure tests cover name rules, malformed
records, unsupported backends, cycles/depth, namespace conflicts, zero IDs, symlinks, existing
locks, failed conditions, write/rename failure, owned-lock cleanup, and preservation of unrelated
data. Two-writer races exercise girt/girt and girt/Git conditional updates; exactly one writer
succeeds. These cover cooperating writers, not arbitrary direct file rewrites or crash consistency.

The runnable `publish_branch` example stores two commits, publishes an unborn branch through HEAD,
and conditionally advances it while preserving HEAD's symbolic value. Criterion measures warm loose
reads, HEAD resolution, no-reflog updates, and packed lookups over 10 and 10,000 refs; see the
[reference baseline](benchmarks.md#reference-baseline). Reflogs, multi-ref transactions, deletion,
reftable, object packs, graph traversal, discovery and transport remain outside this capability.

## SHA-1 Pack Reading

`Repository::objects(PackLimits)` returns a synchronous reader with live loose-object access and an
owned snapshot of pack/index pairs. `Objects::read(id, ReadLimits)` returns a verified kind and
exact payload, or `None` for absence. Blob, tree, commit, and tag payloads share this boundary;
structured parsing remains separate. Existing loose typed reads and writes retain their contracts. A
corrupt loose copy fails without falling through to a valid packed duplicate.

### Formats and Validation

Only SHA-1 pack v2 and index v2 are supported, including index large-offset tables, ordinary
objects, OFS_DELTA, and REF_DELTA. Pack v3, index v1, and unknown versions return explicit version
errors. Reserved object types fail when read. Index binary search uses strictly ordered full IDs; an
offset-ordered table identifies entry boundaries and OFS_DELTA bases.

Opening reads all pairs within caller-supplied aggregate byte and pack-count limits. It validates
index lengths, sorted unique IDs, exact fanout counts, large-offset references, unique in-range
entry offsets, pack headers/counts, both SHA-1 trailers, their agreement, and every entry CRC32. An
index without its pack is an I/O error. Unindexed packs are ignored: indexes are publication markers
(see [fetch installation](#upload-pack-fetch)). Entry framing, exact zlib termination, declared
lengths, delta programs, and object identities are checked on demand, including all traversed bases
and intermediate results. Opening alone does not certify payload syntax or all object identities.
The checks detect corruption; SHA-1 collision detection is not provided.

REF_DELTA resolves only inside its own pack, including forward references. An external or missing
base returns `MissingBase`, even if another pack or loose storage contains that identity. Iterative
traversal detects cycles and bounds depth. Reads bound each object, each delta program, and the sum
of all inflated and reconstructed bytes. Limits are charged before decoding/allocation. These bounds
exclude allocator overhead, index tables, fixed decompression scratch space, and structured parsing
performed by callers; they are not a total process-memory quota.

Pack bytes and index tables are retained for the reader's lifetime. Decoded objects are not cached.
Repacking after opening cannot invalidate the owned bytes, but new packs require reopening. Races
while opening can return I/O errors; callers may reopen. Loose reads remain live. Trusted paths and
ancestors are required; this API does not secure hostile concurrent filesystem mutation. Multi-pack
indexes, bitmap/reverse indexes, alternates, partial/shallow repositories, thin packs, live pack
installation, transport, and traversal are outside this capability. Auxiliary acceleration files are
ignored.

### Independent Evidence

The implementation uses the public
[Git pack format specification](https://git-scm.com/docs/gitformat-pack), not Git source code.
`src/pack/tests.rs` constructs original binary fixtures for malformed/truncated inputs, checksums,
CRCs, offsets, ordering, fanout, large-offset tables, reserved kinds, delta instructions, forward
references, nested deltas, missing/external bases, cycles, exact limits, and resource failures.
Fixtures are constructed in memory; no downloaded binary fixtures are used.

`tests/support/pack_git.rs` generates isolated SHA-1 bare repositories using Git `hash-object`,
`mktree`, `commit-tree`, and `mktag`. `pack-objects --stdout --no-reuse-delta --no-reuse-object`
with and without `--delta-base-offset` produces independent OFS_DELTA and REF_DELTA packs.
`index-pack --index-version=2` builds indexes. `verify-pack -v` proves that actual delta entries
exist, and their reported offsets let the fixture assert the intended encoding. After
`prune-packed`, `tests/packs.rs` compares all object IDs, kinds, and exact payload bytes with Git
`cat-file`. It also covers mixed storage, loose corruption precedence, absent objects, orphaned
files, loading limits, and snapshot reads after `repack -ad` deletes the original pair.

The exercised environment is Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64 (Apple M2 Max). Other
operating systems and actual multi-gigabyte packs have not been exercised; small fixtures cover the
64-bit offset representation. The new direct dependency `crc32fast` uses `MIT OR Apache-2.0`
(checked in the resolved 1.5.2 package metadata); it was already transitive via flate2. A
[Criterion baseline](benchmarks.md#pack-read-baseline) records opening, indexed misses, ordinary
reads, and delta reconstruction. `examples/packed_repository.rs` is the runnable consumer.

## Commit History

`tests/support/history_git.rs` generates original commit payloads and stores them through Git
`hash-object`, with alternating timestamps that disagree with ancestry. `tests/history.rs` compares
reachable sets with `git rev-list`, ancestry with `git merge-base --is-ancestor`, and best common
ancestors with `git merge-base --all`. Criss-cross graphs verify two bases in loose and packed
storage; endpoint cases include identity and disconnection. Git 2.55.0 on macOS arm64 was exercised.

Walk ordering is a girt contract, not a reproduction of Git CLI ordering: breadth-first first
encounter, supplied root order, stored parent order, without duplicates. Commit parsing and identity
verification reuse the existing APIs. Missing parents are errors, including when an endpoint already
answers the query. Referenced trees are not resolved. Annotated tags must be peeled by the caller.
Shallow/partial repositories and replacement-object semantics are not supported.

## SHA-1 Pack Writing

`write_pack` exports the caller's explicit object set to separate pack and index `Write` sinks. It
hashes canonical kind/length framing and exact payloads, rejects mismatched identities and
conflicting duplicates, and collapses exact duplicates. `ObjectKind` admits only the four logical
Git kinds. It does not parse payload syntax, traverse references, or require graph closure. This
permits byte-preserving forwarding of objects while leaving graph policy to the caller.

Pack v2 entries and index v2 IDs are ordered by ascending SHA-1 identity. Entries use zlib level 6
without deltas. Identical inputs produce identical artifacts with the same compression backend and
version; dependency upgrades may change compressed bytes. Similar revisions can take substantially
more space than delta-selected packs. The opt-in delta writer is described
[below](#bounded-pack-delta-compression). SHA-256, thin packs, repacking, pruning, GC, multi-pack
indexes, and transport are excluded. No dependencies were added.

`PackWriteLimits` bounds input occurrences before allocation/sorting, each payload, total input
bytes including duplicates, and both output lengths. Payloads are borrowed and compressed directly
into the sink; metadata grows with input count and zlib uses its own scratch space. Limits are not a
process-memory ceiling. Pack counts fit `u32`; byte counters use checked `u64` arithmetic. Index
large-offset references have 31-bit slots. Output checks include trailers and never write beyond the
configured byte limit.

Input validation failures leave both sinks untouched. Later failures may leave partial artifacts, or
a complete pack and incomplete index. Both must be discarded. Success includes flushing both sinks,
not filesystem synchronization. Sinks must start at artifact offset zero. Buffered sinks are
recommended for files because index tables are emitted incrementally.

The API does not install artifacts. It never removes loose objects or existing packs. The example
assembles a new, private repository before any readers exist; its renames are not a live publication
protocol. Validated received-pack installation is now provided separately by
[fetch](#upload-pack-fetch), with index-last discovery and no-clobber publication. The writer itself
continues to produce caller-owned artifacts only.

Original writer code and synthetic fixtures follow the public
[Git pack format specification](https://git-scm.com/docs/pack-format); no Git source or tests were
copied. `tests/packs.rs` exports Git-generated mixed-kind payloads and original binary/4 MiB blobs,
then runs independent `git index-pack --index-version=2`, compares the complete regenerated index,
runs `git verify-pack`, and checks exact `git cat-file` and girt reads from pack-only private
stores. Empty packs and repeated input objects are included. Tests beside the writer cover
deterministic ordering, identity/kind mismatches, duplicate conflicts, short writes, write/flush
errors, limits, header boundaries, and counter overflow.

The original synthetic index fixture checks offsets immediately below and at 2 GiB and at 4 GiB,
including exact 32-bit slots, 64-bit table bytes, checksums, and reader interpretation on 64-bit
hosts. It does not allocate or validate an actual multi-gigabyte pack. Git 2.55.0 on macOS 26.6.2
arm64 was exercised; other platforms and actual multi-gigabyte output remain untested.

## Upload-Pack Fetch

`fetch::receive` implements a single protocol v0 upload-pack session over blocking `Read`/`Write`
streams. `fetch::receive_local` is the local transport adapter: it starts a trusted local
`git upload-pack` server. It does not use `git fetch`, `fetch-pack`, `index-pack`, or Git parsers on
the client path. The optional [HTTP adapter](#smart-http-and-https) is separate; SSH and credential
discovery are not provided.

The client exposes complete byte-preserving advertisements, capability tokens, and peeled tag hints.
The caller selects explicit advertised tip IDs; peeled hints are not wants. Empty selection sends a
flush and expects EOF. Nonempty selection requires `side-band-64k`, requests `ofs-delta` only when
advertised, sends no haves, then sends `done` and requires NAK. This deliberately simple negotiation
requests complete history on every fetch, including incremental fetches. It sacrifices bandwidth to
keep the received graph independent of destination state. Thin/shallow/filter features, include-tag,
are never requested by the full-transfer entry point. The incremental entry point below also
supports bounded multi-ACK. v1/v2 and non-SHA-1 advertisements fail explicitly.

Packet lengths, aggregate wire bytes, advertisements, refs, wants, and pack bytes have explicit
bounds. Channel 1 carries pack data, channel 2 reaches the caller's progress callback, and channel 3
or `ERR` fails with the peer's message bytes. Truncated packets, unexpected negotiation/channel
messages, missing final flush, and bytes after that flush fail. Streams must end at EOF; this is not
a reusable or stateless HTTP session. The local adapter also requires a successful server exit.

Import checks pack v2 framing, object count, SHA-1 trailer, exact zlib entry termination, and delta
programs. OFS_DELTA and forward/backward REF_DELTA resolve only inside the received pack; external
bases fail even if present locally. All resolved objects receive independently computed identities,
and duplicate identities are rejected. girt generates index v2 offsets, CRCs, fanout and checksums
from the received entries. Reachability from each selected tip checks commit trees and parents, tree
child kinds, and annotated tag target kinds using girt's parsers. Gitlinks refer to separate
submodule stores and are not followed. Tree names and ordering are validated. This is supported
syntax/connectivity validation, not full `git fsck`, signature verification, or SHA-1 collision
detection. Unreachable extras get pack/identity validation but not structured-payload validation.

Object count, individual payload/program sizes, cumulative inflation/reconstruction bytes, delta
depth, resolution visits, and reachable edge occurrences are bounded. Resolution retains decoded
objects until connectivity succeeds and then releases them. It visits unresolved entries in passes;
the work limit bounds pathological forward chains rather than promising linear resolution time.
Index/table memory grows with object count; graph metadata grows with objects/edges. Parsing can
copy structured payloads. These are input/work bounds, not an allocator or process-memory quota.

Cancellation is checked between I/O calls, packets, objects, and graph steps, and a progress
callback can cancel. Interrupted I/O is returned without retrying. A flag cannot interrupt a blocked
caller-owned stream read or one inflation/hash. The local adapter now provides
[owned transport interruption](#owned-transport-interruption), including absolute deadlines and
process-group cleanup on macOS/Linux. Generic streams must provide their own interruption.

### Installation and Reference Policy

The received result owns validated pack/index buffers and makes no filesystem changes. Explicit
`install` writes and syncs private temporary files in the destination pack directory, then publishes
the pack first and index last without replacing either path. Identical existing files are reused;
different bytes fail. Readers discover indexes, so an unindexed pack is invisible and an index must
have its complete pack. Existing snapshots retain their bytes; new snapshots see the old object set
or the new pair. Concurrent deletion or repacking by other tools can still require an opening retry.
Paths must be trusted, and callers must coordinate pruning/GC until references protect the objects.

Failure never removes or overwrites preexisting objects. Failure between publications may leave a
complete unindexed pack; retrying the same received result can finish it. Ordinary failures clean
private temporaries; crashes may leave them. Directory entries are not synced, so power-loss
durability is not promised. Installation does not repair corrupt existing loose objects that shadow
packed objects. Reopen and read with suitable limits before using the destination as a source.

Fetch never updates refs, reflogs, or `FETCH_HEAD`. Callers may use the existing explicit
`update_without_reflog` operations after installation, with expected old values. Each update is
independent; failure of a later update does not undo earlier successes. No multi-ref atomicity is
implied. Remote configuration, refspecs, automatic tag following, pruning, shallow/partial stores,
authentication helpers and push remain outside this capability.

### Fetch Evidence and Provenance

Original code and synthetic fixtures follow the public
[pack protocol](https://git-scm.com/docs/pack-protocol),
[capability](https://git-scm.com/docs/protocol-capabilities), and
[pack format](https://git-scm.com/docs/gitformat-pack) specifications. No Git source/tests were
copied. `tests/fetch.rs` starts disposable local upload-pack servers using independently generated
Git objects. It covers all object kinds, actual server-produced deltas, selected branches/tags,
empty, repeated and incremental fetches, exact Git/girt payload agreement, strict Git fsck, and
byte-for-byte agreement with independently regenerated Git indexes. Local tests exercise
malformed/truncated protocols, peer errors, cancellation/interrupted I/O, corrupt packs,
internal/absent bases, missing and mistyped connectivity, limits and exact boundaries. Publication
tests cover conflicting paths, retry after index failure, preserved existing objects, concurrent
publishers/openers, old snapshots, and concurrent reference changes with conditional-update
rejection.

The runnable `cargo run --example fetch_local` builds both repositories from scratch, fetches three
objects, publishes a branch conditionally without a reflog, and reads the fetched blob. The
[Criterion baseline](benchmarks.md#fetch-baseline) measures advertisement processing and replayed
protocol/import/connectivity separately from server execution and disk publication. Git 2.55.0,
rustc 1.98.1, and macOS 26.6.2 arm64 were exercised. Other platforms, actual multi-gigabyte packs,
hostile filesystems and concurrent GC are not covered by this evidence.

## Receive-Pack Push

`push::PreparedPush::new` prepares an immutable command list and a complete non-thin SHA-1 pack.
`push::send` implements the receive-pack v0 client over caller-owned blocking streams;
`push::send_local` starts a trusted local Git receive-pack server with binary pipes. No Git client
command selects objects, builds the pack, applies client force policy or parses status. The adapter
clears inherited environment except PATH, disables system/global configuration and forces v0. Local
server configuration and hooks still apply. Both the executable and destination must be trusted.

### Commands and Reachability

Commands name full `refs/heads/` or `refs/tags/` destinations and carry an expected old value,
desired nonzero new ID, and explicit force policy. `None` expects absence; `Some(id)` expects that
exact nonzero ID. Duplicate destinations, HEAD, other namespaces and deletion are rejected.
Advertisement mismatch rejects the whole request before commands are sent. Hidden refs cannot
satisfy an expected existing value. Server old-value checks protect against races after discovery;
force does not remove the expectation.

Branch tips must be commits even with force enabled. By default, an existing branch can advance only
when the old ID is reachable through the new commit's parent graph. An existing tag can retain its
ID but replacement requires `ForcePolicy::Allow`. Creation is allowed, and tags may directly name
any supported object type. Explicit force permits branch rewinds or tag replacement; it cannot
bypass server configuration or hooks. Same-ID commands are still sent conditionally and can be
rejected by server policy. Local refs, tracking refs, configuration, reflogs and working-tree files
never change. The server controls its own ref/reflog and object-storage effects.

Selection follows all commit parents and trees, typed tree children and annotated-tag targets,
including nested tags. Gitlinks are external submodule commits and are not followed. Every selected
object must exist locally and pass identity and supported payload validation, even if the
destination already has it. Each distinct object is read once through existing loose/packed readers,
although a packed read can decode shared bases again. All typed edges are checked. A missing or
invalid object fails preparation before any remote command. This is not a claim of full Git fsck
equivalence.

The default writer emits ordinary zlib entries. `PushLimits::compression` can enable bounded
internal REF_DELTA entries; both policies produce complete packs without external bases. Its
companion index is generated into a sink, not sent or installed. Repeated and incremental pushes
using `new` retransmit full selected histories; `new_excluding` can reduce them as described below.
Preparation releases selected payloads after buffering the pack; the caller can drop its object
reader before connecting. Inputs, graph objects/bytes/edges, cumulative ancestry visits/parent
edges, individual reads, pack/index output, command bytes, advertisement bytes/entries and status
bytes have explicit bounds. These bounds exclude allocator overhead, the caller's preexisting
snapshot, and server memory. Read decoding budgets apply per object, not across the selection.

### Status, Failure and Transport Boundaries

Nonempty pushes require and request `report-status`. Unknown optional capabilities are ignored.
Protocol v1/v2 advertisements, SHA-256, shallow advertisements and report-status-v2-only servers are
rejected. No atomic, sideband, deletion, push-options, signed-push or report-status-v2 capability is
requested. Receive-pack `.have` entries are validated and counted and can confirm an explicit
receiver root used for exclusion; fetch peeling hints are not accepted as receive-pack refs. The
wire framing implementation is shared with fetch, while service-specific negotiation and parsing
remain separate.

A complete `PushReport` preserves unpack status and one result per command in caller order,
retaining rejection messages as bytes. `Ok(report)` can contain complete rejection or partial
success: callers must inspect `all_succeeded` and individual results. Multiple refs are never
promised atomicity. Missing, duplicate, unrequested, contradictory or malformed status, premature
EOF and trailing bytes are errors. Server success is an acknowledgement, not a durability guarantee
or a claim that another writer cannot subsequently change a ref.

`PushError::NotSent` means no commands were attempted. Once transmission starts, transport,
cancellation, bound or protocol failure becomes `Uncertain`, preserving every valid status received
so far. Missing acknowledgements mean unknown outcomes, not rejection. Inspect destination refs
before retrying; killing receive-pack does not roll back updates already applied. Even a complete
report is retained when final EOF or local process completion fails. A successful unpack alone does
not establish any ref update. Rejected pushes may leave server-side objects, and hooks can have
independent side effects.

Cancellation is checked between I/O calls, packets, graph steps and pack writes. Interrupted
protocol I/O is propagated without retrying. A flag cannot interrupt a blocked stream, a single
storage read, hash, parse or compression call. The local adapter provides
[owned transport interruption](#owned-transport-interruption) for blocked pipe and server-exit
waits, including stalled hooks. Caller-owned streams must provide interruption/deadlines if needed
and must be closed after errors. Streams are one-shot and must end at EOF after the final status
flush.

SSH, credential discovery, remote/refspec configuration, automatic force, pruning, thin packs,
deletion, atomic multi-ref push, report-status-v2/proc-receive rewriting and server infrastructure
remain deferred. Empty command lists exchange only advertisement and flush, with no pack or status
report.

### Push Evidence and Provenance

The implementation and synthetic packets are original, based on the public
[pack protocol](https://git-scm.com/docs/pack-protocol) and
[capability](https://git-scm.com/docs/protocol-capabilities) specifications and observable server
behavior. No Git implementation or test expression was copied; no dependencies were added.
`tests/push.rs` uses disposable repositories and independently Git-generated OFS/REF-delta sources.
It verifies empty destinations, branch creation/advancement, annotated tags, repeated publication,
all object kinds, exact Git/girt payload agreement and strict Git fsck. Original girt-written
fixtures also cover nested tags, tree nesting, binary blobs, symlinks and missing external gitlinks.

Failure tests cover stale advertisements, an update-hook ref race, explicit force with and without
server permission, checked-out-branch rejection, pre-receive rejection, and partial success with an
update hook. Unit tests cover malformed/truncated status, unsupported protocol/capabilities, missing
or mistyped graph edges, malformed reachable payloads, duplicate commands, resource bounds and exact
limits, short writes, interrupted reads/writes, flush failure and cancellation. Unknown results
remain distinguishable from acknowledged rejection, and valid status prefixes survive truncation.

`cargo run --example push_local` creates two private repositories, publishes a branch and annotated
tag, checks the report and reads the transferred blob. The
[Criterion baseline](benchmarks.md#push-baseline) separates reachable selection/read/validation/pack
construction from prepared-protocol replay. Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64 and Apple
M2 Max were exercised. Other platforms, multi-gigabyte packs, concurrent source GC, hostile
filesystems and blocked-I/O cancellation are not established by this original push baseline. Later
platform and transport evidence is recorded separately.

## Owned Transport Interruption

`TransportControl` carries a borrowed cancellation flag and an optional absolute monotonic deadline.
`fetch::receive_local_with_control` and `push::send_local_with_control` use it for owned local Git
servers. Existing `receive_local` / `send_local` signatures remain usable and now interrupt blocked
pipes when their flag is set. Generic `receive` / `send` streams retain cooperative cancellation:
only their owner can arrange to unblock an arbitrary `Read` or `Write` implementation.

The deadline expires at the caller's chosen `Instant`, includes time already elapsed before entry,
and is never reset by traffic. Checks run before path resolution/spawn, at each owned pipe
operation, and during server-exit waits. Waiting polls use at most 20 ms, subject to scheduler
delays. Path resolution, spawn, callbacks, decoding, hashing and other synchronous computation
remain outside forced interruption; no whole-call hard real-time bound is claimed. Push preparation
and subsequent fetch installation are separate operations. Cleanup and kernel-delayed child reaping
can extend elapsed time beyond the deadline.

Cancellation takes precedence if both controls are observed together. Already-returned protocol
errors are not overwritten during cleanup. Each pipe attempt checks control before reading/writing;
bytes merely buffered by the OS do not count as known status. At the final server-exit wait, an
already observable exit wins over a racing cancellation/deadline. Otherwise interruption returns
`Cancelled` or `Deadline`, distinct from peer rejections. Push failures after attempted transmission
are uncertain, preserving every valid unpack/ref acknowledgement, including a complete report if EOF
or exit is still pending. Killing a server cannot roll back ref updates or hook side effects.

On macOS/Linux, servers start in their own process group. Nonblocking pipes and polling run on the
calling thread; there are no background I/O workers to detach or join. Stderr is drained with
bounded work and discarded so a full diagnostic sink cannot block the operation. Protocol
progress/status remains available. On completion, error or unwinding, cleanup sends SIGKILL to the
owned group before reaping the direct child. Exit observation uses `waitid(WNOWAIT)` to reserve the
leader's PID until the group is signalled. Callers must not reap these children globally or enable
automatic SIGCHLD reaping. Descendants which deliberately leave the group are outside this
trusted-server contract; girt signals remaining group members but cannot reap grandchildren.
Unrelated process groups are never intentionally signalled. Windows and other OSes explicitly return
unsupported before spawn.

Material compatibility changes are interruptible local flags, discarded rather than inherited
stderr, group cleanup even on success, removal of upload-pack's implicit 30-second idle timeout in
favor of caller-controlled deadlines, new deadline error variants, and explicit unsupported local
adapters outside macOS/Linux. The examples use 30-second absolute deadlines. The target-specific
`rustix = "1"` dependency supplies safe nonblocking/poll/wait/signal operations; its MIT or
Apache-2.0 license options are compatible with this crate (already a transitive dependency through
tempfile).

Original finite shell servers exercise silent advertisement, stalled reads/writes, final EOF and
exit waits, partial/complete push acknowledgements, full stderr/stdout pipes, cancellation races,
pre-spawn interruption, child reaping, unwinding and descendant cleanup without harming an unrelated
child. Scripted servers are independent fixtures, not copied Git implementations. Disposable Git
pre-/post-receive hooks establish cancellation after transmission, unchanged refs before commit, and
already-changed refs after commit. Pre-spawn push interruption leaves refs and objects absent. No
transfer contacts a network or a real remote.

Runtime evidence for this change is macOS arm64 with Git 2.55.0. The parent revision's Linux/Windows
CI evidence above does not cover these new paths; this change has not been published or run
remotely.

Validation passed `just check` (817 unit, integration and documentation tests, formatting,
all-target Clippy and docs.rs), warning-denying private Rustdoc, both local examples and
markdownlint-cli2. Warning-denying library Clippy cross-checks passed for
`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-gnu` and `x86_64-pc-windows-msvc`. These are compile
checks, not target runtime checks.
[Owned transport benchmark evidence](benchmarks.md#owned-transport-baseline) retains local
process/import/cleanup measurements and source fingerprints.

## Bounded Incremental Transfers

The full-transfer entry points remain available as reproducible baselines. Fetch adds
`KnownHistory::new`, `receive_with_known` and `receive_local_with_known`. Explicit local roots are
read with verified identities and complete typed connectivity before use. Trees, commit parents and
tag targets are followed; gitlinks remain external. Missing, corrupt or mistyped local history fails
preparation. No local refs are inferred and no shallow or partial history is accepted.

Negotiation follows the public [pack protocol](https://git-scm.com/docs/pack-protocol) and
[capability specification](https://git-scm.com/docs/protocol-capabilities), implemented
independently. The client sends one batch of at most 32 verified commits followed by `done`,
requesting `multi_ack` when advertised. Without that capability it sends at most one have. Each
continuation must name a unique offered ID, followed by a plain ACK of an offered ID; NAK is
accepted when no continuation was received. Unsupported states, truncation and excess replies fail.
The fixed small batch avoids mutually blocked pipes while both peers write negotiation packets. This
is a bounded strategy, not an optimal ancestry search: distant shared history outside the batch can
be retransmitted.

Locally known wants are omitted from wire wants. An entirely known selection sends a flush and
receives no pack, retaining selected IDs and dependency evidence. Received delta bases must still be
internal, including when a matching local object exists. Connectivity checks traverse the union of
received and verified local objects. The result retains IDs only for local objects actually used by
that traversal. Installation reopens the destination with default `PackLimits` and rechecks their
identities before creating pack artifacts, including on no-op fetches. GC/pruning must remain
coordinated through installation and separate ref publication; this API takes no retention lock.

Local verification separately bounds root occurrences, unique objects, retained payload bytes, edge
occurrences and decoding per read. Decoding work is at most the object-count bound times the
per-read bound. Receive/import budgets remain separate, and the connectivity edge limit now covers
the combined graph. Installation rechecks at most the retained local dependency count and payload
budget, with the same per-read bound. Knowledge preparation is outside the transport deadline;
blocking local I/O, parsing and hashing retain the existing cooperative cancellation contract.

Push adds `PreparedPush::new_excluding` with explicit receiver roots. It first validates the
complete selected graph and proves force policy, then omits each usable root's entire closure. A
usable root must itself be in that graph; missing or disconnected roots are ignored, producing a
conservative full transfer where necessary. A rewritten receiver tip outside the selected graph
cannot establish shared descendants in this implementation, even if that tip happens to exist
locally. Tags and shared trees/blobs are supported; gitlinks remain external. Exclusion gets a
separate `max_edges` allowance and at most `max_refs` root occurrences, and visits at most the
selected object count.

Every root actually used for exclusion must still appear in the live receive-pack advertisement as a
tip or `.have`. Otherwise `KnowledgeChanged` fails before commands; callers can explicitly retry
with full preparation. Command expectations are independently checked before transmission and again
by the server when committing refs. Races after advertisement retain server rejection or uncertain
outcome semantics; complete and partial statuses are unchanged. Server-side concurrent object
pruning still requires coordination. Packs remain non-thin; ordinary entries remain the default and
bounded delta selection is now opt-in. No HTTP/SSH, credentials, pruning, shallow/partial support or
new ref features are introduced.

Disposable Git tests cover initial, no-op and incremental transfers, shared/divergent and merge
histories, branch/tag selection, gitlinks, unavailable receiver roots, corrupt/missing local
objects, ACK states and bounds, installation failures without ref publication, expected-ref races
and partial push status. The existing push integration helper now exercises exclusion using expected
old tips, including the rejection and race tests. Fixtures are independently generated with Git
plumbing and girt writers. Measurements and exact platform evidence are recorded with the
[incremental benchmark](benchmarks.md#incremental-transfer-comparison) and
[completion checklist](testing.md#incremental-transfer-completion).

## Bounded Pack Delta Compression

`write_pack_with_compression` accepts `PackCompression::Delta(DeltaOptions)`; `write_pack` and
`PushLimits::default()` retain the ordinary streaming path. No dependencies are added. Options bound
the preceding-entry window (16), suitable candidates (4), eligible payload bytes (1 MiB), search
units per object (8 million), and emitted dependency depth (4). Zero bounds disable search; objects
outside the size bound stream ordinarily. The default minimum complete-entry saving is 16 bytes. The
public Rustdoc defines the work units and scratch-memory estimate; these are algorithmic bounds, not
measured RSS or hard latency guarantees.

Sorting and deduplication still precede selection. Pack and index entries remain in ascending
object-ID order. Candidates come from the preceding window, newest first, with the same kind, length
within a factor of two, and depth below the bound. A fixed 4096-slot anchor table searches byte
sequences without interpreting text or structured payloads. Collisions retain the earliest base
position. Bounded sampling can miss useful matches, especially far into large objects or outside the
window; this writer does not promise Git's compression ratio.

Original delta programs encode exact base/result sizes, literals of at most 127 bytes, and copies
with four offset bytes and three size bytes. A copy size of zero means 65,536 bytes; longer copies
split at 16,777,215 bytes. Encoding follows the published
[pack format](https://git-scm.com/docs/pack-format), without consulting or adapting Git
implementation source. Unit vectors and fixtures are original.

Selection compares actual zlib-level-6 output including the entry header and 20-byte REF_DELTA base
identity. It chooses a delta only when it strictly improves the current best and meets minimum
savings against ordinary encoding. Ties preserve the first candidate. Exhausting search work drops
the unfinished candidate and retains the best completed entry. Fixed inputs/options and compressor
versions reproduce identical bytes regardless of input order. Pack sizes never exceed the ordinary
policy for the same object set, but preparation can cost substantially more CPU and scratch memory.

Only preceding internal bases are used, so there are no forward dependencies, cycles, thin packs, or
external-base retention assumptions. REF_DELTA uses an object identity rather than a relative pack
offset; index offsets retain the existing checked absolute-offset and large-offset encoding. No
OFS_DELTA entries are emitted. This avoids requiring the optional `ofs-delta` receive-pack
capability described by Git's
[protocol capabilities](https://git-scm.com/docs/protocol-capabilities). A server advertising only
`report-status` is explicitly exercised. OFS optimization is deferred.

Push selection runs after receiver-history exclusion, keeping even incremental delta bases inside
the outgoing pack. Cancellation is checked between candidates and every 4096 search units; one hash,
storage call, or zlib compression remains non-interruptible. Input identity, output bounds,
no-clobber installation, force/expected-old checks, and uncertain-outcome semantics are unchanged.

Independent `index-pack` produces the exact same index; `verify-pack -v` proves emitted deltas, and
Git `cat-file` and girt check exact payloads/kinds/identities. Tests include shifted 1 MiB binary
objects, generated histories, empty/tiny entries, and a disposable delta push followed by
incremental transfer. Library tests cover every logical kind without assuming valid structured
syntax. Existing malformed-delta reader tests remain applicable. Evidence is macOS arm64 with Git
2.55.0 and Rust 1.98.1; no new Linux or Windows runtime claim is made. HTTP/SSH, credentials, thin
packs, GC, and repacking remain outside this change.
[Measurements](benchmarks.md#bounded-delta-comparison) retain size, timing, search-count, and
source-fingerprint evidence.

## Smart HTTP and HTTPS

The optional `http` feature adds explicit v0 discovery and upload-pack/receive-pack RPCs. Requests
use exact smart service media types and service preludes. A fetch sends the complete want/have/done
batch in one POST; no-op fetches and empty pushes use discovery only. Full/incremental graph
validation, receiver-history exclusion and optional internal delta compression keep their existing
contracts. HTTP fetch returns a bounded download for explicit synchronous validation and
installation; HTTP push consumes a previously prepared pack. See [HTTP contracts](http.md) for
runtime, authentication, TLS, resource and failure semantics.

Original loopback fixtures invoke actual Git `http-backend` as a CGI service in private
repositories. Git generates the source history, branch, annotated tag and delta pack. Tests compare
girt reads, Git `cat-file` bytes, selected IDs and published refs. Initial fetch/push, a one-commit
incremental transfer, known-only fetch and empty push all succeed; recorded request methods verify
when a POST is omitted. HTTP push also exercises explicit supplied authorization and mixed
accepted/rejected refs through an original update hook.

Independent fixture modes cover authentication rejection, redirects, HTTP 503, malformed/duplicate
headers, wrong media types, content encoding, malformed service prelude, protocol v2, body
truncation and decoded/wire limits. Truncated receive-pack responses retain completed
acknowledgements while unknown refs remain uncertain even when the server has updated them. Complete
Git status followed by incomplete HTTP framing also remains uncertain. Deadlines/cancellation
interrupt stalled discovery, TLS handshakes, a large incompressible upload and status bodies;
partial acknowledgements are retained. Request counts establish that failures do not trigger
retries.

TLS fixtures generate an isolated CA and localhost leaf with OpenSSL, exercise real HTTPS push and
fetch, and reject untrusted chains and hostname mismatch. No real remotes or global trust stores are
used or modified. Fixture code is original; behavior was guided by Git's
[HTTP protocol specification](https://git-scm.com/docs/http-protocol), rather than copied Git
implementation or tests.

New runtime evidence is macOS arm64, Rust 1.98.1, Git 2.55.0, Python 3.14.7 and OpenSSL 3.6.4 on
2026-09-23. Earlier CI for other capabilities does not validate this adapter. Linux workflow
coverage is configured but has not been run for this change; Windows HTTP runtime remains untested.
SSH, credential discovery, proxy use, redirects, protocol v2, shallow/partial repositories and
remote/refspec policy remain outside this change. The HTTP fixture is test infrastructure, not a
supported server.

## SSH Transport

The optional `ssh` feature adds one OpenSSH process per v0 upload-pack/receive-pack exchange on
macOS/Linux. Literal endpoint components and an explicit trusted executable/config file define the
boundary. Host-key verification and noninteractive policy are enforced; remote paths are POSIX-shell
quoted. Fetch returns a bounded download for synchronous validation, while push borrows a prepared
pack. See [SSH contracts](ssh.md) for supported inputs, runtime, bounds and recovery.

Original isolated sshd fixtures with throwaway keys demonstrate actual Git full, incremental and
known-only fetch; initial, subsequent and no-op push; branches, annotated tags, internal deltas and
receiver-history exclusion. Git and girt read the resulting payloads independently, and Git
verify-pack checks fetch delta entries. Real update-hook rejection preserves mixed ref outcomes;
cancellation during a post-receive hook remains uncertain after Git has committed the ref. Host
trust tests accept the fixture key and reject unknown/changed keys; a different client key fails
authentication. Paths containing spaces, quotes and shell metacharacters round-trip literally.

Separate transport fault services exercise malformed envelopes, bounded wire input, stalled SSH
handshake and Git service, blocked upload, full stderr, status/EOF/exit waits, and cancellation.
Complete or partial acknowledgement prefixes remain visible on uncertain outcomes. Unit process
tests check dropped futures, reaping, cancellation before writes, and group cleanup without killing
an unrelated child. Fault services do not substitute for actual Git interoperability.

Implementation and original fixtures use the public
[Git pack protocol](https://git-scm.com/docs/gitprotocol-pack),
[OpenSSH client](https://man.openbsd.org/ssh) and
[OpenSSH configuration](https://man.openbsd.org/ssh_config) contracts and observed behavior. No Git
source or tests were copied or adapted. New runtime evidence is macOS arm64, Git 2.55.0, OpenSSH
10.3p1 and Rust 1.98.1. No Linux runtime result or Windows SSH support is claimed. Broader async
filesystem/object-store architecture remains undecided.

## Transfer Ownership and Operational Review

The review follow-up preserves exact object bytes, typed graph checks, retained pack snapshots,
owned negotiation history, installation/reference separation, and uncertain push reports. Focused
regressions cover caller-selected installation limits and retry, cancellation before pack output,
artifact paths and reachable-object diagnostic context, and bounded local upload/status progress
with retained acknowledgements. HTTP/SSH lifetime and actual Git interoperability tests exercise the
common downloaded-fetch state.

The original early-rejection fixture fills stdout before resuming stdin consumption. It reproduced a
local write-before-read deadline and now completes with its rejection report. This establishes
finite bidirectional backpressure handling; it is not a claim that a particular Git release emits
that exact response. Actual Git push integration remains separate evidence.

The original forward REF_DELTA fixture in `tests/support/forward_delta.rs` contains unique
eight-byte blobs and literal-only delta programs, with each chain ordered deepest-first. Git
`index-pack --strict` accepts it and `cat-file` returns the expected tip. Girt accepts the
65-object, depth-64 chain with 4,225 resolution visits and rejects a budget one visit smaller.
[Measurements](benchmarks.md#forward-delta-ordering) quantify this supported worst-order shape.

Fixture startup owns its process before parsing readiness, uses a ten-second startup deadline, and
includes retained stderr in failures. SSH readiness uses test-only `serde_json` (MIT OR Apache-2.0,
as declared by the resolved package); no runtime dependency is added.

Review follow-up validation on macOS arm64 passed `just check` (685 unit tests, 314 integration
tests, 12 doctests, formatting, all-target/all-feature Clippy and docs.rs), warning-denying private
Rustdoc, the disposable HTTP/SSH/local-fetch examples, owned-history worker compiler probes, and
Markdown lint. Independent core-only, HTTP-only, and SSH-only library checks also passed. Linux GNU
and Windows GNU library cross-checks with SSH passed. Windows all-feature cross-compilation was
blocked in `aws-lc-sys` by the unavailable `x86_64-w64-mingw32-gcc`; native Windows CI remains the
follow-up for that build. No new Linux or Windows runtime result is claimed.
