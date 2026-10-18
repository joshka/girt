//! Print the `git diff` patch between two revisions: `cargo run --example patch`.
//!
//! Run without arguments for a disposable fixture, or pass a repository and two revisions.
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use girt::config::ConfigInputs;
use girt::content_diff::DiffLimits;
use girt::patch::{Limits, Options};
use girt::revision::Resolver;
use girt::tree::TreeEditor;
use girt::workflow::Environment;
use girt::{
    InitKind, ObjectFormat, ObjectKind, PackLimits, ReadLimits, Repo, Repository, rewrites,
};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Existing repository. Omit to use a disposable fixture.
    path: Option<PathBuf>,
    /// Old revision.
    #[arg(default_value = "HEAD~1")]
    old: String,
    /// New revision.
    #[arg(default_value = "HEAD")]
    new: String,
    /// Unchanged lines around each change, as `git diff --unified`.
    #[arg(short = 'U', long, default_value_t = 3)]
    unified: usize,
    /// Lift the default storage and content limits, for a large repository you trust.
    #[arg(long)]
    trusted: bool,
    /// Disable indentation scoring while retaining change-group compaction.
    #[arg(long)]
    no_indent_heuristic: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let temp = tempfile::tempdir()?;
    let mut out = io::BufWriter::new(io::stdout().lock());

    let Some(path) = args.path else {
        // The fixture compares two trees directly; no commits are needed.
        let repo = Repository::init(ObjectFormat::Sha1, temp.path().join("r"), InitKind::Bare)?;
        let loose = repo.loose_objects();
        let mut editor = TreeEditor::new(&loose);
        editor.set(b"notes.txt", loose.write_blob(b"one\ntwo\nthree\n")?)?;
        editor.set(b"old name", loose.write_blob(b"moved\ncontent\n")?)?;
        let old = editor.write()?;

        editor.set(b"notes.txt", loose.write_blob(b"one\n2\nthree")?)?;
        editor.remove(b"old name")?;
        editor.set(b"new name", loose.write_blob(b"moved\ncontent\n")?)?;
        let new = editor.write()?;

        let git = Repo::new(repo);
        git.write_patch(&mut out, old, new)?;
        out.flush()?;
        return Ok(());
    };

    if args.unified == 3 && !args.trusted && !args.no_indent_heuristic {
        Repo::discover(path)?.write_patch(&mut out, &args.old, &args.new)?;
        out.flush()?;
        return Ok(());
    }

    // Explicit options and limits go through the core API the handle composes.
    let environment = Environment::from_process();
    let repo = Repository::discover_with_config(path, &ConfigInputs::git_default(&environment)?)?;

    let (packs, rewrite_limits, limits) = if args.trusted {
        let diff = DiffLimits::default()
            .with_max_input_bytes(usize::MAX)
            .with_max_lines(usize::MAX)
            .with_max_trace(usize::MAX)
            .with_max_work(usize::MAX);

        // Keep the default rename candidate limit, which matches Git's, but lift resources.
        let rewrites = rewrites::Limits::default()
            .with_read(ReadLimits::trusted())
            .with_max_blob_bytes(usize::MAX)
            .with_max_spans(usize::MAX)
            .with_max_work(usize::MAX);

        (
            PackLimits::trusted(),
            rewrites,
            Limits::default()
                .with_read(ReadLimits::trusted())
                .with_diff(diff),
        )
    } else {
        (
            PackLimits::default(),
            rewrites::Limits::default(),
            Limits::default(),
        )
    };

    let objects = repo.objects_with(packs)?;
    let resolver = Resolver::new(&repo, &objects)?;
    let old = resolver.resolve_to(&args.old.parse()?, ObjectKind::Tree)?;
    let new = resolver.resolve_to(&args.new.parse()?, ObjectKind::Tree)?;

    let cancel = AtomicBool::new(false);
    let comparison = objects.diff_trees_with(
        Some(old),
        Some(new),
        rewrites::Options::default().with_track_empty(true),
        rewrite_limits,
        &cancel,
    )?;

    // Apply `.gitattributes` and `diff.<driver>` settings as `git diff` does.
    let mut attributes = repo.diff_attributes(&environment)?;
    objects.write_patch_with(
        &mut out,
        &comparison.entries,
        Options::default()
            .with_context_lines(args.unified)
            .with_indent_heuristic(!args.no_indent_heuristic),
        Some(&mut attributes),
        limits,
        &cancel,
    )?;

    out.flush()?;
    Ok(())
}
