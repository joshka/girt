---
title: Objects and history
description: Construct a snapshot and inspect a branch point with verified object reads.
---

A commit connects a directory snapshot to its parents. To build one, store file bytes as blobs,
store the root tree with entries pointing to those blobs, then store a commit pointing to that tree.
Objects are immutable, so a branch name lives in a separate reference that can move between commits.
The program below builds a small snapshot and traces its ancestry.

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
    let objects = repo.objects_with(PackLimits::default())?;
    assert_eq!(
        objects.walk_with(&[left], HistoryLimits::default())?,
        [left, base]
    );
    assert_eq!(
        objects.merge_bases_with(left, right, HistoryLimits::default())?,
        [base]
    );
    let decoded = objects.read_commit_with(left, girt::ReadLimits::default())?;
    let root = objects
        .read_with(decoded.tree(), girt::ReadLimits::default())?
        .ok_or("tree missing")?;
    assert_eq!(root.kind(), girt::ObjectKind::Tree);
    let entries = Tree::parse(ObjectFormat::Sha1, root.data())?;
    let file = objects
        .read_with(entries.entries()[0].id, girt::ReadLimits::default())?
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

Open an `Objects` reader once and reuse it for related reads. Call `read_commit` for an ID that must
be a commit; it gives the commit's tree and parent IDs, and a missing or non-commit object is an
error. For other objects, call `read` with a payload budget; `None` means the object is absent.
Check the returned kind before choosing a parser. Read its tree separately to get entry names,
modes, and child IDs. Read a blob to obtain file bytes; your application decides whether to decode
them as text. Preserve raw names when displaying or exporting non-UTF-8 repositories.

[Objects::read](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.read) verifies
framing and identity. Tree and commit parsing decodes payloads while retaining accepted original
bytes. Validation can impose additional rules. Rebuilding an object from its parsed fields may
normalize its spelling and so change its ID, which is why a tool that inspects objects should report
malformed data instead of rewriting it.

## List what a store holds

`Objects::stores` yields the primary object directory, then each alternate. Each store's `packs`
reports the pack and index paths, object count, sizes, checksum and whether a `.keep` or `.promisor`
file marked the pack. Pack IDs come from the reader's pinned index in ascending order, with no file
reads. `loose_objects` scans the store's live fan-out directories one entry at a time. It is bounded
by `ListLimits`, can be cancelled and may miss or include files written during the scan.
`object_ids` combines every pack and loose file across all stores into one sorted list without
duplicates, as `git cat-file --batch-all-objects` does. `headers_controlled` then reads kinds and
sizes in pack order without decoding payloads.

Alternates are borrowed: a pruning tool must check `is_alternate` and leave them alone. Multi-pack
indexes are not read and stray files are not reported. See
[object reader](https://docs.rs/girt/latest/girt/objects/struct.Objects.html) for the contracts.

## Choose the history query

`walk` collects reachable commit IDs in breadth-first order, using supplied root and stored parent
order. It doesn't sort by date or guarantee topological order. `is_ancestor` answers reachability,
and `merge_bases` returns every best common ancestor. Criss-cross history can have several bases, so
a caller that needs one base must decide how to combine them.

These queries collect and validate the complete ancestry within their limits, even for an ancestry
test whose two endpoints are the same commit. They don't read trees or blobs. Because the whole
ancestry is walked, size graph budgets for the repository, not for the expected distance between
tips. See the
[history query contracts](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.walk)
for ordering, complexity, shallow-boundary, and error contracts.

## Turn user input into commits

When IDs come from a person rather than your program, parse them as Git revisions. `Revision`
accepts expressions such as `main~2`, `v1.0^{tree}`, `HEAD:src/lib.rs`, `main@{1}` and `@{u}`;
`Range` adds `A..B`, `A...B` and `^A`. Parsing reads nothing, so syntax errors surface first. A
`Resolver` then looks the expression up in one repository, and `Resolver::select` turns ranges into
commits to include and exclude:

```rust
use girt::revision::{Range, Resolver, Selection};
use girt::{PackLimits, Repository};

/// Resolves an argument such as `main..topic` to the commits to walk and to skip.
fn select(
    repository: &Repository,
    argument: &str,
) -> Result<Selection, Box<dyn std::error::Error>> {
    let range: Range = argument.parse()?;
    let objects = repository.objects_with(PackLimits::default())?;
    let resolver = Resolver::new(repository, &objects)?;
    Ok(resolver.select(&[range])?)
}
```

Pseudo-references such as `ORIG_HEAD`, dated reflog selectors, `@{-N}` and message searches are
refused with an explicit error rather than guessed. The `girt::revision` module documentation lists
the full syntax and its limits; the `girt` command line tool accepts the same syntax, as its
[README](https://github.com/joshka/girt/blob/main/girt-cli/README.md#limits) notes.

<!-- Link the module on docs.rs (girt/latest/girt/revision/) once a release includes it. -->

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
[object reader](https://docs.rs/girt/latest/girt/objects/struct.Objects.html) covers reader
snapshots and budgets; [pack APIs](https://docs.rs/girt/latest/girt/pack/index.html) cover artifact
production.
