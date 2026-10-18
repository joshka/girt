# Apply text patches in memory

Use `girt::apply` when an application owns file contents and needs to apply a unified diff without
opening a working tree. Parse the complete patch, select a file edit, and pass its preimage bytes:

```rust
use girt::apply::Patch;

let patch = Patch::parse(b"--- a/name\n+++ b/name\n@@ -1 +1 @@\n-old\n+new\n")?;
let applied = patch.files()[0].apply(b"old\n")?;
assert_eq!(applied.content, b"new\n");
# Ok::<(), girt::apply::Error>(())
```

`Applied` owns the complete postimage and reports each applied hunk's patch line and zero-based
preimage/postimage positions. A failure returns no partial content. The caller decides whether and
how to publish the bytes; this API reads and writes no objects, index entries or working files.

Paths remain bytes, including the original `a/` and `b/` prefixes. Quoted Git paths are decoded and
traditional tab-separated timestamps are removed. `/dev/null` becomes an absent side. Callers must
check source existence, destination absence and destination-specific path safety themselves. A
successful content application does not authorize opening a path from an untrusted patch.

`Patch::parse_with` and `FilePatch::apply_with` accept `Limits` to bound input bytes, file count,
hunk/content-line counts and postimage bytes. The default byte bounds are 16 MiB, with at most
100,000 content-line allocations. Parsing and exact-position matching take work proportional to
input and output size; there is no offset search. File edits remain in input order and duplicate
paths are retained for the caller to interpret.

## Supported scope

The initial API applies unified text replacements, insertions and deletions at their declared
positions. It preserves CR, non-UTF-8 bytes and missing final newlines. Context and removed lines
must match exactly, and the declared postimage positions must agree with preceding hunks.
Zero-context hunks are accepted, analogous to opting into Git's `--unidiff-zero`.

Git `diff --git` and `index` lines are accepted as framing; object IDs are syntax-checked but are
not preimage identity assertions. File paths come from `---` and `+++`. Binary patches, combined
diffs, mode changes, rename/copy metadata and submodule modes return explicit unsupported errors.
Offset/fuzz matching, whitespace options, reverse application, recount, tree/index/worktree
publication, format-patch and mailbox/am remain tracked in
[issue 506](https://github.com/joshka/girt/issues/506).

The parser requires a standalone patch: mail headers, prose and signatures must be removed first.
Empty context lines require their leading space, and missing-newline markers require the English
`\ No newline at end of file` spelling. Reduced trailing context does not pin an exact-position hunk
to EOF as Git's default matching does; a missing-final-newline marker always requires EOF.
