# Merge byte content

Use `girt::merge::content` when a caller already has a common ancestor and the two replacement
payloads. It needs no repository or working tree. A clean result contains accepted bytes; an
unresolved result retains each conflicting base, ours and theirs payload independently of marker
text. This lets a sequencer or a jj adapter keep conflicts in its own representation.

```rust
use girt::merge::{content, MarkerOptions};

let result = content(b"one\ntwo\nthree\n", b"ONE\ntwo\nthree\n", b"one\ntwo\nTHREE\n")?;
assert!(result.is_clean());
assert_eq!(result.render(&MarkerOptions::default()), b"ONE\ntwo\nTHREE\n");
# Ok::<(), girt::merge::Error>(())
```

Use `content_with` to choose binary detection, favoring, comparison bounds and cancellation. Bounds
apply independently to each base-to-side comparison. Favoring ours or theirs selects only
conflicting regions; independent edits from both sides still survive. Union keeps common boundary
lines once and puts our conflicting lines before theirs. It inserts an LF between unterminated
payloads when needed. Union uses Git's default merge-style boundary refinement independently of
marker presentation; it does not reproduce `git merge-file --diff3 --union` concatenation.

`ContentMerge::hunks` exposes exact unresolved region bytes. `render` chooses merge, diff3 or zdiff3
markers, labels and width through `MarkerOptions`. Diff3 includes the complete base region. Merge
and zdiff3 move common side boundary lines outside markers. Marker rendering adds an LF before a
marker following an unterminated payload; it never changes retained input bytes. Labels are emitted
verbatim and should be validated when they come from untrusted input.

Automatic binary detection recognizes NUL anywhere in the three inputs. An unresolved binary result
retains all three complete payloads and renders ours without markers. Choosing ours or theirs can
resolve it explicitly; union leaves it unresolved. Forced text permits NUL bytes in line comparison.

This increment does not merge trees, infer renames, read attributes or configuration, or publish
index entries, working files or commits. Repeated-line placement and internal common-line conflict
refinement can differ from Git. Check the [compatibility evidence](compatibility.md#content-merging)
before depending on exact marker placement. The
[API reference](https://docs.rs/girt/latest/girt/merge/) defines the byte and failure contracts.
