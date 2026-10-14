# Local clone of git/git with jj on girt (#119)

Evidence for the stack #130–#133. Raw per-run records: [119-clone.json](119-clone.json). Driver:
[119-clone-bench.py](119-clone-bench.py).

## Setup

- Host: macOS 26.6.2 arm64 (Apple M2 Max, 12 cores), Git 2.56.0, rustc 1.99.0.
- jj: `joshka/girt-backend` at `0a1a36e0` (jj 0.45.1 base `35dbc362`), built release with debug
  info. Locally its girt requirement is raised to 0.6 and `git_maintenance.rs` adapts to the 0.5
  reflog expiry API; neither change is pushed. Each girt build patches crates.io girt to a local
  tree, and the build fails if `Cargo.lock` still names a registry source.
- `girt-new`: this stack on main `ea5e1bd0` (v0.6.3). `girt-main`: main `ea5e1bd0`.
- `stock`: jj `35dbc362` (gix). `git`: `git clone -q --no-local`.
- Origins: bare git/git (409,143 objects in one 301 MiB pack, 1,015 refs) and the jj fixture
  `/tmp/opus-med-23/source.git` (131,152 objects, 325 refs). Clones only read the origin, so it is
  not copied per run. Page cache warm.
- Command: `jj --config user.name=b --config user.email=b@b git clone ORIGIN DEST` with
  `JJ_CONFIG=/dev/null`, under `/usr/bin/time -l`.
- Five runs per tool, tool order rotated each round, each run holding `/tmp/girt-timing.lock`. Other
  sessions kept the load average around 8–13 throughout, so absolute times are provisional; the lock
  does not exclude unrelated host work. Every jj clone passed `git fsck --strict`.

## Results

| repo       | tool      | median s |     range s | peak RSS MiB | runs s                        |
| ---------- | --------- | -------: | ----------: | -----------: | ----------------------------- |
| git-git    | girt-new  |    14.85 | 14.74–15.34 |          578 | 15.34 14.74 14.85 14.82 14.93 |
| git-git    | girt-main |    39.68 | 39.47–40.22 |         3658 | 39.68 39.47 39.54 39.97 40.22 |
| git-git    | stock     |    14.60 | 14.48–14.97 |          884 | 14.51 14.48 14.97 14.85 14.60 |
| git-git    | git       |    11.55 | 11.48–11.60 |          872 | 11.48 11.54 11.57 11.55 11.60 |
| jj-fixture | girt-new  |     4.05 |   4.02–5.38 |          387 | 5.38 4.06 4.05 4.02 4.04      |
| jj-fixture | girt-main |     7.25 |   7.16–7.55 |          543 | 7.54 7.55 7.16 7.24 7.25      |
| jj-fixture | stock     |     4.21 |   4.20–4.30 |          269 | 4.30 4.26 4.20 4.21 4.21      |
| jj-fixture | git       |     3.44 |   3.43–3.49 |          266 | 3.49 3.46 3.44 3.43 3.44      |

On git/git the stack is 1.29× Git and 0.66× Git's peak RSS, at parity with stock jj (1.02×), against
2.67× slower and 6.3× more memory on main. On the fixture it is 1.18× Git, faster than stock jj.
