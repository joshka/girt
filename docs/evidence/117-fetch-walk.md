# One-commit fetch validation walk (#117)

`jj git fetch` of one new commit that adds one blob at the root, as `scripts/jj-e2e/bench-ops.py`
measures `fetch-1`. Origins are the bare git/git and jj fixture (131k objects) repositories from the
everyday-operation benchmark. [`117-fetch-walk.py`](117-fetch-walk.py) prepares one clone per tool
outside the lock: a jj clone, or `git clone --no-local`, plus one no-op fetch. Then, holding
`/tmp/girt-timing.lock`, it runs the slots G S A B B A S G with three samples each. Every sample
copies its template with `cp -cR`, points the clone's remote at the copied origin, adds the origin
commit and times the fetch with `/usr/bin/time -l`. It fails unless the fetch moved the
remote-tracking branch to the new commit. Caches are warm.

G is Git 2.56.0 and S is stock jj built from `35dbc362` (gix). A and B are jj `joshka/girt-backend`
`6da19de6` release builds patched to girt `main` `9c8750a7` and to this change. The machine is an
Apple M2 Max running macOS 26.6.2, with a one-minute load average of about 6.

| Repository | Tool      | Median s (range)  | Peak RSS MiB (median, max) |
| ---------- | --------- | ----------------- | -------------------------- |
| git/git    | Git       | 0.130 (0.13–0.13) | 29, 29                     |
| git/git    | stock jj  | 0.250 (0.25–0.28) | 50, 51                     |
| git/git    | girt main | 0.965 (0.95–0.97) | 255, 269                   |
| git/git    | change    | 0.340 (0.33–0.35) | 157, 160                   |
| jj fixture | Git       | 0.065 (0.06–0.07) | 14, 14                     |
| jj fixture | stock jj  | 0.140 (0.13–0.17) | 30, 30                     |
| jj fixture | girt main | 0.280 (0.27–1.35) | 94, 97                     |
| jj fixture | change    | 0.140 (0.13–0.14) | 68, 74                     |

Raw samples are in [`117-fetch-walk.json`](117-fetch-walk.json).
