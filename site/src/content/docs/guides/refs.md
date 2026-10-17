---
title: References and reflogs
description: Publish a stored commit with a precondition and inspect its name history.
---

Storing a commit leaves it unnamed. Publish a reference when it should become a branch or tag tip.
The object graph and its names have separate lifetimes: many names can point to one commit, and
moving a name does not change that commit or its parents.

## Publish a first branch

Add dependencies with `cargo add girt tempfile`. This program creates a complete empty snapshot,
publishes its commit as `refs/heads/main` only if that name is absent, then resolves `HEAD` and
reads the reflog:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::refs::{RefEdit, RefName, Reflog};
    use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature};
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r.git"),
        InitKind::Bare,
    )?;
    let loose = repo.loose_objects();
    let person = Signature {
        name: b"A. Writer".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let tree = loose.write_tree(&repo.new_tree(vec![])?)?;
    let id = loose.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: person.clone(),
        committer: person.clone(),
        extra_headers: vec![],
        message: b"Initial snapshot\n".to_vec(),
    })?)?;
    let refs = repo.references()?;
    let main = RefName::branch("main")?;
    let edit = RefEdit::create(
        main.clone(),
        id,
        Reflog::Append {
            committer: person,
            message: b"commit (initial)".to_vec(),
        },
    );
    refs.transaction(&[edit])?;
    assert_eq!(repo.head()?.id, Some(id));
    let logs = refs.reflog(&main)?.ok_or("reflog missing")?;
    assert_eq!(logs[0].new, id);
    Ok(())
}
```

Run it with `cargo run`. The temporary repository is removed at exit. Its `HEAD` initially names an
unborn `main`; publishing the branch gives that existing symbolic name a resolvable ID. A direct
branch edit logs only the branch. To update through `HEAD` and log the symbolic chain, use a
`RefEdit` for `HEAD` with `Dereference::Follow`.

## Update an observed value

When moving an existing branch, first read its stored target and choose
`Expected::Value(observed_target)` for the edit. `RefEdit::update(name, old, new, reflog)` builds
that edit for a direct ID, and `RefEdit::delete(name, old, reflog)` removes the branch only if it
still holds `old`. For a complete compare-and-swap update example, see
[References::transaction](https://docs.rs/girt/latest/girt/refs/struct.References.html#method.transaction).
The transaction compares that value under its locks, so if the branch moved after your read, the
update fails instead of overwriting the newer value. The other preconditions:

- `Expected::Absent` requires that the name doesn't exist yet. Use it to create a reference.
- `Expected::Exists` requires some stored value.
- `Expected::AbsentOr(value)` accepts either absence or that exact value.
- `Expected::Any` accepts any valid stored value or absence; it checks nothing.

A mismatch means the stored value doesn't satisfy the precondition. With `Expected::Value`, that
means the reference changed since you read it. Read it again and decide whether the update still
makes sense; don't retry with `Any`.

Store and validate the intended object graph before publishing. Reference updates check names,
formats, and preconditions; they do not establish object existence, object kind, ancestry, or
permission to discard another branch tip. A tool that only allows fast-forwards must check ancestry
itself before attempting its conditional update.

## Resolve names and annotated tags separately

`read` returns the stored direct ID or symbolic name. `resolve` follows symbolic names to a terminal
name and optional ID;
[`Repository::head`](https://docs.rs/girt/latest/girt/repository/struct.Repository.html) resolves
`HEAD` with Git's default limit of four symbolic hops. An absent terminal name can be an ordinary
unborn branch; it is not an object-storage error. `Objects::peel` then follows annotated tag objects
if required. A lightweight tag names its target directly; an annotated tag reference names a stored
`Tag` object. Listing references excludes pseudorefs such as `HEAD`. `list_namespace` selects whole
`/` components; `list_prefix` selects names by byte prefix, such as `refs/heads/fe` for every branch
starting `fe`.

## Choose and interpret logging

Low-level transactions take explicit `Reflog` policy. They don't read Git config or take an identity
from the environment. `Repository::reflog_for` returns the policy Git's `core.logAllRefUpdates`
would choose for a name: `AppendIfChanged` with the identity and message you supply, or `Preserve`.
`Append` records even an unchanged ID; `AppendIfChanged` records stored-target changes. `Preserve`
leaves existing logs untouched. The example supplies a fixed identity and timestamp for
reproducibility; an application supplies its actual actor and time.

A reflog records the values a name has held, oldest first. It says nothing about commit ancestry.
Zero IDs represent creation or deletion. Old objects may already be unavailable. For bounded
inspection of imported logs, use
[imported_reflog](https://docs.rs/girt/latest/girt/refs/struct.References.html#method.imported_reflog)
and inspect completeness before treating recovered records as a full inventory.

## Recover from a failed publication

A preparation failure leaves reference and log contents unchanged. A publication failure includes
completed outcomes and has no rollback: a branch may advance while its log append fails. Preserve
those outcomes, inspect current state, and decide which effects still need to happen. Repeating the
whole batch can duplicate log entries or overwrite newer work.

A lock conflict means the attempt did not obtain exclusive access. The ordinary files transaction
already retries reference and packed-ref locks for Git's default waits before returning a conflict;
reflog locks fail immediately. Select `transaction_files_with_options` for explicit waits and
cancellation. Whether to try again is up to the application; don't delete another writer's lock.
Files-backed batches publish sequentially and readers may see intermediate states. Reftable
publishes references and logs together within one stack, with partial effects possible across
linked-worktree stacks. See
[transaction contracts](https://docs.rs/girt/latest/girt/refs/struct.References.html#method.transaction)
and [errors](https://docs.rs/girt/latest/girt/refs/enum.TransactionError.html) for the full
guarantees.
