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

## Protected data

Treat protected data as untouchable, not as proof. Never delete or rewrite `.keep`, recent or
alternate storage, and don't copy its objects into new output again: retirement keeps the original,
so the copy is permanent duplication. Omit an object because a protected copy holds it only after
that copy decodes to the right object.

## Lookup copy

When several packs or stores hold an object, copy the representation object lookup reads; another
copy may be corrupt where the scanned one was not.

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

Fetch may reuse kind, size and typed-edge evidence recorded from identity-checked received objects.
This evidence is owned by that receive result and never supplies object content. Before a
destination walk uses it, the pinned installed pack must match the receive artifact and recorded
pack checksum, and its retained index must match the generated receive index. Normal lookup must
select that exact pack; a different primary, alternate or duplicate copy receives the ordinary
checked read. Independent walk object, payload-byte and edge budgets still apply. Recorded uncached
representation costs must satisfy the caller's per-read limits; otherwise the ordinary reader checks
those limits. Full ancestry and cycle validation remain unchanged. This is no process-wide or
persistent trust cache, and the caller's existing trusted-directory and GC coordination obligations
still apply.

A walk locates blobs without decoding them, so it does not see a damaged blob. A content read of
that blob reports the damage, as do `Objects::verify_packs` (entry CRCs and pack checksums) and
`git fsck`. Repack and retirement fail at the blob because copying checks its CRC and retirement
decodes it.

## Fast path contracts

Copied or reused data still counts against caller limits and statistics; skipping them silently
changes the API.

## Tests

The inventory's [executable acceptance tests](testing.md#specify-before-implementing) cover these
rules. Assert which data survives and that every retained object still reads.
