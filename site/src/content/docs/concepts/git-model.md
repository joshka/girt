---
title: The Git model
description: Follow content, identities, names, and storage through girt's APIs.
---

Git stores immutable content and gives it mutable names. To see how girt models this, follow one
file into a snapshot, then follow the snapshot's branch name back to its bytes. Each step has its
own validation rules and lifetime.

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
[commit codec](https://docs.rs/girt/latest/girt/commit/struct.Commit.html) docs describe which
representations it accepts.

## An ID is a name for content, not proof of storage

`ObjectId` carries either SHA-1 or SHA-256 digest bytes. A repository selects one format, and
embedded IDs must agree with it. Parsing hexadecimal text does not prove that an object exists or
reveal its kind. The ID parser accepts complete IDs; expanding an abbreviation needs a lookup in a
repository.

The all-zero ID is an absence sentinel in contexts such as reflogs. It is not the ID of an empty
blob or empty tree. Converting an object graph to another hash format would require translating its
embedded references as well as hashing its outer payload; girt does not do that implicitly.

## Reading, parsing, and validation answer different questions

A combined object reader locates loose or packed storage and verifies framing and identity. It
returns the logical kind and exact uncompressed bytes. A payload parser then reads the fields for
that kind. A separate validation step checks the rules for constructing new objects, such as unique
tree names or valid author fields.

This separation lets an inspector report unusual imported data without changing it. Parsing a commit
can expose its parents even when author dates are unreadable. A parsed tree can retain records whose
names fail validation. Validation failure does not erase the original bytes. Reconstructing editable
fields can normalize spelling and create a different ID even when the logical fields appear
unchanged. Cryptographic signature headers remain opaque until the application verifies them; author
and committer identity metadata does not prove authorship.

Choose the stage your task needs. A history walk needs readable commit graph records; a tool
constructing a replacement commit needs valid construction fields. Neither result proves that every
referenced tree or blob is present. Check the detailed
[commit contract](https://docs.rs/girt/latest/girt/commit/struct.Commit.html) before converting
imported data.

## Physical storage can change without changing content

Loose storage keeps an individually compressed object in a path derived from its ID. Packs group
objects and may encode one as a delta against another. Their indexes locate IDs by offset. Repacking
changes physical representation while preserving logical object identities.

Keep one [`Objects`](https://docs.rs/girt/latest/girt/objects/struct.Objects.html) reader for
related reads: the packs it opened stay usable even if the repository is repacked. It sees new packs
only after a refresh or in a new reader, but it reads loose files live. A loose-only object can
disappear from an old reader if repacking removes its loose file after publishing a new pack.
Refresh the reader to discover that pack before retrying. Refreshing a reader does not refresh
repository configuration or shallow boundaries. Alternates add borrowed object stores to lookup;
being able to read them does not authorize pruning them.

Opening ([`PackLimits`](https://docs.rs/girt/latest/girt/objects/struct.PackLimits.html)) and
decoding ([`ReadLimits`](https://docs.rs/girt/latest/girt/objects/struct.ReadLimits.html)) have
separate resource budgets. A small final payload can require substantial delta reconstruction, and a
large pack does not necessarily require holding all pack bytes in memory. The budgets limit specific
work and allocations; they do not cap total process memory or time. See
[object storage](https://docs.rs/girt/latest/girt/objects/struct.Objects.html) for visibility and
limits.

## References connect content to user-visible names

A direct reference stores an ID; a symbolic reference stores another full reference name. `HEAD`
commonly points to a branch. Before the first commit, that branch is unborn: resolution reaches a
name with no ID. Resolving symbolic names and peeling annotated tag objects are separate operations.
A reflog records changes to a name, rather than commit ancestry, and can mention objects no longer
available in storage.

Store the objects before publishing a reference to them. Give the update a precondition so you don't
silently overwrite a change another writer made in between. Reference transactions do not prove
object existence, kind, ancestry, or authorization. Files-backed batches can have partial
publication; inspect their outcomes before deciding how to recover. The
[references guide](/girt/guides/refs/) shows how to publish a reference and choose whether to log
the change.

## Repository metadata supplies context

A repository locates its private Git directory, common directory, object database, configuration,
and shallow declarations. Bare repositories have no working directory. Linked worktrees share common
storage but retain private metadata, so appending `.git` to every path is incorrect.

Opening a repository captures its metadata at that moment. Configuration resolution combines the
sources the application selects and records where each value came from; it sees the process
environment only through a snapshot the application supplies. A shallow declaration stops history
traversal at a commit while raw parsing still shows its stored parents. After deepening, refresh
repository shallow metadata and create a new object reader.

Continue with [opening a repository](/girt/guides/open-a-repository/) and
[configuration](/girt/guides/configuration/). Working-tree state belongs to the separate
[index and working-tree guide](/girt/guides/index-and-working-tree/), and transfers are covered in
[fetch, push, and clone](/girt/guides/fetch-push-clone/).
