---
title: Objects and history
description: Construct a snapshot and inspect a branch point with verified object reads.
---

A commit connects a directory snapshot to its parents. To build one, store file bytes as blobs,
store the root tree with entries pointing to those blobs, then store a commit pointing to that tree.
Objects are immutable; the branch name is a separate reference. This guide builds a small snapshot
and traces its ancestry.

## Construct a branch point

Add dependencies with `cargo add girt tempfile`, then use this complete program:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::{
        Commit, CommitFields, HistoryLimits, InitKind, ObjectFormat, PackLimits, Repository,
        Signature, Tree,
    };
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r.git"),
        InitKind::Bare,
    )?;
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"# Project\n")?;
    let tree = loose.write_tree(&Tree::new(
        ObjectFormat::Sha1,
        vec![girt::TreeEntry {
            mode: girt::EntryMode::Blob,
            name: b"README.md".to_vec(),
            id: blob,
        }],
    )?)?;
    let person = Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = |parents, message: &[u8]| {
        loose.write_commit(&Commit::new(CommitFields {
            tree,
            parents,
            author: person.clone(),
            committer: person.clone(),
            extra_headers: vec![],
            message: message.to_vec(),
        })?)
    };
    let base = commit(vec![], b"base\n")?;
    let left = commit(vec![base], b"left\n")?;
    let right = commit(vec![base], b"right\n")?;
    let objects = repo.objects(PackLimits::default())?;
    assert_eq!(
        objects.walk(&[left], HistoryLimits::default())?,
        [left, base]
    );
    assert_eq!(
        objects.merge_bases(left, right, HistoryLimits::default())?,
        [base]
    );
    let stored = objects
        .read(left, girt::ReadLimits::default())?
        .ok_or("commit missing")?;
    assert_eq!(stored.kind(), girt::ObjectKind::Commit);
    let decoded = Commit::parse(ObjectFormat::Sha1, stored.data())?;
    let root = objects
        .read(decoded.tree(), girt::ReadLimits::default())?
        .ok_or("tree missing")?;
    assert_eq!(root.kind(), girt::ObjectKind::Tree);
    let entries = Tree::parse(ObjectFormat::Sha1, root.data())?;
    let file = objects
        .read(entries.entries()[0].id, girt::ReadLimits::default())?
        .ok_or("blob missing")?;
    assert_eq!(file.kind(), girt::ObjectKind::Blob);
    assert_eq!(file.data(), b"# Project\n");
    Ok(())
}
```

Run it with `cargo run`. It creates a base commit and two children that share the same tree.
Different messages and parent edges give the commits different identities even when their file
snapshots match. The temporary repository is removed when the program exits.

To add a file, call `loose.write_blob`, put its ID in a `TreeEntry` with `EntryMode::Blob`, then
pass those entries to `Tree::new`. A tree represents one directory; subdirectories need their own
trees. Construction alone does not check that referenced objects exist. Write the child objects
before publishing the commit's reference.

## Read a stored snapshot

Obtain an `Objects` reader once and retain it across related reads. For each ID, call `read` with a
payload budget, handle `None` as absence, then check the returned kind before choosing a parser. A
commit parser reveals its tree and parent IDs. Read its tree separately to get entry names, modes,
and child IDs. Read a blob to obtain file bytes; decoding those bytes as text is application policy.
Preserve raw names when displaying or exporting non-UTF-8 repositories.

[Objects::read](https://docs.rs/girt/latest/girt/struct.Objects.html#method.read) verifies framing
and identity. Tree and commit parsing decodes payloads while retaining accepted original bytes.
Validation can impose additional rules; rebuilding parsed fields may normalize spelling and change
identity. Inspectors should report malformed data without silently rewriting it.

## Choose the history query

`walk` collects reachable commit IDs in breadth-first order, using supplied root and stored parent
order. It does not sort by date or promise topological order. `is_ancestor` answers reachability,
and `merge_bases` returns every best common ancestor. Criss-cross history can have several bases; a
caller needing one must define how it combines them.

These queries collect and validate complete ancestries under their limits, including an ancestry
test with equal endpoints. They do not read trees or blobs. Pick graph budgets for the repository
size rather than the expected distance between tips. See the
[history query contracts](https://docs.rs/girt/latest/girt/struct.Objects.html#method.walk) for
ordering, complexity, shallow-boundary, and error contracts.

## Diagnose missing history and refresh storage

A missing object can mean incomplete storage, an incorrect ID, or a reader that predates a newly
published pack. Reopen or refresh the object reader for new packs; it already reads loose files
live. A loose-only reader cannot find packed objects. For a partial clone, retrieval of missing
objects belongs to the application.

Shallow boundaries deliberately stop ancestry at declared commits even when parent objects exist.
After deepening, call `Repository::refresh_shallow` and create a new reader. Increasing history
limits does not supply missing parents. A corrupt object needs investigation or replacement from a
trusted source; it must not become a successful partial history result.

Continue with [references guide](/girt/guides/refs/) to give stored commits a durable name. The
[object reader](https://docs.rs/girt/latest/girt/struct.Objects.html) covers reader snapshots and
budgets; [pack APIs](https://docs.rs/girt/latest/girt/pack/index.html) cover artifact production.
