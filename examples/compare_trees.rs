//! Compare explicit tree IDs: `cargo run --example compare_trees -- REPO OLD NEW`.
//! Use `-` for either empty side. Without arguments, compare two disposable sample trees.
use std::sync::atomic::AtomicBool;

use girt::{
    EntryMode, InitKind, ObjectId, PackLimits, Repository, Tree, TreeCompareLimits, TreeEntry,
    TreeValue,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let temporary;
    let (repo, old, new) = if args.is_empty() {
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
    } else {
        if args.len() != 3 {
            return Err("expected REPO OLD-TREE-ID NEW-TREE-ID; use - for empty".into());
        }
        (
            Repository::open(&args[0])?,
            parse_side(&args[1])?,
            parse_side(&args[2])?,
        )
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

fn parse_side(text: &str) -> Result<Option<ObjectId>, girt::ParseObjectIdError> {
    if text == "-" {
        Ok(None)
    } else {
        text.parse().map(Some)
    }
}

fn describe(value: Option<TreeValue>) -> String {
    match value {
        Some(value) => format!("{:?} {}", value.mode, value.id),
        None => "absent".into(),
    }
}
