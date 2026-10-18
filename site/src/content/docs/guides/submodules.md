---
title: Working with submodules
description: Inspect submodules, publish initialization config, and update existing child checkouts.
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

## Plan initialization and updates

Select definitions corresponding to the parent's gitlinks. `Definition::plan_init` compares one
selected definition against an explicit effective config snapshot and proposes local assignments.
Pass the chosen superproject remote; the planner does not select a default remote or read ambient
configuration. Existing URL/update settings survive, explicit metadata update settings are copied,
and an inactive definition receives `active=true`, including one with an existing URL.

```rust
use girt::{Config, config::Document, submodule::Modules};

let modules = Modules::parse(
    b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\nupdate = checkout\n",
)?;
let effective = Config::parse(b"")?;
let plan = modules.get(b"math").unwrap().plan_init(
    &effective, b"https://example.com/team/parent.git",
)?;
let mut draft = Document::parse(b"")?;
for (key, value) in plan.settings() {
    draft.append("submodule", Some(plan.name()), key, value)?;
}
assert_eq!(draft.config().string("submodule", Some(b"math"), "update"), Some(b"checkout".as_slice()));
# Ok::<(), Box<dyn std::error::Error>>(())
```

The example changes an in-memory document. `InitPlan::apply` instead locks the parent's local
config, rereads effective settings from the original opening sources, checks the plan's relevant
values, and publishes through `ConfigEdit`. It preserves unrelated direct-file bytes and returns
whether config was published. Reopen afterwards. Other configuration sources and repository metadata
must remain stable under caller exclusion; this is not a multi-source configuration transaction.
Attribute-dependent activation pathspecs are explicitly refused because planning performs no lookup;
[issue 695](https://github.com/joshka/girt/issues/695) tracks an explicit lookup input.

`Definition::plan_update` requires an initialized effective URL and the recorded commit selected
from the parent index. It preserves that target and selects checkout, rebase, merge, none, or an
explicit external command. Checkout means detached HEAD; branch-following updates are separate. The
planner does not fetch, check object availability, validate child cleanliness or write files.

External `!command` methods in `.gitmodules` are always rejected during parsing, regardless of
permission. User-supplied methods require resolved configuration provenance and separate
`submodule::Commands::Run` permission. System/global/local/worktree/environment/command scopes are
eligible only because the caller explicitly selected them. Parsed bytes without provenance return
`PlanError::UntrustedCommand`, while denied permission returns `PlanError::CommandRefused`. Choosing
a config source is a trust decision; assigning a scope does not prove a file is trusted. Plans
execute no shell command. Avoid logging URLs and command text, which can contain credentials.

## Apply a recorded-commit checkout

`UpdatePlan::apply` supports checkout and none for an already populated child whose recorded commit
is present. It checks the current parent gitlink and relevant effective config, locks the child's
HEAD/references, uses the child's baseline and target attributes/filters, publishes files/index,
then publishes detached HEAD. The child's branch is preserved. If HEAD already matches, attachment
and dirty files remain unchanged, as with Git update without force.

This example needs an existing populated submodule. Choose the normal gitlink and matching metadata
definition before constructing its plan. No transport occurs.

```no_run
use girt::{Repository, Signature, submodule};

let parent = Repository::open("project")?;
let objects = parent.objects()?;
let modules = submodule::Modules::read(&parent, &objects, submodule::Source::WorktreeOrIndex)?
    .ok_or("missing .gitmodules")?;
let index = parent.read_index(Default::default())?.ok_or("missing index")?;
let entry = index.entries().iter().find(|entry| {
    entry.path == b"vendor/math" && entry.mode == girt::index::Mode::Gitlink
        && entry.stage == girt::index::Stage::Normal
}).ok_or("normal gitlink missing")?;
let math = modules.get(b"math").ok_or("definition missing")?;
let plan = math.plan_update(parent.config(), entry.id, submodule::Commands::Refuse)?;
let committer = Signature {
    name: b"A. Writer".to_vec(), email: b"writer@example.com".to_vec(),
    seconds: 1_700_000_000, offset_minutes: 0,
};
let options = submodule::UpdateOptions::new(committer); // Filter programs are refused by default.
let report = plan.apply(&parent, &options)?;
assert!(report.head_updated || report.checkout.is_none());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `UpdateOptions::with_environment` for captured attributes and filter-program environment, and
`with_filter_commands` to permit filter programs separately from custom update methods. Programs
receive only the supplied snapshot; choose `Environment::from_process()` explicitly when those
variables are wanted. Entering the child clears parent repository context, sets its `GIT_DIR` and
empty prefix, and retains user runtime config parameters/count pairs. Children inherit exactly the
parent's caller-selected non-local config sources and add their own local config. Parent-local
settings are excluded. `with_limits` bounds checkout/index/object reads; `apply_with` additionally
accepts cancellation. None performs no child operation. Rebase/merge execution is tracked in
[issue 700](https://github.com/joshka/girt/issues/700); custom-method execution is explicitly
refused even when a plan was authorized to contain the command.

Callers must exclude parent index/config writers and child metadata/worktree writers throughout
execution. Symlink paths are refused, but nested repository metadata remains a trust boundary, as in
repository opening. Dirty/staged/conflicted content, missing child/commit and active operations are
refused before checkout. Windows supports init config publication and an explicit unsupported
checkout result; checkout runs on macOS/Linux.

Inspect `UpdateError::Checkout` for partial file/index effects with HEAD unchanged.
`UpdateError::AfterCheckout` retains the completed checkout and late failure, including any
reference transaction effects. No rollback is attempted: observe the residual state under exclusion
before deciding how to recover. Successful reports do not authorize later writes.

Missing-child clone/fetch, absorbed-layout creation and recursion remain tracked in
[the submodules issue](https://github.com/joshka/girt/issues/507).
