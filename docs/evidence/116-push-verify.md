# Native local receiver verification: issue 116

Measured on 2026-10-13 with macOS 26.6.2, Apple M2 Max, Git 2.56.0 and Rust 1.99.0.

## Workload and method

The girt-level driver performs a configured local push of ten Git-generated commits. Each commit
adds a small top-level file. Non-tip stacks fork at `master~20` (git/git) or `main~20` (131k); tip
stacks fork at the advertised branch tip. This isolates the receiver regression and excludes jj
command overhead. The original jj benchmark remains in
[issue 66](https://github.com/joshka/girt/issues/66#issuecomment-6142467038).

Inputs are the bare git/git fixture (409,143 packed objects, 1,015 refs) and the jj fixture (131,152
packed objects, 325 refs). Git creates blobs, trees and commits with `hash-object`, `mktree` and
`commit-tree`. Setup and release builds precede the timing lock. Each run has a fresh APFS `cp -cR`
receiver and source; each source has exactly one local destination URL, verified before measurement.
No clone is measured. Reads use a warm shared filesystem cache; storage cache is not flushed. Main
is `695f6586`; the runtime change is `f0197c4f`.

Three G–main–change–change–main–G rounds give six samples per tool and workload. The complete block
holds `/tmp/girt-timing.lock`; `/usr/bin/time -l` records process peak RSS. Wall time includes
process startup and synchronized publication. Load averages are recorded per sample; one-minute load
ranged from 8.5 to 24.7. Treat absolute timings as provisional on this shared machine.

## Wall time

Cells contain the median and minimum–maximum in seconds.

| Workload    |                   Main |              Change |                 Git | Change / Git |
| ----------- | ---------------------: | ------------------: | ------------------: | -----------: |
| git-git     | 49.759 (48.902–53.934) | 0.294 (0.284–0.637) | 0.266 (0.242–0.583) |        1.11× |
| 131k        |    0.164 (0.162–0.190) | 0.074 (0.074–0.076) | 0.122 (0.118–0.137) |        0.61× |
| git-git-tip |    0.590 (0.565–0.600) | 0.211 (0.204–0.217) | 0.196 (0.192–0.206) |        1.08× |
| 131k-tip    |    0.168 (0.160–0.177) | 0.072 (0.071–0.076) | 0.125 (0.114–0.139) |        0.58× |

## Peak RSS

Maximum process RSS across the six runs, in MiB.

| Workload    |  Main | Change |  Git |
| ----------- | ----: | -----: | ---: |
| git-git     | 552.1 |   85.3 | 28.8 |
| 131k        |  58.2 |   21.9 | 14.5 |
| git-git-tip | 198.8 |   75.3 | 23.8 |
| 131k-tip    |  51.8 |   22.2 | 14.4 |

The git/git change still uses about 3× Git's peak RSS. These results establish the measured
wall-time improvement; memory parity remains unresolved.

## Proof and validation

The original profile in [issue 116](https://github.com/joshka/girt/issues/116) attributes the cost
to two full destination-history reads during connectivity validation and dependency installation.
The new path proves boundaries from identity-checked receiver parent edges and verified base-tree
and subtree edges. It keeps received-object identity, typed connectivity, dependency rechecks,
synchronized publication and conditional refs. Discovery caps do not grant trust; unproven history
uses the full verifier.

Git-generated rejection tests send the same incomplete pack to girt and Git receive-pack. Tests
cover absent parents, absent unreferenced ancestry, an incomplete stored child, same-kind commit and
tree substitution, shallow roots, budgets, cancellation and fallback. The existing skewed-date and
merge cases pass. `benches/push.rs` includes a non-tip native receiver Criterion workload.

All 72 receivers have the exact pushed ref value and preserve existing object-file names and sizes.
Within each tool/workload group, all six runs have identical new object-file bytes (including pack
and index files). A representative of each of the 12 groups passes `git fsck --strict --no-reflogs`.
Final `just check` results are recorded in the pull request. No network transport or Windows
performance is measured. The non-tip results cover bases only 20 commits behind the branch tip.
Discovery proves at most 256 ancestors; stale jj bookmarks based on `master~400` or `master~1000`
can exhaust that budget and retain the old full-history verifier cost. Those workloads have not been
measured. Above 4096 ref roots, object-ID seed ordering can omit the relevant branch. External
target extraction also caps edges before deduplicating, so duplicates can leave an arbitrary subset.
Direct subtree discovery examines at most 256 subtrees; the remainder uses ordinary verification.
Pre-existing corruption in unproven history can be rejected where Git leaves it unvisited.

[Deep-bookmark and root-selection follow-up](https://github.com/joshka/girt/issues/125) records
these limits, candidate designs and restart criteria. Receiver objects in alternates use the same
store lookup, but no dedicated discovery test covers an alternate-only base. Quarantine interaction
also needs a regression test after that feature is integrated. These coverage gaps are tracked in
the follow-up.

Earlier diagnostics with duplicated origin URLs are excluded from these results. The final block
asserts a single isolated local URL and an absent destination bookmark before every run.

## Driver

The measured standalone release driver uses the same dependencies for main and the change. Its local
source has an isolated `origin` URL configured by fixture setup. Build with a path dependency on the
revision under test, outside the timing lock.

```rust
use girt::push::{ForcePolicy, PushCommand};
use girt::refs::RefName;
use girt::transfer::{Environment, NoCallbacks, PushOptions};
use girt::{Repository, Signature};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let repo = Repository::open(&args[1])?;
    let command = PushCommand {
        name: RefName::new("refs/heads/bench")?,
        expected: None,
        new: args[2].parse()?,
        force: ForcePolicy::FastForwardOnly,
    };
    let options = PushOptions::default().identity(Signature {
        name: b"bench".to_vec(),
        email: b"bench@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    });
    let report = repo.push(
        "origin",
        vec![command],
        &options,
        &Environment::from_process(),
        &mut NoCallbacks,
    )?;
    assert!(report.all_succeeded(), "{report:?}");
    Ok(())
}
```
