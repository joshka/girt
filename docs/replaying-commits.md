# Replay a commit without a working tree

Use `Objects::cherry_pick` to apply one commit's change onto an explicit destination. Use
`Objects::revert` to apply its inverse. Both work with bare repositories and return a `Replay`
containing the existing `TreeMerge`, a status and an optional unsigned commit. The
[`replay` module example](../src/replay.rs) shows creation and installation in a disposable
repository.

Supply the new committer explicitly. Cherry-pick retains the source author, including the author
date. Revert uses the supplied identity for both author and committer. Neither copies signatures or
opaque source headers. Enable `Options::record_origin` for the full source identity in a cherry-pick
provenance paragraph. Revert creates the conventional title and explanation automatically.

For merge sources, set `Options::mainline` to a one-based parent number. Root commits compare
against an empty tree. Originally empty sources and sources made redundant by the destination have
separate policies: stop returns a decision point, keep proposes an empty commit, and skip proposes
none. Keeping originally empty sources corresponds to `--allow-empty`; keeping both empty classes
corresponds to `--keep-redundant-commits`.

Inspect `Replay::status` before publication. Conflicts retain original base/ours/theirs entries,
rename paths and optional content hunks. A conflicted candidate is provisional and has no proposed
commit. jj and rerere consumers can retain these structured sides directly. For a working-tree stop,
use the [merge publication APIs](merging.md) to prepare and publish original index stages and
markers.

For a ready or kept-empty result, install `result.blobs`, then `result.trees` in returned order,
then the proposed commit through `LooseObjects`. Only then publish the commit identity through a
separate reference transaction. Computation never updates HEAD, writes sequencer state or touches
working files. Object installation alone also moves no references.

The [compatibility contract](compatibility.md#commit-replay) records Git-oracle coverage and
inherited merge limitations. The unreleased API and remaining rebase, sequencer and stash work are
tracked in
[the replay design](https://github.com/joshka/girt/issues/608).

## Rebase an explicit sequence

`Objects::rebase` takes an ordered slice of source identities and an explicit destination. Select
the range before calling; it does not discover commits, filter patch IDs or reconstruct merge
topology. Each source preserves its author and receives the supplied committer. Merge sources use
the replay mainline policy. `RebaseOptions::max_commits` bounds the sequence before any commit
reads.

The result contains completed steps, the last completed tip and an optional stopped replay. A
conflict or stop-on-empty leaves that source unresolved; skip records a mapping to the unchanged tip
and continues. Generated trees and blobs remain available to later steps in memory. Errors and
cancellation discard the in-memory result without repository changes.

Install each completed step's generated blobs, trees and optional commit in sequence order before
publishing the resulting tip. A stopped replay owns additional provisional objects and original
conflict sides for a caller-owned resolution workflow. It does not move HEAD or write Git's
sequencer state. The caller chooses whether to retain completed objects after a stop.
