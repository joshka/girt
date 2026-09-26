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
  Exercise ownership, errors, and setup as a library consumer would. For workflows composing
  existing primitives, cover supported policy combinations through the public workflow API;
  component tests alone do not establish that the policies compose.
- **Git compatibility:** Compare with Git when reading, writing, or interpreting Git data. Exercise
  both directions where applicable. Round trips alone can hide a bug shared by reader and writer;
  include independent expected values or Git-generated input.
- **Failure behavior:** Test relevant corruption, resource limits, partial writes, cleanup, and
  concurrency guarantees. Document what may remain after failure. For workflows with live mutation
  guards, project their preconditions over planned intermediate/final states, including retained
  data, before the first mutation. Keep the live guards to detect independent changes; a workflow
  must not predictably reject its own output only after applying earlier operations.
- **Documentation:** Update affected contracts, examples, status, compatibility evidence, and
  limitations. Record fixture provenance and the Git versions and platforms exercised.
- **Validation:** Run the applicable [development checks](../CONTRIBUTING.md#development-checks).
  Fix failures caused by the change and report checks that could not run.

Keep integration tests under `tests/` and original fixtures under `tests/fixtures/` when files are
needed. Generate fixtures independently and isolate filesystem tests from the working checkout and
global configuration. Follow [Rust Conventions](rust-conventions.md#unit-tests) and the
[Rustdoc Standard](rustdoc.md#examples-and-validation) for local tests and executable examples.

## Roadmap Completion and Coordination

Before dispatch, name the capability's acceptance cases and required platform evidence using the
[jj acceptance matrix](jj-acceptance.md). Implement one queued item at a time. Keep repeatable rules
here and in the convention guides instead of expanding worker prompts. Keep jj unchanged until the
roadmap's final integration item; earlier corpus work exercises girt public APIs and observes an
unchanged jj baseline in disposable repositories.

A worker completion must report implemented behavior; new or changed public calls, types, and
modules; exact tested revision; validation commands and results; interoperability, boundary, fault,
race, platform, and benchmark evidence; and remaining limitations. Mark inapplicable evidence with a
reason. Link retained artifacts and independently generated fixture provenance. Never describe a
finite corpus as exhaustive parity proof.

Make task updates easy to scan without losing the decision trail. Open with the concrete problem,
intended capability and central uncertainty. During work, report findings and why they change the
next investigation or decision. Lead completion with the outcome and material blocker, if any;
summarize the major design choices, enabled behavior and limits in a few substantive points. Keep
revision lists, test counts and CI chronology in linked evidence, while naming the exact tested
revision in the completion callback. Separate the original delivery from later corrections when
summarizing history.

The coordinator presents that concise summary, updates the roadmap status and completion links, adds
discovered follow-ups and dependency changes, and dispatches the next ready item after acceptance or
an explicit deferral under the
[roadmap prioritization policy](jj-roadmap.md#prioritization-and-deferral-policy). Use completion
callbacks instead of polling. A blocked acceptance criterion remains open; unit-test success or a
platform cross-build does not close it. Deferred heuristic work stays visibly unaccepted in the
comeback backlog without automatically gating unrelated work or first integration. Final integration
must assess and report its user-visible impact. Newly discovered core required behavior must be
queued and resolved before final integration rather than silently excluded; data loss, corruption
and unsafe mutation remain blockers.

Review architecture, layering, cohesion, and abstraction debt after approximately ten completed
items, and earlier when cross-cutting issues accumulate. Record concrete findings and bounded
remediation tasks; resolve correctness or layering blockers before depending on them. Native CI
milestones execute on each supported OS and retain exact revisions and logs. They do not replace
capability-specific native testing.

When a transport fault test requires a post-send outcome, synchronize cancellation with an observed
request before asserting uncertainty. Keep fixture watchdogs separate from the transport deadline
being tested: slow discovery can legitimately expire an overall deadline before mutation. Assert the
specific structured error and retained effects, and include request observations in failure
diagnostics. See [R14's HTTP repair](evidence/r14-http.md) for a controlled example.

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

The portable `decoding` integration suite exercises R07 in SHA-1 and SHA-256, including executable
Git interpretation, loose/history reads, tag peeling and resource/corruption errors. Its transfer
case is SHA-1-only because negotiation remains owned by R26/R29. Linux/macOS run it in the full
suite; Windows explicitly selects it. Retained provider-capture comparisons need no installed key or
signing provider.

## Native Platform Coverage

The platform workflow runs the full all-feature suite on Ubuntu 22.04/24.04 and macOS 14. Windows
runs all-feature library units and doctests, an explicit core-only integration selection, and a
separate HTTP-only runtime suite. All hosts independently compile core-only, HTTP-only, and SSH-only
libraries and run all-feature/all-target Clippy. Unix also runs core-only units and selected
both-format interoperability suites, plus isolated HTTP and SSH runtime suites, with tracing
disabled. Windows runs core-only units and its portable selection without tracing. Compilation is
not runtime evidence; SSH feature compilation on Windows does not expose a Windows SSH adapter.

The Windows integration selection follows implemented operations, not just file portability:

- `object_ids`: Format-aware identity/hash vectors and storage-boundary refusal (both hashes).
- `sha256`: Both-format codecs, loose storage, Git interoperability, layouts, traversal and SHA-256
  storage. Portable cases use Git-created refs/worktrees; packed checkout/status cases are
  explicitly scoped to macOS/Linux; files-reference rejection cases require Unix.
- `blobs`, `trees`, `commits`, `tags`: Object formats, loose storage, Git byte interoperability.
- `decoding`: Both-format tolerant tree/tag/commit interpretation, peeling and corruption/resource
  boundaries. Its transfer case is SHA-1-only pending R26/R29.
- `packs`, `history`, `tree_compare`, `content_diff`: Pack/index I/O, deltas, graph queries,
  structural tree comparison and byte-preserving content diff.
- `index`: SHA-1/SHA-256 v2/v3/v4 parsing/encoding, held-lock replacement, Git stat/flag
  observations, byte paths, and linked/separate-gitdir routing. No checkout or reference-backend
  operation is required.
- `colocation`: Both-format conditional HEAD/index composition, flagged entries, stat reuse,
  linked-worktree routing and Git-generated operation metadata cleanup.
- `status_portable`: Cancellation before storage access and explicit unsupported-platform status
  rejection. The `status` suite requires macOS/Linux descriptor-relative traversal and is excluded
  on Windows; Linux byte filenames and macOS normalization restrictions have distinct local cases.
- `checkout_portable`: Cancellation before locking and explicit unsupported-platform checkout
  rejection. The `checkout` suite and native symlink tests run only on macOS/Linux.
- `repositories`: Opening, initialization, discovery, configuration and Git-written layouts.
- `layout_shallow`: Both-format relative/moved layouts, worktree inventory and shallow-depth
  observations. Filesystem symlink/permission cases are Unix-only; raw byte paths are Linux-only.
  Case-alias cases require a case-insensitive native volume and report absence when unavailable.
- `config_edit`: Lossless configuration/remote edits, lock cleanup, races and Git interpretation.
- `config_resolution`: Portable layered configuration, conditions, provenance, environment and Git
  observations in both formats; arbitrary-byte filenames are Linux-only.
- `remotes`: Config/refspec mapping compared with Git-managed refs and transfers.
- `http_portable`: Real Git HTTP fetch/install/reuse and push, status errors, truncation, deadline.
- `references_portable`: R11 conditional refs, imported reflogs and packed/shadowed deletion execute
  in both formats on Windows. Reference unit/fault tests also run natively.
- `references`: Both-format loose/packed/symbolic reads, publication, deletion, worktree routing,
  reflogs and concurrent Git writers. Non-UTF-8 argv cases remain Unix-only; raw filename cases are
  Linux-only. Windows storage-name refusals also run in `references_portable`.
- `http`: SHA-1 fetch/push, TLS trust/hostname checks, authentication, limits, cancellation,
  uncertain/partial outcomes and HTTP-backed fetch/clone publication. Its shell-hook fixture is
  Unix-only; the remaining suite is selected on Windows with `http_portable`.
- `local_native`: Both-format files/reftable local fetch and push, with Git reading the destination,
  execute on Windows. `fetch`, `push`, `fetch_workflow`, and `clone` still include Unix-only
  process/hook fixtures and are excluded as complete suites on Windows. Portable planning and clone
  completion units execute there; HTTP exercises public fetch/clone publication. R27/R28/R30 retain
  the broader transfer, publication and cancellation corpus.
- `ssh`: Excluded: the adapter and its process-lifetime implementation are macOS/Linux-only (R24).

Git-managed refs inside fixtures do not by themselves establish girt reference-storage support; the
reference suites exercise girt operations explicitly. Tree symlink modes and non-UTF-8 entry names
are object bytes, not Windows filesystem symlinks or byte filenames. Windows raw status and checkout
remain unsupported, with refusal-only tests; C02 retains assessment and assignment of any required
native implementation before R34.

HTTP fixtures use disposable loopback servers, Python and Git's public CGI backend. TLS fixtures
also need OpenSSL. Protocol operations remain SHA-1-only pending R26/R29; two-format storage tests
do not establish SHA-256 negotiation. R36's [evidence](evidence/r36.md) records exact native results
and the C02/R19/R20 UNC, WSL, ACL and worktree-administration exclusions. Maintenance remains with
R31–R33. A CI compile or explicit unsupported error does not close those capability gaps.

For Git CLI fixtures, pass repository-relative path arguments under an explicit working directory
when possible. Rust's Windows canonical paths use verbatim prefixes that some Git commands and CGI
variables do not accept. Compare discovery results by canonical path identity rather than display
bytes; keep byte-level comparisons for formats whose bytes are the contract.

When adding an integration suite, decide its Windows applicability here and in the workflow. Keep
unsupported operations separate from fixture assumptions; do not disable otherwise portable coverage
because another suite requires a Unix backend. Record exact native run revisions in
[compatibility evidence](compatibility.md#platform-and-git-version-validation).

The portable `tracing` integration suite uses original local data and loopback HTTP fixtures. Run it
with `tracing` alone and with `tracing,http`; reference and HTTP clone completion cases run on
Windows too. Windows CI runs both configurations. The `tracing,ssh` library is checked
independently; existing SSH process and cancellation tests run under all features and SSH-only on
supported native platforms. Named SHA-1/SHA-256 cases in `object_ids`, `sha256`, `packs`, `index`,
and `references` establish format-specific execution; a successful compile or a suite's filename is
not sufficient evidence. Native run logs record OS, filesystem, compiler and Git versions for
checkout and temporary-fixture storage.

## Historical Capability Completion Records

The following sections record evidence collected when each capability landed. Counts, commands, API
names, and platform results describe those revisions, not a fresh run of the current checkout.
Current implementation expectations are above; current capability and platform boundaries are in
[compatibility evidence](compatibility.md#current-capabilities-and-evidence).
Later records may supersede earlier limitations, with the original evidence retained for provenance.

### Blob Baseline Completion

- Exact encoding and independently established identities are covered by named `rstest` cases for
  9/10-byte and 99/100-byte lengths and all 256 byte values in `src/object.rs`.
- [Compatibility evidence](compatibility.md) records fixture provenance and both directions of Git
  interoperability, plus existing corruption, size-limit, concurrency, and cleanup coverage.
- [Performance evidence](benchmarks.md) records reproducible hashing, cached reads, new-object
  writes, and existing-object writes over varied sizes and compressibility, with environment and
  results.
- The baseline remains limited to SHA-1 loose blobs on the exercised macOS platform. No new Git
  capability or numerical performance threshold is implied by completion.

### In-Memory Tree Completion

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

### Loose-Tree Completion

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

### Commit Completion

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

### Annotated Tag Completion

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

### Repository Opening Completion

- Explicit roots, metadata directories and gitfiles open ordinary, bare, separate-directory and
  linked-worktree Git fixtures. Paths and worktree relationships are checked, and Git-written loose
  blobs are read through the detected store.
- Configuration unit tests cover byte parsing, quoting, escapes, implicit/empty/repeated values,
  subsection case, malformed syntax and numeric interpretation. Independent Git CLI comparisons
  establish the supported forms and relevant core/extensions behavior.
- Rejection tests cover unsupported formats, extensions, storage layouts and malformed metadata.
  Snapshots check non-mutation; missing paths are not created. An example subprocess proves that
  inherited Git overrides do not affect explicit opening.
- `examples/open_repository.rs` runs against a disposable Git fixture. Public Rustdoc describes
  source semantics and limitations; [compatibility evidence](compatibility.md) owns the detailed
  boundary and provenance.
- The repository Criterion harness measures small/large configuration parsing and cached bare
  opening. [Performance evidence](benchmarks.md#repository-opening-baseline) records the baseline;
  there is no numerical performance gate.
- Only macOS arm64 is exercised. Initialization, upward discovery, refs, packed reads, history and
  transport are excluded. R08 subsequently adds layered resolution and worktree configuration;
  unknown extensions fail.

### Reference Completion

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

### Pack Reading Completion

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
- [Compatibility evidence](compatibility.md#pack-reading) states validation timing, owned snapshot
  lifetime, no decoded cache, supported versions, fixture provenance, and exclusions.
- The [pack baseline](benchmarks.md#pack-read-baseline) uses Criterion on 16- and 256-blob Git
  workloads, measuring validated opening, indexed absence, ordinary reads, and reconstruction. Reads
  include live loose-path misses and identity verification. Pack bytes are in memory and filesystem
  caches are warm; these are not cold-storage or large-repository claims.
- Pack v3, index v1, external/thin bases, SHA-256, multi-pack indexes, and pack writing remain
  outside the pack-reading capability. Only macOS arm64 is exercised; no performance threshold is
  set.

### Commit History Completion

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

### Pack Writing Completion

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

### Upload-Pack Fetch Completion

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

### Receive-Pack Push Completion

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

### Owned Transport Interruption Completion

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

### Incremental Transfer Completion

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

### Bounded Delta Compression Completion

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

### Smart HTTP Completion

- [x] SHA-1 protocol v0 discovery and single stateless RPC boundaries preserve stream/local APIs.
  Full/incremental fetch, receiver-history exclusion and optional delta push remain explicit.
- [x] Focused unit tests reject unsafe URLs and protocol/routing header overrides, prevent header
  injection and verify sanitized errors. Existing protocol tests exercise the extracted phases.
- [x] `http_local` demonstrates disposable push/download/validation with a caller-owned runtime.
- [x] Loopback Git `http-backend` tests compare actual Git/girt objects and refs for full,
  incremental, no-op, branch, tag and delta transfers. Mixed ref outcomes remain visible.
- [x] Authentication, framing/media types, statuses, redirects, truncation, wire/decode limits,
  stalled discovery/TLS/upload/status waits and uncertain reports have failure tests. Local CA
  fixtures validate HTTPS trust and hostname checks and perform real HTTPS fetch/push.
- [x] Criterion separates full, incremental and known-only loopback transfers with setup excluded;
      [retained evidence](benchmarks.md#smart-http-loopback-baseline) includes limits of
      interpretation.
- [x] [HTTP contracts](http.md) explain runtime/lifetime/concurrency, explicit synchronous CPU work,
  credentials, trust, interruption, memory, unsupported cases and fixture setup.
- [x] `just check` passes: 665 unit tests, 271 integration tests (32 HTTP), 12 doctests,
  all-target/all-feature Clippy and docs.rs. Core-only `cargo check --no-default-features`,
  warning-denying private Rustdoc, the disposable HTTP example and Markdown lint also pass.

The adapter is opt-in. `just check` now tests/clippies all features and docs.rs enables all
features; core-only compilation is checked separately. New runtime evidence is limited to macOS
arm64. Earlier Linux CI is not HTTP validation, and HTTP integration fixtures remain Unix-gated.

### SSH Completion

- [x] Optional macOS/Linux OpenSSH adapters expose async v0 fetch/push with caller-owned runtime,
      explicit endpoint components/config, separate synchronous fetch validation and push
      preparation.
- [x] Unit tests reject unsafe endpoint/options shapes and verify literal quoting, redacted errors,
  pre-write cancellation, process reaping, dropped-future cleanup and isolated group cleanup.
- [x] Real loopback sshd and Git verify full/incremental/no-op fetch, initial/subsequent/no-op push,
  branch/tag objects, delta transfer and exclusion. Mixed Git ref rejections return reports.
- [x] Temporary trust/key fixtures verify known, unknown and changed hosts, failed authentication,
  config override protection, and repository paths with spaces, quotes and shell metacharacters.
- [x] Fault fixtures verify blocked diagnostics, handshake/service/upload/response/exit stalls,
  cancellation, byte limits, malformed framing and acknowledged push status retention.
- [x] `ssh_local` demonstrates disposable publication and download with validation outside the
      runtime. Criterion measures full/incremental/known-only SSH fetch including handshake and
      cleanup.
- [x] [SSH contracts](ssh.md) state executable/config trust, enforced noninteractive policy,
      unsupported endpoint/options, sanitized errors, local cleanup versus remote uncertainty, CPU
      and memory boundaries, dependencies and deferred architecture decisions.

Runtime interoperability evidence is macOS arm64 with Git 2.55.0 and OpenSSH 10.3p1. Fault services
and compile cross-checks are separate evidence; they do not establish Linux or Windows runtime
compatibility. The fixture uses the existing local OS identity with throwaway keys only, without
persistent account/SSH changes or external remotes.

Validation passed `just check` (681 unit tests, 294 integration tests and 12 doctests, formatting,
all-feature/all-target Clippy and docs.rs), warning-denying private Rustdoc, core-only compilation,
the disposable SSH example and Markdown lint. The final additional real-Git post-receive-hook
cancellation case passed, bringing integration coverage to 295 tests. After final fixture cleanup
and SSH option tightening, all 16 SSH unit tests, all 24 SSH integration tests and affected Clippy
passed again. Linux GNU and Windows GNU library Clippy cross-checks with `ssh` passed. A Windows
all-target cross-check was blocked by the existing `alloca` dev dependency requiring unavailable
`x86_64-w64-mingw32-gcc`; no Windows runtime evidence is claimed. The concrete follow-up is to run
all-target Clippy on the configured native Windows CI runner or provide that cross C compiler.

### Owned Network Fetch Handoff Completion

- HTTP/SSH receive entry points take `Option<Arc<KnownHistory>>`. Downloads privately own the exact
  negotiation history, have no history lifetime parameter, and require no allocated history for
  `None`. Stream and local-process signatures stay unchanged.
- Actual Git full, incremental and known-only downloads validate in `Send + 'static` blocking
  workers. Weak observers prove that downloads retain the initiating owner's history after it is
  dropped and release it after success, cancellation, decode-limit failure or download disposal.
- Known-only and incremental results still reject installation into a repository missing local
  dependencies before publishing artifacts; installation into the original destination succeeds.
- HTTP/SSH examples show caller-owned bounded admission, joined validation and explicit
  installation. The current compiler probes require direct owned-history and no-history handoffs to
  compile; the scheduling report labels earlier rejected probes as historical evidence.
- Transport deadlines, resource limits, validation cancellation and installation semantics remain
  covered by the existing full suites. No library runtime, worker pool or storage framework is
  added.

Ownership revision validation passed on macOS arm64: `just check` (681 unit tests, 301 integration
tests, 12 doctests, all-target/all-feature Clippy and docs.rs), both disposable transport examples,
the current compiler probes, core-only compilation, warning-denying all-feature private Rustdoc, and
Markdown checks. No new scheduling timings were collected and no Linux/Windows runtime validation
was performed for this revision.

### Repository Discovery and Initialization Completion

- [x] Discovery selects the nearest candidate in canonical ancestor order, with an inclusive
      ceiling; ordinary and bare SHA-1 initialization create an unborn `main` branch.
      Reinitialization is refused.
- [x] Focused unit tests cover ceilings, starts, symlink ancestry, malformed markers, exclusive
      writes, concurrent creation, partial population failure, and preservation of unrelated files.
- [x] `examples/init_repository.rs` demonstrates disposable creation, nested discovery, and blob
      storage.
- [x] Git uses generated repositories for object access, commits, reference updates, and strict
      fsck; girt discovers Git-generated ordinary, bare, separate, and linked layouts.
- [x] At this historical checkpoint, unsupported SHA-256, reftable and worktree config failed
      without mutation or fallback to an outer repository. Partial initialization remains available
      for inspection.
- [x] No benchmark is required: this increment adds small metadata setup and parent traversal
      without a performance claim or a change to an existing processing hot path.
- [x] Public contracts, the README, and compatibility evidence describe the supported boundaries.
- [x] Local checks pass on macOS arm64 with Git 2.55.0 and Rust 1.98.1: `just check`, the executable
      example, private-item Rustdoc, and Markdown lint. Linux, Windows, and actual mount-point
      crossing remain untested for this increment.

### Reference Enumeration and Conditional Deletion Completion

- [x] `list` and `list_namespace` return ordered stored values with loose-over-packed precedence,
      current-worktree routing and explicit live-read semantics. HEAD is read separately; selected
      namespaces use exact-or-descendant matching rather than arbitrary byte prefixes.
- [x] Local tests cover filtering, malformed data, byte names, conflicts, ignored lock/dot entries,
      symbolic shadowing, packed metadata retention, expected-value mismatch and absence,
      cycle/depth failures, lock ownership, failed packed publication and partial unlink failure.
- [x] `publish_branch` and the namespace doctest show enumeration followed by conditional deletion;
      resolved deletion preserves symbolic HEAD and leaves the branch unborn.
- [x] Git-created branches and annotated tags exercise both packed-only and loose-over-packed
      deletion. Git verifies absence, surviving tag peeling, symbolic names, and later
      updates/repacking. Linked worktrees cover shared and all three private namespaces; a gitfile
      covers separate metadata.
- [x] Conditional-writer and packed-writer races exercise Git coordination. An independent
      retained-lock experiment checks Git packing across packed-file replacement. Precondition
      failures preserve reference contents; later unlink errors explicitly report partial packed
      deletion.
- [x] The existing Criterion reference harness includes packed enumeration/deletion at 10 and 10,000
      tags and traversal of 1,000 loose tags, each alongside one loose branch. Source fingerprints
      and estimates are retained in the
      [enumeration/deletion baseline](benchmarks.md#reference-enumeration-and-deletion).
- [x] Public contracts and
      [compatibility evidence](compatibility.md#reference-enumeration-and-deletion) describe the
      no-reflog, no-snapshot, partial-I/O-failure and best-effort cleanup boundaries.
- [x] `just check` passes on macOS arm64 with Rust 1.98.1 and Git 2.55.0: 744 unit tests, 332
      integration tests and 14 doctests, plus all-target/all-feature Clippy and docs.rs with
      warnings rejected. Reference coverage includes 96 unit tests and 42 integration tests. No new
      platform support is claimed; these deletion/enumeration tests have not been executed on Linux
      or Windows.

The implementation uses no Git source, upstream tests, or copyright-audit material. Packed-first
ordering follows the independently reasoned storage invariant and the maintainer's chosen contract.
No batch transactions, reflog policy, remote/refspec policy, crash recovery, or new dependencies are
included. Benchmark results describe warm local storage, not reader isolation or crash durability.

### Reference Transactions and Reflogs Completion

- [x] Conditional batches validate every stored chain/precondition before publication, retain sorted
      ref/log locks, and distinguish preparation failure from per-ref/per-log publication outcomes.
      Packed removals precede loose deletion without rollback.
- [x] Tests cover overlapping names/chains, namespace conflicts, symbolic cycles, malformed logs,
      explicit preservation, invalid messages, lock ownership/cleanup, whole-batch mismatch, and
      retained deletion history. Controlled failures cover packed replacement, loose unlink and
      replacement, first/later log appends, and byte progress after short writes.
- [x] The disposable `reference_transaction` example publishes HEAD/branch and tag records, handles
      failure reports, and conditionally deletes the tag while retaining its history.
- [x] Independent Git fixtures verify exact identities/times/messages and old/new IDs, subsequent
      Git appends and selectors, empty-message records, packed/shadowed deletion, detached/symbolic
      HEAD, separate metadata and linked-worktree/private log routing. A Git conditional writer
      races a two-ref transaction.
- [x] Criterion measures publication at 1/32 refs and parsing at 10/10,000 records. Estimates and
      measured source fingerprints are retained in the
      [transaction baseline](benchmarks.md#reference-transactions-and-reflogs).
- [x] API contracts and [scope/evidence](compatibility.md#reference-transactions-and-reflogs) state
      sequential visibility, partial reflog writes, explicit creation/deletion policy, live reads,
      whole-log allocation and external-writer boundaries.

Validation on macOS arm64 with Rust 1.98.1 and Git 2.55.0 passed `just check`: 776 unit tests, 339
integration tests and 14 doctests, plus all-target/all-feature Clippy and docs.rs. The runnable
workflow, private Rustdoc with warnings rejected, rendered transaction documentation review, and
Markdown checks also passed. This increment adds no dependencies or platform support. Runtime
validation on Linux/Windows, reflog removal/expiry, streaming logs, recovery journals and
filesystem-wide atomicity are deferred.

### Remote Configuration and Refspec Mapping Completion

- [x] Named remote interpretation preserves byte values, occurrence order, URL resets and pushURL
      fallback. Missing remotes/keys and malformed values have explicit outcomes. Only four keys are
      interpreted; global sources, rewriting and implicit orchestration policies are deferred.
- [x] Seventy-seven focused named unit cases exercise direction-specific syntax, unsupported forms,
      wildcard captures, exclusions, force intent, deletion, ordering, duplicates, collisions,
      missing sources, byte names, zero IDs and advertisement hints. Mapping returns no partial
      plan, reads no objects and performs no I/O; storage races and recovery are inapplicable.
- [x] Twenty-seven independent Git CLI integration cases verify configuration and actual local
      fetch/push mappings in disposable repositories, including annotated tags and symbolic HEAD. No
      upstream implementation/test source or copyright-audit material is used.
- [x] `remote_plan` demonstrates configuration snapshots, fetch callback selection/error retention
      and explicit-policy push preparation without sending a transfer.
- [x] Criterion measures two wildcard mappings and two exclusions at 10 and 10,000 sources, with
      source fingerprints and median intervals retained in the
      [mapping baseline](benchmarks.md#refspec-mapping). No hard resource or performance gate is
      claimed; input sizes remain caller-bounded.
- [x] [Compatibility contracts](compatibility.md#remote-configuration-and-refspec-mapping)
      distinguish supported syntax, syntactic force from authorization, deletion planning from
      transport support, duplicate conflict policy and deferred consumer policy.

On macOS arm64 with Rust 1.98.1 and Git 2.55.0, `just check` passed: 853 unit tests, 366 integration
cases and 14 doctests, all-target/all-feature Clippy and docs.rs with warnings rejected. The
runnable example, private-item Rustdoc, rumdl and markdownlint-cli2 also passed. Generated
HTML/navigation anchors were inspected; browser security policy blocked local-file rendering, so
visual Rustdoc review remains unperformed. No dependency, platform-support, transfer, or
reference-storage changes are included. Linux and Windows runtime evidence for this increment
remains uncollected.

### Fetch Orchestration Completion

Acceptance for this increment is remote-tracking/tag publication through explicit local/HTTP/SSH
endpoints, preserving transfer/installation/transaction failure boundaries:

- Supported namespace/object/force rules are documented in
  [fetch orchestration](compatibility.md#fetch-orchestration). Local branch destinations and
  `FETCH_HEAD` are explicitly deferred; protected HEAD chains include linked worktrees.
- Focused workflow/update unit tests cover mapping/exclusion, source-only/no-op decisions, symbolic
  and unsupported destination rejection, changed advertisements, and independent force intent and
  authorization requirements.
- `examples/fetch_remote.rs` demonstrates named configuration, explicit endpoint policy, validated
  transfer, installation, conditional publication, reflog identity and failure reports.
- Independent Git CLI tests compare initial/incremental multi-ref fetches, exclusions, tags and
  object-kind/non-fast-forward decisions, including the observed stricter remote-tracking behavior
  than the manual describes.
- Failure tests cover destination races, cancellation, malformed SSH input, installation failure,
  residual unindexed packs, lost known dependencies, corrupt loose shadows,
  verification/ancestry/peeling limits, publication locks and worktree aliases. Existing reference
  transaction tests cover exact partial reference/log effects retained by the workflow.
- HTTP and SSH loopback fixtures exercise owned download handoff, synchronous validation and shared
  publication. No synchronous storage work is introduced inside network polling.
- `benches/fetch_workflow.rs` measures mapping/planning at 10 and 10,000 refs and validated
  installation plus ten-ref publication, with setup and transfer outside the measured finish
  operation. Existing history traversal and object-transfer benchmarks remain the underlying
  processing baselines.

Validation for this increment on 2026-09-24, macOS arm64, rustc 1.98.1 and Git 2.55.0:
`cargo test --all-features` passed 1,279 tests, including 23 workflow integration tests and the
HTTP/SSH loopback cases. All-target/all-feature Clippy, docs.rs and private Rustdoc builds with
warnings denied, `cargo check --no-default-features`, and `cargo run --example fetch_remote` passed.
Nightly formatting, rumdl and markdownlint-cli2 (100-column configuration) passed for changed prose.
The [Criterion baseline](benchmarks.md#fetch-orchestration-baseline) retains estimates and source
fingerprints. Linux runtime and Windows portability were not rerun for this increment; the workflow
unit/integration tests needing reference storage are Unix-gated.

### Clone Orchestration Completion

Acceptance is a usable full clone into a new bare or ordinary no-checkout repository, with explicit
transport, branch choice, persistent remote metadata, budgets and observable partial state:

- Focused unit tests cover default/selected/unborn/detached planning, inconsistent symbolic hints,
  missing/duplicate refs, actual-advertisement mapping, config quoting and conflicting writers.
- Original Git CLI integration tests cover both layouts, all branches/tags, strict fsck/object
  observations, explicit selection, empty remotes, detached/missing HEAD and a moved remote tip.
  Ordinary clone has the same absent-index status as `git clone --no-checkout --no-local`.
- Both Git and girt consume stored origin refspecs in subsequent fetches; local branch refs remain
  protected by the unchanged fetch destination/worktree rules.
- Failure tests cover preexisting files/directories/symlinks, reservation races, absent parents,
  cancellation before and after initialization, dropped downloads, invalid branch objects,
  verification limits, unindexed residual packs, config and reference locks, and config conflicts.
  Nested fetch/transaction reports preserve the existing exact partial-publication contract.
- Real loopback HTTP and SSH downloads move to owned workers for validation and finish, with
  separate validation-cancellation cases proving destination absence.
- `examples/clone_repository.rs` demonstrates explicit policy, download-before-initialization,
  failure reports, persisted origin and the no-checkout boundary in disposable repositories.
- The new Criterion planning harness measures 10 and 10,000 branches plus HEAD. Existing fetch,
  mapping, transaction and transport baselines cover the reused processing paths; initialization and
  minimal config writing add bounded metadata steps, without a throughput claim.

See [clone compatibility](compatibility.md#clone-without-checkout) for scope, layout differences,
selection policy, publication order and residual-state contracts. The
[planning baseline](benchmarks.md#clone-planning-baseline) retains Criterion estimates and source
fingerprints.

On 2026-09-24, macOS arm64 with Rust 1.98.1 and Git 2.55.0, `just check` passed 907 unit tests, 410
integration cases and 14 doctests, plus all-target/all-feature Clippy and docs.rs. Three final
integration regressions and the new lifecycle doctest passed in focused reruns, bringing the current
suite to 413 integration cases and 15 doctests. Final Clippy and docs.rs builds also passed.
Core-only compilation, warning-denying all-feature private Rustdoc, the runnable clone example,
nightly formatting, rumdl, Markdown lint and the planning benchmark passed. Rendered public Rustdoc
and its example/navigation were inspected through a temporary loopback preview. Linux/Windows
runtime evidence remains uncollected; finish retains the existing Unix reference backend
requirement.

### Clone Reflog Composition Remediation

The architecture checkpoint demonstrated that detached clone with `Reflog::Append` failed after
fetch/config publication: the reference primitive could not derive a log identity from the
initializer's symbolic HEAD. The transaction now locks and rechecks that old chain for a stored
direct replacement, logs its resolved old ID (zero when unborn), and publishes/logs only the named
ref. Exact stored-value preconditions remain in force. Symbolic new targets and stored symbolic
deletion still require preserved logs; clone's symbolic branch/unborn HEAD remains unlogged while
its created direct refs honor the caller's policy.

- Twelve public clone cases cover branch/detached/unborn selection with Preserve/Append in both bare
  and ordinary layouts. They assert exact HEAD/log bytes, reported log effects, no index and Git
  fsck.
- Transaction unit tests cover born/unborn and multi-hop packed old identities, stored-value
  mismatch, dependency/log lock failure, concurrent creation refused by an owned dependency lock,
  cycle/batch overlap rejection, preservation of the old branch's opaque log, and published-HEAD
  outcomes when its subsequent append fails.
- An independent Git CLI comparison checks exact symbolic-to-direct HEAD log bytes and preservation
  of the former branch. A clone failure test retains completed fetch/config while an old-branch lock
  prevents detachment and preserves the initial symbolic HEAD.
- Initialization now owns the exact initial config bytes and branch identity used by clone's
  preconditions. `CloneRequest::prepare_tracking` makes the reference-layout choice explicit;
  `InitKind` still selects only physical placement. Serving/enumerating local branches sees at most
  the selected branch in both layouts, and conventional all-local-branch bare copies remain outside
  scope. The unreleased constructor was renamed and all in-repository callers were updated.

`just check` passed 919 unit tests, 426 integration cases and 15 doctests, plus
all-target/all-feature Clippy and docs.rs on macOS arm64 with Rust 1.98.1 and Git 2.55.0. The clone
example also passed. Core-only compilation, warning-denying private Rustdoc, final all-target
Clippy, nightly formatting, rumdl and Markdown lint passed. The existing reference harness now
includes unborn detachment with append logging;
[retained measurements](benchmarks.md#symbolic-head-detachment-with-reflog) record that path and
direct-ref publication. Native Linux/Windows evidence is still outstanding; no platform support or
fetch safeguards were changed.

### Tree Comparison Completion

Acceptance for this slice is recursive leaf comparison with exact byte paths, old/new IDs and modes,
deterministic full-path ordering, explicit identity skips, bounded resources and contextual
failures. The [compatibility contract](compatibility.md#tree-comparison) states supported behavior
and exclusions.

- [x] Focused parameterized tests cover unchanged, added, removed and content-changed entries,
      executable and symlink transitions, gitlinks, empty sides, nested byte paths, empty
      directories and file/directory replacement across Git ordering positions.
- [x] Missing, wrong-kind, corrupt, malformed, duplicate, invalid-name and unsorted trees fail with
  context. Equal missing/malformed/wrong-kind roots and equal missing subtrees explicitly skip
  validation; leaf targets are not type-checked.
- [x] Exact and exhausted input/output bounds, cumulative bytes across both sides, cancellation
  before identity skips/reads and between reads, and a 2,000-directory traversal are tested.
- [x] Public integration tests compare structural results with Git in both directions and against
  empty trees, in loose and packed storage. The runnable `compare_trees` example shows old/new
  modes and hexadecimal IDs and escapes byte paths without losing non-UTF-8 names.
- [x] The Criterion [baseline](benchmarks.md#tree-comparison-baseline) measures sparse changes in a
      10,000-leaf tree, broad 10,000-leaf changes and a 512-directory changed chain. Setup is
      outside timing, filesystem caches are warm, and there is no arbitrary performance gate.

Storage and comparison are synchronous. Pure in-memory name alignment is separate from verified
object reads. Output is eager and bounded; identical IDs omit validation work. These contracts are
public Rustdoc and are intended to let later content diff consume leaf identities without adding
content diff now. Partial writes and cleanup testing are inapplicable because comparison is
read-only. No dependency or CI changes are part of this slice.

On 2026-09-24, the full all-feature runtime suite passed 960 unit tests, 428 integration cases and
16 doctests on macOS arm64 with Rust 1.98.1 and Git 2.55.0. After updating the Git-output test
parser to satisfy Clippy's fixed-size chunk guidance, both interoperability cases passed again;
all-feature/all-target Clippy and docs.rs passed with warnings denied. Nightly formatting, rumdl,
Markdown lint using the global 100-column config, core-only compilation, warning-denying private
Rustdoc and the runnable example passed. The rendered comparison contract and navigation were
inspected in a temporary loopback preview. Native Linux/Windows execution remains pending in the
separate platform refresh; no cross-platform runtime claim is made here.

### Content Diff Completion

Acceptance is exact byte comparison, explicit binary handling and deterministic shortest LF-line
edits, composing with tree changes through separately bounded verified blob reads. The
[content diff contract](compatibility.md#content-diff) owns limits, provenance and exclusions.

- [x] Named unit cases cover addition, deletion, insertion, replacement, separated changes, repeated
      and blank lines, CRLF, bare CR, NUL, invalid UTF-8 and missing final LF. Exact ranges
      distinguish bytes from lines; empty inputs have no lines.
- [x] All 3,969 pairs of binary-alphabet sequences through five lines reconstruct the destination
  and preserve unchanged gaps. A separate dynamic-programming oracle checks minimum edit length;
  80 deterministic varied-byte pairs exercise CR, LF, NUL and invalid UTF-8 combinations.
- [x] Tests cover exact/exceeded input, line, trace and work bounds, cancellation checkpoints,
  checked budget arithmetic, long-line byte charging, 100,000-line additions/small edits and
  bounded failure for large unrelated inputs. Errors return no partial script.
- [x] Blob adapter tests cover all accepted modes, absent sides, equal missing IDs, missing new
  objects, wrong kinds, corrupt storage, per-read and cumulative byte limits, unsupported modes
  and cancellation. Storage is read-only; publication/cleanup tests are inapplicable.
- [x] Sixteen portable integration cases compare Git zero-context spans and exact bytes on simple
  inputs, reconstruction and minimum costs on ambiguous inputs, NUL binary handling, and
  tree-to-content composition through loose and packed Git-written blobs. Windows selection is
  explicit in the workflow; no unsupported reference backend or owned transport adapter is used.
- [x] The runnable `content_diff` example and doctest demonstrate borrowed byte ranges without lossy
  rendering. The Criterion [baseline](benchmarks.md#content-diff-baseline) retains small-edit,
  repeated-line, unrelated-input and bounded-rejection measurements without numerical gates.

On 2026-09-24, `just check` passed 1,025 unit tests, 455 integration cases and 17 doctests on macOS
arm64 with Rust 1.98.1 and Git 2.55.0, plus all-target/all-feature Clippy and docs.rs with warnings
denied. The core-only library check, runnable example, warning-denying all-feature private Rustdoc
build, nightly formatting, rumdl and Markdown lint with the global 100-column configuration passed.
Rendered module navigation, the diff contract and its example were inspected in a local preview. The
benchmark harness ran after the checks without competing task builds/tests and retains source
fingerprints and confidence intervals. Native Linux/Windows runs for this increment remain pending;
existing platform evidence applies only to earlier revisions.

### Working-Tree Index Completion

Acceptance is bounded SHA-1 v2 parsing/encoding and a synchronous held-lock replacement lifecycle,
with byte paths, exact stat words, modes, assume-valid and unresolved stages. The
[index contract](compatibility.md#working-tree-index) records unsupported features, extension
policy, resource semantics and filesystem assumptions.

- [x] Eighty-four local cases cover empty indexes, canonical modes, byte/long paths, stat and flag
      preservation, malformed framing/checksum/order/stages/prefixes, version and extension
      boundaries, exact/exhausted budgets, and original-state preservation after failed edits.
- [x] Seventeen original Git integration cases exercise both directions, independent stat/flag
      observations, byte paths without materialization, conflict stages, tree-object compatibility,
      `TREE` invalidation, `REUC` preservation, unsupported features and separate/linked-gitdir
      routing.
- [x] The held-lock lifecycle distinguishes missing/empty storage, excludes cooperating writers,
  detects observed noncooperating creation/change/deletion, and retains the original on injected
  encoding, descriptor-write and rename failures. Existing foreign locks remain untouched.
- [x] A historical-timestamp regression verifies that publication does not hide equal-size content
  edits whose mtime is restored when Git's ctime checks are disabled. Published index timestamps
  are conservatively old and nonzero; cached stat words remain exact.
- [x] `examples/index.rs` demonstrates explicit object storage, locked entry construction,
  publication and readback without creating a working file. Public Rustdoc documents pure and
  storage contracts; rendered navigation, lifecycle text and scraped examples were inspected.
- [x] The [Criterion baseline](benchmarks.md#working-tree-index-baseline) retains parse/encode
  measurements for 0 through 100,000 entries with source fingerprints and confidence intervals.
- [x] Windows portable integration selection includes `index`; no reference backend or Unix-only
  fixture is needed by that suite. A Unix symlink-rejection unit case is separately gated.

On 2026-09-24, revision `81aa6bdc40cb80c6d2ada95e9d3778c6644fe964` passed `just check`: 1,109 units,
472 integration cases and 18 doctests, plus all-feature/all-target Clippy and docs.rs. Core-only
compilation, the runnable example, warning-denying all-feature private Rustdoc, nightly formatting,
rumdl and Markdown lint passed. After strengthening the historical-timestamp fixture to avoid
wall-clock timing dependence, that regression and all-target Clippy passed at
`8d1a2c2481ea4db1aa414d42e57763d81d5c8826`; implementation code was unchanged. Host: macOS arm64,
Rust/Cargo 1.98.1 and Git 2.55.0. Native Linux/Windows execution for this increment remains pending.
No crash-durability or broader platform claim is made. The content-diff parent
`d3b47737320afbd9c2224384cea68c7c2ff5cbd5` is preserved unchanged.

### Raw Working-Tree Status Completion

Acceptance is a useful read-only HEAD/tree-to-index and index-to-working-file workflow, with raw
normalization and untracked policy visible in the API and results. The
[status contract](compatibility.md#raw-working-tree-status) owns platform and snapshot limitations.

- [x] Parameterized local cases cover raw content, assume-valid verification, file/symlink byte
      bounds, mode changes, equal-size edits with restored mtime, invalid paths, case aliases,
      obstructions and metadata boundaries.
- [x] Deterministic checkpoints exercise cancellation and concurrent file, directory, ancestor, HEAD
      and index observations. Results fail without partial reports; snapshots verify no file
      content/mtime writes or index lock creation.
- [x] Independent Git CLI fixtures cover staged/unstaged combinations, conflicts, unborn/detached
      HEAD, missing index, file/directory and symlink changes, loose/packed blobs and
      linked/separate worktree routing. Raw EOL/ignore/assume-valid differences are asserted
      deliberately.
- [x] Missing, wrong-kind and corrupt objects, malformed index, unsafe byte paths, exhausted
      index/tree/object/worktree bounds and unchecked gitlinks have explicit failure/result
      evidence.
- [x] `examples/status.rs` demonstrates the public workflow in a disposable repository or a supplied
  existing checkout. Rustdoc states raw normalization, ignores, stat verification and non-atomic
  snapshot limits, including the boundary against using observations to authorize checkout.
- [x] `benches/status.rs` measures clean and ten-percent-changed 100/1,000-file working trees with
      warm storage, complete raw calls and result destruction. The
      [baseline](benchmarks.md#raw-working-tree-status-baseline) retains source fingerprints and
      confidence intervals; no numerical gate is imposed.
- [x] `status_portable` is explicitly selected for Windows cancellation/unsupported-platform
  behavior. Actual traversal and native symlink tests are macOS/Linux-only; Linux byte filenames
  and macOS non-ASCII rejection are independently gated.

On 2026-09-24, revision `9affad69fac53f2c022f47f46b40f8ee7cc588af` passed `just check`: 1,146 unit
tests, 507 integration cases and 19 doctests, plus all-feature/all-target Clippy and docs.rs with
warnings denied. The status-specific evidence comprises 37 local cases, 34 Git/workflow cases and
one portable cancellation case on macOS arm64, Rust/Cargo 1.98.1 and Git 2.55.0. The runnable
example, warning-denying private Rustdoc, rendered navigation/contracts/examples, nightly
formatting, rumdl, global-config Markdown lint and benchmark fingerprint verification passed.

Core-only Linux and Windows libraries compile. Linux core test targets also compile using the
installed Zig C compiler with `CRATE_CC_NO_DEFAULTS=1` and
`CC_x86_64_unknown_linux_gnu='zig cc -target x86_64-linux-gnu'`; this resolves the existing
Criterion C dependency's missing cross-compiler without a repository change. Linux core-library
Clippy passed with warnings denied. Native Linux/Windows execution remains pending; compilation is
not runtime evidence. The index parent `beb052d387c74968ad4199014e51dcfed67b2c28` and content-diff
ancestor `d3b47737320afbd9c2224384cea68c7c2ff5cbd5` remain unchanged. Subsequent changes only record
this validation evidence.

### Raw Tree Checkout Completion

Acceptance is a usable explicit-baseline tree checkout with no HEAD/ref switching: materialize a
no-checkout clone or replace clean tracked content, publish the matching index, and preserve user
content on refusal or partial failure. The [checkout contract](compatibility.md#raw-tree-checkout)
owns platform, normalization, concurrency and recovery limits.

- [x] Focused local cases cover raw bytes, executable/symlink modes, additions/deletions and
      file/directory transitions, staged/unstaged/conflicted state, obstructions, nested
      repositories, gitlinks, unsafe/colliding paths, corrupt/missing/wrong-kind objects and
      exhausted budgets.
- [x] Deterministic checkpoints cover changes after planning and at mutation boundaries, ancestor
      directory/symlink/root replacement, destination substitution, cancellation before/during work,
      write/install/delete faults, publication preconditions and exact completed operations.
- [x] Temporary and index-lock cleanup failures are observable; replaced artifacts are preserved.
      Post-mutation failure still reports completed operations. Index units inject write and rename
      failures and check preservation of original index bytes.
- [x] Independent Git CLI fixtures verify no-checkout clone through both girt and Git, subsequent
      Git use, raw bytes, modes, `ls-files`, refreshed `diff-files`, `write-tree`, status and
      `fsck`. HEAD remains unchanged; linked/separate worktrees route metadata correctly. Raw
      attributes/EOL behavior is compared deliberately against Git checkout conversion.
- [x] The disposable `examples/checkout.rs` demonstrates initialization, explicit baseline
      selection, mutation, publication, partial reports and owner-controlled cleanup. Public Rustdoc
      explains lifecycle stages, caller exclusion, deferred filters and recovery.
- [x] Windows explicitly selects `checkout_portable` for cancellation and unsupported-platform
      evidence. Actual checkout and native symlink cases remain macOS/Linux-only; Linux byte names
      and macOS non-ASCII rejection are separately gated.
- [x] The [Criterion workload](benchmarks.md#raw-tree-checkout-baseline) measures complete initial
      and tracked-update operations for 10/100 root files with 1-KiB blobs, retaining source
      fingerprints and confidence intervals without a numerical gate.

On 2026-09-24, revision `0429350cc1a64b665a18044c507fd8b640244e52` passed `just check`: 1,212 unit
tests, 515 integration cases and 19 doctests, plus all-feature/all-target Clippy and docs.rs with
warnings denied. Checkout-specific coverage comprises 65 local cases, seven Git/workflow cases and
one portable cancellation case on macOS arm64, Rust/Cargo 1.98.1 and Git 2.55.0. The runnable
example, warning-denying private Rustdoc and rendered public navigation/contracts/scraped example
were checked. The subsequent lock-identity acquisition error refinement at
`cb230d2a0f2b9d2141b4578319e4b481639fdcaf` passed all 65 checkout units, all 16 index-storage units,
all-target Clippy and docs.rs again. No new dependency was added.

Core-only Linux and Windows libraries compile. Native execution of this checkout increment on
Linux/Windows remains pending; compilation is not runtime evidence. Windows checkout is explicitly
unsupported. The status parent `e5380d364a91d437a1190a058501e58073b2109f` and index ancestor
`beb052d387c74968ad4199014e51dcfed67b2c28` remain unchanged. No changes were published or merged.

#### Checkout Closing Review Remediation

The closing review reproduced a valid Git tree whose ordinary `d/HEAD`, `d/objects` and `d/refs`
files activate checkout's conservative nested-repository guard after previous operations.
Preparation now projects marker presence through the operation sequence, including retained
untracked/tracked siblings, directories surviving child deletion, created directories and ASCII
aliases. The live mutation guards are unchanged. Unsupported plans fail in preparation with no
applied operations, unchanged worktree/index bytes and a released owned lock. This intentionally
retains the restricted marker policy rather than claiming Git-default support for those trees.

Twenty-four additional local cases cover initial/tracked refusal, final-verification-only targets,
file/directory transitions, mixed retained/target markers, symlink markers, root exemption, a valid
remove-before-add transition, a hypothetical unsafe intermediate ordering, and live marker insertion
after preparation. Ordered prefix lookup replaces the all-target scan for each absent expected path;
component-boundary cases and a 96-directory deletion workload verify behavior. A deterministic
checkpoint proves cancellation during that final expected-path pass leaves the prior index and
reports the completed removals. This is bounded structural evidence, not a measured speedup claim.

On 2026-09-24, remediation revision `63755c63bcb32898bd5f49f484f3e27c00d60560` passed `just check`:
1,236 unit tests, 515 integration cases, 19 doctests, all-feature/all-target Clippy and docs.rs,
with warnings denied. Checkout now has 89 focused local cases. The original external public-API
reproduction was rerun unchanged: the marker target fails during preparation with an empty applied
report, `existing` and its index entry survive, and Git still accepts the same target. Its
independent clone/checkout/edit/refusal scenario also passes. Warning-denying private Rustdoc,
global-config Markdown lint and core-only Linux/Windows library cross-compilation passed. Native
platform CI for this remediation remains a separate pending checkpoint; Windows checkout remains
unsupported.
