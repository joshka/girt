# Operation Tracing

Enable the optional `tracing` feature to observe operations on the `girt` target. The caller owns
subscribers, filtering, sinks, runtimes, cancellation and scheduling. Girt emits spans with safe
categories and counts; it does not emit error events, install a subscriber, retry failures, or
format arguments, return values or errors. Structured results remain the authority for recovery.
Subscriber callbacks execute synchronously and can add latency or panic, so caller policy must
account for the subscriber as well as the operation.

Run `cargo run --features tracing --example tracing` for a scoped formatting subscriber. Its
`FmtSpan::CLOSE` option makes the subscriber print span completion, including busy/idle durations.
The application can instead export spans or capture them without printing. An application should log
its returned error once at the boundary that owns the diagnostic. Restrict subscriber targets to
`girt` when the application wants only these fields; dependencies have their own instrumentation and
policies. The example is a local-storage program and installs no global subscriber.

## Coverage and Levels

| Level | Operations                                                                                        | Fields beyond completion                                                                                                        |
| ----- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| TRACE | `loose.read`, `loose.write`, `objects.read`                                                       | Loose read limit; write payload bytes                                                                                           |
| DEBUG | `objects.open`                                                                                    | Snapshot-opening outcome                                                                                                        |
| DEBUG | `history.walk`, `history.is_ancestor`, `history.merge_bases`, `history.graph`                     | Graph roots and successfully collected commit count                                                                             |
| DEBUG | `fetch.receive`, `fetch.local`, `fetch.local_server`, `fetch.http`, `fetch.ssh`, `fetch.validate` | Buffered HTTP/SSH response bytes                                                                                                |
| DEBUG | `fetch.import`, `fetch.connectivity`, `fetch.install`                                             | Pack bytes, object counts, charged decode bytes, known/received objects, wants, visited objects, edges and installation phase   |
| DEBUG | `push.prepare`, `push.send`, `push.local`, `push.local_server`, `push.http`, `push.ssh`           | Prepared commands, objects and pack bytes; accepted/rejected/pending acknowledgements, unpack status and uncertain transmission |
| DEBUG | `refs.transaction`, `refs.prepare_transaction`, `refs.publish`                                    | Requested edit count and possible partial publication                                                                           |
| DEBUG | `index.read_index`, `index.edit_index`, `index.commit`, `index.abort`                             | Outcome and failed cleanup                                                                                                      |
| DEBUG | `fetch.finish`, `clone.finish`                                                                    | Composed completion and possible residual state                                                                                 |

Nested calls form phase relationships: a history query owns graph reading; a received fetch owns
pack import and connectivity; clone completion owns fetch completion, installation and reference
transactions. Local-server spans include process completion and cleanup, so a successful protocol
phase does not imply a successful local transfer. HTTP and SSH require their respective features;
SSH network spans exist only on supported macOS/Linux targets.

DEBUG avoids object-by-object output. TRACE deliberately makes individual storage calls visible,
including storage calls within traversal. There are no byte, packet or tree/index-entry events. Pure
codecs and hashing, config/discovery, standalone reference reads/writes outside transactions, pack
writing outside push, tree comparison, content diff, status and checkout have no dedicated spans
yet; calls they make to covered operations can still appear. Future roadmap operations extend this
coverage at their owning boundaries rather than claiming crate-wide instrumentation.

## Outcomes, Effects and Counts

Every operation starts with `outcome = "incomplete"`. A returned result records `success`, `failure`
or `cancelled`; a dropped future or unwind remains `incomplete`. Incomplete does not mean that a
mutation was rolled back. A successful download is not validated or installed, and a successful push
exchange can contain per-reference rejection statuses; `accepted`, `rejected`, `pending` and
`unpack` expose that distinction without remote message text. Missing-object reads returning `None`
are successful lookups; their result remains the caller's responsibility.

`failure_class` records fixed categories such as `missing`, `wrong_kind`, `corrupt`, `unsupported`,
`limit`, `cancelled`, `deadline`, `io`, `transport`, `protocol`, `remote`, `conflict`,
`precondition`, `invalid_input` or `repository`. Nested storage and transfer causes retain their
categories. Repository-opening/initialization failures and planning failures have coarser
categories; consult the original structured source chain. No error display text is recorded, even
for an I/O source or remote error containing arbitrary bytes.

`effects` is absent unless a boundary supplies useful evidence. Push failures distinguish `not_sent`
from `uncertain`; inspect the returned acknowledgement report before deciding what to do. Reference
publication and fetch/clone completion failures use conservative `possibly_partial`. Index errors
with a failed lock cleanup use `cleanup_failed`. Fetch installation records `pack_visible` after
successful publication/reuse of its pack, then `pack_and_index_visible` after its index. These
values describe progress, not crash durability, new artifact counts or rollback. An unset field does
not prove absence of effects: directories or private artifacts can precede the first publication
boundary. Returned reports and the documented storage contracts remain binding.

Loose reads/writes and object snapshot opening record the closed `object_format` value (`sha1` or
`sha256`). Format refusals use the existing `unsupported` failure class, including index locking and
fetch installation refusal before writes.

Counts describe the named phase, not total allocations or unique work across nested spans. Import
`decoded_bytes` is the charged decode budget, including delta work. Connectivity records visited
objects and edge occurrences after successful completion. Successful graph/import counts can be
absent after failure; they are not progress callbacks. Limits already belong to operation inputs. No
paths, URLs, environment values, reference names, object IDs, identities, credentials, signatures or
object contents are recorded by these spans.

`objects.peel` owns its synchronous `objects.read` children and records categorical completion. It
exposes no IDs, tag names, payloads or identity bytes. Cancellation is checked on both sides of each
blocking read; tracing does not add cancellation inside storage I/O. Its errors preserve the failing
chain link independently of instrumentation.

## Async Lifetimes and Worker Handoff

Async operations use `Instrument` to enter their span only while polling or dropping the inner
future. Never hold an entered-span guard across an await. Interleaved operations on one executor
therefore retain separate parents. The caller should use `Instrument` for its own async parent spans
and `WithSubscriber` when a scoped dispatch must follow a future between executor threads. A
thread-local default alone does not follow arbitrary spawned futures.

`DownloadedFetch` captures the initiating dispatch and current span when constructed. With tracing
enabled, it retains that subscriber and span until validation or drop. `validate` restores both for
synchronous work on the caller's worker, then restores the worker's previous context. Span release
also uses its owning dispatch, including abandoned downloads. The value remains
`Send + Sync + 'static`; girt creates no worker or runtime. Clone and fetch workflow download
wrappers inherit this behavior through their owned download.

The retained HTTP/SSH span's close time includes the queue delay and validation lifetime. Its
`success` outcome refers to downloading; the child `fetch.validate` span records validation's own
outcome. Subscriber busy/idle time is time spent entered/outside the span, not CPU accounting or a
network-only timer. A caller needing network latency should time the receive future separately.
Preparation, installation and finish values do not retain tracing context automatically: carry a
caller span and dispatch explicitly when scheduling those operations. Join owned workers even after
cancellation; tracing does not extend cancellation or process-cleanup guarantees.

## Cost and Future Instrumentation

Without the feature, tracing calls, fields, context storage and the direct tracing dependency are
compiled out. Operation closures preserve the original return/error paths. With the feature but no
interested subscriber, callsites perform tracing checks and completion matching; network downloads
also retain an inert span and dispatch. With an interested subscriber, span creation, field
recording, entry/exit, retention and export costs depend on that subscriber. Girt adds no clock
reads; timing is subscriber-owned. See the [R03 evidence](evidence/r03.md) for measured
whole-operation costs with the feature disabled, enabled without a subscriber, and enabled with a
formatting subscriber to a sink. These measurements are observations, not a numerical regression
gate.

For a new operation:

1. Choose one operation boundary and meaningful phase children. Add aggregate counts only where they
   explain work or resource use; keep per-object storage detail at TRACE.
1. Create an explicit span with literal safe field names. Record completion on that exact handle;
   `Span::current()` can refer to an enabled parent when the intended child is filtered out.
1. Preserve the original structured result and classify variants without formatting their payloads.
   Record partial/uncertain effects at the boundary that knows them; never infer rollback.
1. Enter synchronous work with `in_scope` and instrument async futures across suspension. Do not
   alter Send/Sync requirements or add runtime, worker, retry or logging policy.
1. Test hierarchy, filtering, sensitive sentinels, failures and ownership/cleanup. For owned
   handoffs, test both validation and abandonment under a different subscriber. Measure material
   hot-path costs with the feature disabled and representative subscriber configurations.
