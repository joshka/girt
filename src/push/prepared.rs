use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{PreparationProgress, PushCommand, PushFailure as Error, PushLimits, graph};
use crate::packet::{check_cancelled, packet, put};
use crate::{ObjectId, Objects, PackObject};

/// An immutable command list and non-thin pack ready for one push.
///
/// # Preparation choices
///
/// [`Self::new`] prepares a full wire transfer. [`Self::new_excluding`] can omit validated
/// advertised receiver history; [`Self::new_local`] prepares the native local path. Add explicit
/// options with [`Self::with_push_options`] or progress with [`Self::with_progress`] before
/// sending. [`super::send`] and [`super::send_local`] own exchange and status reporting.
///
/// Keeps exact caller expectations; wire advertisement checking occurs in [`super::send`].
/// [`Self::new`] sends complete histories; [`Self::new_excluding`] omits explicit receiver history
/// after validating its selected closure. The source object reader can
/// be dropped after preparation. Reuse is permitted but each session rechecks the same
/// expectations.
#[derive(Debug)]
pub struct PreparedPush {
    pub(super) format: crate::ObjectFormat,
    pub(super) commands: Vec<PushCommand>,
    pub(super) request: Vec<u8>,
    pub(super) pack: Vec<u8>,
    pub(super) index: Vec<u8>,
    pub(super) checksum: Option<ObjectId>,
    pub(super) limits: PushLimits,
    objects: u32,
    pub(super) receiver_roots: Vec<ObjectId>,
    pub(super) has_options: bool,
    pub(super) options: Vec<Vec<u8>>,
    pub(super) progress: bool,
}
impl PreparedPush {
    /// Validates commands, selects all reachable objects, proves permitted branch ancestry, and
    /// builds a pack without modifying either repository or invoking Git.
    ///
    /// Follows commit parents/trees, tree entries and typed tag targets; gitlinks name external
    /// submodule commits and are not followed. Branch tips must be commits. No reachable object
    /// may be missing, except Git's canonical empty tree. An old branch tip outside the new
    /// history requires explicit force; it need not be present locally when force is allowed.
    /// The fast-forward proof is exact whatever the commit dates; it walks from the new tip in
    /// committer-date order and usually stops soon after reaching the old tip.
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
    /// Rejects duplicate destinations, zero expected IDs, destinations outside `refs/`, missing or
    /// mistyped edges, invalid payloads, implicit force, shallow snapshots, exhausted bounds,
    /// cancellation and storage/pack errors. Shallow push negotiation is not implemented.
    /// No remote commands or local filesystem writes occur on either success or failure.
    pub fn new(
        objects: &Objects,
        commands: Vec<PushCommand>,
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::new_excluding(objects, commands, &[], limits, cancel)
    }

    /// Prepares a non-thin pack omitting objects the receiver has because it has the roots.
    ///
    /// Like `git rev-list --objects <tips> --not <roots>`, preparation walks commits from the tips
    /// and the locally present roots together in committer-date order. Commits reachable from a
    /// root are marked known, and the walk stops once every queued commit is known (plus a small
    /// allowance for clock skew). The trees of known commits whose children are sent are then
    /// marked known, and the sent commits' trees are walked, skipping known objects. Preparation
    /// cost therefore follows the new history and the boundary trees rather than the whole
    /// repository. Roots may be commits, trees, blobs or tags (peeled); gitlinks stay external.
    ///
    /// An object is omitted only when a mark from a root reaches it through parsed edges, so
    /// omitted objects always lie in a root's closure; dates only order the walk. With skewed
    /// dates, commits the receiver already has can occasionally be sent. A tagged tree or blob
    /// is sent when no commit is sent, even if a root's tree contains it, as Git does. The
    /// receiver's copy of a root's closure is trusted: known objects are not validated and may be
    /// absent locally. Roots missing locally are ignored.
    ///
    /// [`super::send`] requires every root that omitted objects rely on to appear in the live
    /// advertisement as a ref tip or `.have`, independently of command expectations; roots whose
    /// closure omitted nothing are not required. If a root has disappeared, prepare a full
    /// transfer or retry using fresh knowledge. Coordinate with server pruning/GC; advertisements
    /// cannot guarantee object retention against concurrent deletion.
    ///
    /// Sent objects, including their edges, are charged to [`PushLimits::max_edges`] and the pack
    /// count/payload bounds. Reading known history has a separate `max_edges` allowance, and at
    /// most `max_refs` root occurrences are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`Self::new`]'s errors or exhausted receiver-root/exclusion bounds. Missing objects
    /// outside every root's closure fail. A known object read locally with the wrong kind or an
    /// invalid payload also fails. No commands are transmitted.
    pub fn new_excluding(
        objects: &Objects,
        commands: Vec<PushCommand>,
        receiver_roots: &[ObjectId],
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::prepare(objects, commands, receiver_roots, limits, cancel, |_| {})
    }

    /// Prepares a native local push in the source format.
    ///
    /// This has the same selection, force, exclusion and work bounds as
    /// [`Self::new_excluding`]. Git's canonical empty tree is materialized when it is named by a
    /// commit but absent from source storage. Other missing objects still fail. The result can be
    /// passed to [`super::send_local`].
    pub fn new_local(
        objects: &Objects,
        commands: Vec<PushCommand>,
        receiver_roots: &[ObjectId],
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::prepare(objects, commands, receiver_roots, limits, cancel, |_| {})
    }

    /// Prepares a native push while observing completed source reads and pack entries.
    ///
    /// Uses [`Self::new_local`]'s source, force and exclusion contracts. `observe` receives
    /// [`PreparationProgress`] synchronously and must return promptly. It cannot fail; set
    /// `cancel` to stop at the next cooperative check. Completion means prepared buffers only,
    /// before sending or publication. Empty and deletion-only batches have no packing events.
    ///
    /// # Errors
    ///
    /// Returns [`Self::new`]'s preparation failures without a completion notification.
    pub fn new_local_with_progress(
        objects: &Objects,
        commands: Vec<PushCommand>,
        receiver_roots: &[ObjectId],
        limits: PushLimits,
        cancel: &AtomicBool,
        observe: impl FnMut(PreparationProgress),
    ) -> Result<Self, Error> {
        Self::prepare(objects, commands, receiver_roots, limits, cancel, observe)
    }

    fn prepare(
        objects: &Objects,
        commands: Vec<PushCommand>,
        receiver_roots: &[ObjectId],
        limits: PushLimits,
        cancel: &AtomicBool,
        mut observe: impl FnMut(PreparationProgress),
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
                id.require_format(objects.object_format())?;
            }
            let request = encode_commands(
                objects.object_format(),
                &commands,
                &[],
                limits,
                cancel,
                false,
                false,
            )?;
            observe(PreparationProgress::Reading { objects: 0 });
            check_cancelled(cancel)?;
            if commands.iter().all(PushCommand::deletes) {
                return Ok(Self {
                    format: objects.object_format(),
                    commands,
                    request,
                    pack: vec![],
                    index: vec![],
                    checksum: None,
                    limits,
                    objects: 0,
                    receiver_roots: vec![],
                    has_options: false,
                    options: vec![],
                    progress: false,
                });
            }
            let selection = graph::select(
                objects,
                &commands,
                receiver_roots,
                limits,
                cancel,
                |objects| observe(PreparationProgress::Reading { objects }),
            )?;
            let inputs: Vec<_> = selection
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
            let mut index = Vec::new();
            let result = crate::pack::write_controlled_observed(
                objects.object_format(),
                &inputs,
                (&mut pack, &mut index),
                limits.pack,
                limits.compression,
                &mut || {
                    if cancel.load(Ordering::Relaxed) {
                        Err(io::Error::other("push cancelled").into())
                    } else {
                        Ok(())
                    }
                },
                &mut |done, total| {
                    observe(PreparationProgress::Packing {
                        objects: (done, total),
                    })
                },
            );
            check_cancelled(cancel)?;
            let written = result?;
            Ok(Self {
                format: objects.object_format(),
                commands,
                request,
                pack: pack.bytes,
                index,
                checksum: Some(written.checksum),
                limits,
                objects: written.objects,
                receiver_roots: selection.receiver_roots,
                has_options: false,
                options: vec![],
                progress: false,
            })
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        let result = result.and_then(|prepared| {
            check_cancelled(cancel)?;
            observe(PreparationProgress::Complete);
            Ok(prepared)
        });
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

    /// Adds byte-preserving push options to a prepared push.
    ///
    /// The server must advertise `push-options`; otherwise sending refuses the push before any
    /// command. Options are sent after the command flush, in caller order. Native local transport
    /// requires the destination's `receive.advertisePushOptions` and passes the options to its
    /// receive hooks.
    ///
    /// # Errors
    ///
    /// Rejects options without commands, NUL or newline bytes, and packet or request limits.
    pub fn with_push_options(mut self, options: Vec<Vec<u8>>) -> Result<Self, Error> {
        if !options.is_empty() && self.commands.is_empty() {
            return Err(Error::Command("push options require commands"));
        }
        self.request = encode_commands(
            self.format,
            &self.commands,
            &options,
            self.limits,
            &AtomicBool::new(false),
            false,
            false,
        )?;
        self.has_options = !options.is_empty();
        self.options = options;
        Ok(self)
    }

    /// Requests sideband progress when the server advertises it. Progress frames are retained in
    /// the bounded push report; they are never interpreted as reference acknowledgements.
    pub fn with_progress(mut self) -> Self {
        self.progress = true;
        self
    }

    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    pub(super) fn selected_commands(
        &self,
        names: Vec<crate::refs::RefName>,
    ) -> Result<Vec<PushCommand>, Error> {
        let mut selected: HashSet<_> = names.iter().collect();
        if selected.len() != names.len() {
            return Err(Error::Command("duplicate selected destination"));
        }
        let mut commands = Vec::new();
        for command in &self.commands {
            if selected.remove(&command.name) {
                commands.push(command.clone());
            }
        }
        if !selected.is_empty() {
            return Err(Error::Command("selected destination was not prepared"));
        }
        Ok(commands)
    }

    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    pub(super) fn selected_request(
        &self,
        commands: &[PushCommand],
        report_v2: bool,
        sideband: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        let options = if commands.is_empty() {
            &[]
        } else {
            self.options.as_slice()
        };
        encode_commands(
            self.format,
            commands,
            options,
            self.limits,
            cancel,
            report_v2,
            sideband,
        )
    }

    pub(super) fn request_for(
        &self,
        report_v2: bool,
        sideband: bool,
    ) -> Result<Cow<'_, [u8]>, Error> {
        if (!report_v2 && !sideband) || self.commands.is_empty() {
            return Ok(Cow::Borrowed(&self.request));
        }
        encode_commands(
            self.format,
            &self.commands,
            &self.options,
            self.limits,
            &AtomicBool::new(false),
            report_v2,
            sideband,
        )
        .map(Cow::Owned)
    }
}

fn encode_commands(
    format: crate::ObjectFormat,
    commands: &[PushCommand],
    options: &[Vec<u8>],
    limits: PushLimits,
    cancel: &AtomicBool,
    report_v2: bool,
    sideband: bool,
) -> Result<Vec<u8>, Error> {
    if commands.len() > limits.max_commands {
        return Err(Error::Limit("commands"));
    }
    let mut names = HashSet::new();
    let mut request = Vec::new();
    for (i, command) in commands.iter().enumerate() {
        check_cancelled(cancel)?;
        command.new.require_format(format)?;
        if let Some(id) = command.expected {
            id.require_format(format)?;
        }
        let name = command.name.as_bytes();
        if !name.starts_with(b"refs/") {
            return Err(Error::Unsupported("destination namespace"));
        }
        if command.expected.is_some_and(ObjectId::is_null) {
            return Err(Error::Command("zero expected object ID"));
        }
        if command.deletes() && command.expected.is_none() {
            return Err(Error::Command("deletion requires an expected old value"));
        }
        if !names.insert(&command.name) {
            return Err(Error::Command("duplicate destination"));
        }
        let caps = if i == 0 {
            let base = match (format, options.is_empty(), report_v2) {
                (crate::ObjectFormat::Sha1, true, false) => b"\0report-status".as_slice(),
                (crate::ObjectFormat::Sha1, false, false) => {
                    b"\0report-status push-options".as_slice()
                }
                (crate::ObjectFormat::Sha256, true, false) => {
                    b"\0report-status object-format=sha256".as_slice()
                }
                (crate::ObjectFormat::Sha256, false, false) => {
                    b"\0report-status push-options object-format=sha256".as_slice()
                }
                (crate::ObjectFormat::Sha1, true, true) => b"\0report-status-v2".as_slice(),
                (crate::ObjectFormat::Sha1, false, true) => {
                    b"\0report-status-v2 push-options".as_slice()
                }
                (crate::ObjectFormat::Sha256, true, true) => {
                    b"\0report-status-v2 object-format=sha256".as_slice()
                }
                (crate::ObjectFormat::Sha256, false, true) => {
                    b"\0report-status-v2 push-options object-format=sha256".as_slice()
                }
            };
            let mut caps = base.to_vec();
            if sideband {
                caps.extend_from_slice(b" side-band-64k");
            }
            caps
        } else {
            vec![]
        };
        let length = (format.digest_len() * 4 + 2)
            .checked_add(name.len())
            .and_then(|n| n.checked_add(caps.len() + 4))
            .ok_or(Error::Limit("command bytes"))?;
        if length > 65520 || length > limits.max_command_bytes.saturating_sub(request.len()) {
            return Err(Error::Limit("command bytes"));
        }
        let old = command.expected.unwrap_or(ObjectId::null(format));
        let mut line = format!("{old} {} ", command.new).into_bytes();
        line.extend_from_slice(name);
        line.extend_from_slice(&caps);
        packet(&mut request, &line, cancel)?;
    }
    if limits.max_command_bytes.saturating_sub(request.len()) < 4 {
        return Err(Error::Limit("command bytes"));
    }
    put(&mut request, b"0000", cancel)?;
    if !options.is_empty() {
        for option in options {
            if option.iter().any(|b| *b == 0 || *b == b'\n') {
                return Err(Error::Command("invalid push option"));
            }
            if option.len() > 65516
                || option.len() + 4 > limits.max_command_bytes.saturating_sub(request.len())
            {
                return Err(Error::Limit("push option bytes"));
            }
            packet(&mut request, option, cancel)?;
        }
        if limits.max_command_bytes.saturating_sub(request.len()) < 4 {
            return Err(Error::Limit("push option bytes"));
        }
        put(&mut request, b"0000", cancel)?;
    }
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
