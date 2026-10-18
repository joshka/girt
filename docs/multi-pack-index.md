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
operations retain their ordinary pack searches. MIDX and bitmap writing are not implemented yet.

Unreleased API progress is tracked in [the MIDX issue](https://github.com/joshka/girt/issues/517).
