# Testing and Completion Criteria

A change is complete when its issue's acceptance criteria hold and the evidence below backs them.
Select tests by the promises and risks involved; one test can satisfy several criteria without
duplicating assertions across suites.

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
- **Documentation:** Update affected contracts, examples, compatibility evidence, and limitations.
  Record fixture provenance and the Git versions and platforms exercised.
- **Validation:** Run the applicable [development checks](../CONTRIBUTING.md#development-checks).
  Fix failures caused by the change and report checks that could not run.

Keep integration tests under `tests/` and original fixtures under `tests/fixtures/` when files are
needed. Generate fixtures independently and isolate filesystem tests from the working checkout and
global configuration. Follow [Rust Conventions](rust-conventions.md#unit-tests) and the
[Rustdoc Standard](rustdoc.md#examples-and-validation) for local tests and executable examples.

A capability may be complete within an explicitly limited scope. Validation on one platform does not
establish support on other platforms, recognizing a format does not establish that it is
implemented, and no finite corpus proves full parity with Git. State these limits in the pull
request.

## Fault and Race Tests

Use deterministic barriers or checkpoints for races and faults before and after publication rather
than sleeps. When a transport fault test requires a post-send outcome, synchronize cancellation with
an observed request before asserting uncertainty. Keep fixture watchdogs separate from the transport
deadline being tested: slow discovery can legitimately expire an overall deadline before mutation.
Assert the specific structured error and retained effects, and include request observations in
failure diagnostics. See [R14's HTTP repair](evidence/r14-http.md) for a controlled example.

## Process Fixtures

Tests often need their own programs, such as a stand-in SSH executable, an askpass helper or a Git
hook. Do not write a new executable file for each test. macOS runs a policy check on the first
launch of every new executable. The checks run one at a time and each takes seconds while builds
create new binaries, so per-test executables spend transport deadlines waiting and fail only on busy
machines. Link the program path to the checked-in `tests/fixtures/stand-in` launcher with
`tests/support/stand_in.rs`, and pass the program text as data.

A fixture that outlives a call, such as a loopback server, must stop when its test stops. That
includes a test killed by a signal or a runner timeout, when no `Drop` runs. Pass the test's process
ID to the fixture and check direct-parent identity on POSIX; a zombie owner still has a PID. Ignore
further termination signals once cleanup has started, so a second signal cannot abandon it halfway.
Freeze and rescan descendants before killing them, including separate sessions, and retain the
leader PID until cleanup finishes. `fixture_process::Process` also kills the fixture's process group
even after the leader exits on its own. The cleanup regressions exercise late forks and owner exit
with delayed reaping on Linux and macOS; Windows retains its normal process teardown.

## Performance Evidence

Performance is a primary goal alongside Git fidelity. Small fixtures hid the clone, fetch and GC
costs that the large-repository benchmark exposed, so changes touching fetch, object reading,
packing or retention must also run the
[large-repository benchmark](jj-replacement-status.md#large-repository-benchmark) and report the
result against the previous one.

Add a reproducible Criterion benchmark when introducing a significant processing path, changing a
known hot path, or making a performance claim. Hashing, compression, pack decoding, and graph
traversal merit representative baselines. A domain-type change or documentation correction usually
does not require a new benchmark; use existing benchmarks when performance could be affected.

Keep benchmark harnesses under `benches/`. Choose representative input sizes and content, keep
fixture setup outside the measured operation, and name what is measured. Separate operations with
different costs, such as publishing a new object and validating an existing one. For filesystem
measurements, record cache conditions and avoid presenting cached reads as cold-storage performance.
Record the command, toolchain, platform, workload, and revision needed to repeat a result.

Use measurements to identify sustained regressions and guide design. Establish numerical acceptance
thresholds only when a consumer requirement or measured budget justifies them; noisy timing changes
should not automatically fail CI. Throughput measurements do not prove memory limits or bounded
resource use: validate those contracts separately. No numerical code-coverage threshold is required.

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
  boundaries. Its transfer case remains SHA-1-only; separate R27/R29 suites cover both-format wire
  transfer. Retained provider-capture comparisons need no installed key or signing provider.
- `discovery`: Native local HEAD and both-format Git v0/v1/v2 advertisement observations, including
  unborn v2 `ls-refs`; no network transfer is claimed by this portable suite.
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
- `http`: Both-format fetch/push, TLS trust/hostname checks, authentication, limits, cancellation,
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
remain unsupported, with refusal-only tests; native implementation would require runtime validation.

HTTP fixtures use disposable loopback servers, Python and Git's public CGI backend. TLS fixtures
also need OpenSSL. Separate R27/R29 transfer suites cover both wire formats; two-format storage
tests alone do not establish SHA-256 negotiation. R36's [evidence](evidence/r36.md) records its
exact native results and exclusions. A CI compile or explicit unsupported error does not close
runtime capability gaps.

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
