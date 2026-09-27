# Retaining Complete Fetch Packs

`ReceivedFetch::install_retained` installs a complete, nonempty transfer under an exclusively owned
Git `.keep` marker. The marker is created before either pack artifact is published. Keep its
`FetchRetention` handle through reference publication, then call `release`. Source-only users must
establish persistent roots before releasing retention. Dropping the handle leaves the marker on
disk, including when publication fails; inspect effects before releasing it. Abandoned markers
retain disk space until their owner removes them.

The API returns the installation result and retention handle together. Installation errors expose
both the original cause and any acquired retention. Errors before marker acquisition have no handle.
Existing markers are never adopted or removed. Existing pack/index artifacts are refused because a
collector may already have selected them for deletion before the new marker was created. Retrying
such a transfer requires a separate existing-pack retention strategy; this API does not establish
one.

Use a full local receive with empty known history. Incremental transfers with dependencies outside
the pack, empty transfers, and shallow installation are rejected. The pack protects the complete
received history, including trees and blobs, without depending on source objects after validation.
The usual trusted-path, object-format, cancellation and non-durable-publication contracts still
apply. Removal of an active marker or deletion by a tool that ignores `.keep` is outside the
contract.

## Relationship to Ordinary Git Concurrency

Git documents creating `.keep` before index publication to protect incoming objects until refs are
updated in [git-index-pack](https://git-scm.com/docs/git-index-pack). This protocol permits
independent Git repacking; requiring exclusion of every GC process is stronger than that protocol.
It does not protect omitted incremental dependencies or a pack already selected for deletion. Git
also warns that [immediate pruning](https://git-scm.com/docs/git-gc) can race concurrent writers.
This API makes no general guarantee for loose-object writers or arbitrary destructive maintenance.

Local fetch reads source objects and can fail before destination publication if concurrent source
changes make selected history unavailable. Once received, a complete pack owns the necessary bytes.
Destination reference updates still need their exact expected values. Ordinary checkout updates to
`refs/heads/*` do not overlap fetch destinations. The dangerous change is a HEAD chain newly
targeting a fetched remote-tracking or tag reference; excluding every checkout or registration
change is stronger than preventing that case. The existing `FetchRequest` workflow still requires
HEAD/worktree exclusion to maintain its supported-HEAD invariant; retained installation does not
weaken or replace that contract.

Local push must additionally enforce receiver branch-in-use and receive policies. Its receiver-known
history can omit objects from the outgoing pack. A keep marker for that pack would not protect the
omitted history or make concurrent HEAD/worktree changes safe. Push behavior is unchanged.

## Acceptance Boundary

This is a prerequisite for ordinary native fetch integration. The existing `install` and workflow
`finish` calls retain their existing contracts. The jj native-transfer gate remains closed. Before
using retained installation in that consumer, resolve existing-pack reuse, incremental history,
HEAD/worktree policy, and retention lifetime through jj's own reference publication and recovery.

Original tests cover exclusive marker ownership, concurrent installers, cancellation after marker
acquisition, partial publication, foreign artifacts, replacement-marker preservation, drop recovery,
and incremental refusal. Git CLI fixtures exercise SHA-1 and SHA-256 full commit/tree/blob
histories, collection between installation and reference publication, subsequent release and
collection, and reference-lock failures. Fixture content is authored here and generated with Git
commands; upstream implementation and test sources are not inputs. Native execution on other
platforms remains required before claiming their concurrency behavior.
