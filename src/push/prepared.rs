use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use super::graph::Graph;
use super::{PushCommand, PushFailure as Error, PushLimits};
use crate::packet::{check_cancelled, packet, put};
use crate::{ObjectId, Objects, PackObject};

/// An immutable command list and non-thin SHA-1 pack ready for one receive-pack session.
///
/// Keeps exact caller expectations; remote advertisement checking occurs in [`super::send`].
/// [`Self::new`] sends complete histories; [`Self::new_excluding`] omits explicit receiver history
/// after validating its selected closure. The source object reader can
/// be dropped after preparation. Reuse is permitted but each session rechecks the same
/// expectations.
#[derive(Debug)]
pub struct PreparedPush {
    pub(super) commands: Vec<PushCommand>,
    pub(super) request: Vec<u8>,
    pub(super) pack: Vec<u8>,
    pub(super) limits: PushLimits,
    objects: u32,
    pub(super) receiver_roots: Vec<ObjectId>,
}
impl PreparedPush {
    /// Validates commands, selects all reachable objects, proves permitted branch ancestry, and
    /// builds a pack without modifying either repository or invoking Git.
    ///
    /// Follows commit parents/trees, tree entries and typed tag targets; gitlinks name external
    /// submodule commits and are not followed. Branch tips must be commits. No reachable object
    /// may be missing, even when it is expected to exist remotely. An old branch tip outside the
    /// new history requires explicit force; it need not be present locally when force is allowed.
    /// Unchanged IDs are sent as conditional commands and remain subject to server policy.
    ///
    /// Cancellation is checked between graph steps, pack input checks and hashes, pack writes,
    /// candidates and every 4096 search units. A single storage read, hash, parse, sort or
    /// compression call cannot be interrupted. Read
    /// limits bound decoding per object, not aggregate decoding across objects. Sources must
    /// meet [`Objects`]'s storage assumptions.
    ///
    /// # Errors
    ///
    /// Rejects duplicate destinations, zero IDs, unsupported namespaces, missing or mistyped edges,
    /// invalid payloads, implicit force, shallow snapshots, exhausted bounds, cancellation and
    /// storage/pack errors. Shallow push negotiation is not implemented.
    /// No remote commands or local filesystem writes occur on either success or failure.
    pub fn new(
        objects: &Objects,
        commands: Vec<PushCommand>,
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::new_excluding(objects, commands, &[], limits, cancel)
    }

    /// Prepares a non-thin pack excluding complete histories of explicit receiver roots.
    ///
    /// Each usable root must belong to the fully validated selected graph. Its complete reachable
    /// closure (including trees and tags, excluding gitlinks) is omitted. Missing roots and roots
    /// outside that graph are ignored: arbitrary local possession never proves remote possession.
    /// This deliberately sends a complete pack for many disconnected/rewritten histories.
    /// [`super::send`] requires every root used for exclusion to appear in the live advertisement
    /// as a ref tip or `.have`, independently of command expectations. If a root has disappeared,
    /// prepare a full transfer or retry using fresh knowledge. Coordinate with server pruning/GC;
    /// advertisements cannot guarantee object retention against concurrent deletion.
    ///
    /// Selection and force proofs use the same full-graph budgets as [`Self::new`]. Exclusion has
    /// a separate `max_edges` allowance, visits at most the selected object count, and accepts at
    /// most `max_refs` root occurrences. Preparation can therefore cost more despite a smaller
    /// wire pack. Pack limits still bound the full selected payload/count before exclusion.
    ///
    /// # Errors
    ///
    /// Returns [`Self::new`]'s errors or exhausted receiver-root/exclusion bounds. Missing objects
    /// in the selected history fail even if expected remotely. No commands are transmitted.
    pub fn new_excluding(
        objects: &Objects,
        commands: Vec<PushCommand>,
        receiver_roots: &[ObjectId],
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "push.prepare",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            commands = commands.len(),
            objects = tracing::field::Empty,
            pack_bytes = tracing::field::Empty,
        );

        let operation = || {
            check_cancelled(cancel)?;
            if !objects.shallow_roots().is_empty() {
                return Err(Error::Unsupported("shallow push preparation"));
            }
            for id in receiver_roots {
                id.require_sha1()?;
            }
            let request = encode_commands(&commands, limits, cancel)?;
            if commands.is_empty() {
                return Ok(Self {
                    commands,
                    request,
                    pack: vec![],
                    limits,
                    objects: 0,
                    receiver_roots: vec![],
                });
            }
            let mut graph = Graph::select(objects, &commands, limits, cancel)?;
            let receiver_roots = graph.exclude(receiver_roots, limits, cancel)?;
            let inputs: Vec<_> = graph
                .objects
                .iter()
                .map(|(&id, object)| PackObject {
                    id,
                    kind: object.kind(),
                    data: object.data(),
                })
                .collect();
            let mut pack = CancelBuffer {
                bytes: vec![],
                cancel,
            };
            let result = crate::pack::write_controlled(
                crate::ObjectFormat::Sha1,
                &inputs,
                &mut pack,
                &mut io::sink(),
                limits.pack,
                limits.compression,
                &mut || {
                    if cancel.load(Ordering::Relaxed) {
                        Err(io::Error::other("push cancelled").into())
                    } else {
                        Ok(())
                    }
                },
            );
            check_cancelled(cancel)?;
            let written = result?;
            Ok(Self {
                commands,
                request,
                pack: pack.bytes,
                limits,
                objects: written.objects,
                receiver_roots,
            })
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::push_failure);
        #[cfg(feature = "tracing")]
        if let Ok(value) = &result {
            span.record("objects", value.object_count())
                .record("pack_bytes", value.pack_bytes());
        }

        result
    }

    /// Number of objects in the buffered pack after any receiver-history exclusion.
    pub fn object_count(&self) -> u32 {
        self.objects
    }

    /// Encoded pack length, including header and checksum; zero for an empty command list.
    pub fn pack_bytes(&self) -> usize {
        self.pack.len()
    }

    /// Explicit commands in caller order; no local references were inferred or changed.
    pub fn commands(&self) -> &[PushCommand] {
        &self.commands
    }
}

fn encode_commands(
    commands: &[PushCommand],
    limits: PushLimits,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    if commands.len() > limits.max_commands {
        return Err(Error::Limit("commands"));
    }
    let mut names = HashSet::new();
    let mut request = Vec::new();
    for (i, command) in commands.iter().enumerate() {
        check_cancelled(cancel)?;
        command.new.require_sha1()?;
        if let Some(id) = command.expected {
            id.require_sha1()?;
        }
        let name = command.name.as_bytes();
        if !name.starts_with(b"refs/heads/") && !name.starts_with(b"refs/tags/") {
            return Err(Error::Unsupported("destination namespace"));
        }
        if command.new == ObjectId::Sha1([0; 20])
            || command.expected == Some(ObjectId::Sha1([0; 20]))
        {
            return Err(Error::Command("zero object ID; deletion is unsupported"));
        }
        if !names.insert(&command.name) {
            return Err(Error::Command("duplicate destination"));
        }
        let caps = if i == 0 {
            b"\0report-status".as_slice()
        } else {
            b""
        };
        let length = 82usize
            .checked_add(name.len())
            .and_then(|n| n.checked_add(caps.len() + 4))
            .ok_or(Error::Limit("command bytes"))?;
        if length > 65520 || length > limits.max_command_bytes.saturating_sub(request.len()) {
            return Err(Error::Limit("command bytes"));
        }
        let old = command.expected.unwrap_or(ObjectId::Sha1([0; 20]));
        let mut line = format!("{old} {} ", command.new).into_bytes();
        line.extend_from_slice(name);
        line.extend_from_slice(caps);
        packet(&mut request, &line, cancel)?;
    }
    if limits.max_command_bytes.saturating_sub(request.len()) < 4 {
        return Err(Error::Limit("command bytes"));
    }
    put(&mut request, b"0000", cancel)?;
    Ok(request)
}

struct CancelBuffer<'a> {
    bytes: Vec<u8>,
    cancel: &'a AtomicBool,
}
impl Write for CancelBuffer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Do not return Interrupted: write_all retries it, which would spin after cancellation.
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("push cancelled"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
