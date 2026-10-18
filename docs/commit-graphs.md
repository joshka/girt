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
