---
title: Objects and history
description: Construct a snapshot and inspect a branch point with verified object reads.
---

A commit connects a directory snapshot to its parents. To build one, store file bytes as blobs,
store the root tree with entries pointing to those blobs, then store a commit pointing to that tree.
Objects are immutable, so a branch name lives in a separate reference that can move between
commits. The program below builds a small snapshot and traces its ancestry.

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
trees. Constructing a tree or commit doesn't check that the objects it references exist, so write
the child objects before you publish a reference to the commit.

## Read a stored snapshot

Open an `Objects` reader once and reuse it for related reads. For each ID, call `read` with a
payload budget; `None` means the object is absent. Check the returned kind before choosing a parser.
Parsing a commit gives its tree and parent IDs. Read its tree separately to get entry names, modes,
and child IDs. Read a blob to obtain file bytes; your application decides whether to decode them as
text. Preserve raw names when displaying or exporting non-UTF-8 repositories.

[Objects::read](https://docs.rs/girt/latest/girt/struct.Objects.html#method.read) verifies framing
and identity. Tree and commit parsing decodes payloads while retaining accepted original bytes.
Validation can impose additional rules. Rebuilding an object from its parsed fields may normalize
its spelling and so change its ID, which is why a tool that inspects objects should report malformed
data instead of rewriting it.

## Choose the history query

`walk` collects reachable commit IDs in breadth-first order, using supplied root and stored parent
order. It doesn't sort by date or guarantee topological order. `is_ancestor` answers reachability,
and `merge_bases` returns every best common ancestor. Criss-cross history can have several bases, so
a caller that needs one base must decide how to combine them.

These queries collect and validate the complete ancestry within their limits, even for an ancestry
test whose two endpoints are the same commit. They don't read trees or blobs. Because the whole
ancestry is walked, size graph budgets for the repository, not for the expected distance between
tips. See the
[history query contracts](https://docs.rs/girt/latest/girt/struct.Objects.html#method.walk) for
ordering, complexity, shallow-boundary, and error contracts.

## Diagnose missing history and refresh storage

A missing object can mean incomplete storage, an incorrect ID, or a reader that predates a newly
published pack. To see new packs, reopen or refresh the object reader; it sees new loose files
without either. A loose-only reader can't find packed objects. In a partial clone, your application
fetches missing objects itself.

A shallow boundary stops ancestry at the listed commits even when their parent objects exist. After
deepening, call `Repository::refresh_shallow` and create a new reader. Raising history limits
doesn't supply missing parents. Investigate a corrupt object or replace it from a trusted source;
don't treat the history that stops at it as a successful partial result.

Continue with the [references guide](/girt/guides/refs/) to give stored commits a durable name. The
[object reader](https://docs.rs/girt/latest/girt/struct.Objects.html) covers reader snapshots and
budgets; [pack APIs](https://docs.rs/girt/latest/girt/pack/index.html) cover artifact production.
