# One-commit fetch with jj on girt (#117)

Evidence for the PR fixing #117. Driver: [117-fetch-bench.py](117-fetch-bench.py); raw samples, one
JSON object per run: [117-fetch.jsonl](117-fetch.jsonl).

## Setup

- Host: macOS 26.6.2 arm64 (Apple M2 Max), Git 2.56.0, rustc 1.99.0; load average about 5–10.
- jj on girt: `joshka/girt-backend` `0a1a36e0`, built release against the girt tree under test
  through a local `[patch]` (its girt requirement raised to 0.6 locally; not pushed). `girt-main` is
  main `297a2989`, which includes the #119 stack; `girt-pr` is this change. Stock jj: `35dbc362`
  (gix).
- Binary SHA-256 (the same in every sample):
- `girt-main`: `264745a11ae080970cd51aba173f9d3d736dd94e9623827f19b31dd0d67543bd`
- `girt-pr`: `c32ed6d16673ac7803f25ed0f208d39185fbeef798924a9e4b68795078c0267b`
- `stock`: `a6cde35cb6d435031f513e2b7e92cca578fa17f3e6621002bda30f53602a9989`
- `git`: `git version 2.56.0`
- The driver clones each origin once per tool, then for every sample copies that base with `cp -cR`,
  adds one origin commit that adds one blob, and times `jj git fetch` or `git fetch -q`. jj runs
  with `JJ_CONFIG=/dev/null`; Git with an empty global and system config (the host's global config
  starts an fsmonitor daemon and auto-maintenance, about 0.9 s that is not fetch work). Each sample
  holds `/tmp/girt-timing.lock` from copy to timing.
- `fresh`: the copy as made. Files are new APFS clones read for the first time; this is not a
  controlled cold cache. `warm`: references, `packed-refs`, pack indexes and `.jj` read once before
  timing.
- Origins: bare git/git (409,143 objects, 1,015 refs) and the jj fixture (131,152 objects, 325
  refs). Five runs per tool and condition, tool order rotated.

Reference layout of each tool's clone:

| repo    | tool    | loose refs | `packed-refs` lines |
| ------- | ------- | ---------: | ------------------: |
| git-git | girt-pr |       3032 |                   0 |
| git-git | stock   |       2016 |                2028 |
| git-git | git     |          0 |                2027 |
| fixture | girt-pr |        707 |                   0 |
| fixture | stock   |        381 |                 328 |
| fixture | git     |          2 |                 327 |

## Results

Median seconds (range); ratios use medians.

| repo    | cache |        girt main |      this change |         stock jj |              git | change / stock | change / git |
| ------- | ----- | ---------------: | ---------------: | ---------------: | ---------------: | -------------: | -----------: |
| git-git | fresh | 0.76 (0.76–0.77) | 0.51 (0.50–0.52) | 0.77 (0.76–0.78) | 0.08 (0.07–0.09) |          0.66× |         6.4× |
| git-git | warm  | 0.48 (0.48–0.49) | 0.26 (0.26–0.27) | 0.55 (0.53–0.56) | 0.04 (0.04–0.05) |          0.47× |         6.5× |
| fixture | fresh | 0.21 (0.21–0.22) | 0.16 (0.15–0.16) | 0.22 (0.21–0.23) | 0.05 (0.05–0.06) |          0.73× |         3.2× |
| fixture | warm  | 0.18 (0.18–0.18) | 0.11 (0.10–0.11) | 0.19 (0.19–0.22) | 0.04 (0.04–0.05) |          0.58× |         2.8× |

The issue's acceptance, within 2× of Git, is **not met**: the change is 6.4–6.5× Git on git/git and
2.8–3.2× on the fixture.

## Relation to the issue's original measurement

The issue reported stock jj at 0.17 s and jj on girt at 1.05 s for this operation on git/git, using
`scripts/jj-e2e/bench-ops.py`. That harness runs each operation in sequence in one clone, so the
fetch follows earlier commands that read the same files, and it measured girt at `14077e32`. Here
stock jj takes 0.55–0.77 s with the same stock revision. The two experiments differ in repository
history before the fetch, cache state and girt revision, but those differences have not been shown
to explain a 3–4× change in stock jj, so the discrepancy remains open. This evidence supports only
the relative change between `girt-main` and `girt-pr` under the conditions above, and that jj on
girt is now faster than stock jj under them; it does not establish that the original stock-jj gap is
resolved.
