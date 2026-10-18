---
title: Working with submodule definitions
description: Read and validate caller-selected .gitmodules bytes without fetching or changing a checkout.
---

A submodule stores a commit ID as a gitlink (tree mode `160000`) in its parent repository. The
parent's `.gitmodules` maps a logical name to a worktree path and URL. The name selects
configuration and absorbed repository storage; the path selects the gitlink. They can differ.

Use `girt::submodule::Modules` to inspect those definitions. Select the bytes explicitly: read
`.gitmodules` from the working tree to inspect pending edits, or read its blob from an index or
commit to inspect that snapshot. This API does not choose a source, follow configuration includes,
or fall back when a file is missing.

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
`branch = .` retains Git's meaning of the superproject's current branch. Relative URLs remain
relative; applications must resolve them against the selected superproject remote before fetching.

Call `validate` before using a definition in an operation. It rejects missing paths/URLs, traversal
components, absolute paths and portable option/path hazards. These lexical checks deliberately
refuse some values Git can register, including absolute logical names. They do not check existing
symlinks, filesystem aliases (such as `.git.`, `git~1` or `.git` followed by a space) or concurrent
changes, and do not authorize a transport. Retain the parsed definition to explain a validation
failure rather than silently dropping it.

External `!command` update methods are rejected in `.gitmodules`. Trusting repository content does
not make it local executable configuration. Parsing never runs a command or accesses the network. An
invalid `ignore` setting uses the operation's default, as Git does after warning.

This increment reads definitions only. It does not inspect nested HEAD or working-tree dirtiness,
resolve relative URLs, initialize absorbed repositories, update checkouts, or recurse during clone.
Parent status continues to expose gitlinks through `unchecked_gitlinks`. Track those operations in
[the submodules issue](https://github.com/joshka/girt/issues/507).
