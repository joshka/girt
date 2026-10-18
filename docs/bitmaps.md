# Read reachability bitmap files

A reachability bitmap records a selected commit's reachable objects as positions in a fixed pack or
multi-pack index (MIDX). Use `girt::bitmap::BitmapIndex` to inspect these files without walking
object contents. Use `ReverseIndex` to read a `.rev` permutation. Both parsers support SHA-1 and
SHA-256 inventories.

```rust,no_run
use girt::Repository;
use girt::bitmap::{BitmapIndex, ReverseIndex};

let repository = Repository::open("repo.git")?;
let objects = repository.objects()?;
let pack = objects.packs().next().ok_or("no pack")?;
let bytes = std::fs::read(pack.path().with_extension("bitmap"))?;
let bitmap = BitmapIndex::parse(&bytes, pack.checksum(), pack.object_count())?;
let reverse_bytes = std::fs::read(pack.path().with_extension("rev"))?;
let reverse = ReverseIndex::parse(&reverse_bytes, pack.checksum(), pack.object_count())?;
let ids: Vec<_> = pack.ids().collect();

for commit_position in bitmap.entry_positions() {
    let reachable = bitmap.reachable(commit_position)?.ok_or("missing entry")?;
    println!("{} reaches {} objects", ids[commit_position as usize], reachable.count());
    for position in reachable.positions() {
        let index_position = reverse.index_position(position).ok_or("missing position")?;
        println!("{}", ids[index_position as usize]);
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

A pack bitmap uses pack-offset order for its bits. Its selected-commit positions and optional
name-hash cache use ordinary index order, which sorts object IDs. A MIDX bitmap instead uses
pseudo-pack order: the preferred pack first, then the remaining packs and their offsets. Bind that
ordering through the matching MIDX reverse index before translating bits to object IDs. A
checksum-valid reverse permutation alone does not establish the expected physical ordering.

The parser validates the embedded inventory checksum, file checksum, type partition, EWAH framing,
XOR references, and optional lookup table. It supports the optional name-hash cache; those hashes
are pathname heuristics and do not establish object identity. Unknown flags and pseudo-merge
extensions return `Error::Unsupported`.

Use `parse_with` and `reachable_with` to choose input, object, selected-commit, and expanded-word
limits. Decoding charges repeated XOR dependencies to the same work budget and accepts cooperative
cancellation. Treat missing, malformed, unsupported, or stale bitmap metadata as an unavailable
optimization and walk the graph instead. Parsing verifies derived metadata, not graph reachability;
a caller who does not trust the producer must independently walk and compare the claimed sets.

Remaining bitmap acceleration and pseudo-merge work are tracked in
[the bitmap issue](https://github.com/joshka/girt/issues/518).

## Query a want/have difference

Use `Objects::reachable_difference` to select every object reachable from wants that is not
reachable from haves. Resolve refs and authorize wants before calling it. Filter unrecognized have
IDs first: a missing root or graph object is an error, rather than evidence that the client has
nothing.

```rust,no_run
use girt::{ObjectId, Objects};

fn response(objects: &Objects, wants: &[ObjectId], haves: &[ObjectId])
    -> Result<(), Box<dyn std::error::Error>>
{
    let needed = objects.reachable_difference(wants, haves)?;
    println!("{} objects needed", needed.len());
    for id in needed.ids() {
        let object = needed.objects().read(id)?.ok_or("object removed")?;
        println!("{} {}", id, object.kind().as_str());
    }
    Ok(())
}
```

The result promises IDs in ascending object-ID order, total and per-type counts, membership checks,
and the snapshot used to produce the set. It retains packed file handles for response construction;
loose objects remain live reads and still require the caller's ordinary exclusion of concurrent
pruning.

When one bitmap inventory covers all tips, the query unions each side and subtracts the have set
using bitwise operations before translating the result into IDs. Other queries combine covered
commit bitmaps with a graph walk. Commit, tag, tree and blob roots are accepted; tree gitlinks are
not followed. The include and exclude closures are independent, so a shared object does not prune an
ancestor's include walk. Shallow repositories use only walking to preserve their boundaries.

`reachable_difference_with` accepts `QueryLimits` and cancellation. Object, edge and walk-byte
limits apply across both closures. Metadata input and inventory counts are aggregate bounds;
expanded-word work covers parsing, XOR decoding and set operations. Type parsing reserves its
worst-case expansion, so a tight metadata budget may choose walking earlier than strictly necessary.
Missing or unusable sidecars, metadata-budget exhaustion and uncovered tips all preserve ordinary
walking. Queries load optional primary-store pack bitmaps and monolithic MIDX bitmaps; incremental
MIDX chains and alternate-store bitmaps currently use walking. `core.multiPackIndex=false` also
skips MIDX bitmaps. `objects.clone().without_bitmaps()` forces walking, including after refresh.

Bitmap checks validate the associated indexes and reverse ordering. They do not independently
validate the producer's reachability or type claims against object payloads. Use the forced walk
when comparing derived metadata with the graph or when the metadata producer is untrusted.

Repeated queries currently reload optional sidecars and revalidate MIDX entries. Pack-index checksum
verification is cached by the pinned pack reader. Reusing complete validated bitmap bindings is
tracked in [the snapshot-cache follow-up](https://github.com/joshka/girt/issues/717); this increment
makes no serving-throughput claim.

## Encode bitmap artifacts

`Objects::encode_pack_bitmap` writes caller-owned bitmap and reverse-index bytes for a primary pack.
`encode_multi_pack_bitmap` also writes the associated MIDX, including RIDX and BTMP ordering chunks.
Neither method changes storage. Keep maintenance exclusion through encoding and installation so
another writer cannot retire the named packs.

```rust,no_run
use girt::Repository;
use girt::bitmap::{WriteLimits, WriteOptions};
use std::sync::atomic::AtomicBool;

let repository = Repository::open("repo.git")?;
let objects = repository.objects()?;
let pack = objects.packs().next().ok_or("no pack")?;
let written = objects.encode_pack_bitmap(
    pack.checksum(), WriteOptions::default(), WriteLimits::default(), &AtomicBool::new(false),
)?;
println!("{} selected commits, {} bitmap bytes", written.commits, written.bitmap.len());
// Install under maintenance exclusion; keep the complete bitmap private until its reverse
// ordering is available. Encoding by itself does not publish these bytes.
# Ok::<(), Box<dyn std::error::Error>>(())
```

The writer verifies every inventory object's contents and graph edges, including dependencies of
unselected commits. An inventory that omits a reachable object is refused even if another pack or an
alternate contains it. Shallow repositories cannot establish full-DAG claims and are refused.
Existing bitmaps are never used to construct new claims. Objects are decoded in pack-offset order to
reuse delta bases; graph and selected-commit positions remain in sorted object-ID order.

Supply `WriteOptions::commits` for explicit selection. An empty list chooses graph tips first, then
samples remaining commits evenly in sorted ID order, up to `max_commits` (64 by default). Explicit
selections are deduplicated and must fit that cap. This policy is deterministic; it does not copy
Git's commit-selection heuristic.

Output uses EWAH runs and literals, independent commit records and a lookup table. XOR search is not
performed. MIDX output includes zero pathname-hash hints for unknown names; those hints affect delta
heuristics only. Both artifact types carry the inventory checksum and their own checksums.
`WriteLimits` separately bounds input objects/packs, decoded graph bytes/edges, repeated selected
reachability work, per-object reads and combined output bytes. No partial artifacts are returned.

## Publish bitmaps and configure repacks

`Repository::write_pack_bitmap` publishes sidecars for an existing primary pack.
`write_multi_pack_bitmap` publishes bitmap and reverse-index sidecars before replacing the
monolithic MIDX. Both require caller-owned `MaintenanceIsolation`; their `_with` variants accept
writer bounds and cancellation. All private files are synchronized first. Replacement is atomic per
file, with Unix directory synchronization afterwards. A failed pre-MIDX publication can leave unused
checksum-named sidecars; the old MIDX remains usable. `InstallError::published` distinguishes
failures after the bitmap (pack) or MIDX (multi-pack) visibility marker was replaced.

The CLI composes the same operation:

```sh
girt -C repo.git multi-pack-index write --bitmap
girt -C repo.git multi-pack-index verify
```

Repacking uses the last resolved assignment of `repack.writeBitmaps` or its deprecated synonym
`pack.writeBitmaps`. Unset means disabled for both bare and nonbare repositories. This deliberately
and temporarily differs from Git's bare default: completed bitmap writing measured 5.82 times a
plain repack on a git.git-sized fixture under load. Restore Git's bare default once the writer is
within about 1.5 times a plain repack. Set either config key to true, or set
`RepackLimits::write_bitmaps = Some(true)`, to opt in. Explicit caller settings override
configuration; `RepackLimits::bitmap` controls the writer's independent bounds.

`RepackPublished::bitmap` reports written, disabled, shallow, protected-survivor or limit-skip
outcomes. Configured/default bitmap resource exhaustion reports `SkippedLimit` and allows
maintenance to continue. `write_bitmaps = Some(true)` and standalone bitmap publication remain
strict, returning their typed limit errors. A shallow graph or a replacement that omits protected
copies skips bitmap writing. A no-op gc that does not publish a replacement does not retrofit a
bitmap. No automatic MIDX writing is added.

Pack sidecars follow complete pair publication. A bitmap error leaves that pair visible and returns
`RepackError::Bitmap` with its published report and the concrete cause. Retirement retains that
report and stops before deleting old packs. Its generation checks allow only the exact replacement's
own sidecars, and continue rejecting independent directory changes. Retained old packs and existing
MIDX artifacts are still invalidated by the established retirement rules.
