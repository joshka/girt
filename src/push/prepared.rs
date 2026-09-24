use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use super::graph::Graph;
use super::{PushCommand, PushFailure as Error, PushLimits};
use crate::packet::{check_cancelled, packet, put};
use crate::{ObjectId, Objects, PackObject, write_pack};

/// An immutable command list and complete non-thin SHA-1 pack ready for one receive-pack session.
///
/// Keeps exact caller expectations; remote advertisement checking occurs in [`super::send`].
/// Complete histories are sent even if the server already has them. The source object reader can
/// be dropped after preparation. Reuse is permitted but each session rechecks the same
/// expectations.
#[derive(Debug)]
pub struct PreparedPush {
    pub(super) commands: Vec<PushCommand>,
    pub(super) request: Vec<u8>,
    pub(super) pack: Vec<u8>,
    pub(super) limits: PushLimits,
    objects: u32,
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
    /// Cancellation is checked between graph steps and pack writes, but cannot interrupt a single
    /// storage read, hash, parse or compression call. Read limits bound decoding per object, not
    /// aggregate decoding across objects. Sources must meet [`Objects`]'s storage assumptions.
    ///
    /// # Errors
    ///
    /// Rejects duplicate destinations, zero IDs, unsupported namespaces, missing or mistyped edges,
    /// invalid payloads, implicit force, exhausted bounds, cancellation and storage/pack errors.
    /// No remote commands or local filesystem writes occur on either success or failure.
    pub fn new(
        objects: &Objects,
        commands: Vec<PushCommand>,
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        check_cancelled(cancel)?;
        let request = encode_commands(&commands, limits, cancel)?;
        if commands.is_empty() {
            return Ok(Self {
                commands,
                request,
                pack: vec![],
                limits,
                objects: 0,
            });
        }
        let graph = Graph::select(objects, &commands, limits, cancel)?;
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
        let result = write_pack(&inputs, &mut pack, &mut io::sink(), limits.pack);
        check_cancelled(cancel)?;
        let written = result?;
        Ok(Self {
            commands,
            request,
            pack: pack.bytes,
            limits,
            objects: written.objects,
        })
    }

    /// Number of distinct reachable objects in the buffered pack.
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
        let name = command.name.as_bytes();
        if !name.starts_with(b"refs/heads/") && !name.starts_with(b"refs/tags/") {
            return Err(Error::Unsupported("destination namespace"));
        }
        if command.new == ObjectId::from_bytes([0; 20])
            || command.expected == Some(ObjectId::from_bytes([0; 20]))
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
        let old = command.expected.unwrap_or(ObjectId::from_bytes([0; 20]));
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
