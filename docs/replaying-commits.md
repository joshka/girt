# Replay a commit without a working tree

Use `Objects::cherry_pick` to apply one commit's change onto an explicit destination. Use
`Objects::revert` to apply its inverse. Both work with bare repositories and return a `Replay`
containing the existing `TreeMerge`, a status and an optional unsigned commit. The
[`replay` module example](../src/replay.rs) shows creation and installation in a disposable
repository.

Supply the new committer explicitly. Cherry-pick retains the source author, including the author
date. Revert uses the supplied identity for both author and committer. Neither copies signatures or
opaque source headers. To choose a distinct revert author, change the proposed commit's
`to_fields()?.author` and rebuild with `Commit::new` before installation. Enable
`Options::record_origin` for the full source identity in a cherry-pick provenance paragraph. Revert
creates the conventional title and explanation automatically; reverting a revert uses a `Reapply`
title.

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

## Execute a linear todo

`replay::todo::Todo::parse` accepts Git's long and short interactive-rebase commands, including
message flags, labels, reset, merge and update-ref. It preserves byte subjects and external command
payloads, with byte/count bounds and original line locations. Parsing checks syntax; execution
resolves commit prefixes and checks operation context.

`Objects::execute_todo` computes pick, reword, edit, squash, fixup, drop, exec, break, label and
update-ref instructions. Supply `TodoHooks` for message editing and external commands. The library
never launches those commands itself. Hooks receive the proposed tip, which may exist only in
memory; any hook side effects remain the caller's responsibility if a later instruction fails.

Merge and reset instructions return typed unsupported-topology errors before any hook runs.
[Generated-history merge lookup](https://github.com/joshka/girt/issues/616) tracks that extension.
Edit and break return a stop position; conflicts and empty decisions retain the unresolved replay.
Callers own continuation context after stops.

`TodoRun::replays` retains the original single-step proposals and generated trees/blobs.
`TodoRun::commits` contains final metadata after reword or combining commits; install these in
creation order. Squash/fixup mappings point every combined source to the final amended identity.
Explicit update-ref instructions produce target intents without changing references.

Use `Objects::autosquash` to order `fixup!`, `squash!` and `amend!` subjects after earlier targets.
`TodoRun::plan_updates` maps an observed reference snapshot through completed rewrites, excluding
branches checked out in any working tree supplied by the caller. Validate old identities again in a
reference transaction before publication. Neither planning operation updates references.

## Recover a Git-started operation

Read `sequencer::State` from the repository's worktree-specific Git directory. It recognizes single
cherry-pick/revert pseudo-refs, multi-commit `sequencer` state and merge-backend `rebase-merge`
state. Author scripts remain opaque data; girt never evaluates them. Todo and file-size bounds apply
while reading. Unknown files, symlinks, apply-backend rebase and unsupported continuation metadata
are refused before publication.

Prepare `Continue(new_commit)`, `Skip` or `Abort` with the observed current HEAD. A recovery plan
names the required checkout and HEAD destination; it does not perform those mutations. For
continuation, create the resolved commit with the operation's chosen author, committer and message.
For skip/abort, explicitly discard the unresolved index/working-file changes through a caller-owned
checkout. Publish objects, perform the guarded checkout and expected-value HEAD transaction, then
call `publish_metadata`.

Metadata publication checks the destination HEAD and original state bytes, stages changed files
under create-new locks, then replaces/removes files individually. The caller must own the operation
exclusively throughout this sequence: Git does not honor a girt-wide sequencer lock. Publication is
not a cross-file crash transaction. Completed operation directories are renamed out of Git's active
namespace before their files are removed. A partial failure reports changed paths; re-read disk
state before choosing recovery. If retired-directory cleanup failed, no active operation may remain;
the reported path identifies inert metadata rather than an interrupted active sequencer. Original
`ORIG_HEAD` is retained, and unrelated/unknown state files cause refusal rather than deletion.

After rebase continuation metadata is published, Git can run the remaining todo and finish branch
reattachment. To finish in girt, use `Finish` only after the todo is exhausted and no source remains
unresolved. Publish the original branch conditionally using `expected_branch_tip`, reattach HEAD,
and publish completed `reference_updates` before removing metadata. Pending update-ref placeholders
prevent finishing. `reflog_for` adds the resolved commit subject to Git's operation prefix; finish
plans provide the conventional returning-to-branch message.

`Recovery::start_single` prepares a Git-readable cherry-pick/revert stop after explicit conflict
checkout. `Recovery::start_rebase` prepares a linear todo after the caller detaches and checks out
the destination, preserving the original branch until finish. Both require explicit metadata
publication; Git can then continue the operation. Squash/fixup persistent continuation,
apply-backend rebase and recreated merge topology remain unsupported.

For attribute-selected or custom content policy, use the single-commit and rebase `with_driver`
variants, or return a `TreeDriver` from `TodoHooks::content_driver`. The same operation retains
structured conflict sides independently of marker rendering. Attribute/configuration snapshots and
custom-process authorization belong to the caller, as in the merge module.
