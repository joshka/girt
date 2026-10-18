---
title: Working with submodules
description: Read submodule definitions and inspect existing checkouts without fetching or changing them.
---

A submodule stores a commit ID as a gitlink (tree mode `160000`) in its parent repository. The
parent's `.gitmodules` maps a logical name to a worktree path and URL. The name selects
configuration and absorbed repository storage; the path selects the gitlink. They can differ.

Use `girt::submodule::Modules` to inspect those definitions. Select the bytes explicitly: read
`.gitmodules` from the working tree to inspect pending edits, or read its blob from an index or
commit to inspect that snapshot. `Modules::parse` does not choose a source or follow configuration
includes.

```rust
use girt::submodule::Modules;

let bytes = b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\n";
let modules = Modules::parse(bytes)?;
let math = modules.get(b"math").ok_or("definition missing")?;
math.validate()?;
assert_eq!(math.path(), Some(b"vendor/math".as_slice()));
assert_eq!(math.url(), Some(b"../math.git".as_slice()));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Parsing interprets update, ignore, shallow and fetch-recursion settings. It preserves name, path,
URL and branch bytes, including non-UTF-8 bytes. Repeated scalar settings use the last assignment.
`branch = .` retains Git's meaning of the superproject's current branch. Use
`Definition::resolve_url` with an explicitly selected superproject remote to resolve relative URLs
before fetching.

Call `validate` before using a definition in an operation. It rejects missing paths/URLs, traversal
components, absolute paths and portable option/path hazards. These lexical checks deliberately
refuse some values Git can register, including absolute logical names. They do not check existing
symlinks, filesystem aliases (such as `.git.`, `git~1` or `.git` followed by a space) or concurrent
changes, and do not authorize a transport. Retain the parsed definition to explain a validation
failure rather than silently dropping it.

External `!command` update methods are rejected in `.gitmodules`. Trusting repository content does
not make it local executable configuration. Parsing never runs a command or accesses the network. An
invalid `ignore` setting uses the operation's default, as Git does after warning.

## Read a repository snapshot

`Modules::read(&repository, &objects, source)` reads at most 1 MiB of metadata. Use `read_with` to
choose another source-byte budget. `Source::Worktree`, `Source::Index` and `Source::Commit(id)`
select a snapshot without fallback. `Source::WorktreeOrIndex` reads the working file first, then the
index only when that file is absent, as Git init does. It never falls back to HEAD. Missing entries
return `None`; conflicts, unreadable sources and non-regular entries are errors.

Worktree `.gitmodules` symlinks are refused, including when an index fallback is requested. This is
a deliberate restriction beyond Git init's observed behavior. Exclude concurrent filesystem
replacement on hosts without no-follow opens. Loading definitions does not validate their paths;
call `validate` before using destinations and enforce containment during filesystem operations.

## Resolve a fetch URL

Relative submodule URLs use repository-relative resolution: `./math.git` appends to the parent's
remote repository path, while `../math.git` selects its sibling. Pass the selected superproject
remote bytes to `resolve_url`; no default remote, current directory or ambient configuration is
consulted. The result can contain credentials, so avoid logging it.

The resolver preserves URL spelling and later path components. It refuses parent traversal beyond
the remote path, helper URLs and bases containing queries or fragments. Git can consume a URL host
or scp host in unusual parent traversals; girt refuses those endpoint changes explicitly. Transport
selection and network access remain caller decisions.

## Inspect existing checkouts

`girt::submodule::status(&repository)` returns one `Checkout` per gitlink. `recorded` is the
parent's index commit and `head` is the populated checkout's HEAD. A logical `name` can differ from
the gitlink `path`. `registered` reports active configuration independently of checkout presence. A
populated but unregistered submodule can still make the parent dirty.

`Checkout::prefix` supplies the structural prefix of `git submodule status`: `U` for unresolved
index stages, `-` for inactive or unpopulated submodules, `+` for a different HEAD, and a space for
a matching HEAD. `display_id` supplies the printed identity (zero for conflicts). Branch/tag
annotation suffixes are not computed. Dirtiness does not change this prefix; `tracked_dirty`,
`untracked_dirty` and `is_modified` describe changes relevant to parent status.

Configured `Repository::status` and `status_with_filters` populate `Report::submodules` and add
visible gitlink modifications to `unstaged`, including dirtiness with an unchanged commit ID. Raw
status retains `unchecked_gitlinks` and does not enter nested repositories. Ignore precedence is
effective `submodule.<name>.ignore`, then `.gitmodules`, then `diff.ignoreSubmodules`, then `none`.
`untracked` hides untracked content; `dirty` hides content changes but retains new commits; `all`
suppresses unstaged parent changes and leaves those paths in `unchecked_gitlinks` without opening
them. Standalone listing still reads HEAD under `all`; it does not hide malformed nested metadata.
Staged gitlink changes remain visible.

Nested status reads each child's own attributes, filters and ignore rules. It inherits the same
explicit system/global/environment/command sources the parent was opened with, adds the child's
local/worktree configuration, and reevaluates conditional includes for the child. Parent-local
settings are excluded. The captured attribute/ignore environment is retained, including
`XDG_CONFIG_HOME`, while parent repository-location overrides are removed. No ambient configuration
is added. Configured parent status without a filter pipeline refuses nested filter commands;
filtered status propagates its pipeline's command permission and per-command output limit.
`submodule::status_with` accepts command permission explicitly and uses the default per-command
limit.

`status::Limits::max_submodule_depth` defaults to 8 (with a hard ceiling of 64), and
`max_submodules` defaults to 1024 paths across the complete recursive scan. Other worktree budgets
apply separately to each repository. A limit or cancellation returns an error, never a partial clean
result. Inspection supports macOS/Linux, refuses symlink ancestors, and requires exclusion of
concurrent repository-metadata or directory replacement. It performs no index refresh, fetch, hook
or update command.

A populated child with an unborn HEAD currently returns a typed configuration error. Git treats it
as clean; [issue 688](https://github.com/joshka/girt/issues/688) tracks an explicit representation.
Regular-file replacements are type changes, subject to `ignore=all`. Symlink replacements are
refused, as Git itself refuses them.

Initialization, checkout updates and clone recursion remain unsupported. Track those operations in
[the submodules issue](https://github.com/joshka/girt/issues/507).
