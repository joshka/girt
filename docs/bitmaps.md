# Read reachability bitmap files

A reachability bitmap records a selected commit's reachable objects as positions in a fixed
pack or multi-pack index (MIDX). Use `girt::bitmap::BitmapIndex` to inspect these files without
walking object contents. Use `ReverseIndex` to read a `.rev` permutation. Both parsers support
SHA-1 and SHA-256 inventories.

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
pseudo-pack order: the preferred pack first, then the remaining packs and their offsets. Bind
that ordering through the matching MIDX reverse index before translating bits to object IDs.
A checksum-valid reverse permutation alone does not establish the expected physical ordering.

The parser validates the embedded inventory checksum, file checksum, type partition, EWAH framing,
XOR references, and optional lookup table. It supports the optional name-hash cache; those hashes
are pathname heuristics and do not establish object identity. Unknown flags and pseudo-merge
extensions return `Error::Unsupported`.

Use `parse_with` and `reachable_with` to choose input, object, selected-commit, and expanded-word
limits. Decoding charges repeated XOR dependencies to the same work budget and accepts cooperative
cancellation. Treat missing, malformed, unsupported, or stale bitmap metadata as an unavailable
optimization and walk the graph instead. Parsing verifies derived metadata, not graph reachability;
a caller who does not trust the producer must independently walk and compare the claimed sets.

The repository reachability query API and bitmap writing are tracked in
[the bitmap issue](https://github.com/joshka/girt/issues/518).
