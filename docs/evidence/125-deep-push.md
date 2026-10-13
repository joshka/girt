# Deep native local push: issue 125

Measured on 2026-10-13 with macOS 26.6.2, Apple M2 Max, Git 2.56.0 and Rust 1.99.0. Main is
`e273ef64`; the change retains identity-checked ref-rooted connectivity proof and removes internal
ancestor, root-seed and external-target cutoffs. Caller resource limits remain.

## Workload and method

The driver from [issue 116's evidence](116-push-verify.md#driver) performs a configured local push
of ten Git-generated commits, each adding a small top-level file. Bases are `master~400` and
`master~1000` on git/git (409,143 packed objects, 1,015 refs), and `main~400` and `main~1000` on the
131,152-object fixture (325 refs). Git creates objects with `hash-object`, `mktree` and
`commit-tree`; no upstream implementation or test source is used. Every sample uses fresh APFS
`cp -cR` copies, with exactly one isolated local destination URL and an absent destination bookmark
asserted before measurement. Setup and release builds are outside the lock.

Three G–main–change–change–main–G rounds provide six samples per tool/workload. The block holds
`/tmp/girt-timing.lock`; `/usr/bin/time -l` records peak process RSS. Wall time includes startup and
synchronized publication. Reads use a warm shared filesystem cache; caches are not flushed. The
shared machine's one-minute load ranged from 3.8 to 25.8. A 0.982 s outlier is retained. Raw samples
are in [125-deep-push.json](125-deep-push.json).

## Wall time

Cells contain median and minimum–maximum seconds.

| Workload                 | Main                   | Change              | Git                 | Change / Git |
| ------------------------ | ---------------------- | ------------------- | ------------------- | ------------ |
| git-git-400              | 0.361 (0.323–0.650)    | 0.365 (0.326–0.617) | 0.340 (0.279–0.357) | 1.07×        |
| git-git-1000             | 0.345 (0.327–0.358)    | 0.331 (0.328–0.345) | 0.293 (0.275–0.308) | 1.13×        |
| 131k-400                 | 0.154 (0.147–0.207)    | 0.157 (0.148–0.982) | 0.262 (0.247–0.276) | 0.60×        |
| 131k-1000                | 0.165 (0.158–0.176)    | 0.164 (0.158–0.185) | 0.271 (0.263–0.283) | 0.61×        |
| git-git-1000-master-only | 59.065 (50.832–63.593) | 0.265 (0.245–0.353) | 0.465 (0.213–0.749) | 0.57×        |

All measured medians meet the 2× Git budget. Main is already fast in these fixtures: their existing
tag roots provide boundaries closer than the branch tip. The master-only workload removes other
receiver refs while retaining all objects; it isolates the cliff and reduces the median from 59.065
s to 0.265 s. The Git-generated `master~1000` regression has only one receiver branch: Git accepts
an old missing tree outside the new graph, main rejects it with `Install(Missing)`, and the change
accepts it. The intact variant passes Git fsck.

## Peak RSS

Maximum process RSS across six samples, in MiB.

| Workload                 | Main  | Change | Git  |
| ------------------------ | ----- | ------ | ---- |
| git-git-400              | 94.7  | 94.6   | 31.5 |
| git-git-1000             | 89.1  | 90.2   | 31.0 |
| 131k-400                 | 33.5  | 33.3   | 19.6 |
| 131k-1000                | 32.2  | 34.3   | 19.8 |
| git-git-1000-master-only | 469.7 | 78.9   | 32.2 |

The change still uses approximately 3× Git's RSS on git/git. Memory parity remains unresolved.

## Proof and validation

Discovery follows identity-checked, typed commit parent edges from every existing verified ref root,
then verifies base trees and direct subtrees. Dates only prioritize work. External commit targets
exclude received objects and are deduplicated before discovery. Caller object, byte and edge limits,
per-object decoding limits, cancellation and shallow boundaries remain. Failed proof reads establish
no boundary. Unproven branches still use complete verification. Dependency rereads, synchronized
additive pack publication, receive policy and conditional refs remain.

Acceptance tests use Git-generated deep history, omitted parents and incomplete unreferenced
history. They also cover 4,100 receiver roots with the required root last in object-ID order, and an
alternate-only non-tip base. Unit tests retain commit/tree substitution, shallow, cancellation and
resource coverage and add more than 256 unique targets and parent-edge limits. Criterion's
`push/native-local-non-tip-{depth}-{count}` workloads cover depths 1, 400 and 1000 with fixture
construction outside the measured operation.

All 90 measured receivers retain existing object filenames and sizes; representatives of all 15
tool/workload groups pass `git fsck --strict --no-reflogs`. The retained APFS harness successfully
replays the 131k depth-1000 workload. The locked Criterion depth-1000 workloads complete with ten
samples each (18.217 ms for the 16-object fixture and 17.038 ms for the 256-object fixture).

No public API or dependency changes are introduced. No network or Windows performance is measured.
The receiver trusts referenced history as complete, as Git does; this is not a destination fsck.
After rebasing over receive quarantine (`d2ab7989`), a Git-generated deep push succeeds with a
pre-receive hook observing the incoming commit in quarantine. An incomplete unreferenced-history
pack is rejected before that hook runs, without ref publication, primary incoming objects or a
remaining quarantine directory. The original timing baseline predates quarantine; its timings are
retained as historical evidence. Native CI and final local validation are reported in the pull
request.

## Reproduction

[The APFS comparison harness](../../benches/deep_push.py) prepares fresh copies and runs the same
rotated samples. Build main/change release drivers using the linked driver source before taking the
lock. Use a baseline workspace at `e273ef64`, rather than an unrelated working copy.

```sh
python3 benches/deep_push.py prepare --output target/deep-push \
  --git-git /path/to/gitgit.git --fixture-131k /path/to/131k.git
until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
trap 'rm -f /tmp/girt-timing.lock' EXIT
python3 benches/deep_push.py run --output target/deep-push \
  --main-driver /path/to/main-driver --change-driver /path/to/change-driver
```

The matching Criterion workload is `cargo bench --bench push -- native-local-non-tip-1000`. Run it
under the same timing lock after compiling outside the lock. The standalone comparison includes
process startup and RSS; Criterion samples the native receive operation without fixture setup. They
measure different boundaries.
