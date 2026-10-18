# Attach metadata without rewriting commits

A note associates bytes with an object ID. Notes live in a separate commit history, commonly
`refs/notes/commits`, so adding a review link or CI result leaves the annotated commit unchanged.

Use `notes::Selection::from_config` with explicit configuration and environment snapshots to select
the primary ref. `GIT_NOTES_REF` overrides `core.notesRef`; the default is `refs/notes/commits`.
Primary names must be full names under `refs/notes/` for writes. Additional `notes.displayRef`
patterns are retained in configuration order for caller-controlled expansion against a reference
snapshot. This increment does not expand display globs or implement `GIT_NOTES_DISPLAY_REF`.

Resolve the selected ref to a commit ID, then call `Notes::read(&objects, tip)`. For an absent ref,
start with `Notes::new(repository.object_format())`. `get` reads one note; `iter` lists object IDs
and bytes in sorted order. Import reads arbitrary hexadecimal fanout depth, including Git's
`ab/cdef…` and `ab/cd/ef…` layouts, with bounded tree entries and total note bytes. Non-note tree
entries and ambiguous duplicate mappings return errors instead of being discarded by later edits.

Compose changes with `add`, `set`, `append`, `copy` and `remove`. `add` and `copy` refuse an
occupied destination; `set` explicitly replaces it. These methods accept literal bytes. Unlike Git's
`-m` option, they do not clean messages or add a final LF. For a conventional text note, supply its
final newline. `append` inserts a blank line between nonempty old and new content.

```rust
use girt::{ObjectFormat, ObjectId};
use girt::notes::Notes;

let format = ObjectFormat::Sha1;
let object = ObjectId::for_blob(format, b"reviewed content");
let mut notes = Notes::new(format);
notes.add(object, b"Review: https://example.com/review/42\n")?;
assert!(notes.get(object).is_some());
# Ok::<(), girt::notes::Error>(())
```

Call `prepare` with the previous tip, author, committer and operation to compute the next tree and
commit. The operation selects Git's notes commit message. The result writes nothing. `store`
installs the computed blobs, tree and commit, then returns the proposed ref target; it never
publishes a reference. Publish through a conditional reference transaction with the original tip as
the expected value. See [reference transactions](reference-transactions.md). If the tip changed,
reload and recompute rather than overwriting another writer's notes.

Fresh and imported lowercase flat collections produce flat trees. Uppercase imported paths also
refuse preparation so edits cannot silently rename them. Preparing an imported fanout collection
returns an explicit layout error before writing anything. A layout-preserving writer and Git's
adaptive fanout creation remain follow-ups in
[issue 508](https://github.com/joshka/girt/issues/508).

## Rewrites and merges

Git uses `notes.rewriteRef` and `notes.rewrite.<command>` to decide which notes to copy after
commands such as amend and rebase; `notes.rewriteMode` selects how existing notes combine. Girt's
replay and amend operations compute commits and never auto-publish notes. These settings do not
activate implicit propagation. Callers retaining an old-to-new commit mapping can explicitly copy
notes, prepare a notes commit and publish its ref separately after their own rewrite succeeds. A
failed rewrite therefore cannot accidentally publish copied notes through these APIs.

Notes merge strategies `manual`, `ours`, `theirs`, `union` and `cat_sort_uniq` are not implemented
in this increment. Callers must resolve concurrent notes updates before publication; the ordinary
content/tree merge module does not by itself implement notes' object-key merge semantics.
