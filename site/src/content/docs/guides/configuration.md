---
title: Configuration and provenance
description: Select configuration sources, explain effective settings, and edit a direct file.
---

A tool needs to decide which configuration sources it trusts and reads before interpreting settings.
girt keeps three operations separate: parse supplied bytes, resolve explicit sources and includes,
and edit one direct file while holding its lock. An effective snapshot can contain many files; it
cannot safely be written back as though it were one source document.

## Select sources and publish a local override

In a Cargo binary project, run `cargo add girt tempfile` and put this program in `src/main.rs`. It
supplies a disposable global source, opens a repository with it, writes a local override, and
explains the winning source:

```rust
use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
use girt::{InitKind, ObjectFormat, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("project.git"),
        InitKind::Bare,
    )?;
    let global = directory.path().join("global");
    std::fs::write(&global, b"[core]\neditor = user-editor\n")?;
    let inputs =
        ConfigInputs::default().with_files(vec![ConfigFile::new(global, ConfigScope::Global)]);
    let opened = Repository::open_with_config(repo.git_dir(), &inputs)?;
    assert_eq!(
        opened.config().string("core", None, "editor"),
        Some(&b"user-editor"[..])
    );

    let mut edit = opened.edit_config(4096)?;
    edit.document_mut()
        .append("core", None, "editor", b"project-editor")?;
    edit.commit()?;
    // Publication does not mutate a previously opened configuration snapshot.
    assert_eq!(
        opened.config().string("core", None, "editor"),
        Some(&b"user-editor"[..])
    );

    let refreshed = Repository::open_with_config(repo.git_dir(), &inputs)?;
    assert_eq!(
        refreshed.config().string("core", None, "editor"),
        Some(&b"project-editor"[..])
    );
    let winning = refreshed
        .config()
        .entries()
        .iter()
        .rev()
        .find(|entry| {
            entry.section.eq_ignore_ascii_case(b"core")
                && entry.subsection.is_none()
                && entry.name.eq_ignore_ascii_case(b"editor")
        })
        .and_then(|entry| entry.origin.as_ref())
        .ok_or("origin missing")?;
    assert_eq!(winning.scope, ConfigScope::Local);
    assert_eq!(
        winning.location.path.as_deref(),
        Some(refreshed.common_dir().join("config").as_path())
    );
    Ok(())
}
```

Run it with `cargo run`. Everything is removed with the temporary directory. The example treats an
editor setting as data; it does not execute it. An application that launches commands needs its own
trust and execution policy.

Appending a local occurrence makes it the scalar winner while preserving earlier occurrences. Use
`Document::set_value` to change a particular existing occurrence identified in the current draft.

## Explain precedence before editing

File scopes run from system and global through local and enabled worktree configuration, followed by
supplied environment and command overrides. Within a scope, caller order remains significant; select
XDG configuration before the home file. Ordinary includes expand in place. Repository format
bootstrap stays separate: an override cannot reinterpret SHA-1 storage as SHA-256.

Use `entries()` and each entry's origin to explain where occurrences came from. Scalar lookup takes
the last value, but repeated variables can have different consumer rules. URLs and refspecs should
be interpreted by their owning APIs rather than flattened indiscriminately. Scope describes
precedence, not permission to execute or trust a value.

An absent setting, an implicit assignment, and an explicit empty value are distinct. Boolean lookup
interprets implicit as true and empty as false; string lookup treats implicit as absent. A
successful parse does not establish that a value is a valid boolean, integer, URL, or safe command.
Preserve a typed lookup error instead of silently substituting a default.

## Supply environment and include context explicitly

Use `ConfigInputs::from_environment` with an application-owned environment snapshot to select
system/global files and runtime assignments. Use `Config::from_environment` only to decode
`GIT_CONFIG_COUNT` and numbered `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` pairs, where `n` starts at
zero. Neither reads or mutates process-global state itself.

Conditional includes can depend on branch names, logical/canonical Git-directory aliases, or remote
URLs. Expansion of home and prefix paths needs explicit context. Supply the facts the application
knows, then resolve again when those facts change. Default resolution matches ordinary include
placement. An adapter that requires each top-level source's URL conditions to ignore other top-level
sources can select `IncludeConditionVisibility::RootSnapshot` explicitly. It can select different
conditional includes from ordinary resolution. See the
[configuration module](https://docs.rs/girt/latest/girt/config/index.html) for those policies and
limits.

## Edit the intended source and recover deliberately

The repository edit entry point selects its common local config. To edit an approved global or other
direct file, use `ConfigEdit::open` on that path. Includes are not followed for editing. `Document`
retains comments and unrelated syntax; its occurrence indices and section ordinals refer to the
current draft. Reacquire them after edits. Use `require_source` when publication should depend on
the exact bytes or absence observed earlier.

A lock conflict does not prove a stale lock. A changed-source error means reread and reconsider the
edit. Syntax errors require correcting the source or draft; a budget refusal requires an explicit
size and trust decision. A failed cleanup needs inspection of the owned residual lock. Explicit
`abort` makes cleanup failure observable, whereas drop performs best-effort cleanup. Publication
does not promise power-loss durability or an atomic transaction across several files.

After a successful commit, reopen or re-resolve before constructing dependent values. Existing
repository, configuration, and remote values remain snapshots. The
[ConfigEdit contract](https://docs.rs/girt/latest/girt/config/struct.ConfigEdit.html) describes the
filesystem assumptions and error evidence that a retry must preserve.
