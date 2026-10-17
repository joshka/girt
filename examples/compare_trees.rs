//! Compare explicit tree IDs: `cargo run --example compare_trees -- REPO OLD NEW`.
//! Use `-` for either empty side. Without arguments, compare two disposable sample trees.
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use girt::{
    EntryMode, InitKind, ObjectId, PackLimits, Repository, Tree, TreeCompareLimits, TreeEntry,
    TreeValue,
};

/// One side of a comparison: a tree ID, or `-` for an empty side.
#[derive(Clone, Copy, Debug)]
struct Side(Option<ObjectId>);

impl FromStr for Side {
    type Err = girt::ParseObjectIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "-" {
            return Ok(Self(None));
        }
        let id = text.parse()?;
        Ok(Self(Some(id)))
    }
}

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open. Omit all arguments to compare two disposable sample trees.
    #[arg(requires = "old", requires = "new")]
    repo: Option<PathBuf>,
    /// Old tree ID, or `-` for an empty side.
    old: Option<Side>,
    /// New tree ID, or `-` for an empty side.
    new: Option<Side>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let temporary;
    let (repo, old, new) =
        if let (Some(path), Some(old), Some(new)) = (args.repo, args.old, args.new) {
            (Repository::open(path)?, old.0, new.0)
        } else {
            temporary = tempfile::tempdir()?;
            let repo = Repository::init(
                girt::ObjectFormat::Sha1,
                temporary.path().join("sample"),
                InitKind::Bare,
            )?;
            let store = repo.loose_objects();
            let old_blob = store.write_blob(b"hello\n")?;
            let new_blob = store.write_blob(b"hello, trees\n")?;
            let old = store.write_tree(&Tree::new(
                girt::ObjectFormat::Sha1,
                vec![TreeEntry {
                    name: b"hello.txt".to_vec(),
                    mode: EntryMode::Blob,
                    id: old_blob,
                }],
            )?)?;
            let new = store.write_tree(&Tree::new(
                girt::ObjectFormat::Sha1,
                vec![TreeEntry {
                    name: b"hello.txt".to_vec(),
                    mode: EntryMode::Executable,
                    id: new_blob,
                }],
            )?)?;
            (repo, Some(old), Some(new))
        };
    let objects = repo.objects(PackLimits::default())?;
    let changes = objects.compare_trees(
        old,
        new,
        TreeCompareLimits::default(),
        &AtomicBool::new(false),
    )?;
    for change in changes {
        // Escaping keeps non-UTF-8 names and control bytes distinguishable on a text terminal.
        let path: String = change
            .path
            .iter()
            .flat_map(|byte| std::ascii::escape_default(*byte))
            .map(char::from)
            .collect();
        println!(
            "{path}: {} -> {}",
            describe(change.old),
            describe(change.new)
        );
    }
    Ok(())
}

fn describe(value: Option<TreeValue>) -> String {
    match value {
        Some(value) => format!("{:?} {}", value.mode, value.id),
        None => "absent".into(),
    }
}
