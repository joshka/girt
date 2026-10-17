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
counts measure caller burden, not correctness or performance.

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

Supported. Discovery already avoids hand-written parent traversal. Configuration is a separate
choice; see the config task.

```rust,no_run
use girt::Repository;

fn main() -> girt::Result<()> {
    let repo = Repository::discover(".")?;
    println!("{}", repo.git_dir().display());
    Ok(())
}
```

Desired sketch: `Repository::discover(".")?`. Gap closure: no discovery capability gap; shared
handle work in [408](https://github.com/joshka/girt/issues/408).

### Read a file at a revision

<!-- ergonomics-status: supported -->

Supported for the revision grammar implemented by `revision`. Resolving to Blob checks the kind;
reading by ID alone does not; `read_blob` checks the kind and reports a missing object as an error.
Verbosity: separate object and resolver handles.

```rust,no_run
use girt::revision::Resolver;
use girt::{ObjectKind, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open("repo")?;
    let objects = repo.objects()?;
    let resolver = Resolver::new(&repo, &objects)?;
    let id = resolver.resolve_to(&"HEAD:src/lib.rs".parse()?, ObjectKind::Blob)?;
    let blob = objects.read_blob(id)?;
    println!("{} bytes", blob.len());
    Ok(())
}
```

Desired sketch: `git.blob_at("HEAD", b"src/lib.rs")?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404) and
[408](https://github.com/joshka/girt/issues/408).

### Log first N in date order with exclusions

<!-- ergonomics-status: partial -->

Partial: this eager walk truncates its result after traversal; it promises neither date order nor
exclusions. Missing capability: a lazy date-ordered excluding walk; parsing a range already produces
include/exclude roots.

```rust,no_run
use girt::Repository;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open("repo")?;
    let objects = repo.objects()?;
    let root = repo
        .references()?
        .resolve(&girt::refs::RefName::HEAD, 5)?
        .id
        .ok_or("unborn HEAD")?;
    for id in objects.walk(&[root])?.into_iter().take(20) {
        println!("{id}");
    }
    Ok(())
}
```

Desired sketch: `git.log("main").exclude("release").date_order().take(20)`. Gap closure:
[416](https://github.com/joshka/girt/issues/416) (iterator and exclusions) and
[408](https://github.com/joshka/girt/issues/408) (facade).

### Diff two commits: tree and content

<!-- ergonomics-status: partial -->

Partial: the compiled snippet resolves both commits to trees, compares them and reads changed blob
contents. It emits debug representations of edit ranges, not unified patches. Gitlinks need an
explicit submodule policy. Verbosity includes the content diff's ReadLimits, DiffLimits and a false
AtomicBool. Byte vectors and repeated ObjectFormat occur only in the linked fixture and are excluded
from the cost count.

```rust,no_run
use std::sync::atomic::AtomicBool;

use girt::content_diff::{BinaryMode, BlobContent, DiffLimits, diff};
use girt::revision::Resolver;
use girt::{ObjectKind, ReadLimits, Repository};

fn compare(repo: &Repository) -> girt::Result<()> {
    let objects = repo.objects()?;
    let resolver = Resolver::new(repo, &objects)?;
    let old = resolver.resolve_to(&"HEAD~1".parse()?, ObjectKind::Tree)?;
    let new = resolver.resolve_to(&"HEAD".parse()?, ObjectKind::Tree)?;
    let cancel = AtomicBool::new(false);
    let limits = DiffLimits::default();
    for change in objects.compare_trees(Some(old), Some(new))? {
        let content = BlobContent::read(
            &objects,
            &change,
            ReadLimits::default(),
            limits.max_input_bytes,
            &cancel,
        )?;
        let edits = diff(
            &content.old,
            &content.new,
            BinaryMode::Auto,
            limits,
            &cancel,
        )?;
        println!("{change:?}: {edits:?}");
    }
    Ok(())
}
```

Disposable end-to-end evidence: [content_diff](../examples/content_diff.rs).

Desired sketch: `objects.diff_with_renames(old_tree, new_tree)?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404), [406](https://github.com/joshka/girt/issues/406)
(combined rename result); patch rendering in [411](https://github.com/joshka/girt/issues/411).
`--name-status` output can already use `girt::display::quote_path` and `TreeChange::status`
([407](https://github.com/joshka/girt/issues/407)).

### Status

<!-- ergonomics-status: partial -->

Partial: raw status compares HEAD, index and literal working-tree bytes. It deliberately uses
`RawFilesWithoutIgnores`; ignored files and normalization need application policy. Ignore-aware
status is a capability gap.

```rust,no_run
use girt::Repository;
use girt::status::{Baseline, Untracked};

fn status(repo: &Repository) -> girt::Result<()> {
    let report = repo.raw_status(Baseline::Head, Untracked::RawFilesWithoutIgnores)?;
    println!("{report:#?}");
    Ok(())
}
```

Disposable end-to-end evidence: [status](../examples/status.rs).

Desired sketch: `git.status()?`. Gap closure: [404](https://github.com/joshka/girt/issues/404) and
[408](https://github.com/joshka/girt/issues/408) (status facade) and
[414](https://github.com/joshka/girt/issues/414) (ignore integration).

### Stage and commit

<!-- ergonomics-status: partial -->

Partial: status shows writing an explicit index entry; publish_branch writes a tree and commit and
conditionally advances HEAD without reflogs. Together these are building blocks, not an
index-to-tree stage-and-commit operation. Callers must not silently discard existing index entries.
Missing: index-to-tree and path editing. Verbose: CommitFields, identity bytes/time, repeated format
and empty vectors; default identity and reflog policy must be deliberate.

```rust,no_run
use girt::refs::{Expected, RefName, Target};
use girt::{Commit, CommitFields, ObjectId, Repository, Signature};

fn commit_tree(
    repo: &Repository,
    tree: ObjectId,
    identity: Signature,
) -> girt::Result<()> {
    // Caller supplies a fully built tree; this does not consume the index or stage paths.
    let refs = repo.references()?;
    let parent = refs.resolve(&RefName::HEAD, 5)?.id;
    let expected = parent.map_or(Expected::Absent, |id| Expected::Value(Target::Direct(id)));
    let commit = Commit::new(CommitFields {
        tree,
        parents: parent.into_iter().collect(),
        author: identity.clone(),
        committer: identity,
        extra_headers: vec![],
        message: b"Snapshot\n".to_vec(),
    })?;
    let id = repo.loose_objects().write_commit(&commit)?;
    refs.update_resolved_without_reflog(&RefName::HEAD, id, expected)?;
    Ok(())
}
```

Disposable end-to-end evidence: [status](../examples/status.rs),
[publish_branch](../examples/publish_branch.rs).

Desired sketch: `git.stage([b"src/lib.rs"])?; git.commit(tree, "Explain change")?`. The
index-to-tree operation in [406](https://github.com/joshka/girt/issues/406) supplies `tree`. Gap
closure: [403](https://github.com/joshka/girt/issues/403),
[405](https://github.com/joshka/girt/issues/405), [406](https://github.com/joshka/girt/issues/406),
[417](https://github.com/joshka/girt/issues/417) (path staging) and
[408](https://github.com/joshka/girt/issues/408).

### Create, move and delete a branch

<!-- ergonomics-status: supported -->

Supported for explicit no-reflog policy. Move uses a conditional update (here to the same tip);
replace the new target with the desired commit. Deletion refuses a concurrent tip change. Verbose:
full reference names, symbolic depth and repeated target/expected values.

```rust,no_run
use girt::Repository;
use girt::refs::{Expected, RefName, Target};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open("repo")?;
    let refs = repo.references()?;
    let tip = refs.resolve(&RefName::HEAD, 5)?.id.ok_or("unborn HEAD")?;
    let branch = RefName::new("refs/heads/topic")?;
    refs.update_without_reflog(&branch, Target::Direct(tip), Expected::Absent)?;
    refs.update_without_reflog(
        &branch,
        Target::Direct(tip),
        Expected::Value(Target::Direct(tip)),
    )?;
    refs.delete_without_reflog(&branch, Expected::Value(Target::Direct(tip)))?;
    Ok(())
}
```

Desired sketch: `RefEdit::update(RefName::branch("topic")?, tip, new_tip)`. Gap closure:
[405](https://github.com/joshka/girt/issues/405) and
[408](https://github.com/joshka/girt/issues/408).

### Lightweight and annotated tags

<!-- ergonomics-status: supported -->

Supported: the snippet creates both lightweight and annotated tag references with explicit no-reflog
policy. The linked reference transaction example also demonstrates reflogs; loose_tag shows the
annotated object separately. Verbose: TagFields, supplied Signature, byte vectors and Expected; no
new tag capability is required for this composition.

```rust,no_run
use girt::refs::{Expected, RefName, Target};
use girt::{ObjectId, ObjectKind, Repository, Signature, Tag, TagFields};

fn tags(
    repo: &Repository,
    tip: ObjectId,
    identity: Signature,
) -> girt::Result<()> {
    // Caller supplies an existing commit tip and deliberately chooses no reflogs.
    let refs = repo.references()?;
    refs.update_without_reflog(
        &RefName::new("refs/tags/light")?,
        Target::Direct(tip),
        Expected::Absent,
    )?;
    let tag = Tag::new(TagFields {
        target: tip,
        target_kind: ObjectKind::Commit,
        name: b"v1".to_vec(),
        tagger: Some(identity),
        extra_headers: vec![],
        message: b"Release\n".to_vec(),
    })?;
    let id = repo.loose_objects().write_tag(&tag)?;
    refs.update_without_reflog(
        &RefName::new("refs/tags/v1")?,
        Target::Direct(id),
        Expected::Absent,
    )?;
    Ok(())
}
```

Disposable end-to-end evidence: [reference_transaction](../examples/reference_transaction.rs),
[loose_tag](../examples/loose_tag.rs).

Desired sketch: `RefName::tag("v1")?; Signature::now(name, email)?`. Gap closure:
[403](https://github.com/joshka/girt/issues/403), [405](https://github.com/joshka/girt/issues/405)
and [408](https://github.com/joshka/girt/issues/408) (shared repository setup; TagFields stays
explicit).

### Fetch by remote name

<!-- ergonomics-status: supported -->

Supported for configured trusted remotes and enabled transports. Errors retain partial effects. The
convenience workflow already supplies limits and never-cancel behavior. Verbosity: explicit
repository, options, environment and callbacks.

```rust,no_run
use girt::Repository;
use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};

fn main() -> girt::Result<()> {
    let repo = Repository::open("repo")?;
    let outcome = workflow::fetch(
        &repo,
        "origin",
        &FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]),
        &Environment::from_process(),
        &mut NoCallbacks,
    )?;
    println!("{:?}", outcome);
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

```rust,no_run
use girt::Repository;
use girt::push::PushCommand;
use girt::refs::RefName;
use girt::workflow::{self, Environment, NoCallbacks, PushOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open("repo")?;
    let head = repo.references()?.resolve(&RefName::HEAD, 5)?;
    if !head.name.as_bytes().starts_with(b"refs/heads/") {
        return Err("current HEAD is not a branch".into());
    }
    let tip = head.id.ok_or("unborn branch")?;
    let command = PushCommand::new(head.name, None, tip);
    let report = workflow::push(
        &repo,
        "origin",
        vec![command],
        &PushOptions::default(),
        &Environment::from_process(),
        &mut NoCallbacks,
    )?;
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

<!-- ergonomics-status: partial -->

Partial: CloneRequest creates a no-checkout repository; checkout_tree can then materialize an
explicit commit tree under exclusive ownership. No workflow clone or combined checkout transaction
is provided. The examples intentionally keep publication reports and cleanup ownership visible.
Verbose: BranchSelection, transfer limits, controls, false cancellation and separate finish.

```rust,no_run
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::InitKind;
use girt::clone::{BranchSelection, CloneRequest};
use girt::fetch::{FetchLimits, FetchUpdateLimits};
use girt::refs::Reflog;
use girt::transport::TransportControl;

fn clone_without_checkout(
    source: &Path,
    destination: &Path,
    url: &[u8],
) -> girt::Result<()> {
    // URL is credential-free metadata; source is the caller-selected native local endpoint.
    let request = CloneRequest::prepare_tracking(
        destination,
        InitKind::Worktree,
        url,
        BranchSelection::Default,
        Reflog::Preserve,
    )?;
    let cancel = AtomicBool::new(false);
    let ready = request.receive_local(
        source,
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| std::ops::ControlFlow::Continue(()),
    )?;
    let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
    println!("{:?}", report.head);
    Ok(())
}
```

Disposable end-to-end evidence: [clone_repository](../examples/clone_repository.rs),
[checkout](../examples/checkout.rs).

Desired sketch: `CloneOptions::new(url, destination).with_checkout(true).run()?`. Gap closure:
[412](https://github.com/joshka/girt/issues/412) (clone orchestration is outside the stated
[408](https://github.com/joshka/girt/issues/408) prototype).

### Read effective config values

<!-- ergonomics-status: supported -->

Supported. Git-default config inputs were delivered by
[390](https://github.com/joshka/girt/issues/390). A plain open does not request these inputs. The
optional subsection is structural, not a resource limit default.

```rust,no_run
use girt::Repository;
use girt::config::ConfigInputs;
use girt::workflow::Environment;

fn main() -> girt::Result<()> {
    let inputs = ConfigInputs::git_default(&Environment::from_process())?;
    let repo = Repository::open_with_config("repo", &inputs)?;
    let email = repo.config().string("user", None, "email");
    println!("{email:?}");
    Ok(())
}
```

Desired sketch: `git.config().string("user", None, "email")`. Gap closure: remaining shared setup in
[408](https://github.com/joshka/girt/issues/408).

### Rev-parse

<!-- ergonomics-status: supported -->

Supported within the documented revision grammar; unsupported expressions return errors. Verbose:
objects and resolver handles.

```rust,no_run
use girt::Repository;
use girt::revision::Resolver;

fn main() -> girt::Result<()> {
    let repo = Repository::open("repo")?;
    let objects = repo.objects()?;
    let resolver = Resolver::new(&repo, &objects)?;
    let id = resolver.resolve(&"HEAD~1".parse()?)?;
    println!("{id}");
    Ok(())
}
```

Desired sketch: `git.rev_parse("HEAD~1")?`. Gap closure:
[404](https://github.com/joshka/girt/issues/404) and
[408](https://github.com/joshka/girt/issues/408).

### Checkout or switch branch

<!-- ergonomics-status: partial -->

Partial: checkout_tree updates raw files and the index from explicit baseline/target tree IDs under
exclusive ownership. It does not switch HEAD. A branch switch must coordinate HEAD, dirty state,
index and files; moving a symref alone is insufficient. Verbosity: tree lookup; missing capability:
branch switch orchestration.

```rust,no_run
use girt::{ObjectId, Repository};

fn checkout(
    repo: &Repository,
    old: Option<ObjectId>,
    new: ObjectId,
) -> girt::Result<()> {
    // Caller holds exclusive ownership and supplies the actual baseline tree.
    let report = repo.checkout_tree(old, Some(new))?;
    println!("{report:?}");
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
