//! Receive pushes from Git clients, as `git receive-pack` does.

mod pack_stream;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use super::ServeError;
use crate::config::ConfigInputs;
use crate::hide_refs::HideRules;
use crate::packet::{self, Frame, Wire};
use crate::push::{
    ForcePolicy, PushCommand, PushError, PushFailure, PushLimits, PushReport, Status,
};
use crate::refs::{RefName, Target};
use crate::transport::TransportControl;
use crate::{ObjectFormat, ObjectId, Repository, Signature};

/// Receives pushes into one repository, as `git receive-pack` does.
///
/// Updates follow the repository's receive policy: `receive.denyCurrentBranch`,
/// `receive.denyDeletes`, `receive.denyNonFastForwards`, `receive.hideRefs` and
/// `transfer.hideRefs`. With `receive.denyCurrentBranch=updateInstead` on macOS and Linux, a clean
/// checked-out working tree is updated to the pushed commit. An atomic push updates every
/// reference or none, and reports `atomic push failure` for the others when one is rejected, as
/// Git does.
///
/// The pushed pack is validated before any reference changes. When a `pre-receive` hook exists,
/// the objects stay in quarantine until it accepts them; `update`, `post-receive` and
/// `post-update` hooks run as Git runs them.
///
/// # Examples
///
/// Advertise an empty repository's references to a client:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::config::ConfigInputs;
/// use girt::serve::ReceivePack;
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("repo.git");
/// Repository::init(ObjectFormat::Sha1, &path, InitKind::Bare)?;
/// let receive = ReceivePack::new(path, ConfigInputs::default());
///
/// let mut advertisement = Vec::new();
/// receive.advertise(&mut advertisement, &AtomicBool::new(false))?;
/// assert!(advertisement.starts_with(b"00"));
/// assert!(advertisement.windows(15).any(|w| w == b"capabilities^{}"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct ReceivePack {
    path: PathBuf,
    config_inputs: ConfigInputs,
    limits: PushLimits,
    identity: Option<Signature>,
}

/// What the client asked for alongside its commands.
#[derive(Debug, Default)]
struct Request {
    commands: Vec<PushCommand>,
    report_status: bool,
    sideband: bool,
    push_options: bool,
    atomic: bool,
}

impl ReceivePack {
    /// Prepares to receive into the repository at `path`, read with `config_inputs`.
    ///
    /// The repository is opened for each session, so configuration changes apply to the next
    /// push.
    pub fn new(path: impl Into<PathBuf>, config_inputs: ConfigInputs) -> Self {
        Self {
            path: path.into(),
            config_inputs,
            limits: PushLimits::default(),
            identity: None,
        }
    }

    /// Sets the identity recorded in reflog entries for received updates, as Git records the
    /// receiving process's committer.
    ///
    /// Without one, a push to a repository that keeps reflogs is refused.
    #[must_use]
    pub fn with_identity(mut self, identity: Signature) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Sets the bounds on commands, status, the received pack and the receiving repository's
    /// object store.
    #[must_use]
    pub fn with_limits(mut self, limits: PushLimits) -> Self {
        self.limits = limits;
        self
    }

    fn open(&self) -> Result<Repository, ServeError> {
        Repository::open_with_config(&self.path, &self.config_inputs)
            .map_err(|error| ServeError::Open(Box::new(error)))
    }

    /// Serves one push over async streams, as [`Self::serve`] does, running the work on
    /// `executor`.
    ///
    /// Dropping the future, or failing to write `output`, cancels the session; see
    /// [`run_blocking`](super::run_blocking).
    ///
    /// # Errors
    ///
    /// Returns the failure that ended the session, and [`ServeError::Executor`] when the
    /// executor dropped the work.
    #[cfg(feature = "serve-async")]
    pub async fn serve_async(
        &self,
        executor: impl super::BlockingExecutor,
        input: impl tokio::io::AsyncRead + Unpin,
        output: impl tokio::io::AsyncWrite + Unpin,
    ) -> Result<PushReport, ServeError> {
        let receive = self.clone();
        super::run_blocking(executor, input, output, move |input, output, cancel| {
            receive.serve(input, output, cancel)
        })
        .await
    }

    /// Serves one push on a full-duplex stream such as stdin and stdout under SSH: advertises
    /// references, reads commands and the pack, applies them and reports each reference's result.
    ///
    /// Returns the report of the applied push, which is empty when the client only listed
    /// references.
    ///
    /// # Errors
    ///
    /// Returns the failure that ended the session. Rejected references aren't errors; they are
    /// in the report and were reported to the client.
    pub fn serve(
        &self,
        input: &mut impl Read,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<PushReport, ServeError> {
        self.advertise(output, cancel)?;
        self.respond(input, output, cancel)
    }

    /// Writes the reference advertisement that starts every session, with capabilities after the
    /// first reference.
    ///
    /// # Errors
    ///
    /// Returns failures to open the repository, read its references or write.
    pub fn advertise(
        &self,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        let repository = self.open()?;
        let hide = HideRules::parse(&repository, "receive").map_err(ServeError::Config)?;
        let refs = repository.references()?;
        let mut capabilities =
            b"report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta"
                .to_vec();
        if config_boolean(&repository, "advertisepushoptions")? {
            capabilities.extend_from_slice(b" push-options");
        }
        capabilities.extend_from_slice(
            format!(
                " object-format={} agent=girt/{}",
                repository.object_format(),
                env!("CARGO_PKG_VERSION")
            )
            .as_bytes(),
        );
        let mut listed = Vec::new();
        for reference in refs.list()? {
            if hide.matches(&reference.name) {
                continue;
            }
            let id = match reference.target {
                Target::Direct(id) => id,
                Target::Symbolic(_) => match refs.resolve(&reference.name)?.id {
                    Some(id) => id,
                    None => continue,
                },
            };
            listed.push((id, reference.name.as_bytes().to_vec()));
        }
        if listed.is_empty() {
            let null = ObjectId::null(repository.object_format());
            listed.push((null, b"capabilities^{}".to_vec()));
        }
        for (index, (id, name)) in listed.into_iter().enumerate() {
            let mut bytes = format!("{id} ").into_bytes();
            bytes.extend_from_slice(&name);
            if index == 0 {
                bytes.push(0);
                bytes.extend_from_slice(&capabilities);
            }
            bytes.push(b'\n');
            packet::packet(output, &bytes, cancel)?;
        }
        packet::put(output, b"0000", cancel)?;
        output.flush()?;
        Ok(())
    }

    /// Reads one push after the advertisement, applies it and reports the results, as a stateless
    /// transport does.
    ///
    /// # Errors
    ///
    /// As [`Self::serve`].
    pub fn respond(
        &self,
        input: &mut impl Read,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<PushReport, ServeError> {
        let repository = self.open()?;
        let format = repository.object_format();
        let mut wire = Wire {
            reader: &mut *input,
            remaining: self.limits.max_command_bytes,
            cancel,
        };
        let Some(request) = self.read_commands(&mut wire, format)? else {
            return Ok(PushReport::pending(&[]));
        };
        let options = if request.push_options {
            let mut options = Vec::new();
            loop {
                match wire.frame()? {
                    Frame::Flush => break,
                    Frame::Data(line) => {
                        if options.len() == self.limits.max_commands {
                            return Err(ServeError::Limit("push options"));
                        }
                        options.push(trim(&line).to_vec());
                    }
                    Frame::Delim | Frame::ResponseEnd => {
                        return Err(ServeError::Protocol("unexpected special packet"));
                    }
                }
            }
            Some(options)
        } else {
            None
        };
        let pack = if request.commands.iter().all(PushCommand::deletes) {
            crate::fetch::Bytes::Memory(Vec::new())
        } else {
            let bounds = pack_stream::Bounds {
                max_pack_bytes: self.limits.pack.max_pack_bytes,
                max_objects: self.limits.pack.max_objects,
                max_object_bytes: self.limits.pack.max_object_bytes,
                max_input_bytes: self.limits.pack.max_input_bytes,
            };
            match pack_stream::read_pack(input, format, &bounds, cancel) {
                Ok(pack) => pack,
                // Like Git, a pack that can't be read is reported to the client as an unpack error.
                Err(error @ (ServeError::Protocol(_) | ServeError::Limit(_))) => {
                    let report = unpack_failed(&request.commands, error.to_string());
                    if request.report_status {
                        write_status(&report, request.sideband, output, cancel)?;
                    }
                    output.flush()?;
                    return Ok(report);
                }
                Err(error) => return Err(error),
            }
        };
        let result = crate::push::local_native::receive(
            &self.path,
            &self.config_inputs,
            format,
            request.commands.clone(),
            pack,
            options,
            self.limits,
            self.identity.as_ref(),
            request.atomic,
            TransportControl::new(cancel),
        );
        let report = match result {
            Ok(report) => report,
            Err(PushError::Uncertain { report, .. }) => *report,
            Err(PushError::NotSent(cause)) => refused(&request.commands, &cause),
        };
        if request.report_status {
            write_status(&report, request.sideband, output, cancel)?;
        } else if request.sideband {
            packet::put(output, b"0000", cancel)?;
        }
        output.flush()?;
        Ok(report)
    }

    /// Reads the command list; `None` when the client sent only a flush or closed the stream.
    fn read_commands<R: Read>(
        &self,
        wire: &mut Wire<'_, R>,
        format: ObjectFormat,
    ) -> Result<Option<Request>, ServeError> {
        let mut request = Request::default();
        loop {
            let line = match wire.frame_or_end()? {
                None | Some(Frame::Flush) => break,
                Some(Frame::Data(line)) => line,
                Some(Frame::Delim | Frame::ResponseEnd) => {
                    return Err(ServeError::Protocol("unexpected special packet"));
                }
            };
            let (command, capabilities) = match line.iter().position(|&byte| byte == 0) {
                Some(nul) => line.split_at(nul),
                None => (line.as_slice(), &b""[..]),
            };
            if request.commands.is_empty() {
                for capability in
                    trim(capabilities.get(1..).unwrap_or_default()).split(|&b| b == b' ')
                {
                    match capability {
                        b"report-status" | b"report-status-v2" => request.report_status = true,
                        b"side-band-64k" => request.sideband = true,
                        b"push-options" => request.push_options = true,
                        b"atomic" => request.atomic = true,
                        _ => {}
                    }
                }
            }
            if request.commands.len() == self.limits.max_commands {
                return Err(ServeError::Limit("commands"));
            }
            request.commands.push(parse_command(trim(command), format)?);
        }
        Ok((!request.commands.is_empty()).then_some(request))
    }
}

/// Parses `<old> <new> <name>`.
fn parse_command(line: &[u8], format: ObjectFormat) -> Result<PushCommand, ServeError> {
    let mut fields = line.splitn(3, |&byte| byte == b' ');
    let mut id = || -> Result<ObjectId, ServeError> {
        fields
            .next()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| ObjectId::from_hex(format, hex).ok())
            .ok_or(ServeError::Protocol("push command"))
    };
    let old = id()?;
    let new = id()?;
    let name = fields
        .next()
        .and_then(|name| RefName::new(name).ok())
        .ok_or(ServeError::Protocol("push command reference name"))?;
    let expected = (!old.is_null()).then_some(old);
    Ok(PushCommand::new(name, expected, new).with_force(ForcePolicy::Allow))
}

/// A report for a pack that couldn't be read.
fn unpack_failed(commands: &[PushCommand], reason: String) -> PushReport {
    let mut report = PushReport::pending(commands);
    report.unpack = Some(Status::Rejected(reason.into_bytes()));
    for reference in &mut report.refs {
        reference.status = Some(Status::Rejected(b"unpacker error".to_vec()));
    }
    report
}

/// A report for a push refused before any reference changed.
fn refused(commands: &[PushCommand], cause: &PushFailure) -> PushReport {
    let mut report = PushReport::pending(commands);
    let unpack_failed = matches!(cause, PushFailure::Install(_));
    report.unpack = Some(if unpack_failed {
        Status::Rejected(cause.to_string().into_bytes())
    } else {
        Status::Ok
    });
    let reason = if unpack_failed {
        b"unpacker error".to_vec()
    } else {
        cause.to_string().into_bytes()
    };
    for reference in &mut report.refs {
        reference.status = Some(Status::Rejected(reason.clone()));
    }
    report
}

/// Writes `report-status`: the unpack result, then `ok` or `ng` for each reference, inside
/// sideband 1 when the client asked for it.
fn write_status(
    report: &PushReport,
    sideband: bool,
    output: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<(), ServeError> {
    let mut status = Vec::new();
    let mut line = |bytes: &[u8]| -> Result<(), ServeError> {
        packet::packet(&mut status, bytes, cancel)?;
        Ok(())
    };
    match &report.unpack {
        Some(Status::Rejected(reason)) => line(&[b"unpack ", reason.as_slice(), b"\n"].concat())?,
        _ => line(b"unpack ok\n")?,
    }
    for reference in &report.refs {
        let name = reference.command.name.as_bytes();
        match &reference.status {
            Some(Status::Ok) => line(&[b"ok ", name, b"\n"].concat())?,
            Some(Status::Rejected(reason)) => {
                line(&[b"ng ", name, b" ", reason.as_slice(), b"\n"].concat())?;
            }
            None => line(&[b"ng ", name, b" not attempted\n"].concat())?,
        }
    }
    status.extend_from_slice(b"0000");
    if !sideband {
        packet::put(output, &status, cancel)?;
        return Ok(());
    }
    for message in report.progress.iter().chain(&report.warnings) {
        for chunk in message.chunks(65515) {
            packet::packet(output, &[&[2u8][..], chunk].concat(), cancel)?;
        }
    }
    for chunk in status.chunks(65515) {
        packet::packet(output, &[&[1u8][..], chunk].concat(), cancel)?;
    }
    packet::put(output, b"0000", cancel)?;
    Ok(())
}

/// Reads a `receive` boolean; an absent key is false.
fn config_boolean(repository: &Repository, name: &str) -> Result<bool, ServeError> {
    match repository.config().value("receive", None, name) {
        None => Ok(false),
        Some(None) => Ok(true),
        Some(Some(value)) => {
            crate::config::boolean(Some(value)).ok_or(ServeError::Config("receive boolean"))
        }
    }
}

/// Strips one trailing newline.
fn trim(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}
