# Shared Retention Plans and Located Blobs

This report records the implementation of [issue 24](https://github.com/joshka/girt/issues/24)
tracked by [issue 66](https://github.com/joshka/girt/issues/66). The issue's spec and comments
precede implementation. Its decisions authorize located blobs, one shared plan, and protection of
recent packs without treating every packed ID as a live traversal root.

## Supported Behavior

`Repository::collect_garbage` expires histories and prunes loose objects under one caller-owned
isolation guard. `collect_garbage_with_checkpoint` adds a synchronous observation checkpoint before
mutation. `run_maintenance` shares the same plan with pack retirement. Standalone phases make one
plan each. SHA-1 and SHA-256 use the same contracts; destructive composites remain Unix-only.

Planning locates blobs through the same store precedence as ordinary reads. It checks canonical
loose headers, indexed packed headers, delta ancestry, existence, kind and declared object size.
Commits, trees and tags still decode, hash and parse. Blob-shaped untyped roots use ordinary
identity-verified reads because no verified parent supplies their kind. This necessary safety
exception is recorded in an
[issue comment](https://github.com/joshka/girt/issues/24#issuecomment-6121779312).
`RetentionPolicy::max_bytes` counts commit/tree/tag payloads only, including when a blob root must
be decoded. Blob edges are located without payload, complete-zlib, CRC or identity validation.
Normal reads retain those validations, and retirement verifies every retained survivor before
unlinking an old pair.

Recent loose objects remain strong roots. Recent and `.keep` packs survive unchanged but do not make
disconnected reflogs live. When deletable storage exists, a separate protection closure keeps their
descendants, including objects held only in old loose files or other packs. Raw pinned-pack
checksums and CRCs authenticate headers before the protection inventory selects graph-bearing
traversal roots. Blob IDs are protected directly. Incomplete protection refuses mutation. This
closes a missing failure case in the original spec, recorded in an
[issue comment](https://github.com/joshka/girt/issues/24#issuecomment-6120105360).

Input fingerprints cover root-bearing file bytes, HEADs, worktree metadata and indexes, reflogs,
reftable stacks, shallow state, selected configuration sources, primary/alternate inventories,
protection markers, pack/index generations, loose generations and alternate topology. Unix stamps
also record device, inode and change time. Exact retained reflog/reftable outputs advance the
fingerprint; unrelated changes refuse. The isolated caller retries with a fresh plan, resolving the
spec's conflicting re-plan and `Changed` acceptance wording conservatively. Fingerprints do not
exclude external writers or replace the isolation contract.

Retirement has a read-only fast path when no old pair is eligible and all required IDs are already
located in protected primary packs. It returns `published: None`, `durable: false` and no removals.
Nothing is copied or deleted, so location-only evidence does not authorize a storage mutation. The
full survivor checks remain on every deleting path. This additional measured cost and design are
recorded in an [issue comment](https://github.com/joshka/girt/issues/24#issuecomment-6120564275).

A located blob must not shadow a graph-bearing copy at the same ID. The plan keeps kind information
and checks other pack and loose headers, including alternates, before declaring completion. A
mislabelled packed blob that hides a healthy commit now refuses before expiry or pruning. The
regression first demonstrated actual descendant deletion in both formats; the
[spec correction](https://github.com/joshka/girt/issues/24#issuecomment-6121437368) records the
missing kind-consistency boundary. Blob edges remain uninflated. Duplicate-copy inventories can add
metadata work in stores with multiple packs or many loose copies.

## Acceptance Evidence

The initial acceptance tests were committed at `cb17e8f1` before implementation. They fail to
compile on main because its composite GC methods and errors do not exist. Tests run Git's public
executable to create object graphs and check surviving bytes, object sets and histories; no Git
implementation or test source was used.

Both-format cases cover ref creation, existing reflog append, pack publication, index publication,
shallow changes, alternates metadata and loose publication after planning. A separate checkpoint
changes a ref after expiry and confirms pruning refuses with expiry's completed report. Other cases
cover deeply nested trees, annotated tags targeting blobs, missing reachable blobs,
commit/tree/tag-only byte accounting, entry limits, one isolation acquisition, reflog-only
retention, Git GC object-set/reflog comparison, configuration symlink target changes, old loose
descendants of protected packs, and read-only retirement without new artifacts. The two new
read-only retirement cases fail on main by observing unnecessary publication before the fast path is
implemented.

The existing retention integrity and survivor suites retain damaged-entry, mislabelled-base,
lookup-precedence, logical-byte, delta-depth, protected-copy, crash-order and partial-effect
coverage. CI runs the full Unix suites on Ubuntu and macOS, portable/refusal tests on Windows, MSRV
compilation and units, and all-feature/all-target Clippy. Windows does not establish runtime support
for destructive composites.

## Measurements

Wall medians, ranges and maximum process RSS on the large fixture:

| Operation                              | Main                              | This change                    | Git                            |
| -------------------------------------- | --------------------------------- | ------------------------------ | ------------------------------ |
| Expiry + prune / composite GC          | 52.00 s (51.32–56.70), 501.14 MiB | 2.91 s (2.65–2.93), 148.70 MiB | 1.52 s (1.50–1.58), 283.67 MiB |
| Additive repack, non-recent cutoff     | 7.39 s (7.31–7.46), 408.50 MiB    | 3.46 s (3.43–4.59), 183.94 MiB | 1.41 s (1.40–1.41), 284.09 MiB |
| Protected-pack maintenance             | Not re-measured                   | 2.71 s (2.70–2.71), 150.81 MiB | Full GC above                  |
| Additive repack, default recent cutoff | 14.88 s (14.80–15.11), 426.80 MiB | 3.44 s (3.36–3.44), 184.05 MiB | Repack above                   |
| Retention plan, default recent cutoff  | Not re-measured                   | 2.69 s (2.59–2.70), 148.69 MiB | Not equivalent                 |

GC main has six samples across the initial and final ABBA rotation. Every other table entry has
three samples. The final safety-guarded implementation samples are in
[guarded timings](../benchmarks/issue-24/guarded-timings.json);
[controls](../benchmarks/issue-24/final-timings.json) and
[initial observations](../benchmarks/issue-24/initial-timings.json) retain the comparison and design
evidence. The 27.68-second intermediate maintenance median predates the read-only fast path and is
not a result for the final implementation. The final composite meets the 3-second/300-MiB budget;
protected-only maintenance also meets the 10-second budget. These are fixture results, not latency
guarantees for repositories that require replacement/survivor decoding.

The [source manifest](../benchmarks/issue-24/source.sha256) identifies the measured implementation,
Cargo inputs, benchmark and fixture builder. Verify it from the repository root with
`shasum -a 256 -c docs/benchmarks/issue-24/source.sha256`.

A fresh large rewritten fixture also passes `git fsck --strict`. The timed copies pass Git's
connectivity check after each operation. Complete local and native validation is recorded on the
[pull request](https://github.com/joshka/girt/pull/71).

The fixture is `/tmp/opus-med-23/source.git`, an existing bare Git-created clone of public jj
history: 131,152 objects, 325 refs, one 199.67 MiB pack, no loose objects or reflogs. Every run uses
a new APFS `cp -cR` copy. Source pack name is `pack-d1eb5c6452be016acbf2a5af319ef0acd4a36484.pack`.
Reads are warm; no cache eviction is claimed. Main is `3a90f06901218782faf7adbb4f4d921451ad9f59`,
after lazy whole-pack validation landed.

Platform: Apple M2 Max, macOS 26.6.2 (`25G83`), aarch64, Rust 1.99.0 (`b940084d7`), Git 2.56.0.
Builds finish before measurement. Each `/usr/bin/time -l` run takes and releases
`/tmp/girt-timing.lock`; the lock is not held during fixture setup, compilation or Git checking. The
[driver sources and run order](../benchmarks/issue-24/) retain the public calls measured. Baseline
GC calls the two standalone phases; this PR calls the composite. Git GC is `git gc --prune=now`;
Git's additive comparison is `git repack -ad`. All girt calls use trusted limits with their default
cutoffs. The read-only Criterion workload is `cargo bench --bench retention -- gc/`; it uses the
independent 256-commit fixture with construction outside sampling.

## Limits and Consumer Follow-up

Finite fixtures do not prove all Git expiry heuristics. Existing conservative classification keeps a
record live if either endpoint is reachable from a strong root; this rule remains unchanged.
Fingerprints rely on immutable object artifacts and a trusted local filesystem. Metadata observers
refuse sources they cannot inventory completely. Unknown hooks, precious objects, partial clones,
unsupported durability and incomplete stores still block mutation.

jj must replace its separate expiry/prune calls with `collect_garbage` while holding one `gc.pid`
guard and satisfy the existing external-writer/reader exclusion contract. `gc.pid` alone cannot
exclude arbitrary Git writers; [B09](../jj-roadmap.md#b09--ordinary-jj-maintenance-isolation)
remains open. That jj change and an actual `bench-large.sh` run with the composite consumer are
outside this PR: the user prohibited jj changes. The girt public-API fixture measurements establish
the library budget; they do not claim measured jj command latency. Read-path hashing policy in
[issue 62](https://github.com/joshka/girt/issues/62) remains unchanged.
