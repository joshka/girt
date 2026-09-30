# Retrospective: the first jj replacement attempt

A read-only review (2026-09-30) of the first attempt to replace jj's gix and Git subprocess use with
girt. It covers the girt stack up to change `qlplnmyl`, the jj stack in `work/r35-https-explicit-ca`
(change `xwwpturt`), the planning documents in `work/delivery-worklist/docs`, and sampled session
logs. It records what that exploration established and why it did not converge, so the replacement
can keep the former and avoid the latter.

## Shape of the work

- girt: 378 commits, about 74k source lines and 954 tests. jj: 353 commits, +53.7k lines.
- The plan grew instead of shrinking. The roadmap went from 165 to 856 lines, the acceptance
  contracts from 296 to 436, and the worklist from 207 to 291 tasks while 69 were closed. Progress
  was reported as ~75–85% on 2026-09-28 and re-baselined to "1 of 10" the next day.
- About 35% of jj commits and 39% of girt commits were gating or evidence records ("Qualify",
  "Admit", "Record", "Verify", "Refuse"). Many "Qualify" commits did contain real tests.

## Depth that was real

Required for a correct replacement:

- SHA-256 end to end, including discovering the remote object format before creating a clone.
- Annotated tag preservation on fetch, export and push; nested tag peeling.
- Nonempty colocated reftable repositories. This work found a Git reftable timezone bug that was
  reported upstream.
- Colocated index correctness: versions and extended flags, TREE cache invalidation, stat reuse,
  intent-to-add, and split or sparse index normalization.
- Clearing merge, revert, cherry-pick and bisect state on HEAD reset.
- Reflog policy that appends only for changed targets.
- Remote rename that preserves push URLs, custom refspecs and unknown keys.
- Mixed push outcomes and partial publication after a lock failure; Gerrit push options.
- Ordinary credential-helper forms, HTTPS trust and proxies, `core.sshCommand`, `includeIf`.
- Depth clone, fetch and deepening. Protocol v2 remains unsupported.

Real but deferrable:

- Negotiation capped at 32 haves (performance only).
- Relative worktree links on Git older than 2.48.
- Ownership edge cases (mixed owners, `SUDO_UID`).
- Symlinked index and config files; timed waits on external locks.
- Copy-detection scoring parity (B01), partial clone (B11), Windows maintenance (B08).

## Why it did not converge

- **Dual path instead of replacement.** Native paths were opt-in (`git.native-backend`,
  `git.native-local-operations`, both off by default) and layered over gix. `lib/src/git.rs` grew by
  13k lines and held 508 gix uses. Nothing was ever removed, so every capability had to coexist with
  its fallback.
- **Refusal counted as progress.** Each configuration shape was refused before effects, then
  admitted one commit at a time. The tracker nested to IDs such as `K01.2b2b2b2b2` across 209
  sub-items. Safe refusal is not parity.
- **Stricter than Git.** GC and worktree retirement required excluding noncooperating writers, which
  Git itself does not do. `docs/fetch-retention.md` describes its rule as stronger than Git's
  `.keep` protocol; that rule forced the last shallow-retention slice. The owner's answer "keep Git
  behavior" was not applied.
- **Full matrix for every slice.** Each small change re-ran macOS plus a Linux guest, both hashes,
  both reference backends and every transport, with evidence manifests (94 files, 1.4 MB).
- **Integration debt.** jj passed only with an undocumented local path patch, and girt's
  all-features lint gate was left failing.

## Carried forward

- girt's object, pack, refs, index, config, ignore and wire-protocol code and their tests.
- Assertions from the first attempt's jj regression tests. Examples: remote rename preservation,
  annotated tag export, per-ref export failures, detaching linked HEADs, clearing operation state,
  split and sparse index normalization, missing exact tag fetch, mixed push outcomes, push reflog
  policy, depth deepening. The date-parsing cases (`lib/src/git_identity/date_cases.txt`) and the
  HTTP/SSH fault fixtures can also be reused.
- The limitations backlog (B01–B11 and the R35 consumer cases in `docs/jj-roadmap.md`).

## Approach for the replacement

- Swap `GitBackend` and `git.rs` internals behind jj's existing API, delete the gix calls, and use
  jj's existing test suites as the acceptance oracle. gix remains only as a test-fixture dependency.
- Match Git's semantics where Git provides no stronger guarantee: a `gc.pid` lock and a grace period
  for GC, and Git's `worktree prune` behavior for worktrees.
- Implement the common configuration and credential surface directly. Unsupported shapes return
  errors; they do not become tracker items.
- Validate one host and one hash per slice, and run the full matrix per milestone.
