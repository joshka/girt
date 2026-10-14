# One-commit fetch validation walk (#117)

`jj git fetch` of one new commit that adds one blob at the root, as `scripts/jj-e2e/bench-ops.py`
measures `fetch-1`. Origins are the bare git/git and jj fixture (131k objects) repositories from the
everyday-operation benchmark. [`117-fetch-walk.py`](117-fetch-walk.py) prepares one clone per tool
outside the lock: a jj clone, or `git clone --no-local`, plus one no-op fetch. Then, holding
`/tmp/girt-timing.lock`, it runs the slots G A B B A G with three samples each. Every sample copies
its template with `cp -cR`, points the clone's remote at the copied origin, adds the origin commit
and times the fetch with `/usr/bin/time -l`. It fails unless the fetch moved the remote-tracking
branch to the new commit. Caches are warm.

G is Git 2.56.0. A and B are jj `joshka/girt-backend` `6da19de6` release builds patched to girt
`main` `9c8750a7` and to this change. The machine is an Apple M2 Max running macOS 26.6.2, with a
one-minute load average of about 5.

| Repository | Tool   | Median s (range)  | Peak RSS MiB (median, max) |
| ---------- | ------ | ----------------- | -------------------------- |
| git/git    | Git    | 0.140 (0.13–0.16) | 29, 29                     |
| git/git    | main   | 0.960 (0.95–1.17) | 258, 268                   |
| git/git    | change | 0.340 (0.33–0.39) | 159, 161                   |
| jj fixture | Git    | 0.060 (0.06–0.06) | 14, 14                     |
| jj fixture | main   | 0.265 (0.26–0.28) | 90, 93                     |
| jj fixture | change | 0.140 (0.14–0.14) | 74, 80                     |

Raw samples are in [`117-fetch-walk.json`](117-fetch-walk.json).
