//! `girt clone`, `girt fetch` and `girt push`: move objects and refs between repositories.
//!
//! Terminal progress goes to stderr; `--quiet` and `--git` suppress it. Transfer result records
//! on stdout retain girt's format.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap_complete::ArgValueCandidates;
use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::clone::CloneHead;
use girt::push::{ForcePolicy, PushCommand, RejectionOrigin, Status};
use girt::refs::RefName;
use girt::remote::RefSource;
use girt::revision::Resolver;
use girt::serve::{Protocol, ReceivePack, UploadLimits, UploadPack};
use girt::workflow::{self, Environment, FetchOptions, PushOptions};
use girt::{InitKind, ObjectId, Repository};

use crate::common::{display_bytes, resolve, resolve_ref, stdout};
use crate::config::find_remote;
use crate::{Context, complete};

/// Copy a repository into a new directory and check out its `HEAD`.
///
/// Supports local paths, `file://`, configured HTTP(S) credentials, and SSH on macOS/Linux.
/// SSH accepts `ssh://` and `[user@]host:path`, normal OpenSSH config and configured commands.
/// Use an agent or askpass for authentication; terminal prompts are unavailable.
/// `--git` suppresses progress but retains girt's result format.
#[derive(Debug, clap::Args)]
pub struct CloneArgs {
    /// Repository to clone.
    url: String,
    /// Destination directory (default: the URL's last component, without `.git`).
    #[arg(value_name = "DIRECTORY")]
    destination: Option<PathBuf>,
    /// Create a bare repository.
    #[arg(long)]
    bare: bool,
    /// Leave the working tree and index empty (`girt checkout HEAD` fills them later).
    #[arg(short, long)]
    no_checkout: bool,
}

/// Fetch a remote's configured refspecs, printing each updated ref as `<old>..<new> <ref>`.
///
/// New refs print the null ID as `<old>`, pruned refs the null ID as `<new>`. Tags are not
/// followed automatically. Local, HTTP(S) credential helpers and system OpenSSH remotes are
/// supported. Interactive credential and SSH terminal prompts are unavailable. `--git` suppresses
/// progress but retains girt's result format.
#[derive(Debug, clap::Args)]
pub struct FetchArgs {
    /// Configured remote name.
    #[arg(default_value = "origin", add = ArgValueCandidates::new(complete::remotes))]
    remote: String,
}

/// Receive pushes into a repository.
#[derive(Debug, clap::Args)]
pub struct ReceivePackArgs {
    /// Repository to receive into.
    directory: PathBuf,
}

/// Serve one receive-pack session on stdin and stdout.
pub fn receive_pack(args: ReceivePackArgs, context: &Context) -> Result<ExitCode> {
    // The receiving repository is trusted; the client's commands and pack are bounded.
    let defaults = girt::push::PushLimits::default();
    let mut limits = girt::push::PushLimits::trusted()
        .with_max_commands(defaults.max_commands)
        .with_max_command_bytes(defaults.max_command_bytes);
    limits.pack = defaults.pack;
    let mut receive =
        ReceivePack::new(&args.directory, context.config_inputs()?).with_limits(limits);
    // Like Git, reflog entries record the receiving process's committer, when it has one.
    let repository = Repository::open_with_config(&args.directory, &context.config_inputs()?)
        .wrap_err_with(|| format!("opening {}", args.directory.display()))?;
    if let Ok(identity) = girt::Signature::committer(repository.config(), &context.environment) {
        receive = receive.with_identity(identity);
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    receive
        .serve(
            &mut std::io::stdin().lock(),
            &mut std::io::BufWriter::new(std::io::stdout().lock()),
            &cancel,
        )
        .wrap_err("serving receive-pack")?;
    Ok(ExitCode::SUCCESS)
}

/// Serve fetches from a repository.
#[derive(Debug, clap::Args)]
pub struct UploadPackArgs {
    /// Repository to serve.
    directory: PathBuf,
}

/// Serve one upload-pack session on stdin and stdout.
pub fn upload_pack(args: UploadPackArgs, context: &Context) -> Result<ExitCode> {
    let repository = Repository::open_with_config(&args.directory, &context.config_inputs()?)
        .wrap_err_with(|| format!("opening {}", args.directory.display()))?;
    let protocol = context
        .environment
        .get("GIT_PROTOCOL")
        .map_or(Protocol::V0, |value| {
            Protocol::from_parameters(value.as_encoded_bytes())
        });
    // The served repository is trusted; the client's request isn't.
    let request = UploadLimits::default();
    let limits = UploadLimits::trusted()
        .with_max_request_bytes(request.max_request_bytes)
        .with_max_wants(request.max_wants)
        .with_max_haves(request.max_haves);
    let upload = UploadPack::new(&repository)?.with_limits(limits);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    upload
        .serve(
            protocol,
            &mut std::io::stdin().lock(),
            &mut std::io::BufWriter::new(std::io::stdout().lock()),
            &cancel,
        )
        .wrap_err("serving upload-pack")?;
    Ok(ExitCode::SUCCESS)
}

/// Push refs to a remote, printing `<status>\t<ref>[\t<reason>]` per ref.
///
/// Each refspec is `[+]<src>[:<dst>]`; an empty `<src>` deletes `<dst>`. The remote must still have
/// the value recorded by the matching remote-tracking ref (or not have the ref at all if there is
/// none), like `git push --force-with-lease`; fetch first if the push is rejected as stale. `+`
/// permits non-fast-forward updates. Local, HTTP(S) credential helpers and system OpenSSH remotes
/// are supported. Interactive credential and SSH terminal prompts are unavailable. `--git`
/// suppresses progress but retains girt's result format.
#[derive(Debug, clap::Args)]
pub struct PushArgs {
    /// Configured remote name.
    #[arg(add = ArgValueCandidates::new(complete::remotes))]
    remote: String,
    /// Refspecs to push.
    #[arg(required = true)]
    refspecs: Vec<String>,
}

pub fn clone(args: CloneArgs, context: &Context) -> Result<ExitCode> {
    let directory = match args.destination {
        Some(directory) => directory,
        None => default_directory(&args.url, args.bare)?,
    };
    let destination = std::fs::canonicalize(context.directory())
        .wrap_err_with(|| format!("reading {}", context.directory().display()))?
        .join(directory);
    let options = workflow::CloneOptions::default()
        .with_kind(if args.bare {
            InitKind::Bare
        } else {
            InitKind::Worktree
        })
        .with_checkout(!args.no_checkout)
        .with_fetch_limits(girt::fetch::FetchLimits::trusted())
        .with_update_limits(girt::fetch::FetchUpdateLimits::trusted());
    let mut progress = crate::progress::Progress::new(context.progress);
    let outcome = workflow::clone_with(
        &args.url,
        &destination,
        &options,
        &context.config_inputs()?,
        &environment(context)?,
        &mut progress,
    )
    .map_err(|error| match &error {
        workflow::CloneError::Checkout { .. } | workflow::CloneError::Read { .. } =>
            eyre!(error).wrap_err(format!(
                "clone completed at {}; inspect files and index before recovery with `girt -C {} checkout HEAD`",
                destination.display(), destination.display(),
            )),
        workflow::CloneError::Finish(_) => eyre!(error).wrap_err(format!(
            "clone installation failed; inspect {} for partial effects", destination.display()
        )),
        _ => eyre!(error).wrap_err("cloning"),
    })?;
    progress.finish();
    let report = outcome.clone;
    let mut out = stdout();
    writeln!(out, "cloned into {}", destination.display())?;
    match &report.head {
        CloneHead::Branch { name, id } => {
            writeln!(out, "HEAD -> {} {id}", display_bytes(name.as_bytes()))?;
        }
        CloneHead::Detached(id) => writeln!(out, "HEAD {id} (detached)")?,
        CloneHead::Unborn(name) => {
            writeln!(out, "HEAD -> {} (unborn)", display_bytes(name.as_bytes()))?;
        }
        other => writeln!(out, "HEAD {other:?}")?,
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Git's default clone directory: the last path component, without `.git` unless bare.
fn default_directory(url: &str, bare: bool) -> Result<PathBuf> {
    let trimmed = url.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/.git").unwrap_or(trimmed);
    let last = trimmed
        .rsplit(['/', ':'])
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| eyre!("cannot derive a directory name from `{url}`"))?;
    let stem = last.strip_suffix(".git").unwrap_or(last);
    Ok(PathBuf::from(if bare {
        format!("{stem}.git")
    } else {
        stem.to_owned()
    }))
}

/// Environment for workflow transfers, resolving relative remote paths against `-C`.
fn environment(context: &Context) -> Result<Environment> {
    let directory = std::fs::canonicalize(context.directory())
        .wrap_err_with(|| format!("reading {}", context.directory().display()))?;
    Ok(Environment::from_process().with_current_dir(directory))
}

pub fn fetch(args: FetchArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let remote = find_remote(&repository, &args.remote)?;
    let refspecs: Vec<Vec<u8>> = remote
        .fetch_refspecs()
        .specs()
        .iter()
        .map(|spec| spec.as_bytes().to_vec())
        .collect();
    if refspecs.is_empty() {
        bail!("remote {} has no fetch refspecs", args.remote);
    }
    let mut progress = crate::progress::Progress::new(context.progress);
    let outcome = workflow::fetch(
        &repository,
        &args.remote,
        &FetchOptions::new(refspecs),
        &environment(context)?,
        &mut progress,
    )
    .wrap_err_with(|| format!("fetching {}", args.remote))?;
    progress.finish();
    let null = ObjectId::null(repository.object_format());
    let mut out = stdout();
    for change in &outcome.updated {
        let old = change.old.unwrap_or(null);
        let new = change.new.unwrap_or(null);
        write!(out, "{old}..{new} ")?;
        out.write_all(change.name.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    for source in &outcome.missing_sources {
        eprintln!("warning: remote has no {}", display_bytes(source));
    }
    Ok(ExitCode::SUCCESS)
}

pub fn push(args: PushArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let remote = find_remote(&repository, &args.remote)?;
    let mut commands = Vec::new();
    for refspec in &args.refspecs {
        commands.push(
            push_command(&repository, &remote, refspec)
                .wrap_err_with(|| format!("planning {refspec}"))?,
        );
    }
    let mut progress = crate::progress::Progress::new(context.progress);
    let report = workflow::push(
        &repository,
        &args.remote,
        commands,
        &PushOptions::default(),
        &environment(context)?,
        &mut progress,
    )
    .wrap_err_with(|| format!("pushing to {}", args.remote))?;
    progress.finish();
    let mut out = stdout();
    for reference in &report.refs {
        let name = display_bytes(reference.command.name.as_bytes());
        match &reference.status {
            Some(Status::Ok) => writeln!(out, "ok\t{name}")?,
            Some(Status::Rejected(reason)) => {
                let stale = reference.rejection_origin == Some(RejectionOrigin::ExpectedValue);
                let hint = if stale { " (stale; fetch first)" } else { "" };
                writeln!(out, "rejected\t{name}\t{}{hint}", display_bytes(reason))?;
            }
            Some(other) => writeln!(out, "{other:?}\t{name}")?,
            None => writeln!(out, "not-attempted\t{name}")?,
        }
    }
    out.flush()?;
    Ok(if report.all_succeeded() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Builds one push command from `[+]<src>[:<dst>]`, leasing on the remote-tracking ref.
fn push_command(
    repository: &Repository,
    remote: &girt::remote::Remote,
    refspec: &str,
) -> Result<PushCommand> {
    let (force, spec) = match refspec.strip_prefix('+') {
        Some(spec) => (ForcePolicy::Allow, spec),
        None => (ForcePolicy::FastForwardOnly, refspec),
    };
    let (source, destination) = match spec.split_once(':') {
        Some((source, destination)) => (source, Some(destination)),
        None => (spec, None),
    };
    let objects = repository.objects()?;
    let source_ref = if source.is_empty() {
        None
    } else {
        Resolver::new(repository, &objects)?
            .expand_ref(source)?
            .map(|(name, _)| name)
    };
    let new = if source.is_empty() {
        ObjectId::null(repository.object_format())
    } else {
        resolve(repository, source)?
    };
    let destination = match (destination, &source_ref) {
        (Some(name), _) if name.starts_with("refs/") => name.to_owned(),
        (Some(name), _) => format!("refs/heads/{name}"),
        (None, Some(name)) if name.as_bytes() != b"HEAD" => {
            display_bytes(name.as_bytes()).into_owned()
        }
        _ => bail!("cannot infer a destination for `{refspec}`; use <src>:<dst>"),
    };
    let name = RefName::new(&destination).wrap_err_with(|| format!("invalid ref {destination}"))?;
    let expected = tracking_value(repository, remote, &name)?;
    Ok(PushCommand::new(name, expected, new).with_force(force))
}

/// The local remote-tracking value for a remote ref, found through the fetch refspecs.
fn tracking_value(
    repository: &Repository,
    remote: &girt::remote::Remote,
    name: &RefName,
) -> Result<Option<ObjectId>> {
    // Mapping needs a nonzero ID; only the destination names matter here.
    let probe = [RefSource {
        name: name.clone(),
        id: ObjectId::for_blob(repository.object_format(), b""),
    }];
    let mappings = remote
        .fetch_refspecs()
        .map(&probe)
        .wrap_err("mapping fetch refspecs")?;
    let references = repository.references().wrap_err("opening references")?;
    for tracking in mappings
        .iter()
        .filter_map(|mapping| mapping.destination.as_ref())
    {
        if let Some(id) = resolve_ref(&references, tracking)? {
            return Ok(Some(id));
        }
    }
    Ok(None)
}
