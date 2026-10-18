# Indexed History Queries

A commit-graph indexes commit trees, ordered parents, dates and generation numbers. Reading this
metadata avoids decompressing commit objects for reachability queries. The file is an auxiliary
claim about objects: its checksum detects damaged bytes, but does not prove that the claim agrees
with the object database.

## Choose the trust boundary

Repository object readers attach a standalone graph or split chain by default after checksum and
structural validation, honoring `core.commitGraph=false`. Missing, damaged or unusable graphs fall
back to ordinary object reads. This trusts indexed object claims without decoding every commit, as
Git does. Loading writes nothing. Bloom read configuration settings are not interpreted.

For diagnostics, `Repository::commit_graph` exposes loading errors rather than silently falling
back. To require object verification before using indexed history, call `CommitGraph::verify`. This
initial scan checks indexed trees, parents and timestamps against verified commit objects; reuse the
resulting reader across queries to amortize it. It does not check generation equations. Use
`Objects::without_commit_graph` when every history query must read commit objects directly.

Attachment checks closed, acyclic ancestry. Generation pruning requires every parent edge to
strictly decrease an available generation value. Typed object reads always verify objects.

```rust
use girt::{ObjectId, Repository};

fn difference(
    repository: &Repository,
    tip: ObjectId,
    base: ObjectId,
) -> girt::Result<Vec<ObjectId>> {
    let mut objects = repository.objects()?.without_commit_graph();
    match repository.commit_graph() {
        Ok(Some(graph)) => {
            graph.verify(&objects)?;
            objects = objects.with_commit_graph(graph)?;
        }
        Ok(None) => {}
        Err(error) => eprintln!("Using objects after auxiliary index failure: {error}"),
    }
    Ok(objects.walk_range(&[tip], &[base])?)
}
```

For the default checksum-based auxiliary-index trust model, use `repository.objects()` directly.
Typed commit/tree/blob reads still read and verify objects. Indexed history queries can answer from
metadata even when indexed objects have since been removed; refresh or verify according to the
application's storage and concurrency policy. Do not use trusted auxiliary metadata to authorize
object deletion.

## Query ranges and ancestry

`walk_range(include, exclude)` returns the commits reachable from any included root and from no
excluded root. Its result order is unspecified. With complete indexed root coverage and checked
generation ordering, it stops when no included work remains in the queue. A near-tip range need not
visit the excluded side's older history. Missing indexed roots or unusable generation data select
complete traversal with ordinary object fallback.

`walk` retains its breadth-first ordering; `merge_bases` returns all best common ancestors sorted by
identity. Indexed ancestry tests stop at the target generation or a match. Declared shallow
boundaries stop edges in every query. Commit and parent budgets cover both sides of explored ranges;
per-object byte/depth limits apply to ordinary object reads, not indexed records.

The index is owned and immutable. Clones share it, and refreshing pack storage retains it. New
commits absent from the index use object reads. Load and attach a new graph when the application
needs newer indexed coverage. Split chains are loaded as a fixed collection of bytes; a concurrent
replacement can cause a load error, after which callers may retain the old index or use objects.

## Changed-path queries

`CommitGraph::might_change_path` returns a conservative first-parent filter answer: `Some(false)`
can skip comparison, `Some(true)` can be a false positive, and `None` requires ordinary comparison.
Version 1 filters are ignored for queries containing non-ASCII bytes. Version 2 supports byte paths.
Missing filters, unknown versions, unsupported hash counts and malformed offsets return unknown;
they do not invalidate otherwise usable commit metadata.

`Objects::walk_path_first_parent` follows only the first-parent chain and returns commits whose
literal file or directory entry changes identity or mode. It includes additions and deletions;
shallow boundaries compare with an empty snapshot. It does not perform pathspec matching, rename
following or general merge-history simplification. Bloom negatives avoid tree comparisons, and
single-parent blame uses the same optimization. Merge blame still examines all parents.

A filter in a merge describes the first-parent difference only. Empty-change and oversized-change
filters remain conservative, and explicit object verification checks commit metadata rather than
recomputing Bloom filters. Applications requiring object-backed path correctness can leave the index
detached.

## Build graph artifacts

`CommitGraph::from_objects` reads and verifies the selected commit closure independently of any
attached auxiliary index, then calculates topological generations and corrected dates. It refuses
shallow history and timestamps outside the format's unsigned 34-bit range. The default and `_with`
forms use the same commit, parent and per-read bounds as history queries. Construction writes no
files.

```rust
use girt::{ObjectId, Repository, commit_graph::CommitGraph};

fn encode_history(repository: &Repository, tip: ObjectId) -> girt::Result<Vec<u8>> {
    let objects = repository.objects()?;
    let graph = CommitGraph::from_objects(&objects, &[tip])?;
    Ok(graph.encode()?)
}
```

`encode` returns a standalone file. `encode_incremental` takes existing layer bytes in base-to-tip
order and returns one new layer. It preserves base positions, checks overlapping metadata and emits
only new commits; base commits absent from the selected graph remain in the chain. Callers must
retain those exact base files and publish a manifest listing their checksums followed by the new
checksum. Encoding checks base structure and checksums, not base object claims. Verify bases when
required by the application's trust policy. New layers omit Bloom filters; any base filters remain
in their original files.

Artifact encoding uses the default 512 MiB output limit and does not publish files or coordinate
writers. A parsed generation-v1-only graph lacks corrected dates required by this encoder; rebuild
it from objects before encoding. Incremental artifact output does not yet avoid a complete
object-verification scan during construction.

## Publish files

`Repository::write_commit_graph(roots, mode)` constructs verified metadata and publishes it to the
repository's object store. Roots are explicit; this operation does not select references or change
maintenance settings. Use `GraphWriteMode::Full` for a standalone file or `Incremental` to retain
base layers and append one split layer. For separate construction and publication, call
`CommitGraph::install` on a graph artifact.

Publication holds both standalone and chain writer locks. It writes complete checksum-named layers
before replacing the chain manifest, then removes a migrated standalone file so readers can see the
chain. Previous base files are retained. Full publication replaces the standalone file and retains
any previous chain. No objects, packs, references or reflogs change.

`GraphInstallError::index_published` reports whether a standalone file or manifest was published
before a later failure. A migrated standalone can still shadow a newly published chain after a
removal failure. Reopen and inspect current state before retrying. Failed publication can leave
unreferenced layer files; lock cleanup after an operation error is best effort. Existing writer
locks are never stolen. Loaded readers retain their bytes, and transient locks participate in
caller-controlled cleanup. Exclude hostile directory/lock replacement and object pruning while
constructing and publishing. File contents are synchronized, but publication does not promise a
power-loss transaction, old-layer compaction or a shared-repository permissions policy.

Git 2.56.0 observations distinguish maintenance entry points: `git gc` writes a standalone graph by
default, and `gc.writeCommitGraph=false` suppresses it. Default `git maintenance run` and its
explicit `--task=commit-graph` task write split chains. girt's explicit writer is independent of
those scheduling policies; automatic maintenance integration is separate.
