# Testing and Completion Criteria

A change is complete when its issue's acceptance criteria hold and the evidence below backs them.
Select tests by the promises and risks involved; one test can satisfy several criteria without
duplicating assertions across suites.

## Specify Before Implementing

Write the specification before any implementation starts when a change deletes, retires, replaces or
re-encodes stored data (objects, packs, references, reflogs, the index), adds a fast path that
reuses stored bytes or skips a validation, or optimizes a path measured by the
[large-repository benchmark](jj-replacement-status.md#large-repository-benchmark). On
[#23](https://github.com/joshka/girt/issues/23), nine independent repack implementations each failed
at least one integrity test, in eight predictable classes, and the design that avoided two of those
classes outright was found only by judges comparing finished attempts. A specification makes the
target a test suite rather than prose, so one implementation can get it right.

Skip the process for routine work: read-only operations, new parsers or writers covered by Git
fidelity tests, API additions, documentation, and bug fixes with a reproducing test go straight to
one implementation with the [required evidence](#required-evidence). A read-path optimization needs
only the inventory and the profile when it keeps every validation, observable contract and resource
account unchanged; one that changes what a planner validates or counts is in scope, because a later
deletion depends on its output. Record the steps below in the issue, in order, before implementation
begins.

1. **Failure-mode inventory.** Walk every class and state whether it applies and why. "Does not
   apply" needs a reason, such as "this phase deletes no objects".
   - *Last readable copy:* the change could remove the only copy that decodes to the right object
     ([rule](storage-safety.md#last-readable-copy)).
   - *Identity of reused bytes:* copied or reused bytes carry a claimed identity that nothing
     verified, such as a delta base chosen by ID from another pack.
   - *Resource and depth limits:* pieces valid in their own source combine past a reader limit
     ([rule](storage-safety.md#bound-combined-data)).
   - *Snapshot consistency:* the plan and the action observe different stores
     ([rule](storage-safety.md#one-pinned-snapshot)).
   - *Store precedence:* with alternates or duplicate copies, the copy taken differs from the one
     object lookup would return ([rule](storage-safety.md#lookup-copy)).
   - *Protected files:* `.keep` packs, recent packs and alternates are rewritten, duplicated or used
     as proof ([rule](storage-safety.md#protected-data)).
   - *Crash ordering:* a failure between publishing and retiring, or an unlink persisted before the
     sync that should precede it, leaves a repository that does not open cold
     ([rule](storage-safety.md#publish-verify-retire)).
   - *Budgets and statistics:* copied or skipped data escapes the caller's limits or the reported
     counts ([rule](storage-safety.md#fast-path-contracts)).
   - *Git fidelity:* Git cannot read the output, or the retained set differs from Git's equivalent
     operation.
   - *Concurrency with readers:* open readers, concurrent Git processes or a second maintenance run
     observe an intermediate state.
2. **Executable acceptance tests first.** Map each applicable class to executable evidence. Reuse an
   existing test when it already asserts the invariant, and write a new failing test only for
   behavior that is new or uncovered, named after the invariant. Use Git to generate the fixture and
   check the result where it can act as the oracle, including the comparison with Git's equivalent
   operation; keep focused unit or fault-model tests where it cannot, such as sync ordering or
   caller budgets. Commit the new tests before the design round; the mapping is the implementer's
   target and the reviewer's checklist.
3. **Profile before choosing a design.** For performance work, time each phase of the current path
   on the large-repository fixture, measured as [Performance Evidence](#performance-evidence)
   requires. State the budget relative to Git and list the validations the fast path must keep, such
   as decoding every retained object from the replacement before unlinking anything.
4. **Cheap design round.** Write two or three candidate designs, about a page each, and judge them
   against the inventory: for each applicable class, say whether the design makes the failure
   impossible by construction, detects it with a check, or leaves it open. Prefer a structural
   constraint that removes a whole class, such as reusing only same-pack, already-written bases,
   over a check added afterwards. Record the choice and its reasons in the issue.
5. **One implementation.** Implement the chosen design once, by default with Sol at medium effort.
   Review it by running the pre-written tests and with one cross-provider review that reads the
   inventory and tries to break each "impossible by construction" claim. Run parallel attempts only
   when the design round cannot choose between candidates.

### Reflog Configuration Acceptance

[Issue 91](https://github.com/joshka/girt/issues/91) records the failure inventory and design round.
`tests/reflog_expiry.rs` compares special values and Unix timestamps with Git's `expiry-date`
interpretation. `tests/retention.rs::configured_expiry_keeps_the_same_records_as_git` compares exact
Git-generated retained records through the public expiry operation, including partial matching
patterns, first match, duplicate settings and stash preservation. The layered case uses explicit
system/global inputs, an included file and repository configuration, comparing retained bytes with
Git. Both commits remain readable after expiry. Parser units cover unsupported dates, overflow,
duplicate rejection and matching work exhaustion. Existing retention suites cover isolated
publication, changed inputs, protected stores and retained-object integrity.

`RetentionPolicy::reflog_expiry` selects one explicit `ReflogExpiryPolicy`: keep all, historical
classified cutoffs, or a configured snapshot. `ReflogCutoffs` names the configured general and
unreachable rules. A matching pattern supplies both rules; an unset key disables its rule instead of
inheriting a global cutoff. Numeric second/minute/hour/day/week intervals, Unix timestamps, `never`,
exact `false`, `now` and `all` are supported. Calendar and natural-language expressions are rejected
before maintenance. Configured retention still uses its conservative repository-wide live roots
rather than Git's per-reference tip for unreachable classification; it can retain additional
history. This is a configuration increment, not full `git reflog expire` parity.

Existing explicit-cutoff callers must migrate both values to
`ReflogExpiryPolicy::ClassifiedCutoffs`, preserving any values formerly inherited through a struct
update. Configured callers select `Configured(ReflogExpiry)`. Modes cannot contradict each other;
`KeepAll` remains the default. No jj source change is made in this PR.

### Native Receive-Hook Acceptance

[Issue 92](https://github.com/joshka/girt/issues/92) records the Git observations, failure
inventory, tests-first revision and design choice. `tests/push.rs` exercises private pre-receive
visibility, rejection cleanup, update visibility after primary publication, cancellation
synchronized with a hook-start marker, post-receive cancellation with published-ref evidence,
combined output limits, explicit refusal of hook-written objects and preservation of a replacement
quarantine directory. Accepted SHA-1/SHA-256 transfers read primary alternates through paths
containing quotes, colons and newlines, then pass Git fsck. Histories come from original
Git-generated fixtures.

Process units exercise full stdin/stdout/stderr pipes and a descendant holding output open after its
leader exits. Fixtures are independently finite if cancellation regresses. Windows selects the
portable native-transfer suite, including refusal of configured hooks before mutation; supported
hook execution needs native macOS/Linux evidence. Existing installation, expected-value and
receive-policy tests remain applicable. Process-group-escaping hooks remain a separate limitation,
not an acceptance claim for this increment.

### Caller-Controlled Lock Cleanup Acceptance

[Issue 93](https://github.com/joshka/girt/issues/93) records the ownership inventory, tests-first
revision and design choice. `tests/lock_cleanup.rs` generates index/config storage with Git and
compares SIGINT cleanup after a clean-filter readiness marker under Git's index lock. It verifies
unchanged published bytes, foreign/replacement lock preservation, stale-guard refusal, idempotence
and Git readability while cleanup races index publication/drop. Fault tests cover
acquisition/registration and publication checkpoints, nested operations,
reference/reflog/packed-ref/reftable lock topologies, reentrant callback refusal, path-specific
inspection failures, shallow/credential stale-guard refusal and a Git-generated worktree repair
checkpoint between link replacements. The writer and identity pin share a single descriptor. A
Git-generated fixture checks cleanup cancellation of a pending packed-ref wait without stealing the
foreign lock.

`girt::locks::cleanup` runs in normal thread context. jj must request cancellation, stop admitting
work, wake writers, call cleanup, then join cancelled writers; raw signal handlers must delegate to
normal context. Cleanup waits for active lock operations and invalidates surviving Unix guards
before releasing locks. Unsupported platforms retain guard validity for ordinary Drop/abort.
Existing public error enums gain no variants. Unix uses pinned device/inode identities; other
platforms preserve locks and report unsupported removal. Caller exclusion of hostile path/parent
replacement remains required. Cleanup does not roll back completed publication, remove persistent
worktree locks or pack protections, or provide SIGKILL/crash cleanup.

### Native Local Validation Progress Acceptance

[Issue 94](https://github.com/joshka/girt/issues/94) records the failure inventory and design
choice. `tests/local_transfer_progress.rs` independently generates packed histories with Git,
compares received entry/delta counts with `git verify-pack -v`, and exercises both object formats.
Forced local Git transfers establish the receiving/resolving phases; girt reports actual validation
work rather than Git's text or notification cadence. Public repository callbacks cover fetch, no-op
fetch and push. Cancellation during decoding and resolution leaves primary objects and references
unpublished; observer panics unwind before incoming storage publication. Existing import tests cover
corruption, connectivity, thin bases and resource/depth failures without validation completion.

Native local observers reuse `ValidationProgress`, separate from construction progress and hook
messages. Completion marks validation, before installation or reference publication; later push
failure remains possible. No-pack paths emit no validation snapshots. Callbacks run synchronously
and must return promptly. jj connects `TransferCallbacks::validation_progress` to its receiving and
resolving UI; lower-level consumers select the explicit validation-observer methods. Existing public
enums gain no variants. Network transports retain their existing progress interfaces.

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

The Unix `retention_noop`, `retention_integrity`, `retention_survivors`, and
`retention_repack_contracts` suites exercise read-only protected-pack retirement, delta reuse,
damaged entries, mislabelled bases, protected-pack survivors, and logical byte limits with original
Git-verified fixtures. They use Unix pack-retirement APIs and run alongside `retention` in the
focused retention job.

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
SSH supervisors and listeners retain the test's session, with a separate process group. Independent
lifetime guards reserve their group identities and stay runnable during tree discovery. They watch
the original test lifetime, plus stdin EOF or the supervisor-held FIFO for forced commands, so
subreaper adoption of a stopped parent does not hide owner death. Git children stay in the forced
command's guarded group. Normal cleanup kills protected children before explicitly shutting down and
reaping its guard; it never kills its own group before returning Git's exit status.

Freeze and rescan the protected tree before terminating and continuing its supervisor. Bound both
discovery and its subprocesses, stop new children immediately, and retain the leader PID until
cleanup finishes. Never freeze an independent lifetime guard. Incomplete discovery reports an error
and leaves that guard armed for group cleanup rather than declaring success. Regression tests cover
supervisor death, stopped parents beneath a Linux subreaper, launch-registration signals, and
blocked discovery.

HTTP and CONNECT fixtures have no independent guard, so teardown never freezes them. Their
descendants stay in the leader's process group, and Drop kills that group directly. Their owner
watch keeps running until then, so an owner killed mid-teardown still ends them. Don't rely on the
kernel's SIGHUP and SIGCONT for orphaned, stopped groups: a same-session subreaper stops the group
from becoming orphaned, and a frozen fixture survived its owner on Linux CI. On Windows each fixture
runs in its own kill-on-close Job Object, joined while the leader is still suspended. Drop
terminates the job, and an abrupt owner exit closes its last handle. The
`abrupt_owner_exit_stops_every_fixture_process` test kills a stand-in owner and checks that every
fixture process exits within a bound.

The supported fixture topology ends at the processes that stay in the leader's group or parent-child
tree long enough for teardown to discover them. On POSIX, a program that double-forks into a new
session and loses its parent first escapes, and a process-tree snapshot cannot promise otherwise.
Before a fixture depends on such a daemon, add a reproduction and extend the contract in
`tests/support/fixture_process.rs`.

## Performance Evidence

Performance is a primary goal alongside Git fidelity. Small fixtures hid the clone, fetch and GC
costs that the large-repository benchmark exposed, so changes touching fetch, object reading,
packing or retention must also run the
[large-repository benchmark](jj-replacement-status.md#large-repository-benchmark) and report the
result against the previous one.

- **Measure against Git under the shared timing lock.** Build and copy inputs first, then hold
  `/tmp/girt-timing.lock` (`shlock`; macOS has no `flock`) for the whole block, run Git and each
  build in ABBA order with Git as the control (G A B B A G), and record load averages. Concurrent
  agent builds otherwise swamp the difference; treat results from a loaded machine as provisional.
- **Report peak RSS beside wall time, for girt and Git.** Use `/usr/bin/time -l` (macOS) or `-v`
  (Linux). Memory gaps to Git have been as large as time gaps.
- **Find where the time goes before optimizing.** Time each phase or profile first, as step 3 of
  [Specify Before Implementing](#specify-before-implementing) requires for scoped work; the obvious
  algorithm is often a small share of the total.
- **Don't repeat full walks or scans.** Compute a history walk or object scan once per snapshot and
  pass the result on; each full walk of a large repository costs seconds. A repeated walk inherited
  from the current code is the first thing to look for.
- **Compare equivalent work.** State when girt and Git finish in different states, such as an
  additive repack against one that also deletes the old packs.

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

Ordinary pull requests run the platform workflow on Ubuntu 26.04. Main pushes, nightly runs,
release-plz pull requests and `full-ci` pull requests also run macOS 14 and Windows 2022. See
[CI selection and the full-ci policy](../CONTRIBUTING.md#development-checks) for the coverage moved
after merge and when agents must request full native coverage.

The Unix jobs run the full all-feature suite. Windows runs all-feature library units and doctests,
an explicit core-only integration selection, and a separate HTTP-only runtime suite. All hosts
independently compile core-only, HTTP-only, and SSH-only libraries and run all-feature/all-target
Clippy. Unix also runs core-only units and selected both-format interoperability suites, plus
isolated HTTP and SSH runtime suites, with tracing disabled. Windows runs core-only units and its
portable selection without tracing. Compilation is not runtime evidence; SSH feature compilation on
Windows does not expose a Windows SSH adapter.

The Windows integration selection follows implemented operations, not just file portability:

- `short_ids`: Both-format prefix resolution, ambiguity, loose/pack/alternate lookup and automatic
  abbreviation parity against Git in original disposable repositories.
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
- `packs`, `history`, `commit_graph`, `tree_compare`, `content_diff`: Pack/index I/O, deltas, graph
  queries, structural tree comparison and byte-preserving content diff.
- `apply`, `apply_tree`, `apply_extended`: Bounded text parsing, exact content application and
  unpublished tree composition against isolated Git CLI fixtures. Both-format tree-ID comparisons
  and explicit additive object installation need no native checkout backend.
- `builders`: Both-format index-to-tree, nested path construction, commit payload signing and
  combined rename results checked against Git CLI in disposable repositories. No native checkout
  operations are required. The SSH signing-oracle cases run on Unix with an isolated temporary key;
  Windows exercises signature framing and payload preservation without invoking a signing provider.
- `repo`: Both-format convenience commit/reflog and refresh behavior, plus lazy history reads, use
  original Git-oracle fixtures. Raw switch cases run only on Linux/macOS.
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
- `multi_pack_index`: Portable isolated Git-generated v1/v2 indexes in both object formats,
  incremental chains, uncovered packs and stale/corrupt/disabled hint fallback.
- `repositories`: Opening, initialization, discovery, configuration and Git-written layouts.
- `layout_shallow`: Both-format relative/moved layouts, worktree inventory and shallow-depth
  observations. Filesystem symlink/permission cases are Unix-only; raw byte paths are Linux-only.
  Case-alias cases require a case-insensitive native volume and report absence when unavailable.
- `submodule_status`: Nested worktree status and absorbed-layout observations require macOS/Linux
  and run in the Unix integration suites. Windows receives explicit unsupported/cancellation checks
  through library-independent portable tests.
- `submodules`: Byte-oriented `.gitmodules` interpretation, lexical validation and isolated Git init
  observations. No native checkout or network transport is invoked.
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
- `push_deltas`: Both-format Git-generated edits, thin REF framing/repair, exact contents, zero/one
  output depth, internal bases, packed sources, search fallback and no-mutation failures run in the
  portable core suite on Windows. Unix all-feature runs also exercise configured HTTP and SSH pushes
  against Git; SSH uses a local client stand-in without an sshd.
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

### Explicit Maintenance Execution

`maintenance_run` is portable and selected by Windows interoperability CI. It covers explicit
selection, isolation and lock ownership, bounded graph inventory, partial results, automatic graph
thresholds, shallow/configuration skips and jj keep refs with a detached HEAD. Git verifies
generated graphs and object readability in SHA-1 and SHA-256 repositories.

### Additive Loose Packing

`pack_loose` runs in the Windows interoperability suite. It preserves newly packed loose copies,
then checks a later explicit prune, both object formats, unreachable objects, bounded selection,
input budgets, corruption refusal and kept/promisor duplicate exclusion. Git checks object counts
and fsck after publication.

The portable `incremental_repack_write` suite runs in Windows interoperability CI. Controlled
repository admission tests cover zero metadata/configuration budgets in both formats with unchanged
MIDX bytes and pack inventory, plus cancellation acquired with isolation before any publication.
Existing default-limit tests still verify additive publication and protected/promisor exclusions.

### Controlled Fetch Completion Admission

`fetch_finish_control` is portable and selected in Windows interoperability CI. It covers
pre-install metadata/configuration/reference limits, cancellation and linked-worktree refusal;
default full, dependency-bearing and shallow completions run in both object formats. Original Git
bundles exercise installation and conditional refs, and Git fsck verifies successful results. A
later admission failure checks that installed objects and published shallow metadata remain visible
and accurately reported before reference publication. Reference transactions retain their trusted
metadata contract; controlled files-transaction bounds remain separate work before prefetch
execution can be called bounded.

### Controlled Prune Admission

`prune_packed_open` and `repository_open_control` run in Windows interoperability CI. Refresh
metadata/configuration limits and cancellation refuse before loose deletion; default admission still
permits verified pruning in both object formats. Existing prune-packed integrity and publication
tests remain unchanged.

`prune_packed_prefix` covers deterministic object/byte prefixes, unchanged all-candidate refusal,
individual survivor refusal and draining a duplicate backlog across bounded passes in both formats.
It also runs in Windows interoperability CI. A unit checkpoint verifies the complete selected prefix
before the first unlink.

The Unix `midx_expire` suite also checks controlled repository admission: zero metadata and
configuration budgets preserve the original MIDX and packs in both formats, and cancellation set
while acquiring isolation reaches the opener before publication or deletion. Existing default-limit
round trips still compare expiry and storage counts with Git. Windows retains the typed platform
refusal for destructive expiry.
