---
title: Working with submodule definitions
description: Read and validate caller-selected .gitmodules bytes without fetching or changing a checkout.
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

Nested HEAD/dirtiness inspection, initialization, checkout updates and clone recursion remain
unsupported. Parent status continues to expose gitlinks through `unchecked_gitlinks`. Track those
operations in [the submodules issue](https://github.com/joshka/girt/issues/507).
