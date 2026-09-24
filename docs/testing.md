# Testing and Completion Criteria

Define completion for each usable capability before implementing it. Name the supported behavior,
compatibility boundaries, and evidence needed to accept the change. Select tests by the promises and
risks involved; one test can satisfy several criteria without duplicating assertions across suites.

## Required Evidence

- **Behavior:** State what callers can do, supported formats, and explicit exclusions. Reject
  unsupported cases rather than silently approximate them.
- **Local correctness:** Cover independently testable rules with focused unit tests beside the
  implementation. Include meaningful boundaries, rejected inputs, and promised absence of side
  effects. Assert observable behavior rather than implementation steps or derived traits.
- **Public API:** Demonstrate a usable operation through an executable example or integration test.
  Exercise ownership, errors, and setup as a library consumer would.
- **Git compatibility:** Compare with Git when reading, writing, or interpreting Git data. Exercise
  both directions where applicable. Round trips alone can hide a bug shared by reader and writer;
  include independent expected values or Git-generated input.
- **Failure behavior:** Test relevant corruption, resource limits, partial writes, cleanup, and
  concurrency guarantees. Document what may remain after failure.
- **Documentation:** Update affected contracts, examples, status, compatibility evidence, and
  limitations. Record fixture provenance and the Git versions and platforms exercised.
- **Validation:** Run the applicable [development checks](../CONTRIBUTING.md#development-checks).
  Fix failures caused by the change and report checks that could not run.

Keep integration tests under `tests/` and original fixtures under `tests/fixtures/` when files are
needed. Generate fixtures independently and isolate filesystem tests from the working checkout and
global configuration. Follow [Rust Conventions](rust-conventions.md#unit-tests) and the
[Rustdoc Standard](rustdoc.md#examples-and-validation) for local tests and executable examples.

## Parameterized Tests

Use `rstest` with named `#[case::name(...)]` inputs for parameterized unit and integration tests.
Each case should run independently and identify the behavior or boundary in its name. Keep test
bodies to setup, the operation, and direct assertions; avoid loops over cases or branches that
select expected results. Supply expected values as case parameters instead. Simple fixture
construction is fine, but do not reimplement the algorithm under test to calculate expectations.
Keep ordinary `#[test]` functions for single scenarios that do not need parameters.

## Performance Evidence

Add a reproducible benchmark when introducing a significant processing path, changing a known hot
path, or making a performance claim. Hashing, compression, pack decoding, and graph traversal merit
representative baselines. A domain-type change or documentation correction usually does not require
a new benchmark; use existing benchmarks when performance could be affected.

Use Criterion for benchmark sampling and analysis rather than a manual timing harness. Keep
benchmark harnesses under `benches/`. Choose representative input sizes and content, keep fixture
setup outside the measured operation, and name what is measured. Separate operations with different
costs, such as publishing a new object and validating an existing one. For filesystem measurements,
record cache conditions and avoid presenting cached reads as cold-storage performance. Record the
command, toolchain, platform, workload, and retained revision or verifiable source fingerprint
needed to repeat a result.

Use measurements to identify sustained regressions and guide design. Establish numerical acceptance
thresholds only when a consumer requirement or measured budget justifies them; noisy timing changes
should not automatically fail CI. Throughput measurements do not prove memory limits or bounded
resource use: validate those contracts separately. No numerical code-coverage threshold is required.

## Implementation Task Checklist

Include a concrete version of this checklist in each capability's task description. Replace general
items with named behaviors, inputs, and expected outcomes before work begins. Mark an item
inapplicable with a reason when the capability does not need it.

- [ ] Supported behavior and exclusions are stated.
- [ ] Local rules, boundaries, and rejection cases have focused tests.
- [ ] A consumer-facing example or integration test exercises the operation.
- [ ] Git compatibility has independent evidence in the applicable directions.
- [ ] Relevant failure, resource, and concurrency guarantees are tested.
- [ ] Performance-sensitive work has a reproducible baseline or comparison.
- [ ] Documentation and compatibility evidence match the implementation.
- [ ] Applicable checks pass; untested platforms and other limitations are recorded.

A capability may be complete within an explicitly limited scope. Validation on one platform does not
establish support on other platforms, and recognizing a format does not establish that it is
implemented. Keep these distinctions visible in the completion report.

## Blob Baseline Completion

- Exact encoding and independently established identities are covered by named `rstest` cases for
  9/10-byte and 99/100-byte lengths and all 256 byte values in `src/object.rs`.
- [Compatibility evidence](compatibility.md) records fixture provenance and both directions of Git
  interoperability, plus existing corruption, size-limit, concurrency, and cleanup coverage.
- [Performance evidence](benchmarks.md) records reproducible hashing, cached reads, new-object
  writes, and existing-object writes over varied sizes and compressibility, with environment and
  results.
- The baseline remains limited to SHA-1 loose blobs on the exercised macOS platform. No new Git
  capability or numerical performance threshold is implied by completion.

## In-Memory Tree Completion

- Structural parsing and construction are separate: exact supported payloads round-trip even when
  name or ordering validation fails. Unit tests cover empty trees, five modes, byte names, prefix
  ordering, duplicates, malformed modes, missing delimiters, and truncated IDs.
- `examples/tree.rs` and the `Tree` doctest demonstrate construction, encoding, parsing, validation,
  and identity through the public API.
- [Compatibility evidence](compatibility.md#in-memory-sha-1-trees) records exact byte and identity
  agreement with Git, Git-produced input, Git consumption of girt output, and noncanonical cases.
- The parsing/encoding [benchmark](benchmarks.md#in-memory-tree-baseline) measures owned operations
  on representative small and large trees. No numerical acceptance threshold is established.
- Filesystem failures, partial writes, cleanup, and concurrency tests are inapplicable: these tree
  operations have no external side effects. Memory grows with the caller-supplied payload; the API
  does not promise a configurable allocation limit.
- The supported boundary is in-memory SHA-1 trees on the exercised macOS platform. This does not
  establish loose-tree storage, checkout safety, or complete `git fsck` validation.

## Loose-Tree Completion

- SHA-1 loose trees share framing, hashing, decompression, and safe publication with blobs. Reads
  preserve the parsing/validation distinction; writes preserve supported payload bytes.
- Focused storage unit tests cover limits, missing objects, framing and parsing failures, duplicate
  and concurrent publication, and preservation/cleanup on failure. Existing blob corruption tests
  exercise the shared decoder without changing blob behavior.
- `examples/loose_tree.rs` demonstrates blobs referenced by a stored and restored tree.
  `tests/trees.rs` checks Git-written storage and Git consumption of girt-written storage, including
  supported noncanonical encodings.
- The [loose-tree baseline](benchmarks.md#loose-tree-storage-baseline) measures cached reads, new
  publication, and existing-object validation for small and large trees.
- Reference resolution, recursive filesystem import, checkout, the index, commits, references,
  packs, repository discovery, and SHA-256 storage remain outside this capability.

## Commit Completion

- Root, single-parent, and merge commits expose ordered references, byte identities, Unix seconds,
  offsets, message bytes, and opaque multiline headers. Parsing preserves accepted lexical details;
  construction and explicit validation apply documented field rules without promising full fsck.
- Local tests exercise literal payload/identity expectations, byte preservation, reconstruction,
  malformed/truncated headers, date limits, invalid identities, and reserved header names.
- `tests/commits.rs` independently compares Git-created and girt-created payloads and identities,
  including actual loose storage in both directions. Ordinary commits pass strict Git fsck;
  noncanonical and NUL fixtures use Git's literal object mode and promise preservation only.
- Commit storage tests exercise resource limits, missing storage, wrong types, framing and parser
  failures, duplicate/concurrent publication, and cleanup without replacing existing corrupt files.
  Shared decoder corruption tests continue to apply.
- `examples/loose_commit.rs` demonstrates blob-to-tree-to-commit publication and reading back the
  snapshot. The doctest teaches construction, parsing, field access, and identity.
- The commit Criterion harness measures construction, parsing, payload copying, hashing, warm reads,
  new writes, and existing-object validation for small and large messages. No numerical threshold is
  set.
- The [compatibility evidence](compatibility.md#sha-1-commits-and-loose-storage) defines supported
  grammar and provenance. SHA-256, history traversal, refs, signature verification, packs, and
  transport remain excluded; only macOS arm64 has been exercised.

## Annotated Tag Completion

- Tags target blobs, trees, commits, or tags and expose names, optional taggers, opaque header
  lines, and byte messages. Parsing preserves exact payloads, including omitted taggers and empty
  messages without separators. Construction validates separately and emits canonical framing.
- Unit tests cover each target type, literal identity, opaque PGP/SSH message content, lexical
  preservation, malformed/truncated headers, unsupported formats, and construction rejection.
- `tests/tags.rs` compares independently Git-created payloads and identities, reads Git loose
  storage, then republishes through girt for Git consumption. Ordinary tags pass strict fsck;
  preservation fixtures use explicitly relaxed mktag or literal hashing as documented in the
  [compatibility evidence](compatibility.md#sha-1-annotated-tags-and-loose-storage).
- Tag storage tests cover exact and exceeded limits, bounded decompression, missing objects, wrong
  types, corruption, parsing errors, duplicate/concurrent publication, and failure cleanup. Shared
  zlib decoder tests continue to apply.
- `examples/loose_tag.rs` and the `Tag` doctest demonstrate construction, storage, field access,
  parsing, and identity without creating tag references.
- The Criterion tag harness measures construction, parsing, copying, identity, warm reads, new
  writes, and existing-object validation with small and large messages. The
  [baseline](benchmarks.md#tag-baseline) records reproducible evidence without a numerical gate.
- Only SHA-1 on macOS arm64 has been exercised. Tag references, recursive peeling, signature
  verification, repository discovery, packs, and transport remain excluded.

## Repository Opening Completion

- Explicit roots, metadata directories and gitfiles open ordinary, bare, separate-directory and
  linked-worktree Git fixtures. Paths and worktree relationships are checked, and Git-written loose
  blobs are read through the detected store.
- Configuration unit tests cover byte parsing, quoting, escapes, implicit/empty/repeated values,
  subsection case, malformed syntax and numeric interpretation. Independent Git CLI comparisons
  establish the supported forms and relevant core/extensions behavior.
- Rejection tests cover unsupported formats, includes, worktree configuration, extensions, storage
  layouts and malformed metadata. Snapshots check non-mutation; missing paths are not created. An
  example subprocess proves that inherited Git overrides do not affect explicit opening.
- `examples/open_repository.rs` runs against a disposable Git fixture. Public Rustdoc describes
  source semantics and limitations; [compatibility evidence](compatibility.md) owns the detailed
  boundary and provenance.
- The repository Criterion harness measures small/large configuration parsing and cached bare
  opening. [Performance evidence](benchmarks.md#repository-opening-baseline) records the baseline;
  there is no numerical performance gate.
- Only macOS arm64 is exercised. Initialization, upward discovery, refs, packed reads, history and
  transport are excluded. Worktree configuration, includes and unknown extensions fail explicitly.

## Reference Completion

- `RefName` unit cases validate full byte names and HEAD without normalization. Git name checks
  independently cover valid and invalid names; byte names also round-trip through packed refs.
- Loose/direct/symbolic parsing, packed headers and peeled-record validation, duplicates, malformed
  data and unsupported backends are tested. Git-generated loose, packed and annotated-tag fixtures
  establish precedence and distinguish resolution from peeling.
- Missing/unborn/dangling refs, missing objects, cycles and depth limits have explicit results.
  Linked worktree comparisons exercise HEAD, all three private namespaces and shared branches.
- Conditional publication checks absence or an exact stored value under owned locks. Tests cover
  symbolic replacement versus terminal updates, packed shadowing, namespace conflicts, existing
  locks, failed conditions, write/rename failure and cleanup. girt/girt and girt/Git races permit
  exactly one writer to replace a common expected old value.
- `examples/publish_branch.rs` stores commits and publishes/advances a branch through HEAD in a
  disposable repository. Reflog tests confirm both existing-log preservation and omitted new logs;
  the [compatibility contract](compatibility.md#files-references) explains recovery/retention costs.
- The Criterion reference harness records warm loose/packed reads, symbolic resolution and no-reflog
  publication without claiming cold-cache or durable-write latency. No numerical gate is
  established. Packed input allocation is proportional to file size, with no configurable limit.
- macOS arm64 is exercised; Linux-only non-UTF-8 loose filename tests remain unrun. Non-Unix
  storage, noncooperating writers, crash durability, reflogs, deletion and multi-ref transactions
  are outside the supported boundary. No object-pack, history or transport capability is implied.

## Pack Reading Completion

- SHA-1 pack/index v2 reads expose all four object kinds and exact payloads through
  `Repository::objects`. Loose reads take precedence; loose writing remains unchanged.
- Original unit fixtures cover index structure, ordering/fanout, large offsets, pack/index
  checksums, CRCs, malformed/truncated entries, zlib completion, delta instructions and base
  references. Every traversed object's identity is verified, including intermediate bases.
- Git-generated OFS_DELTA and REF_DELTA fixtures establish independent identity, kind, and byte
  agreement for blobs, trees, commits, and tags. Mixed storage and post-repack reads exercise the
  unified boundary; `examples/packed_repository.rs` prints an existing object's exact payload.
- Snapshot bytes/count, individual payload/program bytes, cumulative decoding, and delta depth have
  explicit limits and failure tests. Cycles and missing/external bases fail without recursive calls.
  Opening and reading are read-only; publication/partial-write tests are inapplicable.
- [Compatibility evidence](compatibility.md#sha-1-pack-reading) states validation timing, owned
  snapshot lifetime, no decoded cache, supported versions, fixture provenance, and exclusions.
- The [pack baseline](benchmarks.md#pack-read-baseline) uses Criterion on 16- and 256-blob Git
  workloads, measuring validated opening, indexed absence, ordinary reads, and reconstruction. Reads
  include live loose-path misses and identity verification. Pack bytes are in memory and filesystem
  caches are warm; these are not cold-storage or large-repository claims.
- Pack v3, index v1, external/thin bases, SHA-256, multi-pack indexes, and pack writing remain
  outside the pack-reading capability. Only macOS arm64 is exercised; no performance threshold is
  set.

## Commit History Completion

- `Objects::walk` accepts explicit SHA-1 roots, deduplicates shared history, and returns
  breadth-first root/parent discovery order. It is not topological; timestamps never influence
  results.
- `is_ancestor` includes equality; `merge_bases` returns all best common ancestors in object-ID
  order. Both validate the complete ancestry of both endpoints, even when an early answer is
  possible.
- `src/history.rs` tests cycle detection, duplicate edges, pre-enqueue bounds, and iterative
  traversal through 100,000 nodes. `tests/history.rs` covers empty/root, linear, branched, merge,
  disconnected, identical, duplicate-parent/root, skewed-time, and criss-cross histories. Exact and
  exhausted graph limits, per-read limits, missing parents, corruption, wrong types, and malformed
  commits are tested.
- Independent Git-written fixtures compare reachable sets, ancestor answers, and all merge bases.
  Criss-cross fixtures run both loose and repacked. No Git implementation code or fixture was
  copied. See [compatibility evidence](compatibility.md#commit-history).
- `examples/history.rs` is a runnable consumer; [benchmarks](benchmarks.md#commit-history-baseline)
  measure packed linear and merge-heavy traversal and merge bases with setup outside timing.
- Operations have no writes or partial results. Memory grows with bounded commits and parent edges,
  plus transient object parsing and the existing pack snapshot. Every distinct object is read once
  per operation; repeated operations do not share a graph cache. Storage decoding limits apply per
  read, not cumulatively across a walk. Caller-supplied root occurrences take linear input work.
- Revision expressions, path history, content diff/merge, commit-graph files, shallow/partial
  repositories, ref mutation, pack writing, and transport are outside this capability. Results are
  eager; no streaming or topological-order option is promised. Only macOS arm64 has been exercised.

Validation on 2026-09-23 passed `just check` (582 unit, integration, and documentation tests, format
checks, all-target Clippy, and docs.rs), warning-denying private Rustdoc, and markdownlint-cli2 with
the global 100-column configuration. The consumer example ran against this checkout with identical
endpoints and returned that endpoint as the sole merge base. Criterion results and source
fingerprints are retained with the benchmark evidence.

## Pack Writing Completion

- `write_pack` generates SHA-1 pack v2 and index v2 artifacts from explicit borrowed inputs, with
  ordinary entries for blobs, trees, commits, and tags. Reachability and payload syntax remain
  caller responsibilities. No installation, delta selection, pruning, GC, or transport is exposed.
- Focused unit tests cover identity/kind mismatches, conflicting and exact duplicates, deterministic
  ordering, input and output limits, exact bounds, short writes, write/flush failures, size headers,
  and checked output-counter overflow. Input rejection leaves both outputs untouched; later failure
  requires callers to discard both artifacts.
- `examples/write_pack.rs` exports a binary blob to caller-owned files, constructs an exclusively
  owned private repository, and reopens/verifies it. No live pair-publication guarantee is implied.
- `tests/packs.rs` checks empty, mixed-kind, repeated, binary, and 4 MiB exports against independent
  Git index generation, complete index-byte equality, verify-pack, and exact object reads by both
  Git and girt. Test repositories contain only the exported pack objects.
- Synthetic original index fixtures cover 2 GiB and 4 GiB offset encoding without multi-gigabyte
  allocation. Actual multi-gigabyte validation and non-macOS platforms remain untested.
- The Criterion writer harness measures hashing, deduplication/sorting, compression, checksums, and
  index generation for similar and pseudorandom payloads. Output sizes and throughput are recorded
  in [benchmark evidence](benchmarks.md#pack-write-baseline), with no numerical acceptance gate.
- [Compatibility evidence](compatibility.md#sha-1-pack-writing) records contracts, fixture
  provenance, Git/platform versions, failure recovery, and the next publication boundary needed by
  fetch.

Validation on 2026-09-23 passed `just check` (616 unit, integration, and documentation tests,
formatting, all-target Clippy, and docs.rs), warning-denying private Rustdoc, and markdownlint-cli2
with the global 100-column configuration. `cargo run --example write_pack` exported and verified its
private artifact pair. Criterion completed the four writer workloads with five-second sampling
windows; results and source fingerprints are retained with the benchmark evidence.
