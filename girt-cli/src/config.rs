//! `girt config` and `girt remote`: read and edit configuration, manage and query remotes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap_complete::ArgValueCandidates;
use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::Repository;
use girt::config::Origin;
use girt::fetch::{AdvertisementKind, FetchLimits, RemoteDiscovery};
use girt::remote::{Remote, RemoteConfig};
use girt::transport::TransportControl;

use crate::common::{display_bytes, stdout, write_line};
use crate::{Context, complete};

/// Largest local config file `set`/`unset` will edit.
const MAX_CONFIG_BYTES: usize = 16 << 20;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the effective (last) value of a key (≈ `git config --get`).
    ///
    /// Exits with status 1 when the key is not set.
    Get {
        /// Key as `section.name` or `section.subsection.name`.
        #[arg(add = ArgValueCandidates::new(complete::config_keys))]
        key: String,
    },
    /// List every effective entry as `key=value`, lowest precedence first (≈ `git config --list`).
    ///
    /// Section and variable names are lowercased; subsections keep their case. A valueless
    /// (implicit true) entry prints the key alone.
    List {
        /// Prefix each entry with its origin (`file:<path>` or `command line:`) and a tab.
        #[arg(long)]
        show_origin: bool,
    },
    /// Set a key in the repository's local config file, replacing its last local occurrence.
    Set {
        /// Key as `section.name` or `section.subsection.name`.
        #[arg(add = ArgValueCandidates::new(complete::config_keys))]
        key: String,
        /// New value.
        value: String,
    },
    /// Remove every occurrence of a key from the repository's local config file.
    Unset {
        /// Key as `section.name` or `section.subsection.name`.
        #[arg(add = ArgValueCandidates::new(complete::config_keys))]
        key: String,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum RemoteCommand {
    /// List configured remotes, one name per line (≈ `git remote [-v]`; `--urls` is `-v`).
    List {
        /// Also print fetch and push URLs (`<name>\t<url> (fetch|push)`).
        #[arg(short, long)]
        urls: bool,
    },
    /// Add a remote that fetches every branch into `refs/remotes/<name>/`.
    Add {
        /// Remote name.
        name: String,
        /// Fetch (and push) URL.
        url: String,
    },
    /// Show a remote's URLs and refspecs.
    Show {
        /// Remote name.
        #[arg(add = ArgValueCandidates::new(complete::remotes))]
        name: String,
    },
    /// List the references a remote advertises as `<id>\t<ref>` (≈ `git ls-remote`).
    ///
    /// Accepts a configured remote name, a local path, a `file://` URL or an `http(s)://` URL
    /// (anonymous access only), or SSH (`ssh://` and `[user@]host:path`) on macOS/Linux.
    /// SSH uses normal OpenSSH config and configured commands; terminal prompts are unavailable.
    /// Peeled annotated tags print as
    /// `<ref>^{}` lines after their tag.
    Heads {
        /// Remote name or URL.
        #[arg(add = ArgValueCandidates::new(complete::remotes))]
        remote: String,
    },
}

pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    match command {
        Command::Get { key } => get(&key, context),
        Command::List { show_origin } => list(show_origin, context),
        Command::Set { key, value } => set(&key, &value, context),
        Command::Unset { key } => unset(&key, context),
    }
}

pub fn remote(command: RemoteCommand, context: &Context) -> Result<ExitCode> {
    match command {
        RemoteCommand::List { urls } => remote_list(urls, context),
        RemoteCommand::Add { name, url } => remote_add(&name, &url, context),
        RemoteCommand::Show { name } => remote_show(&name, context),
        RemoteCommand::Heads { remote } => remote_heads(&remote, context),
    }
}

/// Splits `section[.subsection].name` the way Git does: subsection is everything between the
/// first and last dots.
fn split_key(key: &str) -> Result<(&str, Option<&str>, &str)> {
    let (section, rest) = key
        .split_once('.')
        .ok_or_else(|| eyre!("key `{key}` does not contain a section"))?;
    let (subsection, name) = match rest.rsplit_once('.') {
        Some((subsection, name)) => (Some(subsection), name),
        None => (None, rest),
    };
    if section.is_empty() || name.is_empty() {
        bail!("invalid key `{key}`");
    }
    Ok((section, subsection, name))
}

fn get(key: &str, context: &Context) -> Result<ExitCode> {
    let (section, subsection, name) = split_key(key)?;
    let repository = context.repository()?;
    let value = repository
        .config()
        .value(section, subsection.map(str::as_bytes), name);
    let Some(value) = value else {
        return Ok(ExitCode::from(1));
    };
    let mut out = stdout();
    // Git prints an implicit boolean as `true` under `--get`.
    write_line(&mut out, value.unwrap_or(b"true"))?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn list(show_origin: bool, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let base = repository.worktree().unwrap_or(repository.git_dir());
    let mut out = stdout();
    for entry in repository.config().entries() {
        if show_origin {
            write_origin(&mut out, entry.origin.as_ref(), base)?;
            out.write_all(b"\t")?;
        }
        out.write_all(&entry.section.to_ascii_lowercase())?;
        if let Some(subsection) = &entry.subsection {
            out.write_all(b".")?;
            out.write_all(subsection)?;
        }
        out.write_all(b".")?;
        out.write_all(&entry.name.to_ascii_lowercase())?;
        if let Some(value) = &entry.value {
            out.write_all(b"=")?;
            out.write_all(value)?;
        }
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Writes Git's `--show-origin` label; files inside the repository are shown relative to it.
fn write_origin(out: &mut impl Write, origin: Option<&Origin>, base: &Path) -> Result<()> {
    let Some(origin) = origin else {
        return Ok(out.write_all(b"command line:")?);
    };
    match &origin.location.path {
        Some(path) => {
            let shown = path.strip_prefix(base).unwrap_or(path);
            write!(out, "file:{}", shown.display())?;
        }
        None => out.write_all(b"command line:")?,
    }
    Ok(())
}

fn set(key: &str, value: &str, context: &Context) -> Result<ExitCode> {
    let (section, subsection, name) = split_key(key)?;
    let subsection = subsection.map(str::as_bytes);
    let repository = context.repository()?;
    let mut edit = repository
        .edit_config(MAX_CONFIG_BYTES)
        .wrap_err("opening local config")?;
    let document = edit.document_mut();
    let last = last_occurrence(document.config(), section, subsection, name);
    match last {
        Some(index) => document.set_value(index, value.as_bytes()),
        None => document.append(section, subsection, name, value.as_bytes()),
    }
    .wrap_err_with(|| format!("setting {key}"))?;
    edit.commit().wrap_err("writing local config")?;
    Ok(ExitCode::SUCCESS)
}

fn unset(key: &str, context: &Context) -> Result<ExitCode> {
    let (section, subsection, name) = split_key(key)?;
    let subsection = subsection.map(str::as_bytes);
    let repository = context.repository()?;
    let mut edit = repository
        .edit_config(MAX_CONFIG_BYTES)
        .wrap_err("opening local config")?;
    let document = edit.document_mut();
    let mut removed = false;
    // Remove from the end so earlier occurrence indexes stay valid.
    while let Some(index) = last_occurrence(document.config(), section, subsection, name) {
        document
            .remove(index)
            .wrap_err_with(|| format!("unsetting {key}"))?;
        removed = true;
    }
    if !removed {
        edit.abort().wrap_err("releasing local config")?;
        // Git exits 5 when there is nothing to unset.
        return Ok(ExitCode::from(5));
    }
    edit.commit().wrap_err("writing local config")?;
    Ok(ExitCode::SUCCESS)
}

/// Index of the last entry matching a key (section and name compared case-insensitively).
fn last_occurrence(
    config: &girt::Config,
    section: &str,
    subsection: Option<&[u8]>,
    name: &str,
) -> Option<usize> {
    config.entries().iter().rposition(|entry| {
        entry.section.eq_ignore_ascii_case(section.as_bytes())
            && entry.subsection.as_deref() == subsection
            && entry.name.eq_ignore_ascii_case(name.as_bytes())
    })
}

/// Finds a configured remote by name.
pub fn find_remote(repository: &Repository, name: &str) -> Result<Remote> {
    Remote::find(repository.config(), name.as_bytes())
        .wrap_err_with(|| format!("reading remote {name}"))?
        .ok_or_else(|| eyre!("no such remote `{name}`"))
}

/// Push URLs as Git reports them: explicit `pushurl`s, else the fetch URLs.
fn push_urls(remote: &Remote) -> &[Vec<u8>] {
    match remote.configured_push_urls() {
        [] => remote.urls(),
        urls => urls,
    }
}

fn remote_list(urls: bool, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let mut out = stdout();
    let mut names = Remote::names(repository.config());
    names.sort_unstable();
    for name in names {
        if !urls {
            write_line(&mut out, name)?;
            continue;
        }
        let remote = Remote::find(repository.config(), name)?
            .ok_or_else(|| eyre!("remote {} vanished", display_bytes(name)))?;
        for url in remote.fetch_url().into_iter() {
            write_url(&mut out, name, url, "fetch")?;
        }
        for url in push_urls(&remote) {
            write_url(&mut out, name, url, "push")?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn write_url(out: &mut impl Write, name: &[u8], url: &[u8], kind: &str) -> Result<()> {
    out.write_all(name)?;
    out.write_all(b"\t")?;
    out.write_all(url)?;
    writeln!(out, " ({kind})")?;
    Ok(())
}

fn remote_add(name: &str, url: &str, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let mut edit = repository
        .edit_config(MAX_CONFIG_BYTES)
        .wrap_err("opening local config")?;
    let fetch = format!("+refs/heads/*:refs/remotes/{name}/*");
    RemoteConfig::new(edit.document_mut())
        .add(name.as_bytes(), url.as_bytes(), &[fetch.as_bytes()])
        .wrap_err_with(|| format!("adding remote {name}"))?;
    edit.commit().wrap_err("writing local config")?;
    Ok(ExitCode::SUCCESS)
}

fn remote_show(name: &str, context: &Context) -> Result<ExitCode> {
    if context.git {
        bail!("--git is not supported for `remote show` (git contacts the remote)");
    }
    let repository = context.repository()?;
    let remote = find_remote(&repository, name)?;
    let mut out = stdout();
    writeln!(out, "name\t{name}")?;
    for url in remote.urls() {
        writeln!(out, "url\t{}", display_bytes(url))?;
    }
    for url in push_urls(&remote) {
        writeln!(out, "pushurl\t{}", display_bytes(url))?;
    }
    for spec in remote.fetch_refspecs().specs() {
        writeln!(out, "fetch\t{}", display_bytes(spec.as_bytes()))?;
    }
    for spec in remote.push_refspecs().specs() {
        writeln!(out, "push\t{}", display_bytes(spec.as_bytes()))?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn remote_heads(target: &str, context: &Context) -> Result<ExitCode> {
    // A configured remote name wins; otherwise the argument is a URL or path.
    let url = context
        .repository()
        .ok()
        .and_then(|repository| {
            Remote::find(repository.config(), target.as_bytes())
                .ok()
                .flatten()
                .and_then(|remote| remote.fetch_url().map(<[u8]>::to_vec))
        })
        .map(|url| String::from_utf8_lossy(&url).into_owned())
        .unwrap_or_else(|| target.to_owned());
    let discovery = discover(&url, context)?;
    let mut out = stdout();
    for reference in &discovery.advertisement.refs {
        write!(out, "{}\t", reference.id)?;
        out.write_all(reference.name.as_bytes())?;
        if reference.kind == AdvertisementKind::PeeledHint {
            out.write_all(b"^{}")?;
        }
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The transport a URL selects.
pub enum Endpoint {
    /// A local repository path.
    Local(PathBuf),
    /// An `http://` or `https://` URL.
    Http(String),
    /// A system OpenSSH endpoint.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Ssh(String),
}

/// Classifies a URL or path; local paths are resolved against `base`.
pub fn endpoint(url: &str, base: &Path) -> Result<Endpoint> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(Endpoint::Http(url.to_owned()));
    }
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(Endpoint::Local(PathBuf::from(path)));
    }
    let scp_like = url
        .split_once(':')
        .is_some_and(|(host, _)| !host.contains('/'));
    if url.starts_with("ssh://") || (!url.contains("://") && scp_like) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        return Ok(Endpoint::Ssh(url.to_owned()));
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        bail!("SSH requires macOS or Linux");
    }
    if url.contains("://") {
        bail!("girt CLI does not support this transport yet (only local paths, HTTP(S) and SSH)");
    }
    Ok(Endpoint::Local(base.join(url)))
}

/// Builds an anonymous HTTP endpoint.
pub fn http_remote(url: &str) -> Result<girt::transport::http::HttpRemote> {
    girt::transport::http::HttpRemote::new(url, &[], &[]).wrap_err("configuring HTTP remote")
}

/// Runs a future to completion on a fresh multi-threaded runtime.
pub fn block_on<F: Future>(future: F) -> Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .wrap_err("starting async runtime")?;
    Ok(runtime.block_on(future))
}

fn discover(url: &str, context: &Context) -> Result<RemoteDiscovery> {
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let limits = FetchLimits::trusted();
    match endpoint(url, context.directory())? {
        Endpoint::Local(path) => girt::fetch::discover_local(&path, limits, control)
            .wrap_err_with(|| format!("listing {}", path.display())),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Endpoint::Ssh(url) => {
            let config = match context.repository() {
                Ok(repository) => repository.config().clone(),
                Err(_) => context.user_config()?,
            };
            let remote = ssh_remote(&url, &config)?;
            block_on(girt::fetch::discover_ssh(&remote, limits, control))?
                .wrap_err("listing SSH remote")
        }
        Endpoint::Http(url) => {
            let remote = http_remote(&url)?;
            block_on(girt::fetch::discover_http(&remote, limits, control))?
                .wrap_err_with(|| format!("listing {url}"))
        }
    }
}

/// Use ordinary OpenSSH policy, including the same trusted shell overrides as workflow transfers.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn ssh_remote(url: &str, config: &girt::Config) -> Result<girt::transport::ssh::SshRemote> {
    use std::os::unix::ffi::OsStrExt as _;

    use girt::remote::ProtocolEnvironment;
    use girt::transport::ssh::{ApprovedSshCommand, OpenSshOptions, SshRemote};

    // Build the destination through the library so protocol restrictions and URL rewrites apply.
    let mut document = girt::config::Document::parse(b"")?;
    document.append("remote", Some(b"source"), "url", url.as_bytes())?;
    let remote = Remote::find(document.config(), b"source")?.expect("source remote");
    let policy = ProtocolEnvironment::from_environment(|name| {
        std::env::var_os(name).map(|value| value.as_bytes().to_vec())
    })?;
    let destination = remote.fetch_destination(config, &policy)?;
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    let command = environment
        .get(std::ffi::OsStr::new("GIT_SSH_COMMAND"))
        .map(|value| value.as_bytes().to_vec())
        .or_else(|| {
            config
                .string("core", None, "sshcommand")
                .map(<[u8]>::to_vec)
        });
    let selected = command.map(|value| (value.clone(), value)).or_else(|| {
        environment
            .get(std::ffi::OsStr::new("GIT_SSH"))
            .map(|value| {
                let raw = value.as_bytes().to_vec();
                let quoted = format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"));
                (raw, quoted.into_bytes())
            })
    });
    let approved = selected
        .map(|(configured, command)| {
            let command = String::from_utf8(command).wrap_err("SSH command must be UTF-8")?;
            Ok::<_, color_eyre::Report>(ApprovedSshCommand::new(
                configured,
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    format!("{command} \"$@\"").into(),
                    command.into(),
                ],
            ))
        })
        .transpose()?;
    let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("ssh"))
        .find(|path| path.is_absolute() && path.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/ssh"));
    environment.remove(std::ffi::OsStr::new("GIT_PROTOCOL"));
    SshRemote::openssh(
        config,
        &destination,
        OpenSshOptions::default()
            .with_default_executable(executable)
            .with_approved_command(approved)
            .with_environment(environment),
    )
    .wrap_err("configuring SSH remote")
}
