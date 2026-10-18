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

## Attributes and Drivers

Look up the `merge` attribute for the path through `attributes::Attributes` or a repository's
`attributes::Lookup`, then pass that state to `merge::Driver::from_attribute`. A set attribute
selects text, an unset attribute selects binary, and a string names text, binary, union or a custom
driver. Unspecified attributes select automatic detection. `Driver::configured` also applies
`merge.default` from an explicit configuration snapshot when the attribute is unspecified.

`Driver::merge` runs built-in content computation. Text and union still detect binary input; union
does not turn binary conflicts into text. A custom driver returns `CustomRequired` with its name.
Match `Driver::Custom` to invoke your own callback or command, using caller-owned process, resource
and trust policy. Return `ContentMerge::resolved` for accepted output or `ContentMerge::conflicted`
to preserve original whole-input sides. No configured shell command is run implicitly.

`MarkerOptions::from_config` interprets `merge.conflictStyle` from the supplied snapshot. Values are
case-sensitive: merge, diff3 and zdiff3 are accepted; implicit, empty and unknown values return a
typed error. Missing configuration selects merge. Attribute/configuration lookup and this driver
selection are explicit composition; path-based tree merging does not yet route attributes itself.

## Renamed Paths

`Objects::merge_trees` infers base-to-side renames, aligns the paths and invokes the same content
and mode computation. `merge_trees_with` selects similarity/candidate policy and inference limits. A
rename on one side carries the other side's source modification to its destination. A source renamed
to two destinations or renamed on one side and deleted on the other remains a structured conflict.
`TreeConflict::rename_source` retains the original path; `stage_paths` records distinct
base/ours/theirs index paths when necessary.

A removed directory with one inferred destination can relocate an opposite-side addition. The result
reports a directory-rename conflict at the new path, leaving the caller to confirm that placement.
Existing content and its original side remain available. Git's conservative default also reports
this case as a conflict.

The operation refuses destination collisions, directory moves with several destinations, incomplete
candidate search and stored empty subdirectories. It never substitutes a plain addition/deletion
approximation for those layouts. Normalized trees remain in memory with the other generated objects
and must be installed before publishing the candidate root. The path-based `merge_tree_entries`
primitive remains available to consumers that already own path alignment and want to preserve stored
empty subdirectories.

## Commit Tips and Virtual Bases

`Objects::merge_commits` finds every best common ancestor before merging the two tip trees. Multiple
bases are recursively merged in object-ID order through the same in-memory tree engine. Generated
base blobs are available to later content comparison and rename scoring without being installed. The
result identifies the original parents, best common ancestors and selected or synthesized base tree.

An unresolved virtual base returns `CommitError::ConflictedBase` with the structured intermediate
result. It does not select one provisional side as the ancestor. Final conflicts, by contrast,
remain an ordinary `CommitMerge::result` for the caller to resolve. Unrelated histories are rejected
by default; `CommitOptions::allow_unrelated_histories` explicitly chooses an empty base. Composition
has depth, base-set, virtual-merge and ancestry-query limits in addition to each operation's history
and tree bounds.

To create a merge commit, require a clean result, install all generated blobs and trees, then use
`Commit::builder(result.result.tree.id(), author, message).parents(result.parents)`. Set an explicit
committer on the builder when it differs from the author. `LooseObjects::write_commit` stores the
new object; updating a reference is a separate caller-owned transaction. The
[`Objects::merge_commits` example](https://github.com/joshka/girt/issues/545) describes this
boundary; the operation's Rustdoc contains a runnable object-installation and ordered-parent
example.
