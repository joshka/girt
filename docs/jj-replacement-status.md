# jj on girt: status

Status of replacing gix, gix-ignore and the Git subprocess in jj with girt. Updated as work lands.
The contract is fixed; implementation details go under the deliverable that owns them.

- jj branch: [`joshka/girt-backend`](https://github.com/joshka/jj/tree/joshka/girt-backend) on the
  `joshka/jj` fork, one commit on jj `main` at `35dbc362`, depending on girt 0.3.0 from crates.io.
- girt: released as 0.3.0 from `main`.
- Baseline progress measure: gix, `git_subprocess` or Git executable uses remaining in jj production
  code (411 references at the start).

## Completion contract

Done when all of the following hold:

1. jj's production code has no gix, gix-ignore, libgit2 or Git subprocess use.
2. jj's existing library and CLI tests for Git, Gerrit, workspaces, colocation, ignores and GC pass
   on macOS, using both SHA-1 and SHA-256 where the tests cover both.
3. A scripted end-to-end run passes against real Git, alternating jj and Git operations: init,
   colocate, commit, export, clone, fetch, push, workspaces and GC.
4. The girt dependency is reproducible: a published girt release, or a committed and documented path
   dependency.

Tests may use gix and the Git executable only as oracles and fixture builders.

Explicitly excluded; recorded as limitations, not counted as parity:

- Windows.
- Servers that support only protocol v2 for fetch.
- Partial clone and filters.
- Submodule recursion.
- `git-remote-*` helpers.
- Attribute filters during copy detection.
- Byte-identical `git gc` packing.

## Deliverables

| #   | Deliverable                                                    | State           |
| --- | -------------------------------------------------------------- | --------------- |
| 1   | Storage backend: objects, commits, trees, signing, keep refs   | tests pass      |
| 2   | Refs: import, export, HEAD and linked-worktree HEADs           | tests pass      |
| 3   | Colocated index: reset, conflicts, intent-to-add, stat reuse   | tests pass      |
| 4   | Config and remotes: add, remove, rename, set-url, list, ignore | tests pass      |
| 5   | Workspaces: worktree create, repair and unlink                 | tests pass      |
| 6   | Fetch and clone: local, HTTP(S), SSH, depth, prune, tags       | e2e passes      |
| 7   | Push and Gerrit: leases, tags, push options, per-ref results   | e2e passes      |
| 8   | GC: keep refs plus girt retention replacing `git gc`           | e2e passes      |
| 9   | CLI call sites: URLs, remote list, colocation, excludes        | tests pass      |
| 10  | Tests: jj suites green, gix only in tests                      | done            |
| 11  | End-to-end interop script, both object formats                 | done            |
| 12  | girt API ergonomics pass, driven by jj call sites              | done            |
| 13  | Reproducible dependency (girt release) and final audit         | done            |

State values: not started, code written (compiles or nearly), tests pass, done (verified).

## Log

- 2026-09-30: Inspected the Codex state and chose direct replacement on a fresh jj `main` branch.
  See the [retrospective](codex-retrospective.md).
- 2026-09-30: girt changes:
  - Fixed the all-features build and Clippy gates.
  - Added `Config::{string, boolean, integer}`.
  - Made `Repository` implement `Clone`.
  - Config documents now write Git-style unquoted values, append to an existing section, and remove
    whole lines. 3138 unit tests pass.
- 2026-09-30: Wrote the new jj `git_backend.rs`, `git_maintenance.rs` and `gitignore.rs`. Ported
  `git.rs` ref import, export, HEAD, index, worktree and remote code (not yet compiled).
- 2026-09-30: jj builds with no gix, gix-ignore or Git subprocess in production code. Added girt
  `transfer` module (`Repository::{fetch, push, remote_head}`) with transport selection, shell-run
  `GIT_SSH_COMMAND`/`core.sshCommand` and credential helpers, and 401 retry.
- 2026-09-30: First local end-to-end workflow passes with output identical to stock jj: colocated
  init, commit, bookmark, remote add, push, clone (default branch tracked, checkout), second push,
  fetch. `git fsck` and `git status` are clean on both sides.
- 2026-09-30: jj-lib suite passes: 1723/1723 (SHA-1 default). jj's own SHA-256 test cases pass. With
  every test forced to SHA-256 (`JJ_TEST_OBJECT_FORMAT=sha256`), 12 fail. All but one hard-code
  SHA-1 IDs or fixtures; the remaining one was a backend panic on an ID of the wrong length, now an
  error.
- 2026-09-30: girt fixes found by jj's tests:
  - The canonical empty tree is always readable, as in Git.
  - Reference transactions wait on locks for Git's default timeouts (100 ms loose, 1 s packed).
  - Push reports "up to date" when the remote already has the new value.
  - Push updates local remote-tracking refs, as `git push` does.
  - Native local push runs `pre-receive`/`update`/`post-receive`/`post-update` hooks with push
    options, instead of refusing.
  - Repository `Debug` no longer dumps configuration values.
- 2026-09-30: jj adapter follows Git's header convention: a multi-line header value's final newline
  terminates its last line. This keeps commit bytes (and IDs) identical to gix-written commits;
  verified against stock jj output.
- 2026-09-30: jj-cli suite: 1401/1444 on first run, all 1444 after fixes.
  - girt fixes:
    - index writes use Git's racy-git smudging instead of an epoch mtime (the epoch hack hid index
      updates from mtime-caching readers and made every entry racy);
    - `init` matches `git init` (`InitOptions`, `init.defaultBranch`, `hooks/`, `info/exclude`,
      Git's default config);
    - fetch publishes prunes before updates;
    - local discovery infers a detached HEAD's branch like upload-pack;
    - receive hooks print Git's "hook declined" message;
    - deletions are never "up to date";
    - relative local remote paths resolve against the working directory;
    - `FetchError::Source` for a missing local repository;
    - store-backed `KnownHistory::from_store` (bounded commit walk; connectivity stops at local
      objects) replaces full-history negotiation in `Repository::fetch`;
    - local push checks receiver roots exist instead of walking their history.
  - jj adapter:
    - copy detection uses postimage sources and exact-only binary matching (gix parity);
    - default push remote falls back to `origin`;
    - init honors `init.defaultBranch`.
  - Snapshot updates were limited to error-message wording, Git-accurate config layout, and
    config-order refspec warnings; each was reviewed.
- 2026-09-30: Full jj workspace suite: 3386/3386.
- 2026-09-30: End-to-end script `scripts/jj-e2e/e2e.sh` passes 196/196 checks. It runs SHA-1 and
  SHA-256, over local path, `file://`, smart HTTP (`git http-backend`) and SSH (a stand-in `ssh`
  running Git's server programs), plus HTTP Basic auth through a credential helper. jj and Git
  alternate on the same repositories; Git's `fsck --strict`, `status` and `rev-parse` are the
  oracle. HTTPS clone and fetch from GitHub also worked by hand.
  - girt fix found by it: retiring packs removes a stale `multi-pack-index`, and pruning removes the
    commit-graph, so Git's auto-maintenance files never describe deleted objects.
  - `FetchError::FormatMismatch` reports a remote whose object format differs from the local
    repository's.
- 2026-09-30: Audit: no gix, gix-ignore, git2 or libgit2 in jj-cli's normal dependency tree
  (`cargo tree -e normal`). Every remaining `gix` use and `Command::new("git")` in jj and girt
  source is inside a `#[cfg(test)]` module.
- 2026-09-30: girt's integration suites (not run before) had 28 failures. 20 were tests pinning
  behavior that deliberately changed to match Git, now updated: receive hooks run instead of
  refusing the push; the empty tree is readable; bare config values; whole-line section removal;
  `FormatMismatch`; an unchanged ref is up to date despite a stale lease (observed with
  `git push --force-with-lease`). Eight failed on the base revision too:
  - `creates_git_usable_orphan`: Git 2.55's own `worktree add --orphan` leaves the first commit's
    empty tree unstored, so `fsck` reports it missing. The test now stores it before `fsck`.
  - `malformed_backlink`: retention accepted a multi-line `gitdir` backlink. It now refuses to plan.
  All 5223 girt tests and 65 doctests pass; Clippy and docs.rs checks are clean.
- 2026-09-30: Push preparation walks commits from the tips and receiver roots in committer-date
  order and stops at known history, instead of reading everything reachable from the tips. On a
  5,000-commit fast-import fixture (25,005 packed objects, 200 files in 20×10 directories), pushing
  one commit over the old tip read 232 objects in about 4 ms (release build, macOS arm64) instead of
  25,005 objects in about 1.0 s; at 20,000 commits it still read 232 objects. Fast-forward proofs
  stay exact under skewed dates.
- 2026-09-30: girt 0.3.0 published. jj depends on it from crates.io and passes the full workspace
  suite (3386/3386), Clippy, and the end-to-end script (196/196). The jj change is published as a
  single experimental commit on the `joshka/jj` fork, not proposed upstream. girt's history was
  rebuilt so `main` keeps the individual commits behind 0.3.0.
- 2026-09-30: Large-repository check on `github.com/jj-vcs/jj` (130k objects, 1,500 branches). It
  found problems no small fixture showed, so the benchmark is now part of acceptance. Release builds
  on macOS arm64:

  | Step        | girt 0.3.0                     | this change      | stock jj (Git)  |
  | ----------- | ------------------------------ | ---------------- | --------------- |
  | clone       | fails: decode limit            | 20.0 s / 4.7 GB  | 13.8 s / 110 MB |
  | no-op fetch | 25.8 s / 4.7 GB                | 2.1 s / 98 MB    | 0.67 s / 28 MB  |
  | `util gc`   | 566 s / 5.1 GB; pack to 1 GiB  | 75 s / 365 MB    | 2.3 s / 262 MB  |

  - Fixes:
    - trusted transfer limits;
    - fetch publication stops at objects existing references name;
    - packs are read before loose objects;
    - a delta base cache, with only the requested object's identity checked.
  - jj's `util gc` now only expires reflogs and prunes loose objects. girt's repack rewrote every
    object without deltas.

## API ergonomics plan (deliverable 12)

Gaps where jj's adapter carries boilerplate or re-derives Git semantics that belong in girt:

- [x] Errors repeat their source in `Display`, so chains print causes twice. Fixed in 109 messages.
- [x] Push preparation reads the full reachable history. It now walks only new history.
- [x] Limits: jj defines near-unbounded `ReadLimits` and `PackLimits` for a user's own repository.
      girt needs defaults, or a named constructor, suited to trusted local repositories, so ordinary
      calls don't spell out limits.
- [x] References: listing under a prefix needs `usize::MAX` bounds and a throwaway cancellation
  flag. Add a plain prefix listing, and resolving a reference to its commit.
- [x] Reflog policy: `core.logAllRefUpdates` (which refs get reflogs, and whether bare repos log) is
      Git semantics jj re-derives. girt should decide it from the repository's config.
- [x] Config inputs: kept explicit. `ConfigInputs` deliberately reads no process-global state, and
  the system file depends on how Git was installed, which girt can't know.
- [x] Init: `InitOptions::defaults_from` applies `init.defaultBranch`.
- [x] Peeling: `PeelLimits::trusted()` covers limits; the cancellation flag stays explicit.

## Known limitations and follow-ups

- GC follows `git gc --prune=<cutoff>`'s safety model: a `gc.pid` lock plus a grace period. It does
  not exclude noncooperating writers.
- Reflog expiry uses Git's defaults (90 days, and 30 days for unreachable entries); the
  `gc.reflogExpire*` settings are not read yet.
- The system Git config is read from `/etc/gitconfig` only, not from Git's compile-time prefix (for
  example Homebrew's `/opt/homebrew/etc/gitconfig`).
- The colocated index is rebuilt from jj's tree iteration rather than girt building it from the tree
  directly. Measure performance on large repositories.
- Push preparation reads the full tree of each known commit adjacent to a sent commit to mark shared
  content, as Git does. On repositories with very large trees this dominates an incremental push; a
  path-aligned comparison against the sent trees would read less.
- Connectivity checks trust any object present locally, where Git trusts only objects reachable from
  refs. jj keeps refs for every commit it writes, so this matters only for dangling partial history
  left by interrupted external operations.
- Many girt error types still repeat their source in `Display` (e.g. "remote url, occurrence 1:
  invalid remote URL authority"). To fix in the API ergonomics pass.
- `credential.helper` names map to `git-credential-<name>` on `PATH` or in known Git exec
  directories. The builtin `store`/`cache` helpers therefore run Git's own binaries.
- Interrupting jj (Ctrl-C) no longer runs gix's lock-file cleanup; girt has no signal-time tempfile
  registry.
- Native local push runs receive hooks without a quarantine: objects are installed before
  `pre-receive` runs, so a declined push leaves unreferenced objects (removed by a later GC). Hooks
  are not interrupted by the transfer's cancellation or deadline.
- The fetch importer keeps every decoded object of a received pack in memory until connectivity is
  checked, about 4.7 GB for `jj-vcs/jj`. Git streams the pack to disk and indexes it with a small
  cache. Fix: a streaming importer.
- girt's repack decodes every object and writes them without deltas, so jj doesn't repack. Fix: a
  repack that reuses existing packed entries and deltas.
- Retention walks the whole object graph once per phase (reflog expiry, then pruning), which keeps
  `util gc` at 75 s against Git's 2.3 s. Packed reads use `pread` rather than memory maps.
- Local fetch/push progress (receiving objects, resolving deltas) is not reported yet; remote
  counting/compressing progress and sideband messages are.

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
