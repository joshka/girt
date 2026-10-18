# Indexed History Queries

A commit-graph indexes commit trees, ordered parents, dates and generation numbers. Reading this
metadata avoids decompressing commit objects for reachability queries. The file is an auxiliary
claim about objects: its checksum detects damaged bytes, but does not prove that the claim agrees
with the object database.

## Choose the trust boundary

`Repository::commit_graph` loads a standalone graph or split chain and honors `core.commitGraph`. An
absent setting enables loading. A disabled or missing graph returns `None`; damaged files return an
error. Applications can report that error and continue with ordinary object reads. Opening the graph
writes nothing.

`Objects::with_commit_graph` explicitly trusts indexed object claims. It checks that indexed
ancestry is closed and acyclic, and enables generation pruning only when every parent edge strictly
decreases an available generation value. To check indexed tree, parent and timestamp claims against
verified commit objects first, call `CommitGraph::verify`. This initial scan costs object reads;
reuse the resulting reader across queries to amortize it. Verification does not check exact
generation equations.

```rust
use girt::{ObjectId, Repository};

fn difference(
    repository: &Repository,
    tip: ObjectId,
    base: ObjectId,
) -> girt::Result<Vec<ObjectId>> {
    let mut objects = repository.objects()?;
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

For Git's checksum-based auxiliary-index trust model, omit the explicit object-verification call.
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
