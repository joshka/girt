//! Run without arguments for a disposable fixture, or pass an existing repository path.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Parser, ValueEnum};
use girt::config::ConfigInputs;
use girt::status::{Baseline, Limits, Untracked, UntrackedFiles, UntrackedKind};
use girt::workflow::Environment;
use girt::{InitKind, Repository};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Existing repository to inspect. Omit to use a disposable fixture.
    path: Option<PathBuf>,
    /// Which untracked paths to list after applying ignore rules.
    #[arg(long, value_enum, default_value_t = Listing::Collapsed)]
    untracked: Listing,
    /// Lift the default status limits, for a large repository you trust.
    #[arg(long)]
    trusted: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Listing {
    Omit,
    All,
    Collapsed,
    /// Raw status: every untracked leaf, ignored or not.
    Raw,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let temp = tempfile::tempdir()?;
    let environment = Environment::from_process();

    let repository = match args.path {
        // Read configuration as `git` does, so `core.excludesFile` and `core.ignoreCase` apply.
        Some(path) => {
            Repository::discover_with_config(path, &ConfigInputs::git_default(&environment)?)?
        }
        None => fixture(&temp)?,
    };

    let limits = if args.trusted {
        trusted_limits()
    } else {
        Limits::default()
    };

    let untracked = match args.untracked {
        Listing::Omit => UntrackedFiles::Omit,
        Listing::All => UntrackedFiles::All,
        Listing::Collapsed => UntrackedFiles::Collapsed,
        Listing::Raw => {
            let report = repository.raw_status_with(
                Baseline::Head,
                Untracked::RawFilesWithoutIgnores,
                &girt::pathspec::Pathspec::all(),
                limits,
                &AtomicBool::new(false),
            )?;
            println!("{} raw untracked leaves", report.raw_untracked.len());
            return Ok(());
        }
    };

    let rules = repository.ignore_rules(&environment)?;
    let report = repository.status_with(
        Baseline::Head,
        untracked,
        &rules,
        &girt::pathspec::Pathspec::all(),
        limits,
        &AtomicBool::new(false),
    )?;

    println!("Tracked content is compared as literal bytes and POSIX modes.");

    for change in &report.staged {
        println!("staged    {}", String::from_utf8_lossy(&change.path));
    }

    for change in &report.unstaged {
        println!("unstaged  {}", String::from_utf8_lossy(&change.path));
    }

    for entry in &report.untracked {
        let suffix = if entry.kind == UntrackedKind::File {
            ""
        } else {
            "/"
        };
        println!("untracked {}{suffix}", String::from_utf8_lossy(&entry.path));
    }

    Ok(())
}

fn trusted_limits() -> Limits {
    let unbounded = usize::MAX / 4;
    let defaults = Limits::default();

    let mut index = defaults.index;
    index.max_bytes = unbounded;
    index.max_entries = unbounded;

    let mut trees = defaults.trees;
    trees.max_trees = unbounded;
    trees.max_tree_bytes = unbounded;
    trees.max_path_bytes = unbounded;
    trees.max_entries = unbounded;
    trees.max_changes = unbounded;

    defaults
        .with_index(index)
        .with_trees(trees)
        .with_packs(girt::PackLimits::trusted())
        .with_read(girt::ReadLimits::trusted())
        .with_max_object_bytes(unbounded)
        .with_max_worktree_bytes(unbounded)
        .with_max_directory_entries(unbounded)
        .with_max_path_bytes(unbounded)
}

fn fixture(temp: &tempfile::TempDir) -> Result<Repository, Box<dyn std::error::Error>> {
    let repository = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("repo"),
        InitKind::Worktree,
    )?;
    let id = repository.loose_objects().write_blob(b"staged\n")?;
    let mut edit = repository.edit_index(Default::default())?;
    edit.replace_entries(vec![girt::index::Entry::new(
        b"hello.txt".to_vec(),
        girt::index::Mode::Regular,
        id,
    )])?;
    edit.commit()?;

    let worktree = repository.worktree().unwrap();
    std::fs::write(worktree.join("hello.txt"), b"working\n")?;
    std::fs::create_dir(worktree.join("target"))?;
    std::fs::write(worktree.join("target/app"), b"build output\n")?;
    std::fs::write(worktree.join(".gitignore"), b"/target/\n")?;

    Ok(repository)
}
