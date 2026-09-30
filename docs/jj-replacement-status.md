# jj on girt: status

Status of replacing gix, gix-ignore and the Git subprocess in jj with girt. Updated as work lands.
The contract is fixed; implementation details go under the deliverable that owns them.

- jj workspace: `/Users/joshka/local/jj/work/girt-swap`, based on jj `main` at `35dbc362`.
- girt workspace: `/Users/joshka/local/girt/work/girt-swap`, on top of `qlplnmyl`.
- Baseline progress measure: gix, `git_subprocess` or Git executable uses remaining in jj
  production code (411 references at the start).

## Completion contract

Done when all of the following hold:

1. jj's production code has no gix, gix-ignore, libgit2 or Git subprocess use.
2. jj's existing library and CLI tests for Git, Gerrit, workspaces, colocation, ignores and GC pass
   on macOS, using both SHA-1 and SHA-256 where the tests cover both.
3. A scripted end-to-end run passes against real Git, alternating jj and Git operations: init,
   colocate, commit, export, clone, fetch, push, workspaces and GC.
4. The girt dependency is reproducible: a published girt release, or a committed and documented
   path dependency.

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
| 1   | Storage backend: objects, commits, trees, signing, keep refs   | code written    |
| 2   | Refs: import, export, HEAD and linked-worktree HEADs           | code written    |
| 3   | Colocated index: reset, conflicts, intent-to-add, stat reuse   | code written    |
| 4   | Config and remotes: add, remove, rename, set-url, list, ignore | code written    |
| 5   | Workspaces: worktree create, repair and unlink                 | code written    |
| 6   | Fetch and clone: local, HTTP(S), SSH, depth, prune, tags       | code written    |
| 7   | Push and Gerrit: leases, tags, push options, per-ref results   | code written    |
| 8   | GC: keep refs plus girt retention replacing `git gc`           | code written    |
| 9   | CLI call sites: URLs, remote list, colocation, excludes        | code written    |
| 10  | Tests: jj suites green, gix only in tests                      | not started     |
| 11  | End-to-end interop script, both object formats                 | not started     |
| 12  | girt API ergonomics pass, driven by jj call sites              | not started     |
| 13  | Reproducible dependency (girt release) and final audit         | not started     |

State values: not started, code written (compiles or nearly), tests pass, done (verified).

## Log

- 2026-09-30: Inspected the Codex state and chose direct replacement on a fresh jj `main`
  branch. See the [retrospective](codex-retrospective.md).
- 2026-09-30: girt changes:
  - Fixed the all-features build and Clippy gates.
  - Added `Config::{string, boolean, integer}`.
  - Made `Repository` implement `Clone`.
  - Config documents now write Git-style unquoted values, append to an existing section, and
    remove whole lines. 3138 unit tests pass.
- 2026-09-30: Wrote the new jj `git_backend.rs`, `git_maintenance.rs` and `gitignore.rs`. Ported
  `git.rs` ref import, export, HEAD, index, worktree and remote code (not yet compiled).

- 2026-09-30: jj builds with no gix, gix-ignore or Git subprocess in production code. Added
  girt `transfer` module (`Repository::{fetch, push, remote_head}`) with transport selection,
  shell-run `GIT_SSH_COMMAND`/`core.sshCommand` and credential helpers, and 401 retry.
- 2026-09-30: First local end-to-end workflow passes with output identical to stock jj:
  colocated init, commit, bookmark, remote add, push, clone (default branch tracked, checkout),
  second push, fetch. `git fsck` and `git status` are clean on both sides.

## Known limitations and follow-ups

- GC follows `git gc --prune=<cutoff>`'s safety model: a `gc.pid` lock plus a grace period. It
  does not exclude noncooperating writers.
- Reflog expiry uses Git's defaults (90 days, and 30 days for unreachable entries); the
  `gc.reflogExpire*` settings are not read yet.
- The system Git config is read from `/etc/gitconfig` only, not from Git's compile-time prefix
  (for example Homebrew's `/opt/homebrew/etc/gitconfig`).
- The colocated index is rebuilt from jj's tree iteration rather than girt building it from the
  tree directly. Measure performance on large repositories.
- Fetch negotiation (`KnownHistory::new`) and push preparation read and validate the complete
  reachable history (trees included) on every transfer. This is slow on large repositories and
  must be replaced with bounded commit walks plus store-backed connectivity checks.
- `credential.helper` names map to `git-credential-<name>` on `PATH` or in known Git exec
  directories. The builtin `store`/`cache` helpers therefore run Git's own binaries.
- Interrupting jj (Ctrl-C) no longer runs gix's lock-file cleanup; girt has no signal-time
  tempfile registry.
- Local fetch/push progress (receiving objects, resolving deltas) is not reported yet; remote
  counting/compressing progress and sideband messages are.
