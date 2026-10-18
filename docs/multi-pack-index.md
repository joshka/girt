# Inspect a multi-pack index

Use a multi-pack index when a repository contains many packs. Its sorted object table chooses a pack
without searching every pack index. Ordinary pack indexes remain required for object entry bounds
and decoding.

`Repository::objects()` automatically uses a usable monolithic MIDX or incremental chain. Set
`core.multiPackIndex=false` to disable loading and lookup. An invalid boolean also disables the
hint. Missing or damaged auxiliary files leave ordinary object reads available. Reopen or refresh
the object store after new packs are published.

For explicit inspection, read the file with a caller-selected input bound, then pass its bytes to
`girt::multi_pack_index::MultiPackIndex::parse_with`. Parsing checks the checksum and structure;
`verify` checks the named packs and every claimed ID and offset against an opened `Objects`
snapshot. Use `Objects::verify_packs` separately when whole-pack checksums and entry CRCs are
required. Verification never writes or repairs files.

The parser accepts versions 1 and 2 in both object formats. Automatic chain discovery is bounded to
256 layers and the remaining aggregate index-byte budget. Nonzero encoded base counts disable
automatic use. Lookup currently accelerates verified logical reads; metadata and maintenance
operations retain their ordinary pack searches. Bitmap writing is not implemented yet.

Unreleased API progress is tracked in [the MIDX issue](https://github.com/joshka/girt/issues/517).

## Write an index

Use `girt multi-pack-index write` to create or replace the primary store's standalone index, then
`girt multi-pack-index verify` to check it. Exclude concurrent pack writers, gc and repacking while
writing. `--preferred-pack <checksum>` chooses the preferred copy of duplicate objects. The default
chooses the most recently modified pack, with filename order breaking ties.

```console
girt -C repo.git multi-pack-index write
girt -C repo.git multi-pack-index verify
```

For library use, `Objects::encode_multi_pack_index` returns bounded caller-owned bytes and never
changes storage. A store with no packed objects is refused, as Git cannot verify an empty MIDX.
Source indexes must pass their checksum and structural checks. It excludes alternate stores and
requires ordinary `pack-*.pack` names. `WriteLimits` charges every input object occurrence,
including duplicates, before sorting; output contains each object ID once.

`Repository::write_multi_pack_index` installs a complete artifact under caller-provided
`MaintenanceIsolation` and the MIDX writer lock. It synchronizes a private file before replacing the
standalone index and checks pinned source generations again before publication. Existing incremental
layers and checksum-named bitmap files remain available; the new standalone file shadows the chain.
A bitmap whose checksum differs cannot attach to the new index.

If installation fails, inspect `InstallError::published`. A pre-publication failure preserves the
previous index. A failure after replacement can leave the new index visible; inspect current storage
before retrying. Neither encoding nor publication deletes objects or packs. Girt's pack retirement
removes derived MIDX files and their bitmap/chain artifacts before deleting named packs.

`girt multi-pack-index write --bitmap` publishes a bitmap and reverse index with the new MIDX. It
verifies full graph closure and selects a bounded set of commits; see [bitmap writing](bitmaps.md).
Automatic MIDX writing by gc or maintenance is separate from these explicit operations.

## Measure index searches

`scripts/create-midx-lookup-fixture.py <new-directory>` creates an independent 100-pack repository
and separate `present-ids` and `absent-ids` lists. Build `midx_lookup_timing` in release mode before
requesting a quiet timing window, then run alternating `index-scan` and `midx` blocks under the
repository timing lock. Use `contains` to measure the current public existence API separately.

```console
cargo build --release --example midx_lookup_timing
target/release/examples/midx_lookup_timing fixture.git fixture.git/present-ids 100 index-scan
target/release/examples/midx_lookup_timing fixture.git fixture.git/present-ids 100 midx
target/release/examples/midx_lookup_timing fixture.git fixture.git/absent-ids 100 index-scan
target/release/examples/midx_lookup_timing fixture.git fixture.git/absent-ids 100 midx
```

Direct MIDX search checks only that index. It excludes candidate verification, loose-object checks
and packs outside the MIDX, so it measures index-search cost rather than the complete object-store
contract. Record load and compare alternating ratios when the machine cannot be fully idle.
