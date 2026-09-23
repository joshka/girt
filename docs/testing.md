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
  grammar and provenance. SHA-256, history traversal, annotated tags, refs, signature verification,
  packs, and transport remain excluded; only macOS arm64 has been exercised.
