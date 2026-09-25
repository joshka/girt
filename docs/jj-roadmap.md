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

| ID  | Bounded deliverable                                         | Depends on                      | Status                | Acceptance / completion                                                                                                                                                      |
| --- | ----------------------------------------------------------- | ------------------------------- | --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R00 | Roadmap, identity decision, contributor contracts           | —                               | Complete (planning)   | This document; [matrix](jj-acceptance.md); [process](testing.md#roadmap-completion-and-coordination)                                                                         |
| R01 | Commit timestamps, identities, exact signature bytes        | R00                             | Complete              | [A01](jj-acceptance.md#a01--commit-identities-timestamps-and-signature-payloads); [evidence](evidence/r01.md)                                                                |
| R02 | Format-bearing identity and hashing foundation              | R01                             | Complete              | [A02](jj-acceptance.md#a02--object-format-and-identity-foundations); [evidence](evidence/r02.md)                                                                             |
| R03 | Optional tracing and operation failure visibility           | R02                             | Complete              | [A03](jj-acceptance.md#a03--instrumentation-errors-and-scheduling); [evidence](evidence/r03.md)                                                                              |
| R04 | SHA-256 object codecs and loose repository storage          | R02                             | Complete              | [A02](jj-acceptance.md#a02--object-format-and-identity-foundations), [A04](jj-acceptance.md#a04--object-representations-and-git-interpretation); [evidence](evidence/r04.md) |
| R05 | SHA-256 packs, indexes, refs and index checksums            | R04                             | Complete              | [A02](jj-acceptance.md#a02--object-format-and-identity-foundations), [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh); [evidence](evidence/r05.md)     |
| R06 | Native CI foundation for both formats                       | R05                             | Complete              | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage); [evidence](evidence/r06.md)                                                                                    |
| R07 | Tolerant tree/tag/commit decoding and peeling               | R04                             | Complete              | [A01/A04 evidence](evidence/r07.md)                                                                                                                                          |
| R08 | Layered config resolution and provenance                    | R03                             | Complete              | [A06](jj-acceptance.md#a06--config-layers-and-remote-editing); [evidence](evidence/r08.md)                                                                                   |
| R09 | Lossless config and remote mutation                         | R08                             | Planned               | [A06](jj-acceptance.md#a06--config-layers-and-remote-editing)                                                                                                                |
| R10 | Repository discovery, linked layouts and shallow roots      | R05, R08                        | Planned               | [A07](jj-acceptance.md#a07--repository-layouts-and-shallow-state)                                                                                                            |
| C01 | First architecture and abstraction-debt review              | R01–R10                         | Planned               | [A18](jj-acceptance.md#a18--architecture-checkpoints)                                                                                                                        |
| R11 | Portable conditional refs and reflogs                       | C01, R05, R09, R10              | Planned               | [A08](jj-acceptance.md#a08--references-and-reflogs)                                                                                                                          |
| R12 | Index versions, flags and extension policy                  | R05, R10                        | Planned               | [A09](jj-acceptance.md#a09--index-and-colocation-primitives)                                                                                                                 |
| R13 | Colocation index/HEAD and operation-state primitives        | R11, R12                        | Planned               | [A09](jj-acceptance.md#a09--index-and-colocation-primitives)                                                                                                                 |
| R14 | External object-store acceptance and required formats       | R07, R10                        | Planned               | [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh), [A07](jj-acceptance.md#a07--repository-layouts-and-shallow-state)                                    |
| R15 | File-backed pack reads and bounded caches                   | R14, R03                        | Planned               | [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh)                                                                                                       |
| R16 | Object-store refresh and concurrent publication             | R15, R11                        | Planned               | [A05](jj-acceptance.md#a05--object-stores-resource-bounds-and-refresh)                                                                                                       |
| R17 | Ignore parsing and hierarchical matching                    | R08                             | Planned               | [A10](jj-acceptance.md#a10--ignore-and-exclude-semantics)                                                                                                                    |
| R18 | Deterministic inferred rename/copy detection                | R07, R15                        | Planned               | [A11](jj-acceptance.md#a11--inferred-copies-and-renames)                                                                                                                     |
| R19 | Worktree creation, registration and orphan HEAD             | R10–R13                         | Planned               | [A12](jj-acceptance.md#a12--worktree-administration)                                                                                                                         |
| R20 | Worktree repair, locks and pruning                          | R19                             | Planned               | [A12](jj-acceptance.md#a12--worktree-administration)                                                                                                                         |
| C02 | Storage/layout coherence and native CI milestone            | R11–R20, R36                    | Planned               | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage), [A18](jj-acceptance.md#a18--architecture-checkpoints)                                                          |
| R21 | URL, environment and transport configuration                | C02, R09                        | Planned               | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R22 | Credential helper and askpass lifecycle                     | R21                             | Planned               | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R23 | HTTP trust, proxy, redirects and authentication             | R22                             | Planned               | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R24 | SSH command/agent/key configuration and cleanup             | R22                             | Planned               | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R25 | Native local transport and explicit helper protocols        | R21, R16                        | Planned               | [A13](jj-acceptance.md#a13--transport-configuration-and-extension-boundaries)                                                                                                |
| R26 | Remote advertisement, default HEAD and protocol negotiation | R23–R25                         | Planned               | [A14](jj-acceptance.md#a14--advertisement-fetch-and-clone-primitives)                                                                                                        |
| R27 | Fetch negotiation, shallow/depth and pack receipt           | R26, R16                        | Planned               | [A14](jj-acceptance.md#a14--advertisement-fetch-and-clone-primitives)                                                                                                        |
| R28 | Fetch install, refspec mapping and prune outcomes           | R27, R11                        | Planned               | [A14](jj-acceptance.md#a14--advertisement-fetch-and-clone-primitives)                                                                                                        |
| R29 | Push command model, deletes, leases and options             | R26, R16                        | Planned               | [A15](jj-acceptance.md#a15--push-commands-and-outcomes)                                                                                                                      |
| R30 | Push partial outcomes, progress and cancellation            | R29                             | Planned               | [A15](jj-acceptance.md#a15--push-commands-and-outcomes)                                                                                                                      |
| C03 | Transport coherence and native CI milestone                 | R21–R30                         | Planned               | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage), [A18](jj-acceptance.md#a18--architecture-checkpoints)                                                          |
| R31 | GC roots, retention and expiry planning                     | C03, R11, R16, R20              | Planned               | [A17](jj-acceptance.md#a17--gc-repack-and-expiry)                                                                                                                            |
| R32 | Repack and concurrent atomic pack publication               | R31, R16                        | Planned               | [A17](jj-acceptance.md#a17--gc-repack-and-expiry)                                                                                                                            |
| R33 | Safe pruning, reflog expiry and maintenance composition     | R32                             | Planned               | [A17](jj-acceptance.md#a17--gc-repack-and-expiry)                                                                                                                            |
| R34 | Full girt acceptance corpus and native readiness            | All earlier items               | Planned               | [A01](jj-acceptance.md#a01--commit-identities-timestamps-and-signature-payloads)–[A18](jj-acceptance.md#a18--architecture-checkpoints)                                       |
| R35 | Final jj replacement and integration                        | R34 and all required follow-ups | Planned               | [A19](jj-acceptance.md#a19--final-replacement-gate)                                                                                                                          |
| R36 | Coherent Windows native integration coverage                | R06; alongside R11/R12          | Planned; close by C02 | [A16](jj-acceptance.md#a16--native-ci-and-platform-coverage)                                                                                                                 |

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
Git observations, limits, benchmarks and platform boundaries. R09 is the next serial item after
coordinator acceptance; final jj integration remains R35.

R14, R21 and R34 are discovery gates as well as deliverables. If characterization reveals a large
required format, helper, platform, or policy feature, append bounded dependent tasks before marking
the gate complete. A documentation-only refusal does not close a required compatibility gap.
Maintenance is similarly split into planning, publication, and destructive expiry so each contract
can be validated before the next depends on it.

R36 audits and organizes native Windows integration coverage alongside R11/R12, with closure
required by C02. Inventory supported public operations and excluded suites, distinguishing
unsupported capabilities from omitted portable tests. Exercise SHA-1 and SHA-256 where supported,
retain explicit capability exclusions with their owners, and record native execution evidence and
remaining gaps against A16. R06 records validation of its selected suites; R36 is queued follow-up
work and does not expand R06's coverage claims.

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
