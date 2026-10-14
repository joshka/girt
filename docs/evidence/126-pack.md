# Pack references after fetch (#126)

`126-pack-bench.py` compares jj `status`, `log`, `new` and a one-commit fetch on git/git.

## Setup

- The block order is G A B B A G, with five samples per operation in each slot.
  - G is Git and stock jj.
  - A is jj on girt `main` at `8c2e992c`.
  - B is jj on this branch's girt, plus the jj change in `126-jj-auto-pack.patch`.
- Both jj on girt builds are `joshka/girt-backend` `0bb6a21d`, with girt patched through
  `patch.crates-io`. Stock jj is the parent of that commit.
- Each tool's base comes from its own clone, colocated by default, followed by one `jj new`.
- Every sample runs on a fresh `cp -c` APFS copy of that base, so the caches are not controlled.
  Copying is not timed.
- The whole timing block holds `/tmp/girt-timing.lock`.
- The load average was about 7, so treat absolute times as provisional. The comparisons hold because
  the slots alternate.

## Results

Ref layout of each base:

| tool      | loose | packed |
| --------- | ----: | -----: |
| Git       |     2 |   1015 |
| stock jj  |  2019 |   1016 |
| jj + main |  3035 |      0 |
| jj + this |     2 |   3033 |

Medians and peak RSS (raw records in `126-pack.json`):

| op               | Git              | stock jj        | jj + girt main    | jj + this branch | this / main | this / stock |
| ---------------- | ---------------- | --------------- | ----------------- | ---------------- | ----------: | -----------: |
| status           | 535 ms / 12 MiB  | 150 ms / 48 MiB | 208 ms / 96 MiB   | 93 ms / 92 MiB   |       0.45x |        0.62x |
| log              | 540 ms / 129 MiB | 158 ms / 49 MiB | 217 ms / 96 MiB   | 99 ms / 93 MiB   |       0.46x |        0.63x |
| new              | 537 ms / 12 MiB  | 204 ms / 54 MiB | 288 ms / 123 MiB  | 182 ms / 120 MiB |       0.63x |        0.89x |
| one-commit fetch | 280 ms / 54 MiB  | 863 ms / 54 MiB | 1328 ms / 265 MiB | 965 ms / 263 MiB |       0.73x |        1.12x |

On fresh copies, Git's `status` and `commit` refresh the index for the whole tree, which explains
their high times here.

## Limitations

- One-commit fetch is still 3.4× Git in time and 4.9× in peak RSS. The remaining cost is not
  reference layout and needs its own profile (#117).
- The jj change can't land until a girt release includes `Repository::auto_pack_references`.
