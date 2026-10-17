# Everyday Git task ergonomics

This catalogue measures application code against the API at `1ecc6fff` on 2026-10-17. It supports
[the ergonomics work](https://github.com/joshka/girt/issues/393). The code revision identifies the
library API, not the later catalogue commit. Current examples compile against this checkout;
proposed sketches are design goals, not APIs. The baseline stays dated while current examples evolve
with the library.

## Measurement contract

Each task uses the shortest direct composition identified here, with inputs supplied by the
application. Rust blocks are compile-only because they open caller-owned repositories or publish
changes. They include imports and error propagation; callers must inspect partial mutation reports
before retrying. `girt::Result` and `Box<dyn Error>` preserve those reports through downcasting but
supply no recovery policy; snippets box only when they add their own string errors. No example
starts a Git process. Disposable end-to-end examples are linked where setup and cleanup matter.

For incomplete tasks, the snippet implements only the supported portion described below. Linked
runnable examples provide disposable setup and cleanup; their fixtures are excluded from caller-cost
totals. Unsupported tasks have no code cost and remain visible in the capability-gap count. These
counts measure caller burden, not correctness or performance. Current variants use `trusted::Handle`
where it supplies the task policy; the dated baseline retains the direct-core costs. Each handle
method links to the explicit APIs it composes. The handle also exposes an empty `TreeEditor` and a
leased-command `push`; staging, push destination selection and the independent capability gaps
remain explicit.

The scorecard counts nonblank, non-comment physical lines, explicitly imported names (including
standard-library and example dependencies), default/trusted constructor calls, never-cancel
constructors, byte-vector conversions, explicit object-format mentions, identity and commit-field
literals, branch-name prefixes, boxed errors and empty vectors. Imports are lexical names, not
inferred Rust types. Counts include function wrappers and repeat shared setup per task. Supplied
tree IDs and identities are explicit inputs, not hidden application helpers; constructing them
remains a documented gap. Changing formatting or moving setup can change counts without improving
ergonomics; review the diff and the capability classification together. The per-task scorecard
detail records all counts.

Run `nice -n 10 python3 scripts/quality-scorecard.py --ergonomics-only` for this axis without
Clippy, Rustdoc, coverage or benchmark work. Run
`nice -n 10 python3 scripts/check-guide-examples.py` to compile the current Markdown snippets
alongside the guides. Linked examples compile with `nice -n 10 cargo check --locked --examples`.

## Tasks

### Open or discover

<!-- ergonomics-status: supported -->

Supported. Discovery already avoids hand-written parent traversal. The handle loads Git-default
configuration; the core `Repository::discover` leaves configuration inputs explicit.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover(".")?;
    println!("{}", git.repository().git_dir().display());
    Ok(())
}
```

Desired sketch: `Repository::discover(".")?`. Gap closure: no discovery capability gap; shared
handle work in [408](https://github.com/joshka/girt/issues/408).

### Read a file at a revision

<!-- ergonomics-status: supported -->

Supported for the revision grammar implemented by `revision`. Resolving to Blob checks the kind;
reading by ID alone does not; `read_blob` checks the kind and reports a missing object as an error.
The handle composes revision resolution and typed reads, refreshing once on an object miss.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover("repo")?;
    let blob = git.blob_at("HEAD", b"src/lib.rs")?;
    println!("{} bytes", blob.len());
    Ok(())
}
```

Desired sketch: `git.blob_at("HEAD", b"src/lib.rs")?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404) and
[408](https://github.com/joshka/girt/issues/408).

### Log first N in date order with exclusions

<!-- ergonomics-status: partial -->

Partial: the handle walks lazily in committer-date priority among discovered commits. It does not
provide exclusions or Git topological date order; timestamp-skewed ancestors can follow older
children. Those remaining walk capabilities belong to
[416](https://github.com/joshka/girt/issues/416).

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover("repo")?;
    for entry in git.log("HEAD")?.take(20) {
        let (id, _) = entry?;
        println!("{id}");
    }
    Ok(())
}
```

Desired sketch: `git.log("main").exclude("release").date_order().take(20)`. Gap closure:
[416](https://github.com/joshka/girt/issues/416) (iterator and exclusions) and
[408](https://github.com/joshka/girt/issues/408) (facade).

### Diff two commits: tree and content

<!-- ergonomics-status: supported -->

Supported: the handle resolves both revisions to trees, detects renames and writes the unified patch
`git diff` prints, streaming each line to the caller's writer. Hunks come from girt's Myers
comparison without Git's indent heuristic, so ambiguous changes may align differently; attributes,
textconv, diff configuration and binary patches are not applied. `Objects::write_patch_with` chooses
context lines, abbreviation, binary policy, limits and cancellation; `patch::write_hunks` renders a
`content_diff` result alone.

With the trusted handle:

```rust,no_run
use std::io::{self, Write as _};

use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover("repo")?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    git.write_patch(&mut out, "HEAD~1", "HEAD")?;
    out.flush()?;
    Ok(())
}
```

Disposable end-to-end evidence: [patch](../examples/patch.rs).

Desired sketch: `git.write_patch(out, "HEAD~1", "HEAD")?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404) and
[411](https://github.com/joshka/girt/issues/411). `--name-status` output can already use
`girt::display::quote_path` and `TreeChange::status`
([407](https://github.com/joshka/girt/issues/407)).

### Status

<!-- ergonomics-status: partial -->

Partial: status lists untracked files through the ignore rules Git reads, but compares HEAD, index
and working-tree content as literal bytes. Attributes, filters, EOL conversion and `core.filemode`
need application policy; `raw_status` remains for consumers that own all of it.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn status(git: &Handle) -> girt::Result<()> {
    println!("{:#?}", git.status()?);
    Ok(())
}
```

`Handle::discover` loads Git-default configuration, including a global `core.excludesFile`;
`Handle::new` uses the caller-supplied repository configuration.

Disposable end-to-end evidence: [status](../examples/status.rs).

Desired sketch: `git.status()?`. Gap closure: [408](https://github.com/joshka/girt/issues/408)
(status facade).

### Stage and commit

<!-- ergonomics-status: partial -->

Partial: the index writes nested trees. The handle resolves configured author and committer
identities, advances HEAD conditionally and appends HEAD/branch reflogs. Path staging and
normalization still need application policy; this snippet commits an already staged index. `commit`
leaves that index and the files unchanged. Supplying a tree built separately requires materializing
its index/files before a later `switch`, whose baseline is HEAD.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;
use girt::index::Index;

fn commit_tree(git: &mut Handle, index: &Index) -> girt::Result<()> {
    let tree = index.write_tree(git.repository())?;
    git.commit(tree, "Snapshot\n")?;
    Ok(())
}
```

Disposable end-to-end evidence: [status](../examples/status.rs),
[publish_branch](../examples/publish_branch.rs).

Desired sketch: `git.stage([b"src/lib.rs"])?; git.commit(tree, "Explain change")?`. The
index-to-tree operation supplies `tree`. Gap closure:
[403](https://github.com/joshka/girt/issues/403), [405](https://github.com/joshka/girt/issues/405),
[417](https://github.com/joshka/girt/issues/417)
(path staging) and
[408](https://github.com/joshka/girt/issues/408).

### Create, move and delete a branch

<!-- ergonomics-status: supported -->

Supported with an explicit reflog policy; this snippet keeps none. `Repository::reflog_for` picks
Git's `core.logAllRefUpdates` policy for a supplied identity. Move is a compare-and-swap (here to
the same tip); replace the new target with the desired commit. Deletion refuses a concurrent tip
change. Verbose: one transaction per step and the reflog choice.

```rust,no_run
use girt::Repository;
use girt::refs::{RefEdit, RefName, Reflog};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open("repo")?;
    let refs = repo.references()?;
    let tip = repo.head()?.id.ok_or("unborn HEAD")?;
    let branch = RefName::branch("topic")?;
    refs.transaction(&[RefEdit::create(branch.clone(), tip, Reflog::Preserve)])?;
    refs.transaction(&[RefEdit::update(branch.clone(), tip, tip, Reflog::Preserve)])?;
    refs.transaction(&[RefEdit::delete(branch, tip, Reflog::Preserve)])?;
    Ok(())
}
```

Desired sketch: `git.branch("topic").create(tip)?`. Gap closure: the reference helpers in
[405](https://github.com/joshka/girt/issues/405) are delivered; the remaining shared setup is
[408](https://github.com/joshka/girt/issues/408).

### Lightweight and annotated tags

<!-- ergonomics-status: supported -->

Supported: the snippet creates both lightweight and annotated tag references in one transaction with
explicit no-reflog policy. The linked reference transaction example also demonstrates reflogs;
loose_tag shows the annotated object separately. Verbose: TagFields, supplied Signature and byte
vectors; no new tag capability is required for this composition.

```rust,no_run
use girt::refs::{RefEdit, RefName, Reflog};
use girt::{ObjectId, ObjectKind, Repository, Signature, Tag, TagFields};

fn tags(
    repo: &Repository,
    tip: ObjectId,
    identity: Signature,
) -> girt::Result<()> {
    // Caller supplies an existing commit tip and deliberately chooses no reflogs.
    let tag = Tag::new(TagFields {
        target: tip,
        target_kind: ObjectKind::Commit,
        name: b"v1".to_vec(),
        tagger: Some(identity),
        extra_headers: vec![],
        message: b"Release\n".to_vec(),
    })?;
    let id = repo.loose_objects().write_tag(&tag)?;
    repo.references()?.transaction(&[
        RefEdit::create(RefName::tag("light")?, tip, Reflog::Preserve),
        RefEdit::create(RefName::tag("v1")?, id, Reflog::Preserve),
    ])?;
    Ok(())
}
```

Disposable end-to-end evidence: [reference_transaction](../examples/reference_transaction.rs),
[loose_tag](../examples/loose_tag.rs).

Desired sketch: `Signature::now(name, email)?`. Gap closure:
[403](https://github.com/joshka/girt/issues/403) and
[408](https://github.com/joshka/girt/issues/408) (shared repository setup; TagFields stays
explicit). `RefName::tag` came from [405](https://github.com/joshka/girt/issues/405).

### Fetch by remote name

<!-- ergonomics-status: supported -->

Supported for configured trusted remotes and enabled transports. Errors retain partial effects. The
handle supplies default limits, no cancellation, the captured environment and no prompt callbacks.
It requires the remote's configured fetch refspecs. Remote tag/prune configuration is not
interpreted by this prototype; explicit workflow options remain available.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover("repo")?;
    println!("{:?}", git.fetch("origin")?);
    Ok(())
}
```

Desired sketch: `git.fetch("origin")?`. Gap closure:
[408](https://github.com/joshka/girt/issues/408).

### Push the current branch

<!-- ergonomics-status: partial -->

Partial: publish the attached current branch to a new same-named branch on origin. This explicitly
requires the remote branch to be absent; an existing branch needs an observed expected tip. Detached
HEAD is refused. Automatic upstream selection and push.default are not supplied by workflow::push.
Inspect the report for rejected commands; errors retain partial effects.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;
use girt::push::PushCommand;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let git = Handle::discover("repo")?;
    let head = git.repository().head()?;
    if !head.name.is_branch() {
        return Err("current HEAD is not a branch".into());
    }
    let tip = head.id.ok_or("unborn branch")?;
    let command = PushCommand::new(head.name, None, tip);
    let report = git.push("origin", vec![command])?;
    if !report.all_succeeded() {
        return Err(format!("push rejected: {report:?}").into());
    }
    Ok(())
}
```

Desired sketch: `git.push_current_branch()?`. Gap closure:
[418](https://github.com/joshka/girt/issues/418) (destination and lease selection) and
[408](https://github.com/joshka/girt/issues/408).

### Clone with checkout

<!-- ergonomics-status: supported -->

Supported: `workflow::clone(url, destination)` resolves Git-default configuration and transport
policy, creates `origin` and branch tracking configuration, and checks out the remote's default
branch. `workflow::clone_with` accepts explicit configuration inputs, environment, callbacks and
`CloneOptions`; `trusted::Handle::clone` constructs the convenience handle.

```rust,no_run
fn clone_repository(url: &str, destination: &std::path::Path) -> girt::Result<()> {
    let outcome = girt::workflow::clone(url, destination)?;
    println!("{:?}", outcome.clone.head);
    Ok(())
}
```

The destination must be absent and exclusively owned throughout the operation. Core clone publishes
objects, refs, configuration and HEAD before checkout writes files and publishes the index. Errors
preserve partial reports; nothing is automatically rolled back or deleted. Checkout retains its
Linux/macOS platform scope and conservative path policy. Bare and no-checkout modes are explicit
options. Empty remote HEAD skips checkout; detached remote HEAD stays detached. A missing default
branch in a nonempty remote is refused. Unsupported checkout platforms are refused before transfer.

Disposable end-to-end evidence: [clone_repository](../examples/clone_repository.rs). The explicit
`clone::CloneRequest` API remains available for caller-owned transport, validation and publication.

### Read effective config values

<!-- ergonomics-status: supported -->

Supported. Git-default config inputs were delivered by
[390](https://github.com/joshka/girt/issues/390). The handle requests these inputs at discovery. The
optional subsection is structural, not a resource limit default.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover("repo")?;
    let email = git.repository().config().string("user", None, "email");
    println!("{email:?}");
    Ok(())
}
```

Desired sketch: `git.config().string("user", None, "email")`. Gap closure: remaining shared setup in
[408](https://github.com/joshka/girt/issues/408).

### Rev-parse

<!-- ergonomics-status: supported -->

Supported within the documented revision grammar; unsupported expressions return errors. Verbose:
the handle composes the resolver and object view.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn main() -> girt::Result<()> {
    let git = Handle::discover("repo")?;
    println!("{}", git.rev_parse("HEAD~1")?);
    Ok(())
}
```

Desired sketch: `git.rev_parse("HEAD~1")?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404) and
[408](https://github.com/joshka/girt/issues/408).

### Checkout or switch branch

<!-- ergonomics-status: supported -->

Supported within raw checkout's Linux/macOS scope: the handle checks out an existing local branch
and changes HEAD with a reflog, holding prepared reference locks across checkout. The caller must
exclude other working-tree/reference writers, including linked-worktree activity. Branch occupancy,
filters and normalization remain caller policy. Dirty and obstructed checkouts are refused; errors
preserve partial effects for recovery.

With the trusted handle:

```rust,no_run
use girt::trusted::Handle;

fn switch(git: &mut Handle) -> girt::Result<()> {
    // Caller excludes other working-tree and reference writers across this call.
    println!("{:?}", git.switch("topic")?);
    Ok(())
}
```

Disposable end-to-end evidence: [checkout](../examples/checkout.rs).

Desired sketch: `git.switch("topic")?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404) and
[408](https://github.com/joshka/girt/issues/408).

### Blame-style queries

<!-- ergonomics-status: missing -->

Missing: there is no supported blame operation. History traversal and content diff are
prerequisites, not a correct blame implementation. There is no current-code snippet or zero-cost
success here.

Desired sketch: `git.blame("HEAD", b"src/lib.rs")?`. Gap closure:
[413](https://github.com/joshka/girt/issues/413) closes the missing capability; blame is outside the
stated [408](https://github.com/joshka/girt/issues/408) prototype.
