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
  caller responsibilities. No installation, pruning, GC, or transport is exposed by this artifact
  API. Optional delta generation is covered separately below.
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

## Upload-Pack Fetch Completion

- Protocol v0 provides explicit advertised wants, no-haves/NAK negotiation, side-band-64k progress
  and errors, exact framing, EOF validation and bounded input. The supplied adapter is local
  upload-pack; HTTP/SSH and credential discovery remain separate work.
- Pack import validates checksums, entries, ordinary objects, OFS/REF deltas, identities and typed
  reachable connectivity, then builds an index. Missing external bases and incomplete selected
  graphs fail before installation. Gitlinks remain external submodule roots.
- Index-last, no-clobber installation resolves concurrent opening: unindexed packs are ignored,
  indexed packs must be complete. Tests cover existing-artifact conflicts, retry after index
  publication failure, concurrent publishers/readers, and snapshot retention.
- Fetch changes no refs. Tests and the example separately exercise conditional no-reflog updates,
  including a concurrent ref change. Callers retain individual results for any multi-ref sequence.
- Focused unit tests cover protocol errors/truncation, cancellation and interrupted I/O, corrupt
  zlib and deltas, forward bases, missing and mistyped edges, work/byte/count limits and exact
  bounds. Integration tests use disposable local Git servers and verify all object kinds and actual
  deltas, selected branches/tags, empty/repeated/incremental fetches, Git fsck, and complete index
  equality.
- `examples/fetch_local.rs` is the runnable consumer. The fetch Criterion harness measures
  10,000-ref advertisement processing and protocol/import/connectivity for two delta-pack sizes,
  with fixture construction and server execution outside timing. No numerical acceptance gate is
  imposed.
- [Compatibility](compatibility.md#upload-pack-fetch) and [benchmarks](benchmarks.md#fetch-baseline)
  state limitations: redundant full-history transfer, cooperative generic-stream cancellation,
  trusted paths, caller coordination with GC, and no power-loss guarantee. Later platform and
  owned-transport evidence is recorded separately.

Validation on 2026-09-23 passed `just check` (710 unit, integration and documentation tests,
formatting, all-target Clippy and docs.rs), warning-denying private Rustdoc, and markdownlint-cli2
with the global 100-column configuration. The disposable local-fetch example completed. Criterion
completed both protocol/import workloads and the advertisement workload; CSV estimates and source
fingerprints are retained. After final local cleanup, affected tests, Clippy and documentation
checks passed again.

## Receive-Pack Push Completion

- Preparation accepts explicit branch/tag commands with exact expected old values, validates the
  complete typed reachable graph, proves fast-forward updates, and builds a bounded non-thin pack.
  Gitlinks stay external. Branch rewinds and tag replacement require explicit force; the expectation
  and server policy remain in effect. Deletion and atomic multi-ref push are deferred.
- The v0 client uses shared pkt-line framing with independent receive-pack semantics. It requests
  only `report-status`, checks advertisement expectations and preserves unpack/per-ref results in
  caller order. Complete rejection and partial success are reports; failures after command
  transmission are uncertain and retain valid acknowledgement prefixes. Local refs never change.
- Focused tests cover graph kinds, missing and malformed payloads, duplicate destinations, force
  policy, unsupported protocols/capabilities, corrupt/truncated reports, exact bounds, cancellation,
  interrupted I/O, short writes and flush failures.
- Disposable real Git receive-pack tests establish empty, repeated and subsequent publication,
  branches/tags, OFS/REF-delta source reads, nested tags/trees, binary/symlink blobs and gitlinks.
  Git/girt reads and strict Git fsck verify the result. Server-policy tests include checked-out
  branches, non-fast-forward rejection, pre-receive/update hooks, partial success and a ref race
  after advertisement. No real remotes or project publication are used.
- `examples/push_local.rs` demonstrates branch/tag publication and report checking. Criterion
  measures selection/read/validation/pack construction and prepared-protocol replay at two graph
  sizes, with server setup outside timing. [Compatibility](compatibility.md#receive-pack-push) and
  [benchmark evidence](benchmarks.md#push-baseline) state limits and provenance.

Validation on 2026-09-23 passed `just check` (782 unit, integration and documentation tests,
formatting, all-target Clippy and docs.rs), warning-denying private Rustdoc and markdownlint-cli2
with the global 100-column configuration. The disposable local-push example published four objects
and read its blob back. Rendered push module/function documentation and error links were inspected.
Criterion completed both preparation and protocol workloads; medians, confidence intervals and
source fingerprints are retained. Validation is limited to macOS arm64 and Git 2.55.0.

## Owned Transport Interruption Completion

- [x] macOS/Linux owned pipes support cancellation and absolute deadlines; other OSes reject local
  adapters. Caller-owned streams remain cooperative. Blocking filesystem/CPU/callback work and
  kernel-delayed cleanup are excluded from a hard whole-call bound.
- [x] Unit fixtures cover silent advertisement, blocked reads/writes, EOF/exit stalls, full
  stdout/stderr, acknowledged push prefixes, complete reports awaiting EOF/exit, cancellation
  precedence, child reaping, unwinding and group cleanup that preserves an unrelated child.
- [x] Disposable Git hooks demonstrate cancellation, deadlines before ref commit and after ref
  commit, and uncertainty after transmission. Pre-spawn interruption changes no destination refs
  or objects. Existing fetch/push interoperability covers normal transfers and rejections.
- [x] Consumer examples use the new control with a 30-second deadline and disposable repositories.
- [x] The fetch Criterion harness includes actual local server startup, pipe transfer, validation,
      and exit/cleanup at two sizes; fixture setup stays outside timing. No performance gate is
      imposed.
- [x] Canonical compatibility and Rustdoc contracts record race precedence, diagnostic disposal,
  process ownership, caller SIGCHLD obligations, escaped-descendant exclusions, and OS evidence.

Stalls have finite fallback exits or an independent in-group watchdog. Tests assert error/status and
cleanup outcomes, not tight elapsed-time thresholds. Test controller threads are scoped and joined.
The new transport requires its own Linux runtime validation; parent CI results do not establish it.

Validation passed `just check` with 817 tests, private Rustdoc with warnings rejected, both local
examples, and Markdown lint. Linux GNU and Windows GNU/MSVC library Clippy cross-checks passed with
warnings rejected. The [owned transport baseline](benchmarks.md#owned-transport-baseline) records
actual process/transfer/cleanup measurements on macOS. Linux runtime tests for this change and
Windows transport support remain outside this evidence.

## Incremental Transfer Completion

- [x] Explicit verified local history supports bounded upload-pack have/ACK negotiation, known-only
  no-ops and combined received/local connectivity; delta bases remain pack-internal.
- [x] Explicit push receiver roots exclude only closures inside the fully validated selected graph,
  with live advertisement confirmation and unchanged force/expected-ref policy. Unavailable or
  disconnected roots preserve full transfer; changed advertised knowledge fails before commands.
- [x] Unit coverage includes local count/byte/edge/root bounds, cancellation, missing/corrupt local
      objects, gitlinks, typed closure validation, ACK states/truncation/repetition and `.have`
      proof.
- [x] Disposable Git comparisons cover initial/no-op/incremental fetch and push, shared/divergent
  histories and merges, tags, unavailable receiver roots, and failed dependency rechecks before
  publication. Existing push rejection, ref-race and partial-status cases now use exclusion.
- [x] Both local examples demonstrate explicit knowledge and a repeated operation with no objects.
- [x] Criterion compares full and reduced preparation/transfers at two sizes, measuring local
  knowledge preparation separately. Original fixtures, source fingerprints, transfer sizes and
  cached-storage timing evidence are retained in the incremental benchmark section.

The supported boundary remains SHA-1 protocol v0 with non-thin packs. A single have batch is not an
optimal graph negotiation algorithm. Full graph validation and local dependency rechecks
intentionally retain traversal costs; default pack-snapshot limits apply when installation opens the
destination. No GC retention lock or whole-call deadline is added. Runtime evidence for this
increment is macOS arm64 with Git 2.55.0; earlier platform CI does not establish Linux runtime
coverage for it.

Validation passed 855 unit, integration and documentation tests, formatting, all-target Clippy,
docs.rs and private Rustdoc with warnings rejected. The affected suites passed again after final
boundary and merge/divergence cases were added. Both disposable examples completed, and Markdown
passed rumdl and markdownlint-cli2 with the global 100-column configuration. Linux GNU and Windows
GNU/MSVC library Clippy cross-checks passed; these are compile checks, not runtime evidence.

## Bounded Delta Compression Completion

- Ordinary writing remains the default; explicit pack and push options select bounded internal
  REF_DELTA compression. Same-kind, size-ratio and backward-window selection preserve deterministic
  ordering and avoid thin packs. SHA-256 and OFS_DELTA writing remain excluded.
- Named unit cases cover size varints, insert lengths 0/1/127/128, sparse/four-byte offsets,
  implicit 64 KiB copies, maximum/split copies, empty/tiny/repeated/dissimilar/binary payloads,
  search exhaustion/cancellation, window/candidate/depth/size/work/savings limits, and deterministic
  order/duplicate handling. Invalid identities still fail before output; delta output limits include
  base IDs and trailers. All four kinds use byte matching without parsing assumptions.
- Public consumer tests independently regenerate indexes with Git, assert exact index equality,
  prove delta emission with verify-pack, and compare Git/girt payloads and identities, including
  shifted 1 MiB binary input. Existing malformed delta, output-failure, and no-clobber tests cover
  shared decoding and storage boundaries.
- A real disposable local push sends deltas and verifies the selected graph, then sends an
  incremental update. Protocol tests exercise a receiver advertising only report-status. Existing
  incremental, force, expectation-race, cancellation, and uncertain-status regressions remain part
  of the full test suite.
- Criterion compares ordinary and delta writing over edited/shifted binary data, generated source,
  independent noise, repeated bytes, oversized objects, and tiny values. Retained byte counts,
  candidate/work/depth counts, timing intervals and fingerprints make the comparison reproducible.
  Scratch memory is bounded analytically; allocator overhead and RSS are not measured.
- [Compatibility](compatibility.md#bounded-pack-delta-compression) and
  [benchmark evidence](benchmarks.md#bounded-delta-comparison) describe defaults, limits, tradeoffs,
  provenance and the macOS-only runtime boundary for this change.

Validation on 2026-09-23 passed `just fmt` and `just check` (650 unit tests, 234 integration tests,
17 doctests, all-target Clippy and docs.rs). Warning-denying private Rustdoc,
`cargo run --example write_pack`, and markdownlint-cli2 with the global 100-column configuration
also passed. `cargo bench --bench pack_delta` completed all seven ordinary/delta comparisons;
retained source fingerprints verify successfully. Runtime evidence remains macOS arm64 and Git
2.55.0; the earlier Linux CI runs do not validate this change.
