# Conditional References and Reflogs

The files backend supports SHA-1 and SHA-256 repositories on Unix and Windows local filesystems.
`Repository::references()` provides live reads and conditional mutations. Object existence, hooks,
configuration-driven reflog policy, and crash durability belong to callers. Reftable remains a
required separate backend under R37; R14 owns broader backend acceptance.

## Preconditions and Publication

`Expected::Absent` requires no loose or packed value. `Expected::Value` compares an exact stored
value, including a symbolic target name. `Expected::Exists` requires a value of either kind.
`Expected::AbsentOr(value)` permits creation or that exact existing value; it never authorizes
replacing a different value. `Expected::Any` deliberately omits the comparison. A resolved edit
compares the terminal value; a stored edit compares the named reference itself.

Transactions hold the packed lock, every old/new symbolic dependency lock, destination locks, and
requested log locks through publication. Preparation rechecks observations under locks. Conflicts,
cycles, depth beyond 32 hops, namespace overlaps, malformed data and predicate failures stop before
content changes. Empty parent directories can remain. Contention returns immediately; callers must
re-read before choosing any retry policy.

Packed deletion publishes first, removing all selected records while preserving unrelated bytes.
Refs then publish in input order, each followed by its requested log effects. Loose values shadow
packed values. Deleting a shadow removes both so the older packed value cannot reappear. Readers can
observe intermediate states; this is not a rollback transaction or a filesystem snapshot.

`TransactionError::Publish` retains every operation's `RefOutcome` and `LogOutcome`. Later
operations can already have `PackedRemoved` when an earlier loose publication fails. A published ref
can have an unattempted, failed or partially appended log. Append failures report the exact number
of record bytes written; zero bytes can still mean an empty log was created. Log-deletion failure
reports zero bytes and leaves the log under the cooperating-writer contract. Inspect the report and
current state before recovery; retrying the original batch may violate its original preconditions.

## History Policy

`Reflog::Preserve` leaves history unchanged. `Reflog::Append` creates a log or appends one record
even when the ID is unchanged. A resolved HEAD edit logs HEAD and each symbolic hop with terminal
old/new IDs. A stored HEAD edit logs only HEAD: detachment records the old branch tip, symbolic
replacement records the newly selected branch tip, and an unborn endpoint contributes the format's
zero ID. Deleting a stored symbolic ref does not delete its target. Direct branch edits do not
discover HEAD aliases automatically.

`Reflog::Delete` is valid only with ref deletion. It removes the destination log after the ref;
resolved deletion removes the terminal branch log while preserving symbolic aliases' logs. It can
remove malformed history because deletion does not interpret log contents. Append writers validate
existing history first. Git may append automatic HEAD logs without girt's log lock; ordering against
such appends is not promised. Callers must exclude independent expiry or log rewriting.

Imported records use a more tolerant grammar than new records. Empty/padded names, empty emails,
noncanonical four-digit zones such as `+0060` and `+2460`, signed timestamps, and leading timestamp
zeros are interpreted without applying construction validation. `ReflogRecord::parse` retains exact
bytes, including ID casing, padding, delimiters and the final newline; `entry()` supplies
interpreted fields. `ReflogEntry::parse` returns only interpreted values. New appends still validate
canonical identities, nonnegative seconds, offsets within 23:59 and single-line non-NUL messages.

The current reader requires exact-width IDs, complete LF records, signed `i64` seconds and a signed
four-digit zone. It rejects CR/NUL records, short zones and zone suffix text. Independent Git
observations accept some of those forms, so these are reader limits, not assertions that Git rejects
them. Reads allocate in proportion to whole ref/log files; no configurable allocation budget or
streaming interface is provided. These limitations remain visible for later acceptance work.

## Platform and Cleanup

Names preserve bytes on Unix. Windows storage requires UTF-8 and rejects Win32 device names, invalid
filename characters and trailing-dot components. Case and Unicode aliases follow the filesystem.
Symlink paths are rejected. Operations require trusted directories, exclusive creation and
same-directory atomic replacement; hostile path replacement and network filesystems are outside the
contract.

Windows lock handles exclude delete sharing while held. Publication uses a separate temporary file,
so the lock remains owned through ref and log publication. Unix publication and cleanup check lock
file identity, leaving replacement locks untouched. Cleanup is best effort; cleanup errors or
process termination can leave stale locks. No global signal handler is installed. Ordinary file
creation permissions and Unix umask apply; Windows inherits directory ACLs, with no promise to
preserve an existing destination's ACL. Successful visibility does not promise fsync or crash
survival.

The executable [reference transaction example](../examples/reference_transaction.rs) demonstrates
unborn publication, retained deletion history, absent-or-same retention refs and explicit ref/log
deletion. [R11 evidence](evidence/r11.md) identifies exact tested revisions and platform results.
