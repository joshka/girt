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

## Fast path contracts

Copied or reused data still counts against caller limits and statistics; skipping them silently
changes the API.

## Tests

The inventory's [executable acceptance tests](testing.md#specify-before-implementing) cover these
rules. Assert which data survives and that every retained object still reads.
