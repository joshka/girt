---
title: The Git model
description: Follow content, identities, names, and storage through girt's APIs.
---

Git stores immutable content and gives it mutable names. A useful way to approach girt is to follow
one file into a snapshot, then follow the snapshot's branch name back to its bytes. Each step has a
different validation and lifetime boundary.

## From bytes to a snapshot

A blob contains bytes. It has no file name, executable bit, timestamp, or text encoding. Git hashes
its kind, payload length, and exact bytes to obtain its object ID. A trailing newline changes that
ID; storing the same bytes under two file names does not create two different blobs.

A tree describes one directory. Each entry contains a byte name, mode, and child ID. Regular files,
executables, and symbolic links all name blobs; a symlink blob contains target bytes. Subdirectories
name other trees. A gitlink names a commit in a separate repository and remains a leaf during
ordinary tree traversal. Executability changes the tree ID, while the blob can stay the same.

A commit names a root tree and ordered parent commits, plus people, dates, headers, and a message. A
root commit has no parents; a merge has several. Two commits can share a tree while recording
different messages or ancestry. Dates are metadata: parent edges establish ancestry even when a
child's timestamp precedes its parent's.

An annotated tag adds a name, optional tagger, and message to another object's ID. Its target can be
any object kind, including another tag. A lightweight tag needs no tag object; it is a reference
pointing directly at its target.

See [objects and history](/girt/guides/objects-and-history/) for a complete program that stores a
file, constructs commits, reads the snapshot back, and finds a branch point. The
[commit codec](https://docs.rs/girt/latest/girt/struct.Commit.html) document accepted
representations.

## An ID is a name for content, not proof of storage

`ObjectId` carries either SHA-1 or SHA-256 digest bytes. A repository selects one format, and
embedded IDs must agree with it. Parsing hexadecimal text does not prove that an object exists or
reveal its kind. Abbreviations require a lookup policy; the ID parser accepts complete identities.

The all-zero ID is an absence sentinel in contexts such as reflogs. It is not the ID of an empty
blob or empty tree. Converting an object graph to another hash format would require translating its
embedded references as well as hashing its outer payload; girt does not do that implicitly.

## Reading, parsing, and validation answer different questions

A combined object reader locates loose or packed storage and verifies framing and identity. It
returns the logical kind and exact uncompressed bytes. A payload parser then interprets the fields
appropriate for that kind. Further validation can impose construction rules, such as unique tree
names or valid author fields.

This separation lets an inspector report unusual imported data without changing it. Parsing a commit
can expose its parents even when author dates are unreadable. A parsed tree can retain records whose
names fail validation. Validation failure does not erase the original bytes. Reconstructing editable
fields can normalize spelling and create a different ID even when the logical fields appear
unchanged. Cryptographic signature headers remain opaque until the application verifies them; author
and committer identity metadata does not prove authorship.

Choose the stage your task needs. A history walk needs readable commit graph records; a tool
constructing a replacement commit needs valid construction fields. Neither result proves that every
referenced tree or blob is present. Check the detailed
[commit contract](https://docs.rs/girt/latest/girt/struct.Commit.html) before converting imported
data.

## Physical storage can change without changing content

Loose storage keeps an individually compressed object in a path derived from its ID. Packs group
objects and may encode one as a delta against another. Their indexes locate IDs by offset. Repacking
changes physical representation while preserving logical object identities.

Retain an [`Objects`](https://docs.rs/girt/latest/girt/struct.Objects.html) reader while doing
related reads. It keeps its opened pack generation usable across repacking. New packs require a
refresh or new reader; loose files remain live. A loose-only object can disappear from an old reader
if repacking removes its loose file after publishing a new pack. Refresh the reader to discover that
pack before retrying. Refreshing a reader does not refresh repository configuration or shallow
boundaries. Alternates add borrowed object stores to lookup; being able to read them does not
authorize pruning them.

Opening ([`PackLimits`](https://docs.rs/girt/latest/girt/struct.PackLimits.html)) and decoding
([`ReadLimits`](https://docs.rs/girt/latest/girt/struct.ReadLimits.html)) have separate resource
budgets. A small final payload can require substantial delta reconstruction, and a large pack does
not necessarily require retaining all pack bytes in memory. Budgets describe selected work and
allocations, not a total process memory or time cap. See
[object storage](https://docs.rs/girt/latest/girt/struct.Objects.html) for visibility and limits.

## References connect content to user-visible names

A direct reference stores an ID; a symbolic reference stores another full reference name. `HEAD`
commonly points to a branch. Before the first commit, that branch is unborn: resolution reaches a
name with no ID. Resolving symbolic names and peeling annotated tag objects are separate operations.
A reflog records changes to a name, rather than commit ancestry, and can mention objects no longer
available in storage.

Store the intended graph before publishing its reference. Use a precondition on the update so an
intervening writer cannot silently be overwritten. Reference transactions do not prove object
existence, kind, ancestry, or authorization. Files-backed batches can have partial publication;
inspect their outcomes before deciding how to recover. The [references guide](/girt/guides/refs/)
shows publication and logging as explicit decisions.

## Repository metadata supplies context

A repository locates its private Git directory, common directory, object database, configuration,
and shallow declarations. Bare repositories have no working directory. Linked worktrees share common
storage but retain private metadata, so appending `.git` to every path is incorrect.

Opening captures metadata. Configuration resolution combines explicitly selected sources and retains
provenance; it does not read the process-global environment unless the application supplies such a
snapshot. A shallow declaration stops history traversal at a commit while raw parsing still shows
its stored parents. After deepening, refresh repository shallow metadata and create a new object
reader.

Continue with [opening a repository](/girt/guides/open-a-repository/) and
[configuration](/girt/guides/configuration/). Working-tree state belongs to the separate
[index and working-tree guide](/girt/guides/index-and-working-tree/); transfer policy belongs to
[fetch, push, and clone](/girt/guides/fetch-push-clone/).
