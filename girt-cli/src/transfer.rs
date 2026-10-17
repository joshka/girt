//! `girt clone`, `girt fetch` and `girt push`: move objects and refs between repositories.
//!
//! Terminal progress goes to stderr; `--quiet` and `--git` suppress it. Transfer result records
//! on stdout retain girt's format.

use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap_complete::ArgValueCandidates;
use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::clone::{BranchSelection, CloneHead, CloneRequest};
use girt::fetch::{FetchLimits, FetchUpdateLimits};
use girt::push::{ForcePolicy, PushCommand, RejectionOrigin, Status};
use girt::refs::{RefName, Reflog};
use girt::remote::RefSource;
use girt::revision::Resolver;
use girt::transport::TransportControl;
use girt::workflow::{self, Environment, FetchOptions, PushOptions};
use girt::{InitKind, ObjectId, Repository};

use crate::common::{display_bytes, resolve, resolve_ref, stdout};
use crate::config::{Endpoint, block_on, endpoint, find_remote, http_remote};
use crate::{Context, complete};

/// Copy a repository into a new directory and check out its `HEAD`.
///
/// Supports local paths, `file://`, anonymous HTTP(S), and SSH on macOS/Linux.
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
    let source = endpoint(&args.url, context.directory())?;
    let directory = match args.destination {
        Some(directory) => directory,
        None => default_directory(&args.url, args.bare)?,
    };
    let destination = context.directory().join(directory);
    // Record an absolute path for local sources, so later fetches work from the new repository.
    let stored_url = match &source {
        Endpoint::Local(path) => std::fs::canonicalize(path)
            .wrap_err_with(|| format!("reading {}", path.display()))?
            .to_string_lossy()
            .into_owned(),
        Endpoint::Http(url) => url.clone(),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Endpoint::Ssh(url) => url.clone(),
    };
    let kind = if args.bare {
        InitKind::Bare
    } else {
        InitKind::Worktree
    };
    let request = CloneRequest::prepare_tracking(
        &destination,
        kind,
        stored_url.as_bytes(),
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .wrap_err("preparing clone")?;
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let limits = FetchLimits::trusted();
    let progress = std::cell::RefCell::new(crate::progress::Progress::new(context.progress));
    let ready = match source {
        Endpoint::Local(path) => request.receive_local_with_progress(
            &path,
            limits,
            control,
            girt::fetch::LocalFetchObserver::default()
                .with_messages(&mut |bytes| {
                    progress.borrow_mut().remote(bytes);
                    ControlFlow::Continue(())
                })
                .with_construction(&mut |event| progress.borrow_mut().local(event))
                .with_validation(&mut |event| progress.borrow_mut().validation(event)),
        ),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Endpoint::Ssh(url) => {
            let remote = crate::config::ssh_remote(&url, &context.user_config()?)?;
            block_on(request.receive_ssh(&remote, limits, control))?.and_then(|download| {
                download.validate_with_progress(
                    &cancel,
                    |bytes| {
                        progress.borrow_mut().remote(bytes);
                        ControlFlow::Continue(())
                    },
                    |event| progress.borrow_mut().validation(event),
                )
            })
        }
        Endpoint::Http(url) => {
            let remote = http_remote(&url)?;
            block_on(
                request.receive_http_with_progress(&remote, limits, control, |bytes| {
                    progress.borrow_mut().remote(bytes)
                }),
            )?
            .and_then(|download| {
                download.validate_with_progress(
                    &cancel,
                    |_| ControlFlow::Continue(()),
                    |event| progress.borrow_mut().validation(event),
                )
            })
        }
    }
    .wrap_err("downloading")?;
    progress.borrow_mut().finish();
    let report = ready
        .finish(FetchUpdateLimits::trusted(), &cancel)
        .map_err(|error| eyre!("{error} (inspect {})", destination.display()))?;
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
    let has_head = matches!(
        report.head,
        CloneHead::Branch { .. } | CloneHead::Detached(_)
    );
    if !args.bare && !args.no_checkout && has_head {
        check_out_head(&destination, context).wrap_err_with(|| {
            format!(
                "the clone succeeded but checkout failed; retry with `girt -C {} checkout HEAD`",
                destination.display()
            )
        })?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Fills the new clone's empty index and working tree from `HEAD`.
fn check_out_head(destination: &std::path::Path, context: &Context) -> Result<()> {
    let mut progress = crate::progress::Progress::new(context.progress);
    let repository = Repository::open_with_config(destination, &context.config_inputs()?)
        .wrap_err("opening the clone")?;
    let objects = repository.objects()?;
    let tree = crate::common::resolve_tree(&repository, &objects, "HEAD")?;
    repository
        .checkout_tree_with_progress(
            None,
            Some(tree),
            Default::default(),
            &AtomicBool::new(false),
            |done, total| progress.update("Updating files", done, Some(total)),
        )
        .map_err(|failure| eyre!("{failure}"))?;
    progress.finish();
    Ok(())
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
