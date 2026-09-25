# jj Git Acceptance Contracts

This is a requirements matrix and corpus plan, not a passing-test report. IDs link the
[roadmap](jj-roadmap.md) to concrete acceptance evidence. Capability completion evidence is linked
from the roadmap; uncompleted rows remain pending. The initial inventory follows jj
`be5f5ebdc200593d8f1be06f11e27b485a97093d`; paths below are relative to that jj checkout.
Requirements describe observed consumer boundaries without copying dependency implementation or
tests. Girt's [testing contract](testing.md) applies to every row.

## Corpus and Evidence Format

For each A-ID retain original fixture generators, named cases, expected observations, exact girt and
baseline jj revisions, Git/toolchain versions, OS/filesystem, commands, results, and limitations.
Link these from the completing roadmap row. Use `tests/` for executable girt consumer tests and
`tests/fixtures/` for retained original data; add evidence reports under `docs/evidence/` as work
lands. Do not copy upstream Git/gix/libgit2 test data or implementation. Keep fixture inputs free of
real credentials and personal content.

Run unchanged jj against disposable repositories to establish consumer behavior; never patch jj
before R35. Earlier acceptance exercises equivalent contracts through girt public APIs, not a
partial jj adapter. Git-generated objects and independent raw fixtures distinguish encoding from
interpretation and operation policy. A successful raw object write does not establish that Git
accepts every use of that object. Record the exact oracle operation and discrepancies.

Every applicable row covers SHA-1 and SHA-256, bytes including non-UTF-8, native OS paths, clean and
malformed boundaries, cancellation/resource limits, and partial effects. Use deterministic barriers
for races and faults before/after publication. Document finite coverage and unavailable platforms.
Benchmarks use Criterion and recorded workload fingerprints; memory, descriptors and cancellation
bounds need separate evidence. Establish budgets from consumer needs and measurements.

## Requirements Matrix

### A01 — Commit identities, timestamps and signature payloads

**Tasks:** R01, R07. **Consumer:** `lib/src/git_backend.rs` commit read/write, signature extraction,
overlapping identity handling; `lib/tests/test_git_backend.rs`.

Construct negative author/committer seconds, including a collision decrement across epoch; preserve
ordered/octopus parents, unknown and folded headers, raw messages and identity bytes. Distinguish
lossless parsing from interpreted dates and canonical construction. Probe malformed dates, overflow,
timezone spelling, whitespace, missing/duplicate headers and truncation. Recover the exact signed
payload by removing only the selected signature header spans; cover repeated signatures and
`gpgsig`/`gpgsig-sha256` deterministically. Opaque signatures and unchanged bytes go to the caller;
jj owns placeholder identities, lossy display, collision policy and signing/verification providers.

R01 supplies signed construction and structural byte access independently of interpretation.
[R07 evidence](evidence/r07.md) closes its retained decoding follow-ups with usable graph access,
explicit identity/date interpretation, both-format signature captures and bounded tag peeling. Its
native runtime evidence is macOS; R34/R36 retain the broader native refresh gates.

### A02 — Object format and identity foundations

**Tasks:** R02, R04, R05. **Consumer:** workspace `Cargo.toml` gix SHA-1/SHA-256 features and
`lib/src/git_backend.rs` dual-format fixtures.

Exercise format-aware IDs, nulls, length/hex failures, hashing, equality and map keys; reject
repository-format mismatch at the operation boundary. Test both-format loose/packed objects, trees,
commits, tags, refs, reflogs, indexes and transport negotiation. Check pack/index trailers and
checksums, not just object ID width. Use independent Git IDs and fsck where meaningful. No dual-hash
conversion requirement has been observed. Measure hashing and pack throughput when those paths
change.

R02 closes the identity/hash and current-boundary-refusal subset only; see its
[evidence and ownership audit](evidence/r02.md). Both-format codecs, storage, refs, indexes and
transport remain open under their owning tasks. R04 codec/loose-storage evidence is recorded in
[its completion report](evidence/r04.md). R05 pack/ref/reflog/index propagation and retained
transport boundaries are recorded in [its completion report](evidence/r05.md). The
[R06 completion report](evidence/r06.md) records the native matrix refresh and its platform
boundaries.

### A03 — Instrumentation, errors and scheduling

**Task:** R03, extended by each operation. **Consumer:** backend errors/Send+Sync in
`core/src/backend.rs`, progress in `lib/src/git_subprocess.rs`, CLI callbacks in
`cli/src/git_util.rs`.

An optional tracing feature exposes useful operation spans, timings, work/resource counts,
cancellation and failure classes. Verify enabled/disabled builds, no global subscriber/runtime
installation, no duplicate user-visible errors, and no credentials, content or sensitive path/URL
leakage. Preserve source errors, missing/corrupt/wrong-kind/unsupported/limit distinctions, partial
publication and uncertain mutations. Test caller-owned bounded CPU scheduling and child cleanup.
Existing jj callbacks ignoring errors are not evidence of stronger cancellation guarantees. R03
evidence and coverage limits are recorded in the [completion report](evidence/r03.md); every
subsequent operation extends the same contract.

### A04 — Object representations and Git interpretation

**Tasks:** R04, R07. **Consumer:** `lib/src/git_backend.rs` blob, symlink, tree and commit
operations; `lib/src/git.rs` tag peeling/import.

Read/write binary blobs, symlink targets as bytes, executable modes, opaque gitlinks, historical
tree mode spellings, unknown headers and annotated tags. Probe noncanonical modes and ordering
separately from write policy. Preserve tag identity while peeling nested targets; distinguish
noncommit target, missing object, corruption and bounded cycle/depth failure. jj owns conflict
trees, extra metadata, virtual roots and path/display restrictions. New annotated-tag creation
already has primitives and is not itself a newly observed jj need. Assert interoperability in both
directions.

### A05 — Object stores, resource bounds and refresh

**Tasks:** R05, R14–R16. **Consumer:** persistent reads, external repository opening and GC refresh
in `lib/src/git_backend.rs`; concurrent write tests in `lib/tests/test_git.rs`.

Characterize accepted pack/index versions, alternates, relative paths, cycles, missing stores and
symlink storage before deciding required follow-ups. Cover REF/OFS deltas, long chains, bad hashes,
missing bases and loose duplicate publication. Read repositories beyond the current 512 MiB pack
budget without retaining all pack bytes in RAM. Measure cold/warm open, lookup/import, decode work,
peak memory and file handles. Test refresh after install, stale negative caches, pack/index pair
races, external repack/GC, concurrent loose writes, missing/corrupt data and interrupted
publication. Retained readers and retries must have explicit lifetime and consistency contracts.

### A06 — Config layers and remote editing

**Tasks:** R08, R09. **Consumer:** `lib/src/git_backend.rs` settings; remote operations in
`lib/src/git.rs`; `cli/tests/test_git_remotes.rs` and `test_git_init.rs`.

Resolve system/global/local/worktree config, includes and conditional includes, environment and
explicit caller overrides with provenance and precedence. Probe cycles, relative include paths,
encoding and repeated keys. Add/remove/rename remotes and edit fetch/push URLs/refspecs while
preserving unrelated text, inherited settings and branch remote sections. Exercise ordered fallback
URLs, concurrent edits, locks, parse/write failures and unchanged old bytes. Isolate HOME and config
sources. jj retains remote selection/naming and view updates; girt owns general file semantics.

R08 resolution and R09 direct-file mutation are complete with [R08 evidence](evidence/r08.md) and
[R09 evidence](evidence/r09.md). File/effective scope, preserved text, held locks, source
preconditions and failure cleanup are covered. Multi-file inherited-setting orchestration and
remote-tracking reference composition remain explicitly assigned to R28; native platform evidence
continues under R36/C02.

### A07 — Repository layouts and shallow state

**Tasks:** R10, R14. **Consumer:** repository open/init in `lib/src/git_backend.rs`, colocation and
workspace paths in `lib/src/git.rs`, CLI init/clone/workspace tests.

Cover bare/nonbare discovery, gitfiles, common directories, relative linked-worktree paths, moved
repositories, detached/unborn HEAD and initialized object-format configuration. Distinguish common
and worktree-private refs/config/index/state. Test consistent shallow-root snapshots with absent
parents, malformed shallow data and refresh after depth changes; jj decides synthetic-root mapping
and metadata caching. Characterize ownership/trust and repository environment overrides accepted by
the consumer. Native Windows UNC/drive paths, WSL relative-path interoperability, Unix non-UTF-8 and
macOS normalization/case aliases need explicit evidence rather than lexical assumptions.

### A08 — References and reflogs

**Task:** R11. **Consumer:** no-GC refs in `lib/src/git_backend.rs`, export/import/reset in
`lib/src/git.rs`, `cli/src/cleanup_guard.rs` tempfile cleanup.

Cover direct/symbolic/unborn/detached HEAD, packed/loose iteration and precedence, invalid names,
expected-old predicates including absent-or-same and must-exist, no-GC refs, deletion and reflog
policy. Test symbolic HEAD logging, ref-and-log deletion, namespace conflicts, concurrent Git
writers and partial batch publication. Preserve conflicts and per-ref outcomes without weakening
compare-and-set guarantees. Execute portable locks, rename/publication and termination cleanup on
Windows as well as Unix; signal cleanup belongs to caller integration where process policy is
needed. Characterize reference backends accepted by the target rather than silently excluding
reftable.

### A09 — Index and colocation primitives

**Tasks:** R12, R13. **Consumer:** index export/reset in `lib/src/git.rs`,
`cli/tests/test_git_colocated.rs`, `lib/tests/test_git.rs` cache-tree regression.

Support required v2/v3/v4 indexes, intent-to-add and extended flags, conflict stages, stat reuse,
byte paths, executable modes and both hash formats. Establish mandatory/optional extension
preservation or invalidation rules; exercise split/sparse index acceptance before claiming parity.
Invalidate stale cache trees after edits. Preserve concurrent index changes and old bytes on failed
publication. Supply recognized merge/rebase/cherry-pick operation-state inspection/cleanup with
partial-failure evidence. jj keeps merged-tree staging, reset policy and working-file
materialization.

### A10 — Ignore and exclude semantics

**Task:** R17. **Consumer:** `lib/src/gitignore.rs`, `cli/src/cli_util.rs` global excludes.

Replace the separate gix-ignore dependency with general Git ignore parsing/matching. Test hierarchy,
last-match precedence, negation, escaped spaces/comments, slash anchoring, directory-only rules,
`**`, case policy and byte paths. Combine per-directory files, info/exclude and core.excludesFile;
probe ignored-parent traversal/re-inclusion constraints. Compare independent `git check-ignore`
observations and unchanged jj behavior. jj owns traversal and tracked-file selection. Benchmark
large pattern/path sets and bound adversarial matching work.

### A11 — Inferred copies and renames

**Task:** R18. **Consumer:** `lib/src/git_backend.rs` copy records and
`lib/tests/test_git_backend.rs` copy/rename cases.

Characterize the 50% similarity threshold, 1000-candidate limit, modified-source copies, empty-file
exclusion, blob-only results and target filters. Test exact/edited/binary copies, ties, renames,
file-directory transitions, symlink exclusion and mode changes with deterministic output. Observe
Git where semantics match; record any consumer-specific policy explicitly. Probe the narrow
attributes/filter setup actually used rather than infer requirements from enabled gix features.
Benchmark candidate explosion and cancellation. jj retains merge and copy-history algorithms.

### A12 — Worktree administration

**Tasks:** R19, R20. **Consumer:** `lib/src/git_subprocess.rs` worktree add/repair/prune and CLI
workspace workflows.

Create/register an orphan worktree with its branch and private HEAD/index state, honoring relative
path configuration. Repair links after repository/worktree moves. Prune missing registrations with
expiry and lock safeguards, preserving live worktrees and per-worktree roots. Compare Git-observable
layout and subsequent Git operations. Inject failures between registration and directory creation,
concurrent repair/prune, permissions and stale locks. Native paths and crash leftovers are required
cases. No Git worktree subprocess remains; jj owns workspace metadata and ordinary checkout.

### A13 — Transport configuration and extension boundaries

**Tasks:** R21–R25. **Consumer:** `lib/src/git_subprocess.rs` command environment and options,
`cli/src/git_util.rs` URLs and prompts, current girt `docs/http.md` and `docs/ssh.md` restrictions.

Inventory local/file/scp/SSH/HTTP and other accepted schemes, URL rewrites, fetch/push precedence,
protocol policy, environment overrides and remote-helper dispatch. Record accepted configuration
keys and endpoint families before implementation. Split newly required protocols into follow-ups.
Local fetch/push must use native library paths; no Git upload/receive-pack executable fallback.

Use fake credential helpers, askpass, agents and SSH processes with synthetic secrets to test
credential lookup/approve/reject, URL/path scoping, prompting, cancellation, exit and cleanup. Cover
SSH agent/key/passphrase, host verification, GIT_SSH/GIT_SSH_COMMAND and variant behavior; HTTP
trust/CA settings, proxy authentication, redirects, credential forwarding and auth challenges.
Trusted command configuration must be explicit. Test helper protocol failures and identify helpers
that secretly require local Git. Such dependencies block readiness until resolved, not hidden behind
a generic provider. Remote Git servers and explicit user signing/SSH/credential programs remain
documented external boundaries. Native Windows spawning, quoting and console behavior need runtime
fixtures. Benchmark connect/transfer resources where paths materially change.

### A14 — Advertisement, fetch and clone primitives

**Tasks:** R26–R28. **Consumer:** `lib/src/git_subprocess.rs` fetch/default branch/remote prune;
`lib/src/git.rs` fetch mappings; `cli/tests/test_git_clone.rs`.

Negotiate endpoint capabilities and object format, including protocol versions required by observed
endpoints; characterize thin packs instead of assuming complete-pack support. Resolve empty/unborn,
missing/symbolic/ambiguous/default HEAD deterministically. Fetch positive/negative/exact/wildcard
refspecs, shallow/depth boundaries, missing exact refs among valid refs, pruning and forced remote
updates. Preserve no unintended FETCH_HEAD/tag following and caller-controlled tag namespaces. Test
installation before partial ref failure, concurrent changes, truncation, sideband bytes/line
terminators, cancellation and cleanup across local/SSH/HTTP. jj owns clone destination, checkout,
metadata and view policy. Measure initial/incremental transfer size, CPU, memory and cancellation.

### A15 — Push commands and outcomes

**Tasks:** R29, R30. **Consumer:** `lib/src/git.rs`, `lib/src/git_subprocess.rs`,
`cli/src/commands/gerrit/upload.rs` and push tests in `lib/tests/test_git.rs`.

Create/update/delete branches and tags, mix commands, accept valid arbitrary namespaces including
Gerrit refs/for and push options. Preserve expected-old lease semantics and independently successful
refs when another is stale/rejected. Model not-sent, accepted, rejected and uncertain separately;
never automatically retry uncertain mutations. Test server policy rejection, malformed/truncated
reports, disconnect before/after send, remote sideband, cancellation, child cleanup and remote hook
behavior; retain jj's local pre-push bypass. jj updates views only for confirmed successes.
Benchmark initial/incremental preparation and sending without unnecessary whole-pack copies.

### A16 — Native CI and platform coverage

**Tasks:** R06, R36, C02, C03, R34. Execute applicable tests on native Linux, macOS and Windows for
both hash formats. Extend the existing `.github/workflows/validation.yml` selection as capabilities
arrive. Record OS/filesystem/toolchain/Git versions, exact revisions and logs. Exercise portable
refs/index/worktrees, Unicode and OS paths, locks/permissions, transport/process cancellation and
maintenance. Cross-compilation is supplementary. Scheduled milestones do not excuse omitting a
capability's required platform evidence; unavailable native runs remain an open gate.

### A17 — GC, repack and expiry

**Tasks:** R31–R33. **Consumer:** `lib/src/git_backend.rs` no-GC ref maintenance and `run_git_gc`
invoking `gc --prune=<cutoff>`; backend GC tests.

jj supplies retained metadata heads and cutoff policy. Girt computes Git reachability and retention
from refs, reflogs, worktree HEADs/indexes and required recent/unreachable roots. Characterize
config, reflog expiration, kept packs, alternates ownership and shallow boundaries before deletion.
Preserve objects reachable from every live root and recent objects needed by concurrent writers.
Repack without changing IDs; publish pack/index pairs safely, refresh readers and prune only owned
expired data. Probe interrupted writes, missing pairs, concurrent ref/loose writes, repack races,
disk-full, permissions and cancellation before/after publication. Compare retained-object sets and
repository usability with controlled Git GC observations, not identical pack bytes. Benchmark large
stores and bound memory/temporary disk use. Accelerators/cruft formats require evidence-driven
decisions; unimplemented required retention/config behavior blocks readiness.

### A18 — Architecture checkpoints

**Tasks:** C01–C03 and earlier reviews if debt accumulates. Review layering, cohesive Git concepts,
public type/trait count, error recovery, byte/path boundaries, runtime ownership, instrumentation,
format propagation and duplicate policies. Record findings, exact inspected revision, decisions and
bounded remediation tasks with dependencies. Resolve blockers before downstream work. Repeat at
roughly ten completed capability items, including newly added follow-ups; the scheduled checkpoints
are minimums. R34 includes a final coherence review after maintenance.

### A19 — Final replacement gate

**Task:** R35 only. Start after all required girt capabilities, discovered follow-ups and R34 native
acceptance are complete. Re-inventory jj call sites, exposed gix types, manifests and transitive
components against the target revision. Integrate once, removing scoped gix/gix-ignore use and Git
executable production endpoints, including GC/worktrees/local transfer. Audit subprocess execution
and explicit extension programs so no fallback hides a missing capability.

Run jj core/lib/CLI Git, Gerrit, workspace, colocation, ignore, signing and GC suites in both
formats on native platforms. Exercise init/clone/import, binary snapshot, conflict/rewrite,
bookmark/tag export, mixed push outcomes, fetch/prune/depth, worktree repair, GC and reopen with
independent Git operations between steps. Preserve jj-owned algorithms and error/UI behavior.
Compare baseline import/log/rewrite resources and inspect failures and leaked locks. Deny local Git
execution in production-path tests while allowing separate oracle invocations; controlled remote
servers may use Git. Inventory remaining dependencies and extension programs with reasons. Publish
no claim of universal edge-case parity: link tested cases, differences, limitations and exact
revisions. The final gate cannot close with a required Git CLI fallback or unresolved
supported-behavior regression.
