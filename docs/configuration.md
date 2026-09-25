# Layered Configuration

`Config::parse` decodes one byte source without I/O. `Config::resolve` reads explicit `ConfigInputs`
and returns the same queryable snapshot with an `Origin` on every entry. `Repository::open` resolves
local and enabled worktree sources; `open_with_config` also accepts inherited inputs. `Remote::find`
consumes the resulting order, including empty URL resets. Try `cargo run --example config`.

## Inputs and Precedence

Files are stably ordered by `ConfigScope`: system, global, local, worktree. Environment pairs follow
files, and caller command entries follow environment pairs. Within each scope, caller order is
preserved. Supply XDG before home configuration. Repeated values remain ordered and scalar lookup
returns the last occurrence; individual consumers own reset semantics. Empty values and implicit
booleans are distinct.

`ConfigInputs::from_environment` accepts a lookup closure over an explicit OS-string snapshot. It
selects HOME/XDG, system/global overrides and NOSYSTEM, then decodes COUNT/KEY/VALUE runtime pairs.
The caller supplies the installation's default system path. The library never reads or changes
process environment. `GIT_CONFIG` is a selector for Git's config command, not a general repository
setting, and is ignored. Private command serialization such as `GIT_CONFIG_PARAMETERS` is not a
public input format; supply parsed command entries instead. Transport environment policy belongs to
R21.

Empty include paths, missing optional roots and missing includes are ignored; a required root
returns its I/O error. Existing malformed or unreadable sources always fail. Include directives
retain their own entries. Included values appear immediately afterward, inherit the root scope, and
carry their physical source line and outermost-first include ancestry. Error locations carry the
same ancestry without printing values. Section and variable spelling is retained while lookup folds
ASCII case. Quoted subsections and values preserve bytes; deprecated dotted subsections are
ASCII-lowercased as in Git.

## Includes and Context

Include paths are relative to the including file. Absolute paths and `~/`, `~user/`, `%(prefix)/`
expansion use explicit home, named-home and installation-prefix mappings. Runtime includes need
absolute paths after expansion because they have no file origin. No environment interpolation is
performed. Unix paths preserve bytes; other platforms require UTF-8 for paths encoded in config.
Explicit root paths remain OS-native.

- `gitdir:` matches byte globs against supplied Git-directory spellings. Relative patterns acquire
  `**/`; `./` is relative to the including file's canonical parent. `~/` uses supplied HOME and a
  trailing slash acquires `**`. `gitdir/i:` folds ASCII case.
- `onbranch:` matches the supplied short branch name, including unborn branches. Detached HEAD has
  no branch match. A trailing slash matches descendants.
- `hasconfig:remote.*.url:` matches all eligible URL occurrences, including later layers and runtime
  overrides. A first pass visits its include targets even when unmatched and prohibits remote URLs
  in those files or their included descendants. This prevents circular URL-dependent resolution.
- Unknown condition keywords do not match, as observed in Git. Globs support `?`, `*`, recursive
  component `**`, escapes, bracket ranges, negation and ASCII POSIX classes.

Repository opening supplies canonical and logical input Git-directory spellings and reads HEAD for
branch context. Callers can add aliases for separate Git-directory paths. `IncludeContext` makes
these choices explicit for standalone resolution, including logical and canonical symlink aliases.
Linked worktrees match their private Git directory, while relative includes in common config remain
relative to that common source.

## Bootstrap, Bounds and Refresh

Repository format is determined from the direct common config before effective resolution. Includes,
global settings, runtime overrides and worktree format entries cannot reinterpret the opened store.
The direct common `extensions.worktreeConfig` enables the private file. Direct common and enabled
worktree core settings determine layout; broader linked-layout changes remain R10. This separation
matches independent `git rev-parse` observations for included/worktree format and bare settings.

Resolution is synchronous and read-only. Defaults allow ten include edges, 16 MiB of loaded source
bytes, independently 16 MiB of expanded key/value bytes per pass, 100,000 visited entries per pass,
and one million pattern/candidate cells per match. Canonical ancestor identities detect include
cycles; repeated non-ancestor includes are legal and count against expansion budgets. File bytes are
cached within one call. Budgets reject excessive work with contextual errors, not partial snapshots.
Direct bootstrap config reads also obey the byte limit. Other repository metadata retains the
existing trusted-filesystem contract.

Re-resolve or reopen to refresh. Old snapshots remain unchanged; no automatic watches or global
cache exist. Concurrent external writers may produce a mixed snapshot across sources, so callers
requiring a coherent multi-file generation must coordinate writers. This operation performs no
locking, persistence or mutation. The separate editing lifecycle below owns writes.

Optional tracing emits `config.resolve` completion and categorical failure only, never paths,
values, URLs or environment contents. Returned provenance is caller-owned diagnostic data.

## Evidence and Boundaries

Original fixtures in `tests/config_resolution.rs` compare public APIs with Git's executable under
isolated HOME/config/runtime inputs. They cover layer order, reset values, nested origins,
conditions, linked worktrees, byte/case behavior, missing and malformed sources, cycles, bounds and
refresh. The suite is selected for native Windows CI; filename byte cases are Linux-only. Local
macOS results and unexecuted native platforms are distinguished in [R08 evidence](evidence/r08.md).

The specification input is the [Git configuration manual](https://git-scm.com/docs/git-config). No
upstream implementation or test source is used. This finite corpus does not establish every
configuration edge case. Repository discovery/layout expansion belongs to R10, URL rewriting to R21,
and jj integration to R35.

## Lossless File Editing

`Document::parse` retains exact source bytes and parser-derived syntax ranges. `set_value` and
`remove` select a direct-file occurrence by index; obtain fresh indices after structural edits.
`append` writes a quoted assignment under a repeated section at EOF. Section rename/remove operates
on all matching headers, including empty headers and deprecated dotted syntax. Untargeted bytes,
comments, line endings, unknown keys and occurrence order survive. Removed syntax leaves surrounding
whitespace and comments; an edited header or value uses canonical quoting. NUL and subsection
newlines are rejected. No include expansion or typed URL interpretation happens in this layer.

`Repository::edit_config(max_bytes)` locks the common local file independently of the repository's
cached effective snapshot. `ConfigEdit::open(path, max_bytes)` explicitly selects another OS-native
path, including a worktree file. Neither follows destination symlinks. Acquire the guard before
computing edits; `require_source` can additionally reject a stale earlier byte/presence snapshot.
Missing and empty files are distinct preconditions, but both provide an empty document.

`commit` checks the byte budget, compares source bytes/presence, writes the owned exclusive
`<path>.lock`, compares again, verifies lock ownership and renames within the same directory. Git
and girt writers honoring the lock are excluded throughout. A noncooperating writer observed before
rename causes rejection; the last check cannot exclude an arbitrary later write. Unix checks lock
inode identity; other platforms require callers to exclude lock replacement. Hostile directory
replacement is outside the contract. Atomic replacement depends on the filesystem; no fsync,
crash-durability, multi-file atomicity or shared-repository permission policy is provided. Unix
replacement preserves source permission bits; new files use the process umask. Ownership, ACLs and
extended attributes are not copied.

Preparation and unsuccessful rename leave the destination intact apart from independent writes.
`EditError` distinguishes syntax, byte limits, contention, source changes, nonregular files and I/O
operations, including publication failures. A `Cleanup` error retains both primary and cleanup
causes. Inspect any residual lock before manual recovery and acquire a fresh lifecycle before
retrying. `abort` reports cleanup failure; drop is best effort. Never delete an existing lock merely
because a new acquisition failed.

## Local Remote Operations

`RemoteConfig::new(&mut Document)` prepares each operation before replacing the draft. `add` takes
an explicit URL and ordered fetch refspecs; it does not guess mappings from the name. `append`,
`set` and `remove_value` operate on `RemoteKey` URL/pushURL/fetch/push occurrences. URL bytes remain
opaque; refspec changes use the existing direction-specific parser. Explicit occurrence selection
avoids regular-expression and consumer naming policy. Empty URL resets and push fallback remain
owned by `Remote::find`.

Rename updates every local remote header, local branch `remote`/`pushRemote`, local
`remote.pushDefault`, and fetch destinations under `refs/remotes/<old>/`. Custom fetch destinations
and push mappings remain exact. Remove deletes local remote syntax, matching selectors, and merge
entries for branches whose local remote selector matched. Other branch settings remain intact.
Repeated scalar selectors referencing the target return `AmbiguousSelector` without partial edits;
Git's CLI can instead warn and retain stale values. Use explicit occurrence editing to resolve that
ambiguity before retrying.

These operations edit one file. Inherited-only remotes return `NotLocal`; removing a local section
can leave an included/global remote effective. Included/global branch references also remain
unchanged. The library never implicitly rewrites those sources or claims effective removal.
Re-resolve using the original inputs, or reopen with `open_with_config`, before constructing a new
`Remote`. Old repository and remote snapshots remain unchanged. Remote-tracking ref rename/deletion
is a separate reference operation, not a side effect of configuration editing; R11 owns the
conditional reference foundation and R28 owns composed remote/prune outcomes.

Run `cargo run --example edit_config` for a disposable lock/edit/commit/refresh example. The
portable `config_edit` suite observes Git add/remove/rename/set-url, byte quoting, inherited
settings, list order and refresh. Local tests inject storage faults and stale-source/lock races. No
performance claim is made: edits target small configuration documents and intentionally reparse
prepared drafts for validation. Larger bulk editing is not optimized or benchmarked.
