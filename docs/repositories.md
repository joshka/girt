# Repository Layouts and Shallow History

`Repository::open` opens one explicit location. `discover` searches physical ancestors, optionally
through an inclusive ceiling. Both are read-only and accept ordinary, bare, separate-Git-directory
and linked-worktree layouts in SHA-1 and SHA-256. Gitfiles and `.git` symlinks retain their checkout
relationship. `open_worktree` additionally verifies common-directory identity, returning
`OpenError::Unrelated` for another repository. A separate nonbare metadata directory can open
without knowing its checkout location: `worktree()` returns `None` and `is_bare()` returns `false`.
Metadata/object operations remain available; checkout/status report an unknown worktree location.
Opening through the checkout or gitfile supplies the relationship without guessing from the current
directory.

## Paths and Configuration

Common metadata owns objects, shared refs, configuration and shallow roots. A linked Git directory
owns HEAD, index, private refs and `config.worktree`. Existing refs/index APIs use these paths;
opening a checkout does not redirect shared object storage to its private directory.

Relative `commondir` and linked `gitdir` backlinks resolve against the private Git directory.
Relative forward gitfile targets resolve against the gitfile's containing directory, including a
symlinked gitfile opened through its caller-visible location. Whole layouts with relative links can
move together. Stale absolute backlinks remain observable until repaired externally. Girt does not
create registrations, repair links, or prune entries; those operations belong to R19/R20.

Without `extensions.worktreeConfig`, linked layouts ignore shared `core.bare` and `core.worktree`.
With it, direct common settings followed by direct private settings determine the checkout.
Includes, inherited sources and runtime config overrides affect the resolved configuration, not this
layout bootstrap. In particular, a `core.worktree` in an included private file does not override a
direct common value for checkout selection. Relative `core.worktree` resolves against the private
Git directory. These distinctions follow independent `git rev-parse` observations. Format bootstrap
continues to use only direct common configuration; `extensions.relativeWorktrees` is recognized.

Existing paths are canonicalized through the filesystem, preserving native case/normalization
identity. Missing or inaccessible checkout paths remain absolute OS spellings and may contain `..`.
Unix metadata paths preserve bytes; non-Unix metadata paths require UTF-8. Rust filesystem paths
retain native Windows prefixes; no Git subprocess receives those canonical paths. Native UNC shares,
WSL path interchange and additional alias contexts still require R36/C02 evidence. There is no
lexical Unicode normalization, case folding, environment expansion or drive remapping.

The opener reads no ambient `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE`, `GIT_OBJECT_DIRECTORY`,
`GIT_SHALLOW_FILE`, discovery ceilings or ownership policy. Callers choose paths and supply config
inputs explicitly. The inspected jj consumer opens its stored repository path, compares canonical
common directories, and supplies identity config overrides. Installed jj 0.45.1 also imports and
reopens an explicit backing repository with each of those five path overrides pointing to a
nonexistent location. No repository-path environment adapter is established by those call sites or
observations. R14 owns external object storage and R21 transport environment policy; R35 must
re-inventory the consumer before integration.

## Registered Worktrees

`Repository::worktrees(max_entries, cancel)` returns linked registrations in OS filename order,
excluding the main repository. Each `Worktree` retains its private `git_dir`, a backlink checkout
path when readable, and `Available`, `Missing`, `Inaccessible` or `Invalid` state. Inaccessible
entries retain an OS error; invalid entries retain their metadata error. One damaged entry does not
hide other entries. Registration symlinks are invalid, rather than followed outside the registry.

Opening a private Git directory remains possible when its checkout is missing or inaccessible. That
lets later reference administration inspect private HEAD without confusing checkout availability
with common repository validity. Opening through a checkout verifies its backlink; opening private
metadata does not require a live checkout. A malformed common directory still fails. Inventory state
describes the registered backlink, while an opened handle's `worktree()` reflects configuration
overrides. Neither is authority to prune or mutate a registration.

The entry budget is checked before collecting another registration. Cancellation is checked between
entries and inspections; individual filesystem reads cannot be interrupted. The operation acquires
no locks and can observe concurrent registration changes. Re-enumerate after repair/moves and
coordinate externally before acting on a result. Paths and object IDs are absent from the optional
`repository.worktrees` and `repository.shallow` tracing spans.

## Shallow Snapshots

Opening captures a sorted, deduplicated `ShallowRoots` set from the common `shallow` file. The IDs
carry the repository format. Empty/absent files mean no boundaries. LF, CRLF and an unterminated
last record are accepted. Each record uses the leading 40 (SHA-1) or 64 (SHA-256) hexadecimal
digits; remaining bytes are ignored, as observed with Git 2.55.0. The repository format selects
the width. Blank records and short or non-hexadecimal prefixes fail with a one-based line number.
Standalone object IDs and commit parent records still require exact widths.

Declarations do not prove object existence or kind. Missing IDs and null declarations are retained,
as Git permits, but walking one still fails if its object is missing. A blob boundary fails with
`HistoryError::NotCommit`; corrupt or malformed commits retain storage/parse causes. Duplicates have
no additional graph effect.

`Repository::objects` inherits the handle's fixed shallow set. `walk`, `is_ancestor` and
`merge_bases` read and parse each visited boundary commit, then stop its parent edges, even if those
parents exist locally. Unrelated missing ancestors remain errors. Raw object bytes and
`Commit::parents()` remain unchanged. History resource limits apply to the graph actually followed;
existing history calls are synchronous and do not take a cancellation token.

`refresh_shallow(max_bytes, cancel)` replaces only that repository handle's boundaries after a
successful read. Failure leaves its prior set unchanged. Existing object readers retain their
original boundaries. After Git deepening, refresh or reopen the repository and create a fresh object
reader to discover new packs too. A single descriptor reads either side of Git's atomic shallow-file
replacement. There is no atomic snapshot across shallow metadata, packs, refs and loose files;
exclude concurrent depth changes while constructing a mutually consistent view. In-place writers
must also be excluded. Missing objects caused by races remain structured read errors.

Default opening bounds shallow metadata at 16 MiB. Explicit reads/refreshes accept a byte budget and
cooperative cancellation, checking between 8-KiB reads and records. Configuration has its existing
budgets; HEAD, gitfile and commondir bootstrap reads retain the trusted-path allocation contract.
Canonicalization is not an ownership/security check, and opening never executes Git.

## Transport Boundary

Reading shallow stores does not enable shallow transport. `KnownHistory` and push preparation reject
shallow snapshots; fetch workflow preparation and received-pack installation reject shallow
destinations. Installation also checks the live shallow marker, including a marker created after
opening. Callers must exclude concurrent depth changes for the whole operation. Empty live markers
are conservatively refused at the destination guard. Low-level advertised shallow-server responses
remain unsupported, and clone does not request depth/deepening.

R26/R27 still own negotiation and depth-changing transfers; R29 owns shallow push policy. These
refusals protect existing SHA-1-only operations and do not close those roadmap gaps. External object
storage/backends remain R14. The original public fixtures in `tests/layout_shallow.rs` use Git for
depth cloning and deepening, not a girt transport adapter.
