# R35: Prepared Worktree Repair

`RepositoryLocation::prepare_worktree_repair` captures one linked registration from its source
checkout's gitfile. `WorktreeRepair::repair` rechecks the captured links and canonical anchors under
the administration lock, then replaces the published gitfile and backlink. It preserves each link's
absolute or relative form independently and leaves `commondir` unchanged. It does not read
repository configuration, HEAD, index, references, objects or shallow metadata, or enable format
extensions.

The caller keeps the source gitfile and excludes noncooperating administration and relocation.
Publication remains a caller-owned no-clobber hard link. Unix checks device/inode identity; other
platforms check file kind, paths and bytes. Errors preserve recovery state and report completed
replacements. This is not an atomic multi-file transaction or a portable lifetime-identity proof.

## Original Qualification

Independent installed-Git fixtures checked gitfile, backlink and `commondir` separately. Each
removes terminal CR/LF runs while preserving interior CR/LF and literal spaces. A trailing space in
a backlink names a nonexistent path and is reported as eligible for pruning. NUL remains
deliberately rejected. Absolute `commondir` values whose final component ends in LF need a following
slash; ordinary `../..` links preserve that component without ambiguity. No upstream implementation
or test source was used.

Disposable probe records are `/tmp/r35-repair-link-kind-probe.py` and its `.log`, plus
`/tmp/r35-commondir-final-newline-probe.py` and its `.log`. Consumer probes also cover spaces,
backslashes, directory aliases and newline/CR components in both common and checkout paths.
Non-UTF-8 filenames could not be created on the local macOS filesystem; other native platforms still
need qualification.

Producer regressions cover absolute, relative and mixed link styles, metadata-only operation with
invalid unrelated repository state, changed links and anchors, same-byte substitutions on Unix,
publication collisions, locks, first/second replacement failures, and literal path components.
Legacy location and worktree-administration tests remain part of the focused gate.

## Consumer Acceptance

The accepted jj change `rvyouuqu` at `b580282e`, above feature-pruning change `26cd93eb`, uses the
prepared repair after Git creates a temporary worktree. It deletes the Git repair function and its
sole production caller. Production Git endpoints decrease from five to four and callers from six to
five. Git worktree add remains responsible for creation and its configured side effects.

The consumer passes 39 distinct tests: 19 unit, 13 CLI and seven integration cases. Strict library
and CLI all-target Clippy with `test-fakes`, and full formatting checks pass. The canonical lock
file remains unchanged (reported digest prefix `86bda001`). Tests cover both object formats and
native operation settings, preserved link styles, metadata-only repair, path variants and recovery.
The Linux-only non-UTF-8 fixture was not run on macOS; native platform qualification remains open.

Producer acceptance includes 56 prepared-repair integration tests, 28 location integration tests,
nine existing administration tests and 53 repository unit tests. The public documentation test,
strict all-feature/all-target Clippy, core check, docs.rs, formatting and documentation checks pass.

## Related Dependency Pruning

The coordinated jj change `26cd93eb` removes unused `gix` attributes and blob-diff features.
Production CLI dependencies decrease from 431 to 421 packages and library dependencies from 274 to
264; lock file and development graphs remain unchanged. The coordinating lane reports
production-only and full feature checks, strict Clippy, Rustdoc and formatting passed, plus 19
public-gix/production-jj configuration parity cases, six retained eager errors and an initialization
smoke test. This is feature pruning, not complete `gix` removal.
