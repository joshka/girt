# Merge content and tree entries

## Byte Content

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

Content computation does not read attributes or configuration or publish index entries, working
files or commits. Repeated-line placement and internal common-line conflict refinement can differ
from Git. Check the [compatibility evidence](compatibility.md#content-merging) before depending on
exact marker placement. The [API reference](https://docs.rs/girt/latest/girt/merge/) defines the
byte and failure contracts.

## Tree Entries

`Objects::merge_tree_entries` combines three tree snapshots by matching byte paths. It needs only an
object reader. The result owns a candidate root tree, generated blob payloads and subtree objects,
and structured conflicts. Computation writes no objects. Install the generated blobs with
`LooseObjects::write_blob` and the generated trees in their returned child-before-parent order with
`LooseObjects::write_tree` before using the candidate root identity elsewhere.

Check `TreeMerge::is_clean` before treating the candidate as resolved. At unresolved paths it keeps
our original entry, or their entry when ours was deleted. `TreeConflict` retains all original
entries and optional content hunks, so callers can choose another representation. Its
`index_entries` method creates stages 1–3 for leaf conflicts without publishing an index. It refuses
directory conflicts: those require explicit expansion and path selection first.

Directories merge recursively. Regular-file payloads and executable modes merge independently;
add/add files compare against empty content. Modify/delete, incompatible types, different symbolic
link targets and different submodule commits remain conflicts. Submodule repositories are never
opened. Default depth and entry bounds limit traversal; `merge_tree_entries_with` also accepts
content bounds and cancellation. Unchanged subtree identities are reused without validating their
descendants.

This is a path-based primitive. It does not infer renames, build virtual merge bases, select merge
drivers or create commits. Callers requiring Git rename behavior must reconcile renamed paths before
using it. The later commit/sequencer layer can reuse the same structured results while owning
history and publication policy.
