# Git Compatibility Evidence

Loose storage supports SHA-1 blobs, trees, commits, and tags with canonical object headers. The
caller can supply an object directory and its known `ObjectFormat::Sha1` format, or use `Repository`
to open an explicit repository path and detect the format from local configuration. SHA-256 storage
is recognized and rejected. Crate Rustdoc owns the API examples and complete limitations.

## Current Capabilities and Evidence

The current API supports SHA-1 loose objects, pack/index v2, complete-history queries, object-only
fetch, conditional branch/tag push, reference enumeration, and conditional transactions with
explicit reflog policy, named remote/refspec mapping, explicit fetch orchestration, and
tracking-layout clone into bare or ordinary no-checkout repositories, recursive tree comparison, and
byte-preserving content diff, SHA-1 working-tree index v2 read/replacement, and raw read-only
working-tree status and conservative raw tree checkout on macOS/Linux. The single-reference
no-reflog operations remain available. HTTP and SSH downloads share owned validation state.
Installation takes explicit destination snapshot limits. Read and operation limits remain per phase;
no process-wide heap or hard CPU-latency guarantee is implied.

| Platform       | Current evidence boundary                                |
| -------------- | -------------------------------------------------------- |
| macOS arm64    | Full suite through raw checkout, including HTTP and SSH. |
| Linux x86_64   | Full suite through raw checkout, including HTTP and SSH. |
| Windows x86_64 | Portable integration suites and bounded HTTP runtime.    |

The [final roadmap validation](#final-roadmap-validation) records the current native results and the
Windows exclusions. Run IDs, counts and environments apply only to their stated revisions; later
code changes require new evidence.

## Platform and Git-Version Validation

### Final Roadmap Validation

[Run 36089083146](https://github.com/joshka/girt/actions/runs/36089083146) passed all four native
jobs on 2026-09-25 UTC at code/workflow revision `cf8b49b3dcaaa7335ebb6454278e9db5d7638bbb`,
published on `joshka/roadmap-validation`. It includes content diff, index read/write, raw status,
raw checkout, and the checkout preparation fix rejecting planned nested-repository markers before
worktree mutation.

| Native runner       | Units | Integration | Doctests | Total |
| ------------------- | ----- | ----------- | -------- | ----- |
| Ubuntu 22.04 x86_64 | 1236  | 517         | 19       | 1772  |
| Ubuntu 24.04 x86_64 | 1236  | 517         | 19       | 1772  |
| macOS 14 arm64      | 1236  | 515         | 19       | 1770  |
| Windows 2022 x86_64 | 941   | 244         | 19       | 1204  |

Unix runtime suites enable all features. Windows units/doctests enable all features; integrations
comprise 237 core-only cases across the deliberate portable selection plus seven HTTP-only cases.
The new Windows selection executes 16 content-diff cases, 17 index cases, two status boundary cases
and two checkout boundary cases. The latter verify cancellation and unsupported-platform refusal,
not Windows status traversal or checkout materialization. All selected tests passed without ignored
or filtered cases. The [suite inventory](testing.md#native-platform-coverage) retains the
operation-specific exclusions.

All runners independently passed core-only, HTTP-only and SSH-only library compilation and
all-feature/all-target Clippy with warnings rejected. Unix private Rustdoc passed with warnings
rejected. The content-diff and index examples ran on every host; raw status and checkout examples
ran on Unix alongside local fetch/push and HTTP/SSH examples. All hosts used Rust 1.98.1; Git was
2.55.0 on Unix and 2.55.0.windows.5 on Windows/MSVC. Compilation and examples are additional
evidence, separate from test counts. Local `just check` passed after the final fixes with 1,770
tests, formatting, Clippy and docs.rs. The four new examples, Windows GNU core-library cross-Clippy
and workflow `actionlint` also passed locally; the cross-check is compilation evidence only.

[The initial checkpoint](https://github.com/joshka/girt/actions/runs/36088621269), revision
`db183b8aa1cb9988e356229c4f80d185b161886e`, exposed two validation failures. Ubuntu 22.04 failed an
assertion expecting a directory mutation to change timestamps during a short scan. The fixture now
sets an old directory mtime first, making the observed metadata change independent of clock tick
resolution. Windows Clippy rejected a cancellation helper returning the large checkout error by
value. The unsupported-platform path now selects its cancellation/refusal cause directly; the helper
is compiled only for the native checkout implementation. Public errors and cancellation precedence
are unchanged. Both previously failing stages pass in the final native run.

Raw status/checkout remain macOS/Linux-only and preserve literal bytes and POSIX modes without
attributes, filters, EOL conversion, ignores or Git configuration normalization. macOS requires
ASCII names in enumerated directories; Linux preserves supported byte filenames. Checkout retains
its conservative path, gitlink, repository-marker and index-extension refusals, caller exclusion of
concurrent writers, partial-operation reporting and lack of rollback/crash durability. This run does
not expand those contracts. Windows refs/reflogs, composed fetch/clone publication, local/SSH
process adapters and actual raw status/checkout remain unsupported. Windows HTTP evidence remains
the bounded plaintext suite; HTTPS/trust and the full Unix transport fault matrix remain untested.

This result record is a later Markdown-only local child, validated with rumdl and markdownlint-cli2;
it is not the CI-tested revision. Historical results below remain tied to their stated revisions.

### Portable Integration Validation

[Run 36078287817](https://github.com/joshka/girt/actions/runs/36078287817) passed all four jobs on
2026-09-25 UTC at revision `86ba2618b4d3ec598428d24a7b075ba3c205608e`, published on
`joshka/portable-platform-validation`. This revision includes repository initialization/discovery,
reference enumeration/deletion, reflog transactions, remote/refspec mapping, fetch orchestration,
clone, and recursive tree comparison. Native results are:

| Native runner       | Units | Integration | Doctests | Total |
| ------------------- | ----- | ----------- | -------- | ----- |
| Ubuntu 22.04 x86_64 | 960   | 441         | 16       | 1417  |
| Ubuntu 24.04 x86_64 | 960   | 441         | 16       | 1417  |
| macOS 14 arm64      | 960   | 439         | 16       | 1415  |
| Windows 2022 x86_64 | 793   | 207         | 16       | 1016  |

Unix runtime suites enable all features. Windows units and doctests enable all features, while
integration coverage consists of 200 core-only tests across nine explicitly selected suites and
seven HTTP-only tests. The [suite inventory](testing.md#native-platform-coverage) records operations
and exclusions. Windows now exercises objects and loose storage, pack/index reading and writing,
history queries, structural tree comparison, repository opening/initialization/discovery, and
remote/refspec mapping against Git. The Linux-only filesystem-path and ref-name cases account for
the two additional Linux tests.

Windows HTTP runtime evidence covers real Git download, owned-worker validation, object installation
and known-object reuse; pushing objects with Git publishing the remote ref; rejection of
authentication failures, server errors and redirects without retries; truncated RPC bodies; and
stalled-discovery deadlines. These fixtures use disposable loopback plaintext HTTP and a bounded Git
CGI subprocess. Windows HTTPS/trust validation, upload cancellation, uncertain push reports and the
broader Unix fault matrix remain untested. Reference storage, reflog transactions and composed
fetch/clone publication remain unsupported on Windows, as do the owned local-process and SSH
adapters. Git's own ref operations in a fixture do not establish girt ref support.

Every runner passed isolated core-only, HTTP-only and SSH-only library compilation and
all-feature/all-target Clippy with warnings rejected. Windows SSH feature compilation does not
provide an SSH adapter. Unix runners also passed warning-denying private Rustdoc and local fetch,
local push, HTTP and SSH examples. All hosts used Rust 1.98.1; Git was 2.55.0 on Unix and
2.55.0.windows.5 on Windows with the MSVC toolchain. These compilation checks are separate from the
runtime counts above; no performance, crash-durability or network-filesystem evidence was added.

[The preceding run](https://github.com/joshka/girt/actions/runs/36077535098), revision
`9be3b78a6aa122a23d8cb0d19194ceca35350e0b`, passed all Unix jobs but found a Windows fixture
assertion comparing Git's `C:/...` spelling to Rust's canonical `\\?\C:\...` spelling.
Canonicalizing Git's reported path fixed the assertion without changing library behavior. The final
Windows run executed all selected suites with no ignored or filtered tests. Portable tests now use
`--no-fail-fast`, and HTTP runs independently after other step failures so its result remains
visible. An earlier superseded run was cancelled after local Clippy identified needless borrows in
the new fixture; no result is claimed for it.

Local `just check` passed on the initial portable-suite revision (1,415 tests); the path correction
then passed its focused regression, all-target/all-feature Clippy and formatting. `actionlint`,
rumdl and markdownlint-cli2 also passed. This result record is a later Markdown-only child, not a
CI-tested revision. Earlier capability sections retain their original revision-specific evidence;
this native run supplies the later platform results.

### Architecture Revision Validation

[Run 36043376324](https://github.com/joshka/girt/actions/runs/36043376324) passed on 2026-09-24 at
revision `c948ec24c56cdb6ac585f32b8a3fcc655f1c9846`, after the architecture fixes.

| Native runner       | Runtime results                               |
| ------------------- | --------------------------------------------- |
| Ubuntu 22.04 x86_64 | 685 units, 316 integration tests, 12 doctests |
| Ubuntu 24.04 x86_64 | 685 units, 316 integration tests, 12 doctests |
| macOS 14 arm64      | 685 units, 314 integration tests, 12 doctests |
| Windows 2022 x86_64 | 609 portable units, 12 doctests               |

All runners passed core-only, HTTP-only, and SSH-only library compilation and all-target Clippy. All
runners used Rust 1.98.1. Unix runners used Git 2.55.0; Windows used Git 2.55.0.windows.5 with the
MSVC toolchain. Unix suites enabled all features and included real loopback HTTP/HTTPS and SSH
interoperability, owned network handoff, transport interruption, incremental transfers, and delta
compression. Local fetch, local push, and HTTP examples passed. Clippy enabled all features and
rejected warnings; Unix private Rustdoc also rejected warnings. Windows runtime commands used
default features, so HTTP compilation is not HTTP runtime evidence.

Windows reference storage and local/SSH process adapters remain unsupported. These portable tests do
not establish Windows repository integration, HTTP runtime compatibility, crash durability,
network-filesystem behavior, or hostile-path safety. Isolated feature checks compile libraries; they
do not execute independent feature-specific runtime suites.

Local `just check` also passed on the published implementation, including formatting, 1,011 tests,
all-feature/all-target Clippy, and docs.rs. Earlier capability sections retain their original
revision-specific limitations; this run supplies later platform evidence without changing those
historical results.

[Run 36043806070](https://github.com/joshka/girt/actions/runs/36043806070) also passed all four jobs
at revision `e26c36345a340292cb5fe04c74e2e09e484f556e`, with unchanged library and test code. This
workflow revision gives each isolated feature check its own step so a later PowerShell command
cannot hide an earlier failure, and adds the disposable SSH example on all Unix runners. Test counts
and toolchains match the preceding run; all four Unix examples now pass on each Unix runner.
`actionlint` passed for the workflow. This evidence document is a later Markdown-only change,
checked with rumdl and markdownlint-cli2; the CI results above identify the tested code exactly.

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
immediately follow a direct record. Peeled IDs are checked syntactically and omitted from lookup
results; the traits do not prove object type, existence, or correctness of the peel. Resolution
returns the direct tag-object ID.

Both headerless/unsorted records and Git-generated sorted/peeled files are supported. Claimed sorted
order is checked bytewise. Duplicate names, misplaced headers, orphan/repeated peel lines, blank
lines, unknown comments, invalid names/IDs, missing final LF and packed per-worktree names fail.
Whenever packed fallback is needed, the entire file is read and validated, including unrelated
records; a loose hit does not open it. Every update, deletion, and enumeration validates
packed-refs. Reads allocate in proportion to file size without a configurable limit. There is no
packed cache or indexing yet.

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
reference data. Conditional deletion removes packed data first, as described below.

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
[reference baseline](benchmarks.md#reference-baseline). Reflogs and multi-ref transactions remain
outside reference storage. Later increments add enumeration/deletion and the other repository
capabilities documented separately below.

### Reference Enumeration and Deletion

`References::list` returns owned `Reference { name, target }` values in bytewise name order. It
includes all `refs/` namespaces visible to the opened worktree, with shared refs from the common
directory and private refs from the current worktree. HEAD and other pseudorefs are excluded. Use
`list_namespace` with `refs/heads` or `refs/tags` to select branches or tags; a namespace selects
the exact name and its slash-delimited descendants. Missing namespaces are empty. Symbolic targets
are returned as stored, including dangling targets and cycles; tags are not peeled. Loose values
shadow packed values. Invalid loose data fails instead of falling back, and conflicting effective
names are rejected. Empty directories, dot-prefixed entries and `.lock` entries are ignored.

Enumeration validates all packed data once and visits selected loose paths without locking. It
allocates for the packed data, effective names and pending directories. There is no configurable
size limit or point-in-time snapshot. Concurrent additions/deletions may be missed or included; a
vanished loose ref can leave a previously read packed value in the result. An owned list stays
unchanged after return, but callers must pass an expected value when acting on its observations.
Malformed selected loose files and filesystem symlinks are errors; unselected loose files are not
read. Packed errors anywhere fail even a filtered enumeration.

`delete_without_reflog` compares and deletes the named stored target. A symbolic name is removed
without following it. `delete_resolved_without_reflog` locks up to 32 symbolic hops and deletes only
the terminal name; HEAD can remain unborn and aliases can remain dangling. Both require an explicit
`Expected`: `Value` checks the effective current value, `Absent` accepts only absence, and `Any`
accepts a valid value or absence. Deleting an absent name with `Absent` or `Any` succeeds without
changing reference contents. Conditions and existing data are checked before any reference mutation.

Deletion holds `packed-refs.lock` and the selected name's lock (or the entire symbolic chain). It
stages a replacement packed file in the common directory and atomically publishes it while retaining
the packed lock, then unlinks the loose file. Removing the packed record first prevents loose
removal from exposing an older packed value. Only the selected record and its following peel line
are removed: unrelated bytes, header spacing/traits, ordering, hexadecimal case and peel records are
retained. Loose-only deletion does not create packed storage. Parent directories, reflogs and
objects are left in place. No hooks or reflog writes occur.

This is not a two-file transaction or crash recovery protocol. A precondition failure preserves both
reference files. Packed publication failure leaves both values intact. If packed removal succeeds
and loose removal fails, `ReferenceError::PackedDeleted` reports partial completion: the current
loose ref remains while its packed copy has been removed. No rollback is attempted. Locks and
temporary files have best-effort cleanup; empty directories or stale cleanup files may remain. No
fsync is performed. Cooperating writers are excluded while the locks are held, but readers holding
old packed data can observe stale values, and later writers can recreate a deleted name. The
Unix/local/trusted-filesystem assumptions above remain in force.

Original unit and integration fixtures cover namespace boundaries and ordering, loose/symbolic
shadowing, malformed input, exact expectations and absence, symbolic-name versus terminal deletion,
cycle/depth failures, lock contention, temporary publication failure and partial unlink failure.
Git-created branches and annotated tags exercise packed removal with unrelated peel preservation;
Git `show-ref`, `rev-parse`, `symbolic-ref` and `for-each-ref` observe the results. Linked worktrees
exercise all three private namespaces alongside shared packed branches, and a separate Git directory
exercises gitfile routing. Races with Git conditional updates and `pack-refs --all` exercise writer
coordination; a retained-lock experiment confirms that Git packing refuses the reserved packed file
after independent replacement. These tests validate observable behavior without Git source or
upstream fixtures. They do not establish arbitrary crash recovery or reader snapshot isolation. The
`publish_branch` example now enumerates and conditionally deletes the branch it published.

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

The lower-level object-transfer APIs never update refs, reflogs, or `FETCH_HEAD`. Callers may use
the existing explicit `update_without_reflog` operations after installation, with expected old
values. Each update is independent; failure of a later update does not undo earlier successes. No
multi-ref atomicity is implied. Remote configuration, refspecs, automatic tag following, pruning,
shallow/partial stores, authentication helpers and push remain outside this capability.

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

## Repository Discovery and Initialization

`Repository::discover` searches an existing directory and its physical ancestors up to the
filesystem root, including across mount points. `discover_with_ceiling` includes the supplied
ancestor but never searches its parents. Both paths are canonicalized; the ceiling restricts
candidate locations, not where a gitfile or linked-worktree backlink may lead. A `.git`, `HEAD`, or
`objects` entry selects a candidate for the existing opener. Invalid or unsupported metadata stops
discovery instead of falling back to an outer repository. This conservative marker policy can also
stop at unrelated files with those names. Environment overrides, ownership checks, and Git's
discovery configuration are not used.

`Repository::init(path, InitKind)` creates ordinary or bare SHA-1 repositories with version-0
config, files-backend refs, empty object storage, and unborn `refs/heads/main`. Ordinary
destinations may be existing directories with unrelated files; the three discovery markers refuse
initialization. Bare destinations must be absent, including when the existing directory is empty.
Parents must already exist. Reinitialization is refused without altering existing metadata;
recognized unsupported formats and layouts retain opening errors. Templates, hooks, initial commits,
indexes, separate Git-directory creation, linked-worktree creation, branch-name options, and ambient
configuration are outside this increment. The executable example is
`cargo run --example init_repository`.

Directory creation reserves the metadata destination, and files use exclusive creation. Competing
initializers have one winner. The caller must exclude other writers and path replacement. A later
failure leaves newly created directories and partial metadata for inspection; no rollback or
power-loss durability is promised. Initialization never removes files or overwrites existing files.

Independent fixtures in `tests/repositories.rs` exercise Git object access, commit creation,
reference updates and `git fsck --strict` in both generated layouts, plus Git staging and committing
in an ordinary worktree. Git-generated ordinary, bare, relative gitfile, and linked-worktree layouts
are discovered from nested directories. SHA-256, reftable configuration, and worktree-specific
configuration are rejected without mutation or outer-repository fallback. These tests derive from
the [repository layout](https://git-scm.com/docs/gitrepository-layout) and
[initialization](https://git-scm.com/docs/git-init) manuals and independent CLI observations, not
Git implementation code or upstream fixtures. Existing opening compatibility evidence remains
applicable.

Unit tests cover inclusive ceilings, invalid ancestry and starts, nearest candidates, symlink
ancestry, malformed and dangling markers, exclusive file creation, partial population failures,
concurrent initialization, and preservation of unrelated files. No benchmark is added: this
increment performs small metadata setup and ancestor traversal, changes no existing processing hot
path, and makes no performance claim. Metadata reads retain the opener's unbounded allocation
policy.

Local validation on 2026-09-24 used Git 2.55.0, Rust 1.98.1, and macOS arm64. All 62 repository
integration cases and 33 focused repository unit cases passed, as did `just check` (including all
features), the disposable initialization example, private-item Rustdoc with warnings rejected, and
Markdown lint with the repository's 100-column policy. This increment has not been executed on Linux
or Windows; earlier platform results do not establish its runtime behavior there. Mount-point
crossing follows the ancestor algorithm but was not exercised with a separately mounted filesystem.

## Reference Transactions and Reflogs

`References::transaction` accepts stored direct/symbolic edits and resolved direct updates/deletion.
All stored values and preconditions are checked with the selected names locked. Symbolic discovery
is rechecked after acquiring locks; changes fail preparation instead of redirecting the operation.
Duplicate names, intersecting chains, and ancestor/descendant batches (including delete/create
namespace swaps) are rejected. A stored direct replacement with append logging resolves and locks
its old symbolic chain for the previous log ID, using zero when unborn. It compares the precondition
against the original stored value, then edits and logs only the named ref; the old branch and its
log remain unchanged. Creating a symbolic target or deleting a stored symbolic value still requires
preserved logs. A resolved HEAD edit preserves HEAD and logs the visited chain with terminal old/new
IDs. Direct branch updates do not discover or log aliases/HEAD.

Preparation takes the common packed lock, ref locks in byte-name order, then log locks in byte-name
order. It validates existing logs selected for append. A preparation failure preserves reference and
log contents, although empty directories can remain. Publication removes all selected packed records
before loose deletion, preserving unrelated packed bytes and peel records. It then processes
operations in input order, publishing each ref before its logs. Locks remain held through return.
Packed-only refs can already be absent when a later operation fails.

`TransactionError::Publish` retains a result for every input: reference unchanged, packed record
removed, or publication complete; each requested log is unattempted, appended, or failed with a byte
count. This permits a published ref with an absent/incomplete log. Failures stop further work and do
not roll back successful publication. Short appends can leave malformed tails. There is no snapshot
for readers, filesystem-wide atomicity, fsync, automatic retry, or crash recovery.

`Reflog::Preserve` leaves logs untouched, including malformed logs and deletion. `Reflog::Append`
creates missing logs and appends even on same-ID updates. Deletion retains the log and appends a
zero new ID; it does not adopt Git's CLI log-removal policy. Identity, nonnegative Unix seconds,
offset, and exact single-line message bytes come from the caller. Config, environment, hooks,
fast-forward policy, and object existence/type checks are not consulted. CR, LF and NUL messages and
tab-containing identities are rejected. Ref and log paths share ordinary, bare, separate-gitdir, and
current linked-worktree routing.

Logs are appended, never replaced wholesale. Git's automatic HEAD appends need not honor a HEAD or
log lock, so ordering with those independent appends is unspecified. Callers must exclude reflog
rewrites, expiry and deletion during transactions. The reader accepts Git's omitted separator for an
empty message, preserves non-UTF-8 identity/message bytes, and returns oldest-first records. Numeric
formatting and negative-zero timezone spelling are normalized; arbitrary malformed or legacy
identity forms are rejected. Reads allocate for the complete file and are live, including possible
incomplete concurrent tails. There is no configurable log-size bound or streaming API.

The [workflow example](../examples/reference_transaction.rs) publishes a branch through HEAD and a
tag, handles preparation versus publication errors, and deletes the tag with retained history. The
[benchmark baseline](benchmarks.md#reference-transactions-and-reflogs) measures batch publication
and existing-log validation separately.

Independent fixtures use Git CLI observable behavior and the
[update-ref documentation](https://git-scm.com/docs/git-update-ref) and
[repository layout documentation](https://git-scm.com/docs/gitrepository-layout). No Git source,
upstream tests or copyright-audit material was used. Git reads exact generated records, reflog
selectors, identities, timestamps and ordering; girt reads subsequent Git appends, including empty
messages. Tests cover packed-only/shadowed deletion, linked-worktree private namespaces, and
separate metadata directories. A conditional Git writer races a two-ref batch. Controlled
post-preparation filesystem failures exercise packed replacement, loose publication/unlink, and
first/later log failures; an injected short writer verifies retained append byte counts. Runtime
evidence for this increment is macOS arm64 with Git 2.55.0; no new platform support is claimed.
Expiry/GC, reflog removal, remote/refspec policy, reftable and recovery journals are deferred.

## Remote Configuration and Refspec Mapping

`remote::Remote::find(repository.config(), name)` reads an owned snapshot of four remote keys:
`url`, `pushurl`, `fetch`, and `push`. Names compare exact bytes; key names ignore ASCII case.
Repeated sections and keys retain file order. Empty URLs clear the preceding list for that key;
fetch uses the first remaining URL, and push uses all remaining pushURLs or falls back to all URLs.
Duplicates remain visible. URLs are opaque bytes, not validated transport endpoints or rewritten
addresses. Missing keys give empty lists; a missing named subsection with entries returns `None`.
Empty section headers are not retained by `Config`. Implicit boolean values fail with key and
occurrence diagnostics. Both refspec lists are parsed eagerly, and empty refspec values fail.

`remote::Refspecs` supports explicit full `refs/` names, exact source `HEAD`, one-star mappings
(including partial components, empty captures, slash-containing captures and non-UTF-8 bytes),
leading force intent, negative fetch exclusions, source-only fetch selection, same-name push and
explicit push deletion. Fetch exclusions apply regardless of position. Missing explicit positive
sources fail, including excluded ones; unmatched patterns and exclusions do not. Empty lists produce
empty plans with no implicit HEAD or branch selection.

Plans follow positive-spec order and then source-input order. Exact duplicate mappings collapse at
first occurrence. Distinct sources targeting the same destination fail even if IDs agree; differing
force intent for one destination also fails. This deliberately conservative conflict rule does not
attempt Git's CLI conflict resolution in every duplicate case. Source-only selections may coexist
with destination mappings. Duplicate input names and zero IDs fail even when unselected. Wildcard
substitutions are revalidated as full names before returning a plan. Errors return no partial plan.

Advertisement mapping discards peeled hints and preserves each advertised tip name/ID, including
symbolic HEAD. It neither follows capability aliases nor creates a symbolic local ref. Callers
resolve local symbolic refs before supplying push inputs. A `+` flag records syntax only; ancestry,
expected old values, object kinds, namespace restrictions, destination prefix conflicts and force
authorization remain separate. Mapping can describe push deletion even though current `push`
transports cannot send it. Push preparation may reject namespaces accepted by this general mapper.

No network, object lookup, config editing or reference mutation occurs in these APIs. System/global
configuration, includes, URL rewriting, credential discovery, `push.default`, branch selection,
matching push, shorthand names, arbitrary revision expressions, raw object-ID sources, empty/default
fetch forms and `tag <name>` shorthand are outside the supported slice. Other configuration options
are uninterpreted, including mirror, pruning, tag following, partial clone, custom service commands
and transport options. Their presence does not change the four-key interpretation. Consumers must
choose their own orchestration policy rather than treat the result as all of Git's remote behavior.

`examples/remote_plan.rs` reads disposable repository config, selects IDs from a supplied
advertisement, retains destination mappings, and prepares a conditional creation push with an
explicit no-force policy. It sends nothing. Existing fetch selection callbacks cannot directly
return mapping errors: a caller can save the mapping result, return an empty selection on failure,
and inspect that result before accepting the transfer outcome. Reference publication follows
validated object installation as a separate operation with explicit expectations and reflog policy.

Independent fixtures in `tests/remotes.rs` create bare SHA-1 repositories using Git `init`,
`mktree`, `commit-tree`, `update-ref`, `tag` and `config`, then compare actual `fetch`, `push`,
`ls-remote`, `remote get-url`, `for-each-ref` and `rev-parse` behavior. They exercise wildcard and
explicit mappings, exclusion position, empty/partial captures, same-name push, HEAD, annotated tag
identity, duplicate mappings, missing sources, collisions, deletion and URL reset/fallback. Fixtures
use throwaway local paths and isolated Git config; no upstream source, test data or copyright-audit
material informed implementation. Format references are the public
[fetch refspec documentation](https://git-scm.com/docs/git-fetch),
[push refspec documentation](https://git-scm.com/docs/git-push), and
[remote configuration documentation](https://git-scm.com/docs/git-config#Documentation/git-config.txt-remotenameurl).

The [Criterion baseline](benchmarks.md#refspec-mapping) measures expansion over 10 and 10,000 source
refs. Input sizes are caller-bounded; the mapper has no cancellation or hard memory limit. Runtime
interoperability evidence for this increment is macOS arm64 with Git 2.55.0; Linux and Windows
runtime checks have not been performed for this increment.

## Fetch Orchestration

`fetch::FetchRequest` composes an explicit fetch refspec list (including one read from a named
`remote::Remote`) with local, HTTP or SSH transfer and a shared install/publication path. The
endpoint, credentials, history, scheduling, limits, force authorization and reflog policy remain
caller inputs. `examples/fetch_remote.rs` runs against disposable repositories; network tests move
the owned download to a caller-owned blocking worker before validation and publication.

Preparation captures stored destination values before network work. Mapping uses the advertisement
from the actual transfer session/discovery; a preview is not reusable authority for a later
advertisement. Missing sources and mapping/policy errors request no objects. HTTP discovery and RPC
can observe a changing server: the request retains discovered IDs and either receives their valid
objects or fails, without silently remapping to new IDs. Publication uses conditional transactions
for changed refs, including expected absence; no-op destinations are not locked or rewritten.

Supported destination rules are deliberately narrower than full CLI fetch:

| Destination      | Supported rule                                                       |
| ---------------- | -------------------------------------------------------------------- |
| `refs/tags/*`    | Create any kind; replace only with `+` and exact-name authorization. |
| `refs/remotes/*` | Any kind may be created; replacements use the object rules below.    |
| `refs/heads/*`   | Rejected, including bare repositories and forced refspecs.           |
| Other namespaces | Rejected; symbolic destinations are also rejected.                   |

For remote-tracking replacements, both tips are peeled under explicit tag/read limits. If both peel
to commits, the old commit must be an ancestor of the new one, unless both `+` and explicit name
authorization permit a non-fast-forward replacement. If either tip is a tree or blob after peeling,
replacement needs no force. Missing or unreadable old objects fail closed. Ancestry uses bounded
synchronous history queries; cancellation is observed before/after each query, not within it. Update
limits apply per destination, so aggregate work can multiply by mapping count.

The [git-fetch manual](https://git-scm.com/docs/git-fetch) describes namespace/type-dependent
updates and force requirements for tags. Its statement about arbitrary replacements outside
heads/tags is broader than the Git 2.55.0 behavior observed here: Git rejects remote-tracking commit
rewinds and annotated tags peeled to older commits without force. This workflow follows those
observed stricter rules. Independent CLI tests retain both rejected unforced and accepted forced
cases, alongside accepted tree/blob replacements. No Git implementation or upstream test source was
used.

Branch destinations are rejected to avoid implementing a partial branch-in-use check. Main and
registered linked-worktree HEAD chains must be detached or stay entirely in `refs/heads/*`; chains
into otherwise supported destinations fail closed. Inaccessible/stale registrations are errors. The
checks run at preparation and after installation. Callers must exclude checkout, symbolic
HEAD/branch edits and worktree-registration changes throughout the operation; these checks do not
lock worktree metadata. Ordinary destination writers remain supported through exact expectations.

Validation precedes installation, and installation precedes every ref edit. After installation,
selected-tip graphs are reread through the destination object store under explicit verification
budgets. This catches corrupt loose objects hiding valid installed pack objects, including corrupt
descendants, before publication. `FetchReport` separates transfer statistics, completed object
installation and successful transaction outcomes. `FetchFinishError` retains the report and the
original transaction error with partial ref/log effects. Installed objects remain after update
rejection, cancellation after installation or publication failure. Installation itself can leave an
unindexed pack on failure. No whole-fetch rollback, atomic visibility or power-loss durability is
promised. Callers must coordinate pruning/GC until publication completes. Reflog identities/messages
are explicit and unchanged/source-only entries do not append logs.

`FETCH_HEAD`, clone, checkout, pruning, implicit tag following, credential discovery, global config
policy, config editing and push orchestration are deferred. Existing protocol/storage limitations
still apply. Preparation and reference metadata operations retain their existing unbounded metadata
allocation contracts; this layer introduces no runtime or backend framework.

Original fixtures in `tests/fetch_workflow.rs` generate blobs, trees, commits and annotated tags
using Git CLI plumbing, then compare Git and girt fetch results. Tests cover initial/incremental
fetch, multiple refs, wildcard exclusions, source-only/no-op results, tag force authorization,
commit rewind rules, tree/blob kind replacement, destination races, cancellation, failed
installation (including an unindexed residual pack), missing local dependencies, publication lock
failure, explicit reflogs and main/linked-worktree HEAD aliases. `tests/http.rs` and `tests/ssh.rs`
cover end-to-end publication on owned workers using the existing independent loopback fixtures; SSH
also rejects malformed advertisements without installation. Existing transaction tests retain
partial-publication evidence.

This increment was exercised on 2026-09-24 on macOS arm64 with Git 2.55.0 and rustc 1.98.1; its
[completion record](testing.md#fetch-orchestration-completion) and
[benchmark baseline](benchmarks.md#fetch-orchestration-baseline) record the checks and retained
measurements. Earlier Linux/Windows validation records do not establish runtime support for these
new workflow paths.

## Clone Without Checkout

`clone::CloneRequest::prepare_tracking` explicitly selects the remote-tracking reference layout and
composes exclusive destination initialization with existing local/HTTP/SSH fetch, validation,
installation and conditional references. The destination must be absent, including empty directories
and dangling symlinks, and its parent must exist. Downloads and validation precede all destination
writes. No hardlinks, alternates, index, checkout, shallow/partial clone, recursive submodules,
mirror behavior or credential discovery are included. Ordinary clones contain only `.git`; Git can
report staged deletions until a later checkout populates the index and files.

Both bare and ordinary layouts store every advertised branch under `refs/remotes/origin/*` and all
tags under `refs/tags/*`. A selected branch additionally gets one `refs/heads/*` ref and symbolic
HEAD. Bare clones deliberately use this same layout rather than Git CLI bare clone's
all-local-branch layout, so later `FetchRequest` calls can use the persisted refspecs without
weakening its branch and worktree safeguards. No `origin/HEAD` alias, pruning or `FETCH_HEAD` is
written. All configured refspecs are unforced; later replacement policy stays explicit at the fetch
boundary.

`prepare_tracking` names the reference-layout choice; `InitKind` selects storage placement only.
Consumers enumerating or serving `refs/heads/*` see only the selected local branch, or none for
detached/unborn selection. Conventional copies with every branch in `refs/heads/*` are unsupported.

Default selection honors one consistent `symref=HEAD:refs/heads/...` capability. Without a symbolic
hint, an advertised HEAD remains detached even when one or several branches share its ID. A missing
HEAD in a nonempty advertisement requires an explicit full branch name. Explicit selection overrides
remote default-branch hints but does not filter the other branches/tags. Empty v0 advertisements can
omit the remote's unborn branch name; use the caller-selected branch or `main` in that case. An
advertised unborn symbolic name is preserved. Tag/revision selection is unsupported; branch and
detached HEAD tips must be commits, without tag peeling. All advertised branch tips are checked.

Planning occurs inside the actual transfer callback. A preview cannot replace that advertisement,
and IDs never change after download. HTTP discovery and RPC can race remote changes; a server may
refuse an unavailable advertised ID, which is a transfer error with no destination effects. Clone
never substitutes a newer tip or promises that its snapshot remains current at the remote.

Finish reserves the root, initializes metadata, uses fetch to install and verify objects and publish
tracking refs/tags, verifies branch object kinds, saves config under an exclusive `config.lock`,
then conditionally publishes the local branch and HEAD in that order. Config replacement compares
the exact initializer contents and refuses other writers' edits. A final reopen supplies a fresh
config snapshot before success is returned. Direct refs use the caller's reflog policy; stored
symbolic HEAD uses preservation because symbolic targets cannot append reflogs through this
primitive. Detached HEAD honors append policy: its old unborn branch is locked while recording zero
as the prior ID, then HEAD is made direct and its log is appended. A failed append retains the
published HEAD and an explicit partial-log outcome.

Errors retain reservation/initialization flags, a completed fetch report or its detailed nested
failure, config completion and final reference outcomes. Initialization can leave partial metadata;
installation can leave an unindexed pack; later errors can leave installed objects, tracking refs,
tags, config, local refs or partial reflogs. No automatic cleanup, rollback, retry, atomic reader
snapshot or crash durability is promised. Callers exclude path replacement, other writers,
checkout/worktree changes and GC during finish. Cancellation is checked between phases; it does not
interrupt config replacement or a started reference transaction. Join synchronous workers even after
requesting cancellation.

The fixed `origin` stores a caller-supplied credential-free URL and two fetch refspecs, plus the
selected branch's remote/merge keys. The URL is independent metadata, not an endpoint resolver;
transport endpoints, credentials, trust, budgets and deadlines stay explicit. Relative local URLs
are relative to the new repository when consumed by Git. HTTP/SSH return owned downloads and perform
no object validation or storage inside network polling. Validation/finish stay synchronous and
caller-scheduled. Configuration replacement is clone-specific, not a general config editor.

Original fixtures in `tests/clone.rs` use Git CLI `init`, `hash-object`, `mktree`, `commit-tree`,
`mktag`, `update-ref` and `symbolic-ref`. Git verifies objects, strict fsck, no-checkout status and
stored remote configuration; both girt and Git perform subsequent fetches. The fixtures cover bare
and ordinary clones, empty/default/selected/detached or missing HEAD, multiple equal branch tips,
annotated tags, remote changes after transfer, invalid branch objects and unsafe destinations.
Focused unit tests exercise planning, config encoding/locking, dropped transfers, cancellation,
reservation races, installation residuals, verification limits and reference/config failure phases.
HTTP and SSH loopback fixtures exercise owned worker handoff and cancellation before initialization.
Existing transaction tests provide the shared per-ref/per-log partial-publication evidence.

Runtime evidence for this slice is macOS arm64 with Git 2.55.0, Rust 1.98.1 and OpenSSH 10.3p1.
Linux/Windows runtime behavior is not established by this run. Finish requires the Unix reference
backend; local process and SSH transport contracts retain their macOS/Linux restrictions; clone
filesystem interoperability tests are Unix-gated. No dependencies or platform support were added,
and no upstream implementation/test source or copyright-audit comparisons were used as
implementation input.

## Tree Comparison

`Objects::compare_trees` compares explicit SHA-1 tree IDs recursively, with `None` representing an
empty side. It returns owned leaf records with byte paths and optional old/new modes and IDs.
Additions, deletions, content identity changes, executable-bit changes and type changes are distinct
through those values. Files, symlinks and gitlinks are leaves; file/directory replacements become a
leaf deletion/addition plus changes below the directory. Empty directories produce no records.
Results sort lexicographically by raw full-path bytes, with shorter prefixes first. Trees read from
storage must satisfy Git ordering, even though matching uses literal names to align replacements
whose positions differ under Git's file/tree ordering rule.

Equal tree IDs, including equal roots, skip all object reads. Success does not establish existence,
syntax or validity of skipped trees or their descendants. Other traversed trees require verified
storage identity, supported tree syntax and valid names, uniqueness and order. Leaf targets are
never read or type-checked; gitlinks can refer to absent commits in another repository. Missing,
wrong-kind, malformed and corrupt traversed trees produce errors with their identity and byte path.
This is structural comparison, not object connectivity validation or fsck.

Traversal is iterative and read-only. Caller limits bound tree-read occurrences, cumulative tree
payload bytes and entries, directory depth, generated path bytes and output records. Per-read
storage decoding limits also apply. Parsing one bounded payload can allocate its entries before the
cumulative entry-count check; limits bound inputs rather than exact heap usage. Cancellation is
checked between reads and entry operations and around final sorting. A single read, parse,
validation or sort remains synchronous and noninterruptible. Failure returns no partial result and
changes no files. No object cache, store trait, async runtime or general diff framework is added.
Content diff is a separate [operation](#content-diff). Rename/copy detection, pathspecs,
index/worktree comparison, merge and checkout remain outside scope.

`tests/tree_compare.rs` generates original fixtures with isolated Git `hash-object` and
`mktree -z --missing` invocations. It compares IDs, modes and paths with
`diff-tree --raw -r -z --no-renames --no-commit-id --no-abbrev --no-ext-diff --ignore-submodules=none`,
normalizing only record order to the API's documented full-path ordering. Cases exercise both
directions and empty sides in loose and packed storage, including nested changes, prefix ordering,
file/directory replacements, executable status, symlinks, foreign gitlinks and
non-UTF-8/control-byte names. Git `pack-objects --revs` and `prune-packed` create the packed
variant. No upstream implementation or test source is used. Fixtures never create those byte names
on the host filesystem.

The local evidence uses Git 2.55.0 on macOS arm64; native Linux and Windows refresh is separate
work. The comparison adds no platform-specific filesystem or process behavior; it inherits the
existing reader's storage boundary. No additional platform support is established by this slice.

## Content Diff

`content_diff::diff` accepts borrowed byte payloads and returns unchanged, binary-changed or owned
edit ranges borrowing both text inputs. LF terminates a line and remains part of its bytes. Empty
input has no lines; a nonempty unterminated suffix is one line. CRLF, bare CR, NUL in forced text,
invalid UTF-8 and a missing final LF are significant, with no decoding or normalization. Edit ranges
are zero-based and half-open in both original coordinate spaces. Adjacent edit edges are coalesced;
unchanged gaps are implicit and byte-identical. Results contain zero context and are not unified
patch hunks. There is no context expansion, heuristic boundary shifting or rendering API. The
example escapes bytes solely for display.

Auto mode classifies changed payloads as binary if either contains NUL anywhere. This is an explicit
whole-input policy, not Git's sampling or attributes policy. Forced text and forced binary override
it; equal bytes always return unchanged. Binary results retain both slices without encoding a binary
patch. No path, attributes, textconv, external driver, whitespace-ignore, rename/copy,
patch-application, merge, index or worktree policy is applied.

`BlobContent::read` separately loads a tree change's regular, executable and symlink sides from
verified loose/packed storage. Absent sides become empty buffers; the original tree change retains
existence and modes. Symlink payloads are target bytes and are never dereferenced. Trees and
gitlinks are rejected before reads. Equal IDs still require reads, so the adapter establishes
existence and kind rather than trusting structural identity. Missing, wrong-kind and storage
failures retain IDs; the caller keeps path/side context. Per-read decoding limits and a combined
retained-payload limit apply before decoding. No storage or reference changes occur.

### Algorithm and Resource Contract

The original implementation uses the edit-graph frontier algorithm described in Eugene W. Myers,
[An O(ND) Difference Algorithm and Its Variations](https://doi.org/10.1007/BF01840446), section 3
([paper](https://neil.fraser.name/writing/diff/myers.pdf)). Only the published algorithm description
informed implementation. No Git/xdiff implementation, upstream tests or copyright-audit material was
consulted. No dependency was added. A compact stored-frontier implementation keeps the search and
traceback small enough to review, while explicit limits reject expensive cases. Linear-space
refinement, line interning and alternate algorithms are deferred until consumer evidence justifies
the additional implementation.

For N total lines and edit distance D, search takes O(ND) line comparisons and O(D²) trace
positions, plus linear line-offset and output storage. Bytes compared also consume work budget. The
algorithm minimizes inserted plus deleted lines, greedily extends equal runs, visits insertion-heavy
diagonals first and selects deletion when predecessor old positions tie. This establishes
deterministic alignment independently of Git's heuristic shifts. No minimum replacement-count,
prettiest patch, or exact Git CLI-output guarantee is made.

Defaults bound combined inputs to 64 MiB, combined lines to 1,000,000, retained frontier positions
to 1,000,000 and work to 64 Mi units. Each frontier row reserves its full distance-plus-one width
before allocation. Scanning and comparison charge bytes conservatively; frontier, line-comparison
and traceback steps also charge units. Work can exhaust before other bounds, even for inputs below
the byte cap. Empty sides bypass search; equal and binary results bypass line/trace allocation.
Input bounds apply to all modes. Bounds are not exact process-memory limits; allocator overhead,
spare vector capacity, caller-owned payloads and storage snapshots are separate. Raising limits can
admit quadratic work/trace growth. Exhaustion returns an error with no approximate or partial edits.

Cancellation is cooperative: checks occur before work, within byte scans at 4 KiB chunks, per
frontier/line comparison, during traceback and before return. Allocations, deallocation and single
storage reads cannot be interrupted. No hard wall-clock bound or async runtime is introduced.

### Content Diff Evidence

`src/content_diff/tests.rs` contains original named examples, an exhaustive short-sequence corpus
checked against an independent dynamic-programming edit-distance oracle, varied byte inputs,
resource/cancellation boundaries and large shapes. Adapter units test storage failure context and
supported modes. `tests/content_diff.rs` writes original bytes into isolated temporary files and
invokes Git `diff --no-index --text --minimal --diff-algorithm=myers --no-indent-heuristic` with
zero context, no external drivers or textconv, and isolated configuration. The parser preserves
patch body bytes and handles Git's missing-final-newline marker. Simple ranges/bytes agree exactly;
ambiguous alignments are checked through reconstruction, unchanged gaps and equal minimum costs.
This does not assert universal alignment equivalence. A separate simple NUL fixture compares
ordinary Git binary classification; the whole-input sampling difference remains intentional.

Git `hash-object --no-filters` supplies blobs for tree-to-content integration, including CRLF and
non-UTF-8 data. Git repack/prune-packed supplies the packed variant and a removed loose object
confirms packed reads. Tests use only portable files and Git-managed fixture refs, without claiming
girt's reference backend works on Windows. The suite is included explicitly in the Windows job.
Local execution uses macOS arm64, Git 2.55.0 and Rust 1.98.1. New native Linux/Windows runtime
results are uncollected; platform support has not expanded.

## Working-Tree Index

`index::Index` provides pure bounded SHA-1 v2 parsing, construction, entry replacement and encoding.
`Entry` drafts represent exact byte paths, canonical regular/executable/symlink/gitlink modes,
object IDs, raw stat words, stages 0–3 and assume-valid. Successful construction sorts unsigned path
bytes then stages. Parsing requires that order, canonical name-length flags and zero padding; it
never repairs malformed input. Both reject duplicate stages, normal/conflict mixtures and
file/directory collisions within a stage. Different conflict stages may represent a directory/file
conflict. Object existence/type, stat correctness and checkout safety remain caller obligations.
Paths reject empty components, NUL, `.`, `..` and `.git`; platform aliases and non-UTF-8 bytes are
retained without filesystem materialization.

Input/output bytes, entry count, per-path bytes and extension count have explicit limits. Parsing
checks the checksum and feasible entry count before allocation, bounds NUL searches, and checks
extension lengths before copying. Owned memory is proportional to these limits, excluding caller
buffers and allocator overhead; storage can retain original, parsed and encoded representations
simultaneously. Zero/omitted checksums, v3/v4, extended flags (skip-worktree and intent-to-add),
noncanonical modes, sparse directories and mandatory extensions, including `link` and `sdir`, are
rejected. Assume-valid is retained as data, without implementing stat-skipping policy.

Optional extension framing is checked but payload semantics are opaque. Unedited parsed indexes
round-trip byte-for-byte, including extension order. Changed entries invalidate and discard only
`TREE`, a derived cache; all other extensions block edits, including unknown optional signatures,
`REUC`, `FSMN`, `UNTR`, `IEOT` and `EOIE`. An unchanged replacement retains them. This intentionally
restricts editing indexes with information the library cannot update safely; there is no generic
extension-discard escape hatch in the held-lock editor. Constructing a separate extension-free index
is a pure operation and does not grant authority to overwrite repository storage.

`Repository::read_index` returns `None` for absence and `Some` for a valid empty index. The path is
always the resolved per-worktree `git_dir()/index`, including linked and separate Git directories;
`GIT_INDEX_FILE` and configuration overrides are ignored. `edit_index` creates `index.lock`
exclusively before reading and holds it until commit/drop. Callers derive drafts from that locked
snapshot. Commit validates/encodes before writing, compares original bytes/presence before and after
the lock write, closes the descriptor and renames the lock over the destination. A cooperating
writer cannot interleave. Exact-byte comparisons detect observed noncooperating changes, but do not
prevent a noncooperating race after the final check. Symlinks and non-regular index files are
rejected; hostile directory/lock replacement is outside the filesystem contract.

The published file timestamp is conservatively set to one second after the Unix epoch, preserving
entry stat words while preventing a later rewrite timestamp from making modern racy entries look
clean to Git. The nonzero timestamp matters: the Git CLI same-stat regression checks that modified
content remains visible with mtime restored and ctime checks disabled. Consumers still own their
stat policy; no status/refresh operation is implemented. Timestamp-setting support is required.
Publication uses same-directory rename and its host/filesystem replacement semantics. No fsync,
crash durability, shared-permission handling or blanket platform guarantee is claimed. Validation,
write and rename failures preserve original destination bytes except for independent concurrent
changes. Only the acquired lock is cleaned; cleanup failures may leave that owned lock for manual
recovery. Existing locks are never stolen. Index ownership is independent of reference storage.

The implementation uses the public
[index format specification](https://git-scm.com/docs/index-format) and original fixtures generated
with `add`, `update-index --index-info`, `read-tree`, `write-tree`, `ls-files --debug`,
`ls-files --resolve-undo`, and worktree/separate-gitdir commands. No Git source or upstream tests
were used. Git-created stat/flag fields and byte paths are read independently; Git observes
girt-written modes, stages, flags and stat words and writes compatible tree objects. Malformed
framing, ordering, flags, extensions, resource boundaries and injected write/publication failures
have focused unit coverage. The portable integration suite is explicitly selected for Windows;
native execution on Windows/Linux remains pending for this increment. No new dependency was added.
Status, staging policy, checkout, filters/attributes, merge resolution and filesystem scanning
remain outside this slice.

## Raw Working-Tree Status

`Repository::raw_status` compares HEAD (including unborn/detached HEAD) or an explicit tree with
this worktree's index, then verifies working files against stage-zero entries. Staged results reuse
`TreeChange`; unstaged results distinguish content/mode changes, missing paths and obstructions.
Conflicts retain their exact index stages. Missing index storage means an empty index, not an empty
baseline tree. Index blobs, including conflict stages, are identity/type verified through the
existing loose/packed reader. Baseline leaf targets and submodule commit targets are not resolved.

The API and every report explicitly select literal bytes and POSIX modes. Attributes, clean filters,
EOL conversion, ignore rules, `core.filemode`, `core.symlinks`, global config and environmental
normalization policy are not applied. Thus raw status can differ from Git status for a CRLF working
file with `text eol=lf`, an ignored file, or an assume-valid entry. Assume-valid is deliberately
ignored: all indexed blob content is verified, even when cached stat words match. No hooks or filter
commands run. Consumers must own normalization policy if they require Git-default status.

Untracked policy is either `Omit` or `RawFilesWithoutIgnores`. The latter lists every encountered
untracked leaf, including ignored and special files, without reading its content; empty directories
are omitted. Both policies enumerate visited directory names for exact byte matching. Gitlinks are
always reported as unchecked and never traversed. Nested `.git` markers and the conservative bare
repository marker combination `HEAD`/`objects`/`refs` stop traversal and appear as boundaries. The
root `.git` marker is excluded silently. Known Git/common/object directories are excluded by
filesystem identity, including a separate Git directory located inside the worktree.

Filesystem traversal uses existing rustix descriptor-relative no-follow operations on macOS/Linux;
no dependency was added. Unsafe index components and metadata aliases are rejected before platform
path conversion. Symlink leaves are compared using target bytes, never followed. Symlink or file
ancestors, directories replacing files and special tracked files are explicit obstructions. Exact
name matching prevents case-insensitive lookup from making a differently spelled index path clean.
Linux preserves non-UTF-8 filename bytes. Both platforms reject NUL, backslashes, colons,
empty/dot/parent components and case-insensitive `.git` components. macOS non-ASCII relative names
are explicitly unsupported because normalization aliases are not implemented. Other platforms reject
traversal before storage access; no Windows worktree-status support is claimed.

Limits cover index reads, baseline tree flattening, pack snapshots, object reconstruction and total
index/HEAD payload bytes, worktree file/total bytes, visited directory entries, generated paths and
retained ancestor prefixes. Directory depth is capped at 128 even with a larger caller limit;
symlink target reads additionally cap their buffer at one MiB plus one truncation-detection byte.
Cancellation is checked between synchronous phases/entries and file-read chunks. Reference
resolution retains its existing 32-hop selection and unbounded metadata-byte contract. Repository
opening and metadata/object trust assumptions are inherited; these are not process-wide memory,
wall-clock or hostile repository-storage guarantees.

Identity, mode, size, mtime and ctime observations are checked around file reads and directory
traversal. HEAD and index are reread before return. An observed change is an error without a partial
report. This is not an atomic snapshot: later changes, ABA replacement and changes invisible to
filesystem timestamps may escape detection. Directory descriptors pin inspected directories if
concurrently moved. Mount manipulation, hostile hardlinks and metadata replacement remain outside
the trust boundary. Reads may update access times; girt performs no writes or index refresh. A
report must never serve as authorization to overwrite files during future checkout work.

Original unit fixtures cover byte/content and mode changes, restored-mtime edits, symlinks, socket
and ancestor obstructions, exact-case lookup, metadata containment, resource limits, cancellation
and deterministic concurrent file/directory/HEAD/index changes. `tests/status.rs` independently
generates Git fixtures with isolated configuration, no rename detection, no optional index refresh
and explicit native mode/symlink behavior. It covers staged/unstaged combinations, conflicts,
unborn/missing index, loose/packed objects, raw normalization/ignore differences, linked/separate
worktrees and absence of writes. No upstream implementation or test source was used.
`tests/status_portable.rs` separately checks cancellation and explicit platform rejection; Windows
CI selects it deliberately. Native Linux/Windows execution of this increment remains pending.

The 2026-09-24 macOS arm64 run at `9affad69fac53f2c022f47f46b40f8ee7cc588af` used Rust/Cargo 1.98.1
and Git 2.55.0. Full checks passed 1,146 units, 507 integrations and 19 doctests, including 37 local
status cases and 35 status integration cases. See the
[completion record](testing.md#raw-working-tree-status-completion) for checks and cross-compilation
evidence, and the [baseline](benchmarks.md#raw-working-tree-status-baseline) for representative
warm-storage measurements. No publication, merge or checkout is part of this increment.

## Raw Tree Checkout

`Repository::checkout_tree(baseline, target, limits, cancel)` materializes a selected SHA-1 tree and
publishes its index on macOS/Linux. This is tree checkout: HEAD, refs, reflogs and configuration
stay unchanged. Callers supply the baseline tree rather than asking checkout to infer intent from
HEAD. Every normal index entry must match that baseline by path, mode and ID. Staged differences
relative to it, conflicts, gitlinks, dirty or missing tracked files and unsupported index extensions
are refused before worktree mutation, even for paths unchanged in the target. A previous raw status
report never substitutes for these independent checks.

`None` denotes an empty tree. A missing or empty index requires an empty baseline, which supports
unborn repositories and initial checkout after either girt or Git no-checkout clone. A missing index
with a nonempty baseline is rejected. An existing nonempty index cannot be bypassed by choosing an
empty baseline. HEAD is not read or validated; after checkout of a different tree, Git can report
staged changes relative to HEAD. Unverified `TREE` caches are discarded, including when entries are
unchanged. New index entries have zero cached stat words and cleared assume-valid flags. Git status
verifies their content; stat-only plumbing such as `git diff-files` may first need
`git update-index --refresh`.

Checkout preserves literal blob bytes, owner-executable mode and native symlink targets. It does not
run filters, apply attributes/EOL conversion, consult ignores or honor
`core.filemode`/`core.symlinks` overrides. Files receive 0644 or 0755 permissions; prior ownership,
ACLs and other metadata are not preserved. Symlink payloads must contain 1 through 1024 non-NUL
bytes. Empty-tree input is supported; empty directories encoded in trees are not materialized
because Git's index records only leaves.

### Preconditions and Filesystem Boundaries

Callers must exclude other worktree writers and root/ancestor renames throughout the operation, and
protect metadata, objects and mount topology from replacement. The per-worktree index lock excludes
cooperating index writers. Component-wise descriptor-relative opens never follow symlink ancestors;
regular-file writes use exclusively created temporary files, and exclusive `linkat` installation
refuses newly appeared destinations. Symlinks are installed as links themselves, never followed.
File identity and content are checked independently; path/ancestor identity is checked again at
mutation boundaries. Final target verification precedes index publication. These mechanisms detect
observed changes but cannot prevent an arbitrary writer racing the last unlink check, directory
rename or ABA replacement. Hostile hardlink/mount manipulation is outside the contract.

Linux preserves non-UTF-8 names. macOS requires ASCII target paths and names in traversed
directories. Both reject case-colliding selected paths, filesystem name aliases, NUL, backslashes,
colons, empty/dot/parent components, case-insensitive `.git` components and components longer than
255 bytes. Nested ordinary/bare repositories and metadata-directory identities block traversal.
Preparation projects marker-name presence through the actual planned operation order, including
retained siblings and directories left empty by tracked deletions. It refuses any planned non-root
directory with all three `HEAD`, `objects` and `refs` names, conservatively including ASCII case
aliases. Entry types do not narrow this predicate: even three ordinary files are unsupported,
although Git accepts that tree. This preflight prevents checkout from activating its own live guard
midway through mutation; it does not disable guards on existing or newly created directories.
Gitlinks, submodule recursion, sparse/split indexes, index v3/v4 and non-`TREE` extensions are
unsupported. Windows and other platforms refuse checkout before locking or reading the index;
portable tests cover that boundary without claiming Windows materialization support. Native hard
links, symlinks and POSIX modes are required; filesystem emulation and shared-permission policies
are excluded.

### Phases and Failure Recovery

Preparation acquires the index lock, verifies the baseline/target trees and blobs, checks clean
tracked content and obstructions, and validates the complete replacement index. Bounds cover tree
traversal, unique retained blob payloads, files, path prefixes, depth and directory enumerations.
Directory names are enumerated again at mutation boundaries, so wide directories can require
quadratic work. Cancellation is checked between entries and 64-KiB file I/O chunks; one syscall or
object decode cannot be interrupted. There is no wall-clock deadline or background worker.

Mutation removes changed tracked leaves, removes known empty directories only when necessary for a
directory-to-file transition, creates missing directories, then installs changed leaves. Unknown
contents, including empty subdirectories, obstruct transitions; no recursive cleanup or force mode
is provided. Unrelated files and directories are retained. Publication rechecks all target content
and original index bytes, then writes and renames the owned index lock. The index uses its existing
conservative timestamp policy. There is no batch atomicity, crash durability or rollback journal.

A failure reports the last entered stage, every completed namespace operation in order, whether the
index was published, and separate cleanup failures identifying owned artifacts that may remain.
Multiple operations can name one path. A post-mutation inspection failure still reports that
mutation. If index publication fails, successful worktree changes remain with the prior index,
except for independent writers. Cleanup checks temporary identity and refuses observed replacements;
it never recursively deletes a directory. Explicit index abort reports lock cleanup failures. Drop
remains best effort for callers that abandon an index edit without explicit abort.

Recovery requires inspecting both the report and actual files. Do not blindly retry with the old
baseline or remove reported artifacts without verifying ownership and excluding writers. A failed
update may leave tracked paths absent or changed and newly created empty directories. A successful
return means the selected tree and index were verified and published under the exclusion contract,
not that another writer cannot change them afterward.

### Checkout Evidence and Provenance

Original local tests exercise initial checkout, additions/deletions, modes, symlinks and tracked
file/directory transitions; staged/conflicted/dirty states; unsafe paths, aliases, gitlinks and
nested repositories; object corruption and bounds. Deterministic checkpoints cover changed files,
symlink/directory/root substitution, newly appeared destinations, cancellation, write/install/delete
faults, post-mutation failures, cleanup failures and index-publication preconditions. Index storage
units separately inject descriptor-write and rename failures and verify original-byte preservation.

Independent Git CLI fixtures create their own commits and compare `ls-files --stage`, `write-tree`,
status, raw objects and unchanged HEAD. Both girt and Git no-checkout clones become usable
checkouts, then accept subsequent Git commits and `fsck`. Linked and separate worktrees route the
index correctly. An attributes fixture proves the deliberate difference between raw LF
materialization and Git's CRLF checkout conversion. No upstream implementation or test source was
used as input.

Run `cargo run --example checkout` for a disposable public lifecycle. See the
[completion evidence](testing.md#raw-tree-checkout-completion) and
[benchmark workload](benchmarks.md#raw-tree-checkout-baseline) for validation scope.
