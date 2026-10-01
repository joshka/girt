# jj on girt: status

Status of replacing gix, gix-ignore and the Git subprocess in jj with girt. Follow-up work is
tracked as [GitHub issues](https://github.com/joshka/girt/issues?q=is%3Aissue+label%3Ajj); read
[Working on an item](#working-on-an-item) before starting one.

## Where things stand

- **jj branch:** [`joshka/girt-backend`](https://github.com/joshka/jj/tree/joshka/girt-backend) on
  the `joshka/jj` fork. It's one commit on jj `main` at `35dbc362`, depending on girt 0.3.1 from
  crates.io. It's experimental and not proposed upstream. The commit message lists behavior changes
  and limitations.
- **girt:** 0.3.1 is published from `main`. The Windows test fixes after 0.3.1 are unreleased; they
  wait in release-plz's release PR.
- **Contract:** met (see below). jj passes its full workspace suite (3386 tests), Clippy, the
  end-to-end script (196 checks) and the no-`git` tripwire against published girt.
- **Performance:** behind stock jj on large repositories. That's the main remaining work; see
  [open work](#open-work).

## Completion contract

Met on 2026-09-30. Done when all of the following hold:

1. jj's production code has no gix, gix-ignore, libgit2 or Git subprocess use.
2. jj's existing library and CLI tests for Git, Gerrit, workspaces, colocation, ignores and GC pass
   on macOS, using both SHA-1 and SHA-256 where the tests cover both.
3. A scripted end-to-end run passes against real Git, alternating jj and Git operations: init,
   colocate, commit, export, clone, fetch, push, workspaces and GC.
4. The girt dependency is reproducible: a published girt release.

Tests may use gix and the Git executable only as oracles and fixture builders.

Explicitly excluded; recorded as limitations, not counted as parity:

- Windows (girt's own CI passes on Windows; jj on girt is untested there).
- Servers that support only protocol v2 for fetch.
- Partial clone and filters.
- Submodule recursion.
- `git-remote-*` helpers.
- Attribute filters during copy detection.
- Byte-identical `git gc` packing.

## Large-repository benchmark

`scripts/jj-e2e/bench-large.sh` on `github.com/jj-vcs/jj` (about 130k objects and 1,500 branches).
Release builds on macOS arm64. The girt column is jj on girt 0.3.1:

| Step        | jj on girt 0.3.1 | stock jj 0.45.1 (Git) |
| ----------- | ---------------- | --------------------- |
| clone       | 20.0 s / 4.7 GB  | 13.8 s / 110 MB       |
| no-op fetch | 2.1 s / 98 MB    | 0.67 s / 28 MB        |
| `util gc`   | 75 s / 365 MB    | 2.3 s / 262 MB        |

The girt `util gc` row is reflog expiry and loose-object pruning only; it doesn't repack
([#23](https://github.com/joshka/girt/issues/23)). Small fixtures hid every one of these costs, so
include this benchmark in the acceptance check of any change touching fetch, object reading, packing
or retention.

## Open work

Tracked as GitHub issues with the `jj` label, each a self-contained brief with acceptance criteria.
Rough priority order:

| Issue                                           | Title                                                       |
| ----------------------------------------------- | ----------------------------------------------------------- |
| [#22](https://github.com/joshka/girt/issues/22) | Stream received packs instead of holding them in memory     |
| [#23](https://github.com/joshka/girt/issues/23) | Repack by reusing existing deltas, then let jj repack again |
| [#24](https://github.com/joshka/girt/issues/24) | Make retention fast enough for jj util gc                   |
| [#25](https://github.com/joshka/girt/issues/25) | Push packs with deltas                                      |
| [#26](https://github.com/joshka/girt/issues/26) | Split the jj-on-girt commit into reviewable steps           |
| [#27](https://github.com/joshka/girt/issues/27) | Stop the SSH test fixture leaking sshd                      |
| [#28](https://github.com/joshka/girt/issues/28) | Make SSH/HTTP deadline tests robust under load              |
| [#29](https://github.com/joshka/girt/issues/29) | Implement built-in credential helpers natively              |
| [#30](https://github.com/joshka/girt/issues/30) | Close smaller jj-on-girt gaps                               |
| [#31](https://github.com/joshka/girt/issues/31) | Housekeeping after the jj replacement                       |

## Working on an item

- **One item per session**, in its own jj workspace: `jj workspace add work/<name> -r main` in the
  girt repo. In the jj repo, work from the `joshka/girt-backend` change. Keep build output out of
  repository roots (see `.git/info/exclude`).
- **Toolchain:** girt needs Rust 1.97.1 or newer. If a mise shim pins an older Rust, put
  `~/.cargo/bin` first on `PATH` and unset `RUSTUP_TOOLCHAIN`.
- **girt checks:** `just check`, or `cargo nextest run --all-features` plus `just clippy` and
  `just docs-rs`. CI also runs Windows, MSRV and feature subsets; see
  `.github/workflows/validation.yml`.
- **jj against unreleased girt:** build with
  `--config 'patch.crates-io.girt.path="<girt workspace>"'`, so jj's manifest still names the
  crates.io version. Restore `Cargo.lock` before committing.
- **jj acceptance:**
  - `cargo nextest run --workspace`;
  - `scripts/jj-e2e/e2e.sh <jj>` (196 checks);
  - `scripts/jj-e2e/no-git.sh <jj>` (zero Git invocations);
  - `scripts/jj-e2e/bench-large.sh` against stock jj for performance work.
- **Shipping:** open girt PRs against `main` and merge them with rebase, keeping meaningful commits.
  release-plz opens a release PR, and merging it publishes. Then bump jj's `girt` version, rerun jj
  acceptance, and force-push `joshka/girt-backend`. Push jj only to the `joshka/jj` fork, never
  upstream.
- **Close out:** reference the issue in the PR (`Fixes #N`), record evidence there, and update this
  file's benchmark or history when the change affects them.

## History

- **Approach:** replaced gix and the Git subprocess directly on jj `main`, rather than continuing
  Codex's opt-in "admission" stack. See the [retrospective](codex-retrospective.md).
- **girt 0.3.0:** configured-remote transfers (`Repository::{fetch, push, remote_head}`), `init`
  like `git init`, receive hooks on local push, racy-git index smudging, Git-accurate config edits,
  error chains without repeated sources, and consumer APIs. The consumer APIs are `trusted()`
  limits, `logs_updates_to`, `list_namespace_observations` and `InitOptions::defaults_from`. Push
  preparation reads only new history: one commit over a 5,000-commit history reads 232 objects
  instead of 25,005.
- **girt 0.3.1:** found by the large-repository benchmark:
  - trusted limits for transfers, after clone of `jj-vcs/jj` failed on a decode limit;
  - fetch publication stops at objects existing references name (a no-op fetch went from 26 s to 2
    s);
  - packs are read before loose objects;
  - a delta base cache, with only the requested object's identity checked.
- **After 0.3.1:** girt's CI passes on Windows, after fixing build warnings, Windows paths in test
  config and lock retries on Windows.
- **jj adapter decisions:**
  - commit header bytes match gix's, so commit IDs are unchanged;
  - copy detection matches gix (postimage sources, exact-only binary matching);
  - the default push remote is `remote.pushDefault`, else `origin`;
  - GC takes over stale `gc.pid` locks as Git does, checked against `git gc`.
- **Details:** commit messages and PR descriptions in both repositories record the evidence for each
  change.

## jj improvements found along the way (independent of girt)

Issues in jj itself surfaced by this work. They would be worth fixing even if girt is never adopted.
They are only noted here, not fixed.

- **Clone doesn't detect the remote's object format.** `jj git clone` of a SHA-256 remote creates a
  SHA-1 repository, then fails with Git's "mismatched algorithms" error. jj could learn the format
  from the advertisement before initializing (jj issue #3813).
- **Default push remote relies on a gix heuristic.** `gerrit upload` picks a remote with gix's
  `remote_default_name(Push)`, which also chooses the only configured remote. Git's rule is
  `remote.pushDefault`, then `origin`. The behavior differs when the sole remote isn't `origin`.
- **Tests snapshot dependency error text.** Snapshots embed gix's and Git's error wording and
  error-chain structure (e.g. `test_git_remotes`, `test_git_init_with_invalid_gitlink`, refspec
  warning order). Any backend change churns them; asserting on jj's own error kind would be less
  brittle.
- **Lib unit tests aren't hermetic.** `lib/src/git_backend.rs` tests read the user's `~/.gitconfig`
  (only `testutils::hermetic_git` isolates configuration), so results can depend on the developer's
  machine.
- **Environment-option tests are coupled to the Git subprocess.** They assert that `GIT_TRACE`
  produces a trace file, which tests an implementation detail rather than jj's contract of passing
  the environment to transport programs.
- **Remote rename copies global remotes into local config.** Already a documented divergence in
  `test_git_remote_with_global_git_remote_config`: renaming a globally defined remote leaves the
  original in effect.
- **Ctrl-C cleanup depends on gix internals.** `cli/src/cleanup_guard.rs` calls gix's tempfile
  registry to remove lock files on interrupt. jj has no backend-neutral hook for this.
- **A pack-files colocation test depended on `util gc` repacking.**
  `test_git_colocation_enable_disable_with_pack_files` used `jj util gc` to create packs; it tests
  colocation, so it now builds its pack with Git.
