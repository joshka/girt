# Header-only blob scan attribution

Controlled comparison on PR #52 (opus medium), commit `03bcce40` (`issue-23-opus-med`). The
full-read variant removes only the branch shown in `full-read.patch`; ordinary reads then validate
packed blobs and charge their decoded bytes. No other implementation changes are included. This
patch is experimental evidence, not a port.

Both binaries were built in release mode before timing with rustc 1.99.0, Git 2.56.0, macOS arm64.
The driver is byte-identical to `/tmp/girt-quiet-bench/driver.rs`. Each invocation used a fresh
`cp -cR` copy of `/tmp/opus-med-23/source.git`, with fixture copying outside the timed command. The
shared `/tmp/girt-timing.lock` covered all four runs, ordered stock/full/full/stock. Warm filesystem
cache; load averages went from 7.17/7.11/5.14 to 5.13/6.53/5.04. No claim of complete machine
idleness is made.

| Variant                     | Repack seconds, run 1 | Repack seconds, run 2 |
| --------------------------- | --------------------: | --------------------: |
| Stock header-only blob walk |                  6.09 |                  5.87 |
| Ordinary full blob reads    |                 21.24 |                 20.68 |

Both variants produce the same pack bytes. Every resulting repository passes `git fsck --strict` and
every final index passes `git verify-pack`, outside the timing lock. Detailed resource outputs and
pack SHA-256 digests are in `results.json`.

This controlled change establishes a substantial causal cost of validating blob payloads in this
implementation. It does not isolate inflation from hashing or byte accounting, and it does not show
the effect of combining validation with root-closure deduplication. The remaining speed gap between
arbitrary candidate PRs cannot be assigned entirely to this branch by subtraction.

To reproduce, apply the patch to the recorded candidate, build the supplied driver against that
local library and save it as `full52`; build the unmodified candidate as `stock52`. The recorded
`run.sh` shows the exact timing commands and lock discipline. Its `/tmp` paths are local fixture
locations, not repository inputs or externally downloaded data.
