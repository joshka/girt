# Deleting and Rewriting Stored Data

Apply these rules to any change that deletes, retires, replaces or rewrites objects, packs,
references or other stored data, including fast paths in maintenance. Losing the last readable copy
of an object cannot be undone, so when a rule cannot be met, refuse the operation.

[Specify Before Implementing](testing.md#specify-before-implementing) owns the process: the
failure-mode inventory, the failing tests and the design round. This page owns the rules and their
rationale; each inventory class links here.

## Last readable copy

Before deleting, decode each retained object from the copy that will remain, under the reader's real
`ReadLimits`. An index entry, pack checksum or CRC shows that a copy exists, not that it
reconstructs the right object.

## Bound combined data

Pieces that are valid in their own source can combine past a reader limit, such as delta chains from
different packs exceeding `max_delta_depth`. Re-encode in the plan; otherwise verification refuses
after the new data is published, and every retry fails the same way.

## Publish, verify, retire

Write and sync the replacement, verify it, and only then remove the old data. Every object must stay
readable if any step fails.

## One pinned snapshot

Scan, select and copy through the same open store. Reopening in between lets the plan describe data
the copy no longer sees.

## Immutable packs and buffered headers

Packs and indexes are immutable once published, as in Git's pack reader. Atomic rename or deletion
leaves an already-pinned handle reading the original artifact. On Windows, girt opens artifacts with
`FILE_SHARE_READ | FILE_SHARE_DELETE`, denying concurrent data writers while allowing Git to rename
and delete files.

Retention operations share one 256 KiB positioned-read header window across all packs on the calling
thread. Nested phases share that allocation; the outermost operation drops it. Pack size and pack
count do not increase window memory. Ordinary content reads outside retention operations have no
header window. Large read-ahead is restricted to offset-ordered blob locating, raw reuse and
survivor verification; scattered requests use small positioned reads. Entry prefixes and decoder
scratch retain their existing bounds.

Each pinned artifact records its handle's size, modification time and file identity (device/inode on
Unix, volume/file index on Windows) at opening. Before pack/index publication, retirement unlinks,
loose pruning and reflog publication, the operation checks every input pack and index through those
same handles. A changed artifact produces a structured error naming its original path before that
step mutates storage. A completed earlier step remains in the failure report. Retirement also
revalidates the replacement's pinned handles before unlinks and keeps the existing survivor
decoding, identity hashes, CRCs, zlib checks, checksum gates, limits and cancellation checks.

In-place modification during an operation can make girt serve stale header bytes. Boundary checks
detect changes to the recorded metadata before mutation; window hits do not call `fstat`. These
checks do not replace caller-owned exclusion: metadata observations cannot prevent a write after a
check, nor detect a same-size edit whose modification time is restored. Git makes the same immutable
pack assumption. Git can also freshen an existing pack's modification time when writing an object
already present there. This metadata-only change conservatively causes the same refusal even though
pack content is unchanged. Re-observe storage and retry under caller isolation, accounting for any
completed steps in the failure report rather than blindly replaying mutations.
[Git's mapped pack windows](https://git-scm.com/docs/git-config#Documentation/git-config.txt-corepackedGitWindowSize)
can crash with `SIGBUS` when truncation makes a mapped page inaccessible, as the
[`mmap` contract](https://man7.org/linux/man-pages/man2/mmap.2.html) describes. Girt uses owned
buffers and positioned reads, so this path does not acquire that mapping fault hazard.

## Protected data

Treat protected data as untouchable, not as proof. Never delete or rewrite `.keep`, recent or
alternate storage, and don't copy its objects into new output again: retirement keeps the original,
so the copy is permanent duplication. Omit an object because a protected copy holds it only after
that copy decodes to the right object.

## Lookup copy

When several packs or stores hold an object, copy the representation object lookup reads; another
copy may be corrupt where the scanned one was not.

## Damaged packed copies

Content lookup retries a damaged packed entry through the remaining packs and loose files in its
existing store order. After packed damage, a corrupt loose fallback also continues to the remaining
stores. Each fallback copy passes complete decoding and identity verification. The reader remembers
failed pinned pack/offset pairs, shared by clones and retained across refresh of the same immutable
pack generations. A rescued read proves only the returned copy. If no good copy exists, the error
names the damaged pack and its source chain includes the entry offset and original corruption cause.

Known input damage refuses retention planning and every existing pinned-input mutation boundary,
including identity failures that leave raw checksums intact. GC and maintenance preserve that pack
and its unique objects. Raw pack/index checksum gates, protected-copy checks, survivor decoding and
pinned-handle freshness checks remain required. Recovery never marks a damaged pack healthy.

Cancellation, limits, unsupported types and missing/external delta bases remain immediate errors;
header damage that presents as one of those errors does not trigger fallback. Header-only locating
keeps its existing policy, and raw reuse refuses a known damaged pack instead of rescuing its bytes.
Retained evidence uses pinned native identity, metadata and the pack checksum. Metadata-only
freshening of a still-pinned pack preserves its damage; weak handle records prevent inode reuse from
inheriting another artifact's failures. Repairing only an index keeps bad pack offsets excluded;
open a new reader after repair. Separate `Objects` handles, including fresh retention scans,
establish their own damage evidence. Broaden these paths when a consumer needs recovery from
header-derived errors or raw transfer failures, while preserving limits and mutation gates.

## Identity checks in walks

Every read that returns object content to a caller hashes the payload against the requested
identity: `Objects::read`, checkout, diff inputs, push and anything jj receives. CRCs, zlib
checksums and framing catch damaged bytes, but only the hash catches an index entry that names
another object's intact bytes, as a buggy writer could produce. Returning that content would hand jj
a wrong object instead of an error.

A walk that consumes only structure (identities, kinds and edges) and never returns content may read
through `Objects::read_unverified`, which skips that one hash and keeps every other check. Its
`UnverifiedObject` cannot become an `Object`. The walk still fails on malformed commit, tree or tag
syntax, on a kind that differs from the parent's assertion, and on framing, CRC, zlib or delta
errors. Today only the native local fetch server's selection walk uses it; the destination hashes
every object it imports.

A walk whose result authorizes a reference update, deleting or rewriting data keeps the hash,
because a mislabelled tree that drops an edge would make that edge's objects look unreachable.
Retention planning, `repack_retained`, retirement, prune and gc therefore read graph objects with
the hash. Retirement then decodes every retained object from the replacement or protected copy
before unlinking, as [Last readable copy](#last-readable-copy) requires. Measured on a 131k-object
history, these hashes were under 3 % of the plan walk; reading entries was the cost.

Native local push can establish additional complete commit boundaries by following identity-checked
parent edges from existing reference roots. Discovery targets external commit dependencies in the
verified received pack and stops at shallow roots. A verified complete base commit's tree and its
verified direct subtrees can also establish boundaries for unchanged subtrees in received trees.
Commit dates only order the search; they never prove reachability. Discovery considers every
verified reference root and has no internal ancestor-depth cutoff. External targets are deduplicated
before discovery. Caller-configured `max_known_objects`, `max_known_edges` and `max_known_bytes`
limit discovery work; proof reads retain the 1 MiB per-object and decoding limits. Native local
receive selects trusted aggregate limits rather than the lower-level `FetchLimits` default totals,
so discovery can visit the complete referenced commit graph. Direct subtree discovery examines at
most 256 subtrees per proven base; remaining subtrees use ordinary verification. Unproven objects
still require the complete connectivity walk. Mere stored-object presence supplies no completeness
evidence. Explicit resource limits, unreadable proof objects or skewed search order can still leave
a branch unproven and reject old corruption that Git would leave unvisited. Referenced history is
trusted complete, so this operation does not replace `git fsck`. Dependency reads, synchronized pack
publication and conditional ref updates remain.

Fetch and native local push also pair each received commit's tree with the tree of its first trusted
complete parent. A checked read of that parent tree makes each of its typed edges a complete
boundary of the kind its entry mode names; gitlinks and the empty tree never are. A changed subtree
whose name maps to a tree in the parent's tree pairs recursively, so reads follow the changed paths.
Pairing proof reads share one discovery allowance of `max_known_objects`, `max_known_edges` and
`max_known_bytes`, with the 1 MiB proof-object and decoding limits; an exhausted allowance supplies
no evidence. These boundaries are reachable from existing references through verified edges, which
is the same trust as the parent commit. The native local source walk records such an edge as a
dependency without reading it from the source, unless the destination's header lookup does not find
it with that kind, in which case the object is sent again. A failed read or parse supplies no
evidence. Unpaired received trees, such as those of commits whose parents arrive in the same fetch,
use the ordinary walk. Each complete dependency keeps the kind its evidence requires. Installation
checks every dependency's presence and size by header lookup, and rejects a complete one whose
selected header has another kind. Publication repeats that header check through the reopened store's
lookup, then stops at complete dependencies as it stops at reference targets, and walks the others
with checked reads.

Import hashes every received object, parses its edges and proves typed connectivity before
installation. On the eligible path below, that is the only identity check. The checked fallbacks can
hash received objects again; they are kept rather than removed to make the single pass
unconditional. It keeps each commit's and tag's typed edges, but not each tree's: on git/git,
163,754 trees hold 62.9 million entries. Instead, import records the kind each distinct tree entry
target must have. If every received tree parsed and named each target with one kind, connectivity
checks those targets once against received objects and complete local history, then walks only
commit and tag edges, checking every edge's required kind even when its target was already visited.
Otherwise it falls back to the exact walk from the wants, which re-reads trees from the received
pack and alone decides rejections. The table can add local dependencies named only by unreachable
received trees, and installation rechecks them too.

Publication reuses that proof only for the representation import checked. The pinned installed pack
must match the receive artifact and recorded pack checksum, and its retained index must match the
generated receive index. Normal lookup must select that exact pack for every received object, and
each object's recorded representation costs must satisfy the caller's per-read limits. Then only
local dependencies are walked, through checked reads down to existing references. Otherwise, as when
a different primary, alternate or duplicate copy is selected, publication walks the whole new graph
with ordinary checked reads where the evidence does not apply. Received objects count against the
independent walk object, payload-byte and edge budgets either way. Fetch's fast-forward query reads
verified commits breadth-first from the new tip and stops after verifying the old tip, without
following its parents. It detects cycles in the explored graph and respects shallow boundaries; a
negative answer exhausts the reachable graph. Unvisited history supplies no evidence. Public
full-history queries retain complete ancestry validation. This is no process-wide or persistent
trust cache, and the caller's existing trusted-directory and GC coordination obligations still
apply.

A walk locates blobs without decoding them, so it does not see a damaged blob. A content read of
that blob retries other verified copies or reports the damage. `Objects::verify_packs` checks entry
CRCs and pack checksums and refuses remembered damage; `git fsck` also reports damage. Repack and
retirement fail at the blob because copying checks its CRC and retirement decodes it.

## Index checksums at open

Opening a pack checks the index structure but not its trailing checksum, as Git does
([#80](https://github.com/joshka/girt/issues/80)). Hashing a 131k-entry index cost about a fifth of
every open, and so of every jj command. The structural checks run in linear time: exact table sizes,
strictly increasing identities, a fanout that matches them, each large offset used once, offsets
that tile the pack from byte 12, and the index's pack checksum against the pack trailer. Damage to
any of those fails the open.

Overlapping readers in one process share fully structurally validated index bytes and entry tables
for an unchanged pair of pinned index/pack generations. The process-local cache keys one weak record
per index/pack pair by both files' native identity, so the number of open packs never evicts a live
record, and a lookup is one ordered-map search. A newer generation of the same pair replaces the
older record: existing readers keep their shared validation, while later opens of the older
generation validate again. A match also requires the same object format and both handles' size,
modification time and metadata change time (Unix ctime or Windows ChangeTime). Change time
discriminates cache generations without changing the pinned-reader freshness contract: rename/unlink
can change it while the original artifact remains readable. Platforms without native file identity
or change time bypass the cache. Concurrent misses can validate independently.

The cache holds only weak references. Readers own the shared validation and independent artifact
pins that prevent file-ID reuse, including during an in-flight cache lookup, without extending
reader-local damage evidence. Dropping the last reader of a generation releases its shared pins and
buffers; an in-flight open can retain them until that open finishes. Dead weak records retain
neither handles nor buffers, only a map slot and a weak allocation; they are pruned once the map has
doubled since the last prune, so it holds at most 16 records or twice those live at the last prune.
Deleted pack/index storage can then be reclaimed, and Windows delete-pending names cease to be held
by shared validation. Other readers or external processes can still pin an artifact. Sequential
opens after all readers drop perform complete validation again.

Every open still charges the full input budgets, pins its own artifacts, checks the pack header,
object count and trailer agreement, and rechecks both handles' metadata before returning. A cache
hit allocates no per-entry tables or index-byte copy. Cold opens retain the complete eager
structural validation. Lookup and verification retain live truncation checks; mutation boundaries
retain freshness checks and explicit checksum gates. Atomic replacement leaves an existing reader on
its original handles and gives new readers a different cache key. The cache adds no mapping and
cannot fault on a truncated mapped page. Windows data writers remain excluded while readers pin a
generation; immutable publication and caller-owned exclusion remain required.

Damage the structure cannot see stays harmless because content reads are identity-checked (see
[Identity checks in walks](#identity-checks-in-walks)). Swapped offsets fail the entry CRC, or the
identity hash for v1. A changed name makes its object read as absent, and the changed name fails its
identity check. A read therefore retries a verified copy or fails rather than returning the wrong
content. The index checksum moves to the paths that trust names without hashing objects:
`Objects::verify_packs`, retention planning (which checks every pinned index before it can authorize
a deletion), verification of a replacement pack before retirement, and entry reuse when writing a
pack, which checks each source index once before copying bytes under its names.

## Fast path contracts

Standalone loose pruning can return without a graph walk only after a complete bounded inventory
finds no primary loose files eligible under the cutoff; disabling expiry makes every file
ineligible. Standalone reflog expiry can return after a complete bounded root observation finds no
record older than either possible expiry cutoff. Both observations run under caller isolation with
refreshed repository metadata. These empty reports certify zero changes, not history integrity:
unvisited missing or corrupt objects can remain undetected. Any possible deletion or rewrite still
requires the complete verified retention plan; composed maintenance keeps its existing plan and
validation contract.

Copied or reused data still counts against caller limits and statistics; skipping them silently
changes the API.

## Tests

The inventory's [executable acceptance tests](testing.md#specify-before-implementing) cover these
rules. Assert which data survives and that every retained object still reads.
