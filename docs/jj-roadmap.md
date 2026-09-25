# Full jj Git Coverage Roadmap

The target is girt coverage of jj's Git responsibilities followed by one final jj replacement task.
There is no early partial migration or final retained Git executable provider. Git is an independent
compatibility oracle in disposable fixtures, not a production fallback. No capability is declared
ready by this planning change. Existing coverage remains described in
[compatibility](compatibility.md).

## Baseline and Ownership

The source assessment used girt `9a44ad7e4aa76e26caca0c48cccf7aac6f170b74` and jj
`be5f5ebdc200593d8f1be06f11e27b485a97093d` on 2026-09-24. Its local artifacts are
[report](/Users/joshka/.codex/reports/girt-jj-2026-09-24/report.md),
[old queue](/Users/joshka/.codex/reports/girt-jj-2026-09-24/queue.md),
[call sites](/Users/joshka/.codex/reports/girt-jj-2026-09-24/call-sites.txt), and
[source manifest](/Users/joshka/.codex/reports/girt-jj-2026-09-24/source-manifest.json).
These are historical local evidence; this repository roadmap supersedes their staged migration plan.
The [acceptance matrix](jj-acceptance.md) retains the actionable scope without requiring those
files. Re-inventory the target jj revision before final integration to catch drift.

Girt owns Git representation, storage, repository/config/ref/index operations, ignore and inferred
copy primitives, maintenance, and transport. jj retains its metadata tables, virtual roots/change
IDs, conflicts, view/bookmark reconciliation, revision index/revsets, merge algorithms, ordinary
snapshot/checkout/status, sparse selection, clone destination policy, UI, and signing provider. Girt
supplies exact signing bytes and signature headers; it does not select keys or implement jj's
signing policy. General Git traversal needed for transfers or GC remains girt work.

## Identity Decision

Settle the foundation before extending format-sensitive APIs: use an explicit closed SHA-1/SHA-256
format choice and format-bearing object identities with exactly 20 or 32 meaningful bytes. Equality,
hashing, ordering, parsing, null IDs, and formatting include or respect the format. A repository has
one storage format; validate IDs at its operation boundary. Keep object identity independent of
existence and kind. Avoid fixed SHA-1 arrays at shared public boundaries, padded-byte comparisons,
and speculative format registries. Dual-hash translation is outside the observed jj requirement.

R01 can improve timestamp and signature-byte contracts without implementing new hashes. R02
implements the identity foundation; R04/R05 propagate it through storage before later APIs settle.
Both signature header spellings need byte-preserving treatment, even before SHA-256 storage works.
No Rust implementation is part of this roadmap change.

## Ordered Queue

IDs are stable; append follow-ups rather than renumbering. Dependencies are minimum prerequisites;
the displayed sequence is the default serial dispatch order. `Planned` means acceptance remains
unverified, including where useful primitives already exist. Each completion cell must eventually
link a retained report containing the exact tested revision and evidence. R00's revision is recorded
in its task completion callback, avoiding a self-referential commit hash in this file.

| ID  | Bounded deliverable                                         | Depends on                      | Status              | Acceptance / completion                                                                                                                                                      |
| --- | ----------------------------------------------------------- | ------------------------------- | ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R00 | Roadmap, identity decision, contributor contracts           | —                               | Complete (planning) | This document; [matrix](jj-acceptance.md); [process](testing.md#roadmap-completion-and-coordination)                                                                         |
| R01 | Commit timestamps, identities, exact signature bytes        | R00                             | Complete            | [A01](jj-acceptance.md#a01--commit-identities-timestamps-and-signature-payloads); [evidence](evidence/r01.md)                                                                |
| R02 | Format-bearing identity and hashing foundation              | R01                             | Complete            | [A02](jj-acceptance.md#a02--object-format-and-identity-foundations); [evidence](evidence/r02.md)                                                                             |
| R03 | Optional tracing and operation failure visibility           | R02                             | Complete            | [A03](jj-acceptance.md#a03--instrumentation-errors-and-scheduling); [evidence](evidence/r03.md)                                                                              |
| R04 | SHA-256 object codecs and loose repository storage          | R02                             | Complete            | [A02](jj-acceptance.md#a02--object-format-and-identity-foundations), [A04](jj-acceptance.md#a04--object-representations-and-git-interpretation); [evidence](evidence/r04.md) |
| R05 | SHA-256 packs, indexes, refs and index checksums            | R04                             | Complete            | [A02](jj-acceptance.md#a02--object-format-and-identity-foundations), [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh); [evidence](evidence/r05.md)     |
| R06 | Native CI foundation for both formats                       | R05                             | Complete            | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage); [evidence](evidence/r06.md)                                                                                    |
| R07 | Tolerant tree/tag/commit decoding and peeling               | R04                             | Complete            | [A01/A04 evidence](evidence/r07.md)                                                                                                                                          |
| R08 | Layered config resolution and provenance                    | R03                             | Complete            | [A06](jj-acceptance.md#a06--config-layers-and-remote-editing); [evidence](evidence/r08.md)                                                                                   |
| R09 | Lossless config and remote mutation                         | R08                             | Complete            | [A06](jj-acceptance.md#a06--config-layers-and-remote-editing); [evidence](evidence/r09.md)                                                                                   |
| R10 | Repository discovery, linked layouts and shallow roots      | R05, R08                        | Accepted            | [A07](jj-acceptance.md#a07--repository-layouts-and-shallow-state); [evidence](evidence/r10.md)                                                                               |
| C01 | First architecture and abstraction-debt review              | R01–R10                         | Accepted            | [A18](jj-acceptance.md#a18--architecture-checkpoints); [remediation](evidence/c01.md)                                                                                        |
| R11 | Portable conditional refs and reflogs                       | C01, R05, R09, R10              | Accepted            | [A08](jj-acceptance.md#a08--references-and-reflogs); [evidence](evidence/r11.md)                                                                                             |
| R12 | Index versions, flags and extension policy                  | R05, R10                        | Complete            | [A09](jj-acceptance.md#a09--index-and-colocation-primitives); [evidence](evidence/r12.md)                                                                                    |
| R13 | Colocation index/HEAD and operation-state primitives        | R11, R12                        | Accepted            | [A09](jj-acceptance.md#a09--index-and-colocation-primitives); [evidence](evidence/r13.md)                                                                                    |
| R14 | External object-store acceptance and required formats       | R07, R10                        | Accepted            | [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh), [A07](jj-acceptance.md#a07--repository-layouts-and-shallow-state); [evidence](evidence/r14.md)       |
| R15 | File-backed pack reads and bounded caches                   | R14, R39, R03                   | Accepted            | [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh); [evidence](evidence/r15.md)                                                                          |
| R16 | Object-store refresh and concurrent publication             | R15, R11                        | Accepted            | [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh); [evidence](evidence/r16.md)                                                                          |
| R17 | Ignore parsing and hierarchical matching                    | R08                             | Accepted            | [A10](jj-acceptance.md#a10--ignore-and-exclude-semantics); [evidence](evidence/r17.md)                                                                                       |
| R18 | Deterministic inferred rename/copy detection                | R07, R15                        | Planned             | [A11](jj-acceptance.md#a11--inferred-copies-and-renames)                                                                                                                     |
| R19 | Worktree creation, registration and orphan HEAD             | R10–R13                         | Planned             | [A12](jj-acceptance.md#a12--worktree-administration)                                                                                                                         |
| R20 | Worktree repair, locks and pruning                          | R19                             | Planned             | [A12](jj-acceptance.md#a12--worktree-administration)                                                                                                                         |
| C02 | Storage/layout coherence and native CI milestone            | R11–R20, R36–R40                | Planned             | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage), [A18](jj-acceptance.md#a18--architecture-checkpoints)                                                          |
| R41 | Representative Git-parity performance                       | C02                             | Planned             | [A20](jj-acceptance.md#a20--representative-git-parity-performance); immediately after C02 and before R21; required before R34.                                               |
| R21 | URL, environment and transport configuration                | R41, R09                        | Planned             | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R22 | Credential helper and askpass lifecycle                     | R21                             | Planned             | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R23 | HTTP trust, proxy, redirects and authentication             | R22                             | Planned             | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R24 | SSH command/agent/key configuration and cleanup             | R22                             | Planned             | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R25 | Native local transport and explicit helper protocols        | R21, R16                        | Planned             | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R26 | Remote advertisement, default HEAD and protocol negotiation | R23–R25                         | Planned             | [A14](jj-acceptance.md#a14--advertisement-fetch-and-clone-primitives)                                                                                                        |
| R27 | Fetch negotiation, shallow/depth and pack receipt           | R26, R16                        | Planned             | [A14](jj-acceptance.md#a14--advertisement-fetch-and-clone-primitives)                                                                                                        |
| R28 | Fetch install, refspec mapping and prune outcomes           | R27, R11                        | Planned             | [A14](jj-acceptance.md#a14--advertisement-fetch-and-clone-primitives)                                                                                                        |
| R29 | Push command model, deletes, leases and options             | R26, R16                        | Planned             | [A15](jj-acceptance.md#a15--push-commands-and-outcomes)                                                                                                                      |
| R30 | Push partial outcomes, progress and cancellation            | R29                             | Planned             | [A15](jj-acceptance.md#a15--push-commands-and-outcomes)                                                                                                                      |
| C03 | Transport coherence and native CI milestone                 | R21–R30                         | Planned             | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage), [A18](jj-acceptance.md#a18--architecture-checkpoints)                                                          |
| R31 | GC roots, retention and expiry planning                     | C03, R11, R16, R20, R40         | Planned             | [A17](jj-acceptance.md#a17--gc-repack-and-expiry)                                                                                                                            |
| R32 | Repack and concurrent atomic pack publication               | R31, R16                        | Planned             | [A17](jj-acceptance.md#a17--gc-repack-and-expiry)                                                                                                                            |
| R33 | Safe pruning, reflog expiry and maintenance composition     | R32                             | Planned             | [A17](jj-acceptance.md#a17--gc-repack-and-expiry)                                                                                                                            |
| R34 | Full girt acceptance corpus and native readiness            | R00–R33, R36–R41                | Planned             | [Acceptance matrix](jj-acceptance.md), including [A20](jj-acceptance.md#a20--representative-git-parity-performance)                                                          |
| R35 | Final jj replacement and integration                        | R34 and all required follow-ups | Planned             | [A19](jj-acceptance.md#a19--final-replacement-gate)                                                                                                                          |
| R36 | Coherent Windows native integration coverage                | R06; alongside R11/R12          | Accepted            | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage); [evidence](evidence/r36.md)                                                                                    |
| R37 | Reftable reference and reflog backend                       | R11, R14                        | Accepted            | [Backend evidence](evidence/r37.md); both-format records, stacks, conditional publication, compaction and native evidence; required before R34.                              |
| R38 | Split and sparse index storage                              | R12, R14                        | Accepted            | [R38 evidence](evidence/r38.md); split resolution/publication, sparse preservation/expansion and both-format native fault/race evidence; required before R34.                |
| R39 | Alternate object stores and known storage extensions        | R14                             | Accepted            | [R39 evidence](evidence/r39.md); required before R15/C02/R34.                                                                                                                |
| R40 | Bounded imported reflog interpretation and roots            | R11, R14, R37                   | Complete            | [R40 evidence](evidence/r40.md); bounded bytes/fields/roots, explicit incomplete outcomes and native evidence; required before C02/R31/R34.                                  |

The first tranche is R01 → R02 → R03 → R04 → R05 → R06. R01 constructs signed negative timestamps,
separates parsed identity bytes from construction policy, and exposes exact signature payload
extraction/removal. R02 implements identities and format refusals; R03 follows its acceptance.
Exercise epoch collision, unusual dates and whitespace, folded/repeated/truncated headers, non-UTF-8
identities, and both signature header names through girt's public API. Preserve raw bytes and
document deterministic repeated-header behavior based on independent observations. Keep jj collision
handling and signing callbacks in jj. Run applicable checks from [testing](testing.md); do not build
a jj adapter.

R04 completion and earlier storage boundaries are recorded in [its evidence](evidence/r04.md). R05
completes pack/ref/reflog/index format propagation; its [evidence](evidence/r05.md) records
validation, public API changes and remaining boundaries. R06 completes the native matrix refresh;
its [evidence](evidence/r06.md) records the tested revision, selected suites, public error-field
changes and Windows fixture correction. Transport negotiation remains with R26/R29; R11/R12 retain
broader refs and index semantics. Concrete format/refresh follow-ups are assigned in
[R05 remaining owners](evidence/r05.md#remaining-owners).

R07 resolves the concrete [R01 decoding follow-ups](evidence/r01.md#r07-follow-ups) through decoded
graph/identity access and bounded tag peeling. Its [evidence](evidence/r07.md) distinguishes Git
read/display/write/fsck behavior, exact signature bytes, tested revisions and native limits. R08
completes layered resolution with provenance; its [evidence](evidence/r08.md) records exact source,
Git observations, limits, benchmarks and platform boundaries. R09 completion is recorded in
[its evidence](evidence/r09.md). The coordinator accepts R10 and C01 after independent verification
of the [remediation](evidence/c01.md#coordinator-acceptance). Native evidence gaps and assigned
follow-ups remain open; final jj integration remains R35.

R14, R21 and R34 are discovery gates as well as deliverables. If characterization reveals a large
required format, helper, platform, or policy feature, append bounded dependent tasks before marking
the gate complete. A documentation-only refusal does not close a required compatibility gap.
Maintenance is similarly split into planning, publication, and destructive expiry so each contract
can be validated before the next depends on it.

R36 audits and organizes native Windows integration coverage alongside R11/R12, with closure
required by C02. Inventory supported public operations and excluded suites, distinguishing
unsupported capabilities from omitted portable tests. Exercise SHA-1 and SHA-256 where supported,
retain explicit capability exclusions with their owners, and record native execution evidence and
remaining gaps against A16. R06 records validation of its selected suites; R36 is follow-up work and
does not expand R06's coverage claims. Its [inventory and evidence](evidence/r36.md) retain the
remaining capability and platform owners.

## Superseding the Earlier Queue

| Old item                                  | Replacement                                          |
| ----------------------------------------- | ---------------------------------------------------- |
| Q00 boundary/corpus                       | R00, acceptance matrix, R34                          |
| Q01 object contracts                      | R01, R07                                             |
| Q02 early adapter                         | Removed; consumer integration occurs only in R35     |
| Q03 config                                | R08, R09, R21                                        |
| Q04 refs                                  | R11                                                  |
| Q05 layout                                | R10, R19, R20                                        |
| Q06 colocation                            | R12, R13                                             |
| Q07 SHA-256                               | R02, R04, R05                                        |
| Q08 shallow/external storage              | R10, R14, R27                                        |
| Q09 large storage                         | R15, R16                                             |
| Q10 inferred copies                       | R18                                                  |
| Q11 local replacement retaining CLI       | Removed; R35 replaces all scoped components together |
| Q12 fetch                                 | R26–R28                                              |
| Q13 push                                  | R29, R30                                             |
| Q14 transport                             | R21–R25; no retained Git provider                    |
| Previously deferred ignore, GC, worktrees | R17, R19, R20, R31–R33                               |

R02's [format audit](evidence/r02.md#remaining-format-propagation) assigns codec/storage propagation
to R04, pack/ref/index propagation to R05, and negotiation to R26/R29. R35 retains integration.

R03's [follow-ups](evidence/r03.md#follow-ups-and-limits) assign direct fetch-install failure
reports to R26 and retain per-operation instrumentation and native-platform gates. No tracing field
replaces structured recovery evidence.

## Decisions Still Requiring Evidence

No answer is needed to start R01. Required endpoint/helper/config coverage must be characterized in
R21–R26; local native transport must not shell out to Git upload/receive-pack. Remote servers may
run Git. Explicit credential, SSH, signing and remote-helper processes are distinct extension
boundaries, not permission to route unsupported library operations through a general Git executable.
Any helper that depends on local Git must be identified and resolved before R34; it cannot hide the
dependency.

R14 must establish alternates, symlink storage, legacy pack/index and reference-backend acceptance
from the unchanged consumer and Git observations. Reftable is not silently excluded if required. R18
characterizes the narrow attributes/filter interaction in inferred diff; enabled gix features alone
do not require a general filter engine. R31 must characterize GC config, reflog retention,
linked-worktree roots and recent unreachable objects before destructive expiry is implemented.

Promisor lazy fetching, LFS, native submodule checkout, explicit tracked copy history, a replacement
jj merge/index engine, and complete Git command emulation are not established requirements of this
baseline. Storage accelerators are performance choices, subject to measured need. Revisit exclusions
when inventory or executable evidence changes; record a required follow-up rather than claiming
unverified parity. Native Windows/macOS/Linux runs, resource budgets, fault/race corpora and their
limitations determine readiness, not an assertion that all edge cases have been proven.

## Configuration Mutation Follow-through

R09's remote operations edit one direct file. R11 supplies conditional reference transactions; R28
must compose remote-tracking ref rename/removal with configuration changes and report partial
outcomes. R28 must also define orchestration for inherited branch selectors and remotes requiring
explicit edits across multiple files; R09 does not silently rewrite those sources or promise
whole-operation atomicity. R35 retains consumer naming/selection and view updates. R36 executes the
portable `config_edit` suite natively on Windows; C02 retains Linux/Windows lifecycle evidence.
Repeated scalar selector ambiguity is an explicit R09 refusal with occurrence editing available; no
Git CLI regex-selection or warning-producing partial-edit emulation is promised.

## Repository Layout Follow-through

R10 implementation and native macOS evidence are [retained here](evidence/r10.md), accepted with
[C01 remediation](evidence/c01.md#coordinator-acceptance). R36 records native Linux and Windows
execution of `layout_shallow`, including Unicode and ordinary/verbatim drive paths; its Linux run
also executes byte-path fixtures. C02 retains UNC, denied-access registration and WSL
relative-backlink evidence. Cross-builds do not close these native requirements. R19/R20 retain
registration, repair, locks and pruning; R26/R27 retain shallow negotiation/depth changes, and R29
retains shallow push policy. Current transport guards prevent unsupported use without claiming those
future requirements complete. R14 retains external storage/backends; R35 re-inventories consumer
environment and caching needs.

## C01 Follow-through

C01 review is delivered. F1 shallow prefix interpretation, F2 discovery candidate recognition, F3
inert NUL comments and D2 configuration tracing are remediated with retained
[evidence](evidence/c01.md). The coordinator accepts R10/C01 on independent verification of F1–F3
and D2. That acceptance did not close native R07–R10 gaps. R36 now records the broader native
refresh and remaining exclusions; C02's milestone remains open.

- **D1 — R11:** Imported reflogs must accept the exact Git-observed `+0060`, empty-name and
  padded-name cases in [A08](jj-acceptance.md#a08--references-and-reflogs), in both object formats.
  Keep imported interpretation distinct from append construction policy.
- **D3 — R21:** When extending configuration file/environment selection, consolidate boolean
  decoding in the configuration owner and exercise repository bootstrap, file selection and typed
  lookup together. Preserve implicit values and source-specific diagnostics. Existing duplication is
  nonblocking; no scalar semantics changed during C01 remediation.
- **D4 — R19, with R20 repair follow-through:** Measure large linked-worktree inventory against the
  R10 baseline and characterize repeated common config/shallow reads. If needed, share only coherent
  layout inspection while preserving per-registration errors and explicit snapshot boundaries. No
  shared mutable cache or general repository context is authorized by this debt item.
- **Configuration characterization — R21:** Independently establish NUL-containing value and path
  semantics using an interpreting Git operation before changing their rejection policy. Git CLI
  display truncation alone does not define the internal value. This is distinct from F3's repaired
  inert comments and must be resolved before R34 if required by consumer configuration.

## Reference Backend Follow-through

R11 observed installed jj 0.45.1 accepting reftable initialization and colocated opening in SHA-1
and SHA-256 disposable repositories. A stronger nonempty-repository probe shows that jj does not
import the existing Git HEAD: it reports the synthetic root parent and cannot export a bookmark at
that parent. Matching files-backend controls import the tip and export the bookmark. Opening success
therefore does not establish reftable reference support. R14 must characterize this baseline gap and
broader backend acceptance. R37 implements reftable records and stack reads, conditional
refs/reflogs and stack publication/compaction with both-format Git and native evidence. R34 must not
declare full readiness before R37; files-backend completion does not close that gap.

R14 must also characterize the remaining imported-reflog reader limits against actual consumer
requirements: CR/NUL and unterminated records, short/suffixed zones and out-of-range dates. R11
retains explicit refusals and whole-file allocation, with independent observations in its evidence;
these are not claims that Git rejects those forms. Queue any required wider interpretation or
resource contract before R34 rather than dropping safe append/precondition validation.

R11's broader Windows run records nine existing native failures: one R08 home-condition match case
and eight R10 discovery oracle assertions comparing slash paths with verbatim Windows spellings. R36
fixes the home-condition glob suffix and discovery path comparisons; its final four-host run passes
those cases and the expanded portable selection. C02 retains the wider native gate. The separate R11
reference matrix passes on Windows, Linux and macOS. See
[R11 evidence](evidence/r11.md#native-and-local-validation); R11 does not close these earlier gaps.

R12 establishes full-index v2/v3/v4 framing and extended flags. R38 adds shared-index resolution,
unchanged split preservation, deliberate full-index publication after edits and explicit sparse
directory preservation/expansion. Its [evidence](evidence/r38.md) tracks remaining acceptance before
R34. Expanded sparse entries retain skip-worktree flags. R13 provides colocation drafts that
preserve or explicitly replace these flags without invoking raw status/checkout. jj owns placeholder
selection and materialization. R14 must assess whether any additional consumer requires raw
status/checkout treatment of these flags and queue bounded work before R34; their existing explicit
refusal does not establish parity.

R13 is accepted for its documented primitives, with local both-format Git/fault evidence and passing
native index/colocation, reference and broad platform matrices on all four hosts. The user
authorized public repository visibility, and attempt 2 of the existing runs validated the unchanged
executable revision. [R13 evidence](evidence/r13.md) preserves both the initial billing-blocked
attempts and successful reruns. R14, R37, R38 and C02 retain their assigned work.

## R14 Discovery Follow-through

[R14 evidence](evidence/r14.md) records the both-format Git/jj acceptance matrix and bounded pack
v3/index v1 delivery. Required alternates/extensions work is R39; bounded imported-reflog
interpretation is R40. R15 retains file-backed resource work and R16 refresh. R37/R38 remain the
reftable and split/sparse owners. Recommended serial placement is R39, R15, R16, R37, R38, R40, then
R17; include all of these in C02. R31 depends on R40 for imported retention roots. Opening success,
baseline omissions and explicit refusals do not close required compatibility gaps.

R14's first Windows broad run exposed existing HTTP/tracing test failures; its native evidence
retains exact failures and retry results. The follow-up below supplies structured HTTP diagnostics
and post-send synchronization. C02 retains span-capture/lifetime investigation under parallel native
tests.

R14's unchanged Windows retry passes its 50 pack cases, units, Clippy and tracing, but repeats three
HTTP push fault failures. R14 was left validation blocked pending a bounded native-test diagnostic
and synchronization repair before coordinator acceptance or successor dispatch. The complete
[attempt history](evidence/r14.md#windows-retry-and-acceptance-blocker) preserves both failures.

The separate [HTTP fixture repair](evidence/r14-http.md) is complete: native Windows passes all 43
HTTP and seven portable cases plus Clippy. Controlled discovery delays return `NotSent(Deadline)`
without POST; repaired fault tests synchronize with POST and retain specific uncertain causes. R14
is ready for coordinator acceptance using the existing storage evidence and focused repair result.
C02 retains the independent tracing-span issue; the earlier failed matrix remains recorded.

Coordinator acceptance of R14 uses the existing native storage evidence and focused Windows HTTP
verification, as recorded in [R14 evidence](evidence/r14.md). Historical failures and the C02-owned
missing tracing span remain open records; no replacement broad matrix is claimed.

## R39 Coordinator Acceptance

[R39 evidence](evidence/r39.md) records bounded canonical alternate traversal, primary/borrowed
loose and packed reads, known storage extensions, original Git observations and the passing scoped
native matrix. The coordinator accepted R39. R15 retains file-backed resource accounting and R16
retains refresh. C02 retains the bounded malformed-record policy review and native path/fault
extensions described in the evidence, alongside its independent tracing issue.

The user cancelled the computer-restart pause and authorized R15. The serial sequence remains R15,
R16, R37, R38, R40, then R17. Acceptance uses executable `842330085e2e973fc444c4d7ba9737a98e7c9b97`,
evidence child `9c539160b06f659ae6cb92afc056e76877dfec87`, and passing native run `36161614154`. The
evidence report preserves all historical failures and remaining owners.

## R15 Completion

[R15 evidence](evidence/r15.md) records file-backed pack/index reads, shared pinned handles, bounded
index/descriptor/directory/decode resources, cooperative cancellation, and measured 544 MiB stores.
The executable `699c1846710696115e5e7ffd1528165d5378a768` passes the scoped native matrix on Ubuntu
22.04/24.04, macOS 14 and Windows Server 2022. Local `just check` passes. The coordinator accepts
R15's file-backed resource scope using evidence child `187b9890d66021cfed5affe4d89852123141df82` and
scoped native run [36164401325](https://github.com/joshka/girt/actions/runs/36164401325). R16
retains refresh, publication races and retained-reader consistency. Windows has no measured
handles/RSS; cold-advised evidence is Linux-only, and many-object indexes retain bounded tables.
C02's malformed-alternate, platform and tracing owners remain unchanged. No successor is dispatched.

## R16 Completion

[R16 evidence](evidence/r16.md) records explicit transactional refresh, retained-reader lifetimes,
primary/alternate publication and GC behavior, one-attempt recovery, deterministic pair-open races
and generation resource costs. Executable `60b52cabfca3cac916c642cd250711b8bec8332c` passes local
`just check` and scoped native run
[36166628885](https://github.com/joshka/girt/actions/runs/36166628885) on Ubuntu 22.04/24.04, macOS
14 and Windows Server 2022. The coordinator accepts R16 using evidence child
`c07d9c3d2ae708c62e89f76cb95df78a5e1b0e85`. Acceptance preserves bounded one-attempt refresh,
immutable pinned artifacts and aggregate resource limits. R15's measurement limits and C02's
tracing, malformed-alternate policy and platform owners remain unchanged. Next remains R37, R38,
R40, then R17; no successor is dispatched.

## R37 Completion

[R37 evidence](evidence/r37.md) records both-format reftable codecs, bounded owned stack snapshots,
conditional reference/reflog publication, explicit compaction and integration with existing
reference and colocation operations. Executable `a16ccdc7b7f178ce422abf8ab16c91ac4f600cd2` passes
scoped native run [36172953299](https://github.com/joshka/girt/actions/runs/36172953299) on all four
hosts. Coordinator acceptance records this documented scope in a separate child of the R37 evidence
revision `33b7f04186815c59a2265397cffe965a0bff20b1`. Reads decode bounded whole stacks; compaction
holds writer locks; common/private publication can have explicit partial effects. R40 retains
interpretation of unsigned binary timestamps above `i64::MAX`; R19/R20 retain operation-specific
pseudoref policy, R31/R32 maintenance policy and R35 final consumer parity. C02's existing owners
remain unchanged. Next remains R38, R40, then R17; this task dispatches none.

## R38 Completion

[R38 evidence](evidence/r38.md) records split shared-file resolution, unchanged byte preservation,
deliberate standalone publication after edits and explicit sparse-directory preservation/expansion
through public index and colocation operations. Executable
`34b5c234c06039866e6d8c8ad1c5eb732e5fa9ca` passes scoped native run
[36175961248](https://github.com/joshka/girt/actions/runs/36175961248) on all four hosts.
Coordinator acceptance is recorded in a separate child of evidence revision
`71d8a9034136568005bf7090649bd57e76822e12`. Git accepts overlapping replacement/deletion bitmaps;
the final decoder matches its replacement-before-deletion behavior. No working files are
materialized, shared files are not collected, and jj retains staging policy. Resource and
publication limits, measurements and the unavailable visual Rustdoc check remain explicit. R35
retains final consumer integration; R31/R32, R40, R19/R20 and C02 retain their established owners.
Next remains R40, then R17; no successor is dispatched.

## R41 Performance Checkpoint

R41 runs immediately after C02 and before R21, with the durable
[A20 acceptance contract](jj-acceptance.md#a20--representative-git-parity-performance). Establish a
representative matched Git/girt corpus and reusable measurement practice, profile material gaps, and
improve normal supported operations toward roughly 0.9–1.1x Git throughput. Faster results are
welcome; the upper value is not a regression threshold. Preserve correctness and document justified
exceptions with evidence and ownership. Later transport and other capabilities extend the corpus;
R34 assesses the complete representative corpus, so R41 cannot pre-accept later functionality.

The initial R15-only warm SHA-1 macOS
[packed-read report](/Users/joshka/.codex/reports/girt-vs-git/README.md) reports ordinary packed
reads at 0.20–0.25x Git throughput and selected deep deltas at 0.017–0.026x. It motivates
investigation, not an all-function or cross-platform baseline. No performance implementation is
included in R40.

## R40 Completion

[R40 evidence](evidence/r40.md) records bounded files/reftable history, exact raw data, wide numeric
interpretation, independent recoverable IDs and explicit EOF/corrupt/limit/cancel/I/O outcomes.
Canonical append construction remains strict; terminated unusual history no longer requires
whole-log parsing before append. Unterminated tails refuse before publication. Original Git
operation probes resolve the R11/R14 symbolic-HEAD fallback and distinguish display from retention
candidates.

Executable `ead930d0c3ceddc5c59b5aa22245bc20638c2156` passes scoped native run
[36178892312](https://github.com/joshka/girt/actions/runs/36178892312) on all four hosts and is
accepted by the coordinator for this scoped contract. R31 must enumerate retained logs independently
of live reference names, including deleted refs and private worktrees, and must refuse unsafe GC
plans when any scan is incomplete. Repository-wide coordination and expiry policy remain R31;
R32/R33 own execution. R35 and existing C02 owners retain their scope. R41 has its separate planning
change and A20 criteria; no performance implementation is included here. R17 remains next; no
successor is dispatched.

## R17 Completion

[R17 evidence](evidence/r17.md) records byte-oriented parsing, explicit source/case policies,
hierarchical precedence and ignored-parent decisions. Traversal, tracked-file selection and source
loading remain caller-owned. Local compatibility, resource limits, unchanged jj observations and
representative matched batches and scoped native validation pass on all four hosts. Windows CLI
backslash interpretation remains an explicit caller-adapter boundary. The coordinator accepts this
byte-matcher scope using implementation `71da3994b2d4690076781286992be3bc54c153ba`, final native
adapter/test revision `6fa1d9142309ae22ec7574a06abc374b179f86fe` and evidence child
`3c424aedf718a087a6a90ae2809e770a4a7fc3ee`. Unix run
[36181119693](https://github.com/joshka/girt/actions/runs/36181119693) and passing Windows follow-up
[36181767288](https://github.com/joshka/girt/actions/runs/36181767288) establish the scoped native
evidence. Source loading, OS conversion and traversal remain caller-owned; the 20 Windows
terminal-backslash adapter differences remain recorded. Process-batch ratios do not establish
overall matcher or library parity. R41 retains A20 performance acceptance after C02 and before R21;
existing C02 owners remain unchanged. R18 is next.
