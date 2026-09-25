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
locking, persistence or mutation. R09 owns lossless editing and concurrent write protocols.

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
