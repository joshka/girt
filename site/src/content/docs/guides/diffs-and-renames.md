---
title: Diffs and renames
description: Compare snapshots, produce byte-preserving line edits, and infer renames within budgets.
---

Git trees store snapshots, so a changed path, a line edit and a rename each need a different
comparison. Use structural comparison for a file list, load both blob sides for a patch view, and
ask for rewrite inference when you need to relate different paths. None of these operations changes
the repository, index or working files.

## Choose the comparison layer

`Objects::compare_trees_with(old, new, limits, cancel)` returns leaf additions, removals and changes
in raw full-path byte order. `None` represents an empty side. A `TreeChange` has a byte path and
optional old and new `TreeValue`s, each holding an identity and mode. Two present sides can describe
a mode-only change. File/directory replacements produce a leaf change and changes beneath the
directory; empty directories produce no leaf records. Gitlinks remain leaves rather than local
subtrees.

Equal subtree identities are skipped without reading them. This makes small changes in large trees
cheaper, but equality does not prove existence or validity. Visited trees are verified, parsed and
validated; leaf targets are not read. Both SHA-1 and SHA-256 repositories are supported.

For each structural change, `content_diff::BlobContent::read` loads and verifies both blob sides.
Missing sides become empty buffers, so keep the original `TreeChange` too: content alone can't show
an added empty file, whose content is unchanged, or a mode-only change. A symlink's content is its
stored target bytes; girt doesn't dereference any filesystem path. Trees and gitlinks are refused as
blob inputs.

The
[tree comparison API](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.compare_trees)
and
[content loading API](https://docs.rs/girt/latest/girt/content_diff/struct.BlobContent.html#method.read)
document each contract in detail.

This program compares two trees, loads both sides of the added file and passes the verified payloads
to content diff:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;

    use girt::content_diff::{BinaryMode, BlobContent, ContentDiff, DiffLimits, diff};
    use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};

    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r"),
        InitKind::Bare,
    )?;

    let loose = repo.loose_objects();
    let id = loose.write_blob(b"added\n")?;
    let tree = loose.write_tree(&Tree::new(
        ObjectFormat::Sha1,
        vec![TreeEntry {
            name: b"file".to_vec(),
            mode: EntryMode::Blob,
            id,
        }],
    )?)?;

    let objects = repo.objects()?;
    let cancel = AtomicBool::new(false);
    let changes = objects.compare_trees(None, Some(tree))?;

    let content = BlobContent::read(&objects, &changes[0], Default::default(), 1024, &cancel)?;
    assert!(content.old.is_empty());
    assert_eq!(content.new, b"added\n");
    assert!(changes[0].old.is_none()); // Keep existence independently from the empty content buffer.

    let ContentDiff::Text(text) = diff(
        &content.old,
        &content.new,
        BinaryMode::Auto,
        DiffLimits::default(),
        &cancel,
    )?
    else {
        panic!("expected an insertion");
    };
    assert!(text.edits()[0].old_bytes.is_empty());
    assert_eq!(text.new_bytes(), b"added\n");

    Ok(())
}
```

## Compare bytes and render line edits

`content_diff::diff` borrows two payloads and returns `Unchanged`, `Binary` or `Text`. Automatic
binary classification looks for NUL anywhere in either changed payload. Explicit text mode accepts
every byte, including NUL and invalid UTF-8. Explicit binary mode still returns `Unchanged` for
identical payloads. girt doesn't produce binary patches.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;

    use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};

    let result = diff(
        b"one\ntwo\n",
        b"one\nthree",
        BinaryMode::Auto,
        DiffLimits::default(),
        &AtomicBool::new(false),
    )?;

    let ContentDiff::Text(text) = result else {
        panic!("expected text edits");
    };

    let edit = &text.edits()[0];
    assert_eq!(edit.old_lines, 1..2);
    assert_eq!(&text.old_bytes()[edit.old_bytes.clone()], b"two\n");
    assert_eq!(&text.new_bytes()[edit.new_bytes.clone()], b"three");

    Ok(())
}
```

Lines split only at LF. CRLF and a missing final LF remain literal bytes. Edits use zero-based,
half-open line and byte ranges in the original inputs. An empty old range is an insertion; an empty
new range is a deletion. Reconstruct by copying unchanged old gaps and the corresponding new ranges.
Do not adjust subsequent offsets as if they referred to progressively edited text. The
[TextDiff API](https://docs.rs/girt/latest/girt/content_diff/struct.TextDiff.html)
exposes the original buffers and edit ranges for reconstruction.

The result is a deterministic shortest line-edit sequence with no patch context or rendering.
Applying attributes, textconv, whitespace normalization and external diff commands is up to the
caller. Escape untrusted bytes when rendering paths or content. A display can decode as lossy UTF-8,
but if it also exports exact edits, keep the original byte ranges.

Input, line, trace and work limits bound the comparison. Unrelated text can require quadratic
search/trace growth; exceeding a limit returns an error rather than approximate edits. Even
identical or binary inputs must fit the input-byte budget. A UI can say which limit was hit and
offer to retry with a larger budget or to show a raw or binary view.

## Infer renames and copies

`Objects::detect_rewrites` compares two trees and infers relationships between deleted regular or
executable files and added regular or executable files. Git stores no rename records, so a returned
`Rewrite` only says the contents are similar. It doesn't prove the user renamed or copied the file,
and it isn't grounds for changing merge history. Symlinks, gitlinks and directories are never
rewrite results.

Default options require 50% similarity, disable copies and pair empty files as exact renames. Use
`.with_track_empty(false)` for Git's `--no-rename-empty` behaviour. `Copies::Modified` also
considers modified files' preimages and permits source reuse; unchanged files are not searched.
`ModifiedPostimage` scores a modified source's new content while reporting its old identity. Use it
only when your caller wants copies scored against the source's new content.

Exact identity matches run first. Edited matching compares repeated byte spans split at LF or after
64 significant bytes; the order of spans doesn't matter. Text scoring drops CR before LF, while
binary scoring keeps it.

Rewrite scoring and `approximate_binary` classify content as binary by a NUL in the first 8000
bytes, while content diff checks the entire payload. A file with a NUL at byte 9000 can therefore be
classified as binary by content diff and as text for rename scoring. A rounded score of 100 can
still mean reordered spans; set `Options::similarity` to 100 to require identical object identities.

Girt implements its own heuristic. Its scores and pairings differ from Git's in known cases, and it
doesn't reproduce every Git shortcut or tie-break.

An optional target-path list filters after inference. It does not reduce candidate work or change
pairing/classification, and an omitted candidate can still cause an error. Keep unmatched additions
and removals alongside inferred rewrites. If you only need a cheap list of changed paths, use
structural comparison rather than filtering a rewrite result.

### Interpret a postimage copy

Suppose `source` changes from blob A to blob B, and the new tree also adds `copy` containing B:

| Path   | Old tree | New tree |
| ------ | -------- | -------- |
| source | A        | B        |
| copy   | absent   | B        |

With `Copies::ModifiedPostimage`, `copy` is an exact copy scored against B. This pair is found at
any valid similarity threshold, including the identity-only threshold of 100. The returned
`source_id` is still A, because it identifies `source` in the old snapshot; `target_id` is B. When
presenting the result, record that it came from `ModifiedPostimage`, and read the new source value
separately if a renderer needs the bytes that were scored. The
[rewrite identity fields](https://docs.rs/girt/latest/girt/rewrites/struct.Rewrite.html#structfield.source_id)
describe the reported source and target identities.

## Handle the candidate limit explicitly

After exact matching, edited detection is skipped when remaining sources times remaining
destinations exceeds `candidate_limit` squared. The default is 1000. This bounds the size of the
approximate search; independent byte, pair, span and work budgets remain in force throughout exact
and edited matching.

With `similarity == 100`, edited detection never runs: the candidate limit is bypassed and no
inexact skip is reported. Independent resource limits can still fail.

Girt's value zero skips edited detection whenever a counted pair remains. Git's configuration value
zero means unlimited; translate it to `usize::MAX` when implementing a Git configuration adapter.
Binary candidates still count when approximate binary matching is disabled. Empty candidates count
when `track_empty` is enabled. Reused copy sources count after exact matching. Git's same-basename
pre-pairing shortcut is not implemented, so near-limit behavior can differ.

Pick the method by how you want a blocked search reported:

- `detect_rewrites` returns `Error::Candidates` with no partial result if the limit blocks
  inference.
- `detect_rewrites_or_exact`, added in 0.10.0, returns exact matches plus
  `Detection::inexact_skipped`. Show the skip, keep all other additions and removals, and offer a
  retry if useful.

`InexactSkipped::suggested_limit` returns the larger remaining count, a sufficient but not always
minimal candidate limit. It does not raise independent resource budgets. The
[exact-only fallback API](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.detect_rewrites_or_exact)
returns the skip alongside the exact matches.

Build one exact rename and one edited rename, accept the exact-only result at a zero candidate
limit, then retry with edited matching:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;

    use girt::rewrites::{Error, Limits, Options};
    use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};

    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r"),
        InitKind::Bare,
    )?;

    let loose = repo.loose_objects();
    let tree = |files: &[(&[u8], &[u8])]| -> Result<_, Box<dyn std::error::Error>> {
        let entries = files
            .iter()
            .map(|(name, bytes)| {
                Ok(TreeEntry {
                    name: name.to_vec(),
                    mode: EntryMode::Blob,
                    id: loose.write_blob(bytes)?,
                })
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        Ok(loose.write_tree(&Tree::new(ObjectFormat::Sha1, entries)?)?)
    };

    let old = tree(&[
        (b"exact-old", b"identical\n"),
        (b"edited-old", b"one\ntwo\nthree\nfour\n"),
    ])?;
    let new = tree(&[
        (b"exact-new", b"identical\n"),
        (b"edited-new", b"one\ntwo\nthree\nFOUR\n"),
    ])?;

    let objects = repo.objects()?;
    let options = Options::default().with_candidate_limit(0);
    let cancel = AtomicBool::new(false);

    let detection = objects.detect_rewrites_or_exact(
        Some(old),
        Some(new),
        options,
        Limits::default(),
        None,
        &cancel,
    )?;

    assert_eq!(detection.rewrites.len(), 1); // The identical pair survived.
    assert_eq!(detection.rewrites[0].source, b"exact-old");
    let skipped = detection.inexact_skipped.expect("one edited pair remains");
    assert_eq!((skipped.sources, skipped.targets, skipped.limit), (1, 1, 0));
    assert_eq!(skipped.suggested_limit(), 1);

    assert!(matches!(
        objects.detect_rewrites(
            Some(old),
            Some(new),
            options,
            Limits::default(),
            None,
            &cancel
        ),
        Err(Error::Candidates { .. })
    ));

    let retried = objects.detect_rewrites_or_exact(
        Some(old),
        Some(new),
        options.with_candidate_limit(skipped.suggested_limit()),
        Limits::default(),
        None,
        &cancel,
    )?;

    assert!(retried.inexact_skipped.is_none());
    assert_eq!(retried.rewrites.len(), 2);

    Ok(())
}
```

## Diagnose failures and schedule work

Structural errors preserve the visited tree ID and path. Content-loading errors preserve the blob
ID, mode and storage cause; keep the structural change to add old/new side and path context. Treat a
missing or corrupt object as something to recover, and never substitute an empty payload for the
failed read. Keep Rust error source chains for diagnostics.

All comparisons are synchronous. Cancellation is cooperative and cannot interrupt one syscall,
object decode, parse, allocation or sort. Run substantial comparisons on a bounded blocking worker
in an async application. Limits and cancellation return no partial edits or inference. Before
retrying, supply a fresh cancellation flag, a larger budget or a repaired object view, whichever
caused the failure.

## API reference

- [Tree changes and comparison](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.compare_trees)
- [Content diff, binary policy and errors](https://docs.rs/girt/latest/girt/content_diff/)
- [Rewrite options, limits and detection reports](https://docs.rs/girt/latest/girt/rewrites/)
