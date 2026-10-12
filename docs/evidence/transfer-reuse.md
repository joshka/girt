# Native Transfer Reuse Evidence

The changes for [local transfer](https://github.com/joshka/girt/issues/63) and
[pack writing](https://github.com/joshka/girt/issues/65) eliminate the source's full decoded blob
vector and unnecessary deflate. Native fetch stops at destination-known complete history. Local
fetch and push reuse stored compressed entries, spool packs, and independently derive received
identities before publication. The end-to-end latency and clone RSS budgets remain unmet.

## Review Correction

The first implementation treated object presence as complete-history evidence. Review reproduced an
interrupted loose-object unpack containing an unreferenced commit without its tree/blob: the native
fast path omitted the missing descendants. Six Git-generated SHA-1/SHA-256 cases reproduced this for
ordinary, retained and configured fetch before the correction.

Available destination objects now remain separate from complete boundaries. Native configured fetch
peels explicit destination reference tips to commit targets without enumerating a wire have batch.
Public store-assisted history also records commits reached from the supplied reference roots during
its bounded have walk. Fully verified snapshots can supply boundary evidence. Mere presence records
a dependency but does not stop source edge traversal or receiver connectivity; retained selection
also requires boundary evidence. Other shared tree data is conservatively walked. The six recovery
cases now supply missing descendants, match source object sets and pass strict Git fsck. Retained
selection of an unreferenced tip supplies a self-contained pack.

Two additional cases mislabel a Git-generated delta base in a correctly checksummed index while the
destination holds the replacement ID. Four damage a stored source blob through native receive or
configured push. Both formats refuse these transfers with destination objects/refs unchanged. The
delta metadata-cache suggestion is deferred: cache lifetime and limit-sensitive metadata need
separate design and measurements, and are not necessary for the integrity correction.

| Operation                         | Main: median (range), RSS      | Corrected: median (range), RSS | Git: median (range), RSS   |
| --------------------------------- | ------------------------------ | ------------------------------ | -------------------------- |
| Configured no-op local fetch      | 16.38 s (16.01–18.09), 687 MiB | 0.07 s (0.06–0.45), 27 MiB     | 0.12 s (0.12–0.14), 14 MiB |
| Configured one-commit local fetch | 18.02 s (17.39–18.52), 666 MiB | 0.32 s (0.31–0.32), 91 MiB     | 0.16 s (0.15–0.16), 15 MiB |

These final commit-only/synthetic-tree correction samples use three fresh copies under the same lock
and environment. Main/Git samples are retained from the rebased rotation below. Raw results are in
[final-workflow.json](transfer-reuse/final-workflow.json) and the exact
[final timing script](transfer-reuse/final-run.py). Only affected configured fetch paths were
remeasured; clone without known history and transfer preparation use the unchanged measured paths.
The conservative boundary proof adds work for incremental fetch and remains above the ≤0.2 s latency
target. The no-op median/RSS passes, but one latency sample exceeds the target.

Tree/blob refs alone do not establish complete boundaries. Four additional cases cover partial tree
refs through explicit and configured fetch; four require the synthetic canonical empty tree to be
materialized for store-assisted or explicitly verified snapshots. The missing physical empty-tree
value supplies neither a dependency nor complete-history/have evidence. All use both formats and
strict Git fsck.

## Measurements After Rebase, Before Review

The final code includes main `5634f763f50608e70ee5d13708ec9412fd736c07`, including deferred
whole-pack verification and preparation against the receiver's advertisement. The same release
builds, fresh-copy fixtures, warm-cache environment and shared lock described below were used for
three new rounds of each operation against that main and Git.

| Operation                         | Main: median (range), RSS        | Changed: median (range), RSS   | Git: median (range), RSS    |
| --------------------------------- | -------------------------------- | ------------------------------ | --------------------------- |
| Configured no-op local fetch      | 16.38 s (16.01–18.09), 687 MiB   | 0.07 s (0.06–0.36), 28 MiB     | 0.12 s (0.12–0.14), 14 MiB  |
| Configured one-commit local fetch | 18.02 s (17.39–18.52), 666 MiB   | 0.25 s (0.24–0.25), 67 MiB     | 0.16 s (0.15–0.16), 15 MiB  |
| Bare local clone                  | 48.12 s (47.82–49.07), 5,455 MiB | 17.76 s (17.73–17.84), 704 MiB | 3.65 s (3.63–3.68), 268 MiB |
| Full transfer preparation         | 38.84 s (38.73–41.92), 5,296 MiB | 7.78 s (7.76–10.47), 233 MiB   | 1.13 s (1.10–1.19), 267 MiB |

The no-op median and RSS meet the spec, although one latency sample exceeds its target. Incremental
latency, clone latency/RSS and writer latency remain above their targets. The clone receive phase is
about 12.0 s and finishing about 5.8 s. Receiving and installed-store graph validation remain
required. The open-time work has landed; graph location and decoding still consume time. No budget
is weakened to make these results pass. The rebased writer phase median is 2.54 s versus main's
32.47 s, and its output remains 207,416,310 bytes.

All three rebased clones also match Git's 131,152-object sets and pass `git fsck --strict`; see
[the final integrity record](transfer-reuse/rebased-large-integrity.log).

Final raw samples are in [rebased-workflow.json](transfer-reuse/rebased-workflow.json) and
[rebased-writer.json](transfer-reuse/rebased-writer.json), with adjacent logs. The
[rebased workflow script](transfer-reuse/rebased-run.py) and
[rebased writer script](transfer-reuse/rebased-writer.py) preserve the exact repetitions.

## Measurements Before Rebase

macOS 26.6.2, Apple M2 Max, arm64, Rust 1.99.0, Git 2.56.0; release builds with `debug = 1`. The
baseline is `main` at `476e705eabc7e8b31ca60d2f15ae7ecba8d89bdd`. Each run uses a fresh `cp -cR` of
`/tmp/opus-med-23/source.git`: 131,152 objects and one source pack,
`d1eb5c6452be016acbf2a5af319ef0acd4a36484`. Reads use warm filesystem cache; these are not
cold-storage results. `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1` apply to both
implementations.

Each cell reports median wall time, full range across three runs, and maximum peak RSS. Every timed
process used `/usr/bin/time -l` while owning `/tmp/girt-timing.lock`. Builds and fixture
construction occurred outside the lock. Main and Git workflow rows are retained from the initial
rotation; the changed implementation was measured again after removing the unused local have-batch
walk. The writer runs rotate main/changed/Git order between rounds.

| Operation                         | Main: median (range), RSS        | Changed: median (range), RSS   | Git: median (range), RSS    |
| --------------------------------- | -------------------------------- | ------------------------------ | --------------------------- |
| Configured no-op local fetch      | 17.14 s (16.90–17.17), 642 MiB   | 0.93 s (0.91–1.29), 27 MiB     | 0.14 s (0.14–0.14), 14 MiB  |
| Configured one-commit local fetch | 17.09 s (17.00–17.34), 637 MiB   | 1.67 s (1.13–2.08), 67 MiB     | 0.16 s (0.15–0.16), 15 MiB  |
| Bare local clone                  | 50.58 s (50.10–51.25), 5,486 MiB | 18.37 s (18.30–18.86), 730 MiB | 3.74 s (3.68–3.75), 270 MiB |
| Full transfer preparation         | 39.68 s (39.13–40.38), 5,298 MiB | 7.59 s (7.47–7.85), 233 MiB    | 1.12 s (1.11–2.06), 266 MiB |

The Git equivalents are `fetch origin`, `clone --bare --no-local`, and
`pack-objects --all --stdout`. Preparation selects every direct source reference; the writer emits
all 131,152 objects. Its changed pack is 207,416,310 bytes. Preparation includes store opening,
graph selection, location, writing and index generation. Git's preparation equivalent also includes
selection, so it is compared with the whole call rather than girt's writer phase alone.

The median writer phase falls from 33.64 s (33.12–33.86) to 2.42 s (2.39–2.43). The measured changed
clone spends about 11.8 s receiving and validating, followed by about 6.5 s finishing through the
installed-store verification and reference workflow. Pack copying does not remove either receiving
or installed-store validation.

The spec targets are no-op fetch ≤ 0.1 s/50 MiB, one-commit fetch ≤ 0.2 s, clone ≤ 8 s/300 MiB, and
pack writing ≤ 1.5 s. Only the no-op RSS target passes in this earlier run. Open-time whole-pack
validation, graph location and receiving/installed-store decoding remain costs; the related
[open-time](https://github.com/joshka/girt/issues/61) and
[decode-path](https://github.com/joshka/girt/issues/62) work was separate at that revision.
Measurements establish an improvement, not satisfaction of these budgets or a hard heap bound.

## Integrity and Fidelity

Original fixtures are generated with the Git executable, using 100 similar file revisions. SHA-1 and
SHA-256 transfers compare complete object sets against `git clone --bare --no-local`, retain
Git-produced deltas, and pass `git index-pack --strict`, `git verify-pack` and `git fsck --strict`.
All three large girt clones also contain the same 131,152 objects as their Git counterparts and pass
`git fsck --strict`; the [large-fixture record](transfer-reuse/large-integrity.log) retains those
results. Incremental tests assert zero examined objects for no-op and exactly three selected objects
for a one-file commit. Other cases cover unrelated destination history, omitted delta bases, zero
output chain depth, and a checksummed source index that claims a false whole-object identity.

Unit tests check verbatim ordinary entries and CRCs, copied logical-byte limits, source mutation and
truncation, temporary-file lifetime, cancellation/progress, and fresh delta depth. Existing repack,
HTTP, SSH, terminal ownership and reference-publication tests remain part of `just check`.

Selection, advertisement and copy share one source object snapshot. Typed edges, unadvertised-want
refusal, object/edge/payload bounds and shallow boundaries remain checked. Source v2 entry CRCs are
checked while copying. A delta reuses only an earlier base from its own source pack within the
output limit; other deltas are reconstructed. Legacy v1 indexes have no entry CRC and use verified
reconstruction. Transfer deltas use REF_DELTA framing. Both girt's receiver and Git independently
compute identities; girt imports and checks connectivity before destination mutation. Installation
keeps its existing no-clobber publication order and dependency checks.

`max_decode_bytes` provides separate allowances for native source selection/writing logical payload
lengths and receiver inflation/reconstruction work. Copied blob lengths still count. Metadata grows
with object count; decoded caches, graph payloads, fresh delta windows and allocator overhead also
consume memory. Spooling the pack does not establish a total-process memory cap.

## Validation and Reproduction

`just check` passed again after the rebase, with exit status 0, under the shared lock with warnings
rejected: formatting, the all-feature unit, integration and doctest suites, all-target Clippy, and
docs.rs. The implementation is tested on macOS; Linux and Windows runtime evidence comes from the
PR's platform CI. Windows now explicitly includes `transfer_reuse` with its native local transfer
selection. No jj repository is modified.

The first full check caught an oversized SSH future stack frame and a change to repack's existing
fresh-depth refusal. The fixes put the async upload buffer on the heap and scope fresh-depth
clamping to transfer writing. The old packed-policy size inequality was updated: both policies reuse
Git's already-deltified input, while the incremental loose-object case still checks fresh delta
search. The full check was then repeated successfully.

Raw samples are retained in [workflow.json](transfer-reuse/workflow.json) and
[writer.json](transfer-reuse/writer.json); their `log` fields identify adjacent time/phase logs. The
[driver](transfer-reuse/driver.rs) contains the three public operations. Build it as a disposable
Cargo binary with a path dependency on the desired girt checkout and `[profile.release] debug = 1`.
The [workflow fixture/timing script](transfer-reuse/run.py),
[final workflow run](transfer-reuse/resume.py), and [writer run](transfer-reuse/writer.py) retain
the exact setup and locking protocol. Adjust their `/tmp` driver paths for another host; each
results directory must be new. The main driver uses an immutable jj export of the baseline source.

The default SHA-1/v2/ordinary paths were measured before follow-up fixes to fresh-depth handling,
legacy-index handling, SSH future allocation, fixtures and documentation. Those fixes do not alter
the measured paths. No end-to-end jj timing, cold-cache comparison or native Windows/Linux timing is
claimed. Legacy v1 indexes and excluded-base deltas still require decoding and recompression.
